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
    // Let the initial startup collect pass finish.
    std::thread::sleep(Duration::from_millis(400));

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
    let mut buf = String::with_capacity(24 * 1024 * 1024);
    let base_epoch: i64 = 1_760_000_000;
    let series = ["KXBTC15M", "KXETH15M", "KXSOL15M", "KXXRP15M", "KXDOGE15M"];
    for w in 0..2000usize {
        let ser = series[w % series.len()];
        let ticker = format!("{ser}-W{w:04}");
        let close_epoch = base_epoch + (w as i64) * 900;
        let close_time = format!("2026-10-{:02}T{:02}:{:02}:00Z", 1 + (w / 96) % 28, (w / 4) % 24, (w % 4) * 15);
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

    // Warm cache once (initial 100k-line parse + Rune scoring).
    let (st0, _, home0) = http_req(port, "GET", "/api/home", "");
    assert_eq!(st0, 200);
    let hj: serde_json::Value = serde_json::from_str(&home0).unwrap();
    assert_eq!(hj["collector"]["snapshots_on_disk"], 100_000);
    assert_eq!(hj["collector"]["settled_windows"], 2_000);

    // 30 concurrent readers across /api/home, /api/markets, /api/markets/history, /api/export/report.
    let mut handles = Vec::new();
    for i in 0..30usize {
        handles.push(std::thread::spawn(move || {
            let path = match i % 4 {
                0 => "/api/home".to_string(),
                1 => "/api/markets".to_string(),
                2 => format!("/api/markets/history?ticker=KXBTC15M-W{:04}", (i * 5) % 2000),
                _ => "/api/export/report?format=md".to_string(),
            };
            let t0 = Instant::now();
            let (st, _, body) = http_req(port, "GET", &path, "");
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(st, 200, "failed {path}: {body}");
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
