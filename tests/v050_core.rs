//! Integration tests for OpenBell v0.5.0 core server upgrades:
//! - True SSE stream `GET /api/tape/stream` (initial snapshot + live collect broadcast)
//! - Daily report export `GET /api/export/report?format=md|csv` (Line 1 venue truth preserved)
//! - Cross-pass settled deduplication on `POST /api/collect`
//! - 100,000-tick / 2,000-window scale + 30 concurrent readers latency budget (<300ms p95)

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ServerGuard {
    child: Child,
    dir: PathBuf,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn http_req(port: u16, method: &str, path: &str, body: &str) -> (u16, String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect server");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let req = if body.is_empty() {
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
    } else {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (hdr, b) = raw.split_once("\r\n\r\n").expect("valid HTTP response");
    let status: u16 = hdr
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, hdr.to_string(), b.to_string())
}

fn spawn_server(dir: &Path, port: u16, extra_path: Option<&Path>) -> ServerGuard {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let strat = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("strategies");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openbell-rs"));
    cmd.env("PORT", port.to_string())
        .env("OPENBELL_DATA", &data)
        .env("OPENBELL_STRATEGIES", &strat)
        .env("OPENBELL_COLLECT_SECS", "3600")
        .env("OPENBELL_COLLECT_PACE_MS", "0")
        .env("OPENBELL_UPDATE_MODE", "off")
        .env("OPENBELL_NO_OPEN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(bin_dir) = extra_path {
        let cur = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{cur}", bin_dir.display()));
    }
    let child = cmd.spawn().expect("spawn openbell-rs");
    let guard = ServerGuard {
        child,
        dir: dir.to_path_buf(),
    };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("openbell-rs did not start on port {port}");
}

#[test]
fn sse_tape_stream_dedup_and_report_exports_work_end_to_end() {
    let dir = std::env::temp_dir().join(format!("ob-v050-sse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("curl");
    let script = r#"#!/bin/sh
url="$*"
case "$url" in
  *status=open*)
    echo '{"markets":[{"ticker":"KXBTC15M-OPEN-1","event_ticker":"KXBTC15M-OPEN","close_time":"2026-12-31T23:45:00Z","status":"open","result":"","yes_bid_dollars":"0.52","yes_ask_dollars":"0.54","no_bid_dollars":"0.46","no_ask_dollars":"0.48","last_price_dollars":"0.53"}]}'
    ;;
  *)
    echo '{"markets":[{"ticker":"KXBTC15M-SET-1","event_ticker":"KXBTC15M-SET","close_time":"2026-11-20T12:15:00Z","status":"finalized","result":"yes","yes_bid_dollars":"0.99","yes_ask_dollars":"0.99","no_bid_dollars":"0.01","no_ask_dollars":"0.01","last_price_dollars":"0.99"}]}'
    ;;
esac
"#;
    std::fs::write(&shim, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let port = free_port();
    let _srv = spawn_server(&dir, port, Some(&bin));
    // Trigger pass #1 explicitly so the settled row is on disk before pass #2 tests dedup.
    let (st_seed, _, body_seed) = http_req(port, "POST", "/api/collect", "{}");
    assert_eq!(st_seed, 200, "initial collect pass failed: {body_seed}");

    // 1. Verify Daily Report exports preserve exact Line 1 venue word.
    let (st_rep, _, body_rep) = http_req(port, "GET", "/api/report", "");
    assert_eq!(st_rep, 200);
    let rep_json: serde_json::Value = serde_json::from_str(&body_rep).unwrap();
    let expected_line1 = rep_json["report"].as_str().unwrap().lines().next().unwrap();
    assert!(expected_line1.starts_with("Day P&L (venue):"));

    let (st_md, hdr_md, body_md) = http_req(port, "GET", "/api/export/report?format=md", "");
    assert_eq!(st_md, 200);
    assert!(hdr_md.to_ascii_lowercase().contains("text/markdown"));
    assert!(hdr_md.contains("openbell-daily-report.md"));
    assert_eq!(body_md.lines().next().unwrap(), expected_line1);

    let (st_csv, hdr_csv, body_csv) = http_req(port, "GET", "/api/export/report?format=csv", "");
    assert_eq!(st_csv, 200);
    assert!(hdr_csv.to_ascii_lowercase().contains("text/csv"));
    assert!(hdr_csv.contains("openbell-daily-report.csv"));
    assert_eq!(
        body_csv.lines().next().unwrap(),
        format!("# {expected_line1}")
    );
    assert!(body_csv.lines().nth(1).unwrap().starts_with("section,name,side,"));

    let (st_bad, _, _) = http_req(port, "GET", "/api/export/report?format=xml", "");
    assert_eq!(st_bad, 400);

    // 2. Connect real SSE client to /api/tape/stream and verify initial snapshot + live broadcast.
    let mut sse = TcpStream::connect(("127.0.0.1", port)).unwrap();
    sse.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    sse.write_all(
        format!("GET /api/tape/stream HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n")
            .as_bytes(),
    )
    .unwrap();
    let mut reader = BufReader::new(sse);
    let mut hdrs = String::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
        hdrs.push_str(&line);
    }
    assert!(hdrs.contains("200 OK"), "bad SSE status: {hdrs}");
    assert!(
        hdrs.to_ascii_lowercase().contains("text/event-stream"),
        "missing SSE content-type: {hdrs}"
    );
    let mut ev1 = String::new();
    let mut data1 = String::new();
    reader.read_line(&mut ev1).unwrap();
    reader.read_line(&mut data1).unwrap();
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap();
    assert_eq!(ev1.trim(), "event: snapshot");
    assert!(data1.starts_with("data: {") && data1.contains("\"collector\""));

    // 3. Trigger POST /api/collect -> verify settled deduplication AND live SSE `event: tape` frame!
    let (st_col, _, body_col) = http_req(port, "POST", "/api/collect", "{}");
    assert_eq!(st_col, 200, "collect failed: {body_col}");
    let col: serde_json::Value = serde_json::from_str(&body_col).unwrap();
    assert_eq!(col["snapshots_written"], 1, "only open snapshot written: {col}");
    assert_eq!(col["deduplicated"], 1, "immutable settled snapshot deduplicated: {col}");

    let mut ev2 = String::new();
    let mut data2 = String::new();
    reader.read_line(&mut ev2).unwrap();
    reader.read_line(&mut data2).unwrap();
    assert_eq!(ev2.trim(), "event: tape");
    assert!(
        data2.contains("\"snapshots_written\":1") && data2.contains("\"deduplicated\":1"),
        "unexpected SSE tape frame: {data2}"
    );
}

#[test]
fn scale_100k_ticks_2000_windows_30_concurrent_clients_under_300ms_p95() {
    let dir = std::env::temp_dir().join(format!("ob-v050-scale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();

    // Generate 100,000 ticks across 2,000 settled 15m windows (50 ticks per window).
    // 2026-10-01T00:00:00Z = 1_790_812_800; 2,000 * 15m = 20.83 days (Oct 1..21).
    let mut buf = String::with_capacity(24 * 1024 * 1024);
    let base_epoch: i64 = 1_790_812_800;
    let series = ["KXBTC15M", "KXETH15M", "KXSOL15M", "KXXRP15M", "KXDOGE15M"];
    for w in 0..2000usize {
        let ser = series[w % series.len()];
        let ticker = format!("{ser}-W{w:04}");
        let close_epoch = base_epoch + (w as i64) * 900;
        let close_time = format!(
            "2026-10-{:02}T{:02}:{:02}:00Z",
            1 + (w / 96),
            (w / 4) % 24,
            (w % 4) * 15
        );
        let result = if w % 3 != 0 { "yes" } else { "no" };
        for t in 0..50usize {
            let fetched_at = close_epoch - 900 + (t as i64) * 17;
            let st = if t == 49 { "finalized" } else { "open" };
            let res = if t == 49 { result } else { "" };
            let ya = 0.55 + ((t % 10) as f64) * 0.02;
            let yb = ya - 0.02;
            let na = (1.0 - yb).clamp(0.01, 0.99);
            let nb = (1.0 - ya).clamp(0.01, 0.99);
            buf.push_str(&format!(
                "{{\"fetched_at\":{fetched_at},\"ticker\":\"{ticker}\",\"event_ticker\":\"{ticker}\",\"series\":\"{ser}\",\"close_time\":\"{close_time}\",\"status\":\"{st}\",\"result\":\"{res}\",\"yes_bid\":{yb:.2},\"yes_ask\":{ya:.2},\"no_bid\":{nb:.2},\"no_ask\":{na:.2},\"last_price\":{ya:.2}}}\n"
            ));
        }
    }
    std::fs::write(data.join("snapshots.jsonl"), &buf).unwrap();
    drop(buf);

    // Block curl so startup collect does not touch the network.
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("curl");
    std::fs::write(&shim, "#!/bin/sh\necho '{\"error\":{\"code\":\"offline\"}}'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let port = free_port();
    let _srv = spawn_server(&dir, port, Some(&bin));

    // Warm cache once (initial 100k-line parse + 12-strategy Rune scoring + analytics).
    let (st0, _, home0) = http_req(port, "GET", "/api/home", "");
    assert_eq!(st0, 200);
    let hj: serde_json::Value = serde_json::from_str(&home0).unwrap();
    assert_eq!(hj["collector"]["snapshots_on_disk"], 100_000);
    assert_eq!(hj["collector"]["settled_windows"], 2_000);

    // Exercise & warm all 5 Quant API endpoints on the 100k-tick / 2,000-window tape.
    let (st_cal, _, body_cal) = http_req(port, "GET", "/api/analytics/calibration", "");
    assert_eq!(st_cal, 200, "calibration failed: {body_cal}");
    let cal: serde_json::Value = serde_json::from_str(&body_cal).unwrap();
    assert_eq!(cal["windows_settled"], 2_000);
    assert_eq!(cal["market_calibration"]["buckets"].as_array().unwrap().len(), 10);
    assert_eq!(cal["by_strategy"].as_array().unwrap().len(), 12);

    let (st_wf, _, body_wf) = http_req(port, "GET", "/api/analytics/walkforward", "");
    assert_eq!(st_wf, 200, "walkforward failed: {body_wf}");
    let wf: serde_json::Value = serde_json::from_str(&body_wf).unwrap();
    assert_eq!(wf["strategies"].as_array().unwrap().len(), 12);
    assert!(wf["portfolio"]["in_sample"]["trades_count"].as_u64().unwrap() > 0);
    assert!(wf["portfolio"]["out_of_sample"]["trades_count"].as_u64().unwrap() > 0);

    let mc_req = r#"{"paths":1000,"seed":20261122}"#;
    let (st_mc1, _, body_mc1) = http_req(port, "POST", "/api/analytics/montecarlo", mc_req);
    let (st_mc2, _, body_mc2) = http_req(port, "POST", "/api/analytics/montecarlo", mc_req);
    assert_eq!(st_mc1, 200, "montecarlo failed: {body_mc1}");
    assert_eq!(st_mc2, 200);
    assert_eq!(body_mc1, body_mc2, "seeded Monte Carlo must be byte-identical");
    let mc: serde_json::Value = serde_json::from_str(&body_mc1).unwrap();
    assert_eq!(mc["paths"], 1000);
    assert!(mc["equity_bands"].as_array().unwrap().len() >= 2);

    // Rune validator: valid, syntax error, and forbidden non-paper rejection.
    let valid_src = "pub fn name() { \"sandbox-probe\" }\npub fn side() { \"yes\" }\npub fn allow_entry(ya, na, secs) { ya >= 0.55 && ya <= 0.75 && secs <= 600 }\n";
    let (st_val_ok, _, body_val_ok) = http_req(
        port,
        "POST",
        "/api/strategy/validate",
        &serde_json::json!({ "source": valid_src }).to_string(),
    );
    assert_eq!(st_val_ok, 200);
    let val_ok: serde_json::Value = serde_json::from_str(&body_val_ok).unwrap();
    assert_eq!(val_ok["valid"], true);
    assert_eq!(val_ok["strategy_name"], "sandbox-probe");

    let (st_val_bad, _, body_val_bad) = http_req(
        port,
        "POST",
        "/api/strategy/validate",
        &serde_json::json!({ "source": "pub fn name() { \"bad\" }\npub fn side() { \"yes\" }\npub fn allow_entry(ya, na, secs) { let u = \"https://evil.example\"; ya > 0.5 }\n" }).to_string(),
    );
    assert_eq!(st_val_bad, 200);
    let val_bad: serde_json::Value = serde_json::from_str(&body_val_bad).unwrap();
    assert_eq!(val_bad["valid"], false);
    assert!(!val_bad["errors"].as_array().unwrap().is_empty());

    // In-memory sandbox backtest over all 2,000 settled windows.
    let bt_req = serde_json::json!({ "source": valid_src, "paths": 1000, "seed": 42 }).to_string();
    let (st_bt, _, body_bt) = http_req(port, "POST", "/api/backtest", &bt_req);
    assert_eq!(st_bt, 200, "backtest failed: {body_bt}");
    let bt: serde_json::Value = serde_json::from_str(&body_bt).unwrap();
    assert_eq!(bt["validation"]["valid"], true);
    assert_eq!(bt["windows_evaluated"], 2_000);
    assert!(bt["score"]["windows_traded"].as_u64().unwrap() > 100);
    assert!(bt["monte_carlo"]["paths"] == 1000);

    // 30 concurrent readers across core + Quant endpoints under 300ms p95 budget.
    let mut handles = Vec::new();
    for i in 0..30usize {
        let bt_payload = bt_req.clone();
        handles.push(std::thread::spawn(move || {
            let (method, path, payload) = match i % 7 {
                0 => ("GET", "/api/home".to_string(), String::new()),
                1 => ("GET", "/api/markets".to_string(), String::new()),
                2 => (
                    "GET",
                    format!("/api/markets/history?ticker=KXBTC15M-W{:04}", (i * 5) % 2000),
                    String::new(),
                ),
                3 => ("GET", "/api/analytics/calibration".to_string(), String::new()),
                4 => ("GET", "/api/analytics/walkforward".to_string(), String::new()),
                5 => (
                    "POST",
                    "/api/analytics/montecarlo".to_string(),
                    r#"{"paths":1000,"seed":20261122}"#.to_string(),
                ),
                _ => ("POST", "/api/backtest".to_string(), bt_payload),
            };
            let t0 = Instant::now();
            let (st, _, body) = http_req(port, method, &path, &payload);
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(st, 200, "failed {method} {path}: {body}");
            ms
        }));
    }
    let mut latencies: Vec<f64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    latencies.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = latencies[(latencies.len() * 95 / 100).min(latencies.len() - 1)];
    assert!(
        p95 < 300.0,
        "100k-tick 30-client p95 latency {p95:.1}ms exceeded 300ms budget (all: {latencies:?})"
    );
}
