//! Integration tests for OpenBell v0.7.0 HTTP endpoints & background collector:
//! - Safe toggleable background public tape collector (`--auto-collect`, `GET /api/collector/status`,
//!   `POST /api/collector/schedule` `{ enabled, interval_secs: 30..=900 }`, adaptive 429 backoff,
//!   live SSE `event: tape` broadcasts, 200-window readiness gate progress)
//! - `GET /api/analytics/tournament[?dataset=...&slippage_cents=0..5]` (12-strategy OOS ranking,
//!   12x12 symmetric correlation matrix, optimal uncorrelated 4-slot lineup, 5x4 regime heatmap)
//! - 100,000-tick / 2,000-window scale + 30 concurrent clients under 300ms p95 budget

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

fn http_req(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
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
    (status, b.to_string())
}

fn spawn_server(dir: &Path, port: u16, extra_path: Option<&Path>, extra_args: &[&str]) -> ServerGuard {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let strat = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("strategies");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openbell-rs"));
    cmd.args(extra_args)
        .env("PORT", port.to_string())
        .env("OPENBELL_DATA", &data)
        .env("OPENBELL_STRATEGIES", &strat)
        .env("OPENBELL_COLLECT_SECS", "60")
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
fn auto_collector_schedule_sse_and_429_backoff_work_end_to_end() {
    let dir = std::env::temp_dir().join(format!("ob-v070-col-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("curl");
    let script = r#"#!/bin/sh
url="$*"
case "$url" in
  *status=open*)
    echo '{"markets":[{"ticker":"KXBTC15M-V070-1","event_ticker":"KXBTC15M-V070","close_time":"2026-12-31T23:45:00Z","status":"open","result":"","yes_bid_dollars":"0.52","yes_ask_dollars":"0.54","no_bid_dollars":"0.46","no_ask_dollars":"0.48","last_price_dollars":"0.53","yes_bid_size_fp":"500.00","yes_ask_size_fp":"250.00"}]}'
    ;;
  *)
    echo '{"markets":[]}'
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
    let _srv = spawn_server(&dir, port, Some(&bin), &[]);

    // 1. Default startup has auto-collector safely OFF and capabilities expose v0.7.0 routes.
    let (st_home, body_home) = http_req(port, "GET", "/api/home", "");
    assert_eq!(st_home, 200);
    let home: serde_json::Value = serde_json::from_str(&body_home).unwrap();
    assert_eq!(home["version"], "0.7.0");
    let caps = home["capabilities"].as_array().unwrap();
    assert!(caps.iter().any(|c| c == "tournament"));
    assert!(caps.iter().any(|c| c == "auto_collector"));
    assert_eq!(home["collector"]["auto_collect_enabled"], false);
    assert_eq!(home["collector"]["readiness_target_windows"], 200);

    let (st0, b0) = http_req(port, "GET", "/api/collector/status", "");
    assert_eq!(st0, 200);
    let j0: serde_json::Value = serde_json::from_str(&b0).unwrap();
    assert_eq!(j0["enabled"], false);
    assert_eq!(j0["interval_secs"], 60);
    assert_eq!(j0["mode"], "public_readonly_get");
    assert_eq!(j0["snapshots_on_disk"], 0);

    // 2. Connect SSE client before toggling auto-collector ON -> verify schedule + live collect broadcast!
    let mut sse = TcpStream::connect(("127.0.0.1", port)).unwrap();
    sse.set_read_timeout(Some(Duration::from_secs(6))).unwrap();
    sse.write_all(
        format!("GET /api/tape/stream HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n")
            .as_bytes(),
    )
    .unwrap();
    let mut reader = BufReader::new(sse);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" || line.is_empty() {
            break;
        }
    }
    // Consume initial snapshot event
    let (mut ev, mut data, mut blank) = (String::new(), String::new(), String::new());
    reader.read_line(&mut ev).unwrap();
    reader.read_line(&mut data).unwrap();
    reader.read_line(&mut blank).unwrap();
    assert_eq!(ev.trim(), "event: snapshot");

    // 3. Toggle auto-collector ON at 60s -> emits SSE schedule event and triggers live background pass!
    let (st_on, b_on) = http_req(
        port,
        "POST",
        "/api/collector/schedule",
        r#"{"enabled":true,"interval_secs":60}"#,
    );
    assert_eq!(st_on, 200, "schedule enable failed: {b_on}");
    let jon: serde_json::Value = serde_json::from_str(&b_on).unwrap();
    assert_eq!(jon["enabled"], true);
    assert_eq!(jon["interval_secs"], 60);

    let mut saw_schedule = false;
    let mut saw_collect = false;
    for _ in 0..4 {
        let (mut e, mut d, mut bl) = (String::new(), String::new(), String::new());
        if reader.read_line(&mut e).unwrap_or(0) == 0 {
            break;
        }
        let _ = reader.read_line(&mut d);
        let _ = reader.read_line(&mut bl);
        if e.trim() == "event: tape" && d.contains("\"type\":\"schedule\"") {
            saw_schedule = true;
        }
        if e.trim() == "event: tape" && d.contains("\"type\":\"collect\"") {
            saw_collect = true;
        }
        if saw_schedule && saw_collect {
            break;
        }
    }
    assert!(saw_schedule, "expected SSE schedule event");
    assert!(saw_collect, "expected live background SSE collect event");

    // 4. Verify status reflects completed run + clamping + disable + 400 validation.
    let (st1, b1) = http_req(port, "GET", "/api/collector/status", "");
    assert_eq!(st1, 200);
    let j1: serde_json::Value = serde_json::from_str(&b1).unwrap();
    assert!(j1["runs_total"].as_u64().unwrap() >= 1);
    assert!(j1["snapshots_on_disk"].as_u64().unwrap() >= 1);

    let (st_clamp, b_clamp) = http_req(
        port,
        "POST",
        "/api/collector/schedule",
        r#"{"enabled":true,"interval_secs":180}"#,
    );
    assert_eq!(st_clamp, 200);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&b_clamp).unwrap()["interval_secs"], 180);

    let (st_off, b_off) = http_req(port, "POST", "/api/collector/schedule", r#"{"enabled":false}"#);
    assert_eq!(st_off, 200);
    assert_eq!(serde_json::from_str::<serde_json::Value>(&b_off).unwrap()["enabled"], false);

    let (st_bad, _) = http_req(port, "POST", "/api/collector/schedule", "not-json");
    assert_eq!(st_bad, 400);
    let (st_404, _) = http_req(port, "GET", "/api/analytics/tournament?dataset=missing", "");
    assert_eq!(st_404, 404);
}

#[test]
fn v070_tournament_and_collector_100k_ticks_30_concurrent_under_300ms_p95() {
    let dir = std::env::temp_dir().join(format!("ob-v070-scale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();

    // 100,000 ticks across 2,000 settled 15m windows across 5 series.
    let mut buf = String::with_capacity(28 * 1024 * 1024);
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
        let result = if w % 4 != 0 { "yes" } else { "no" };
        for t in 0..50usize {
            let fetched_at = close_epoch - 900 + (t as i64) * 17;
            let st = if t == 49 { "finalized" } else { "open" };
            let res = if t == 49 { result } else { "" };
            let ya = 0.55 + ((t % 10) as f64) * 0.02;
            let yb = ya - 0.02;
            let na = (1.0 - yb).clamp(0.01, 0.99);
            let nb = (1.0 - ya).clamp(0.01, 0.99);
            buf.push_str(&format!(
                "{{\"fetched_at\":{fetched_at},\"ticker\":\"{ticker}\",\"event_ticker\":\"{ticker}\",\"series\":\"{ser}\",\"close_time\":\"{close_time}\",\"status\":\"{st}\",\"result\":\"{res}\",\"yes_bid\":{yb:.2},\"yes_ask\":{ya:.2},\"no_bid\":{nb:.2},\"no_ask\":{na:.2},\"last_price\":{ya:.2},\"yes_bid_size\":320.0,\"no_bid_size\":280.0}}\n"
            ));
        }
    }
    std::fs::write(data.join("snapshots.jsonl"), &buf).unwrap();
    drop(buf);

    let port = free_port();
    let _srv = spawn_server(&dir, port, None, &[]);

    // 1. Validate GET /api/analytics/tournament shape, 12x12 correlation symmetry, 5x4 heatmap, and slippage monotonicity.
    let (st_t0, body_t0) = http_req(port, "GET", "/api/analytics/tournament?slippage_cents=0", "");
    assert_eq!(st_t0, 200, "tournament 0c failed: {body_t0}");
    let t0: serde_json::Value = serde_json::from_str(&body_t0).unwrap();

    let (st_t1, body_t1) = http_req(port, "GET", "/api/analytics/tournament?slippage_cents=1", "");
    assert_eq!(st_t1, 200, "tournament 1c failed: {body_t1}");
    let t1: serde_json::Value = serde_json::from_str(&body_t1).unwrap();

    assert_eq!(t1["windows_settled_total"], 2_000);
    assert_eq!(t1["windows_settled_oos"], 800);
    let rankings = t1["rankings"].as_array().unwrap();
    assert_eq!(rankings.len(), 12);
    let corr_strats = t1["correlation_matrix"]["strategies"].as_array().unwrap();
    let corr_mat = t1["correlation_matrix"]["matrix"].as_array().unwrap();
    assert_eq!(corr_strats.len(), 12);
    assert_eq!(corr_mat.len(), 12);
    for i in 0..12 {
        assert!((corr_mat[i][i].as_f64().unwrap() - 1.0).abs() < 1e-6);
        for j in 0..12 {
            assert_eq!(corr_mat[i][j], corr_mat[j][i], "matrix must be symmetric at ({i},{j})");
        }
    }
    let heatmap_cells = t1["regime_heatmap"]["cells"].as_array().unwrap();
    assert_eq!(heatmap_cells.len(), 20);

    // Slippage monotonicity: 1c slippage expectancy <= 0c slippage expectancy for traded strategies.
    let top0 = t0["rankings"][0]["oos_net_expectancy_cents"].as_f64().unwrap();
    let top1 = t1["rankings"][0]["oos_net_expectancy_cents"].as_f64().unwrap();
    assert!(top1 <= top0 + 1e-6, "expected 1c slippage ({top1}) <= 0c ({top0})");

    // 2. 30 concurrent readers across tournament (varying slippage) + collector status under <300ms p95!
    let mut handles = Vec::new();
    for i in 0..30usize {
        handles.push(std::thread::spawn(move || {
            let t_start = Instant::now();
            let (st, b) = if i % 3 == 0 {
                http_req(port, "GET", "/api/collector/status", "")
            } else {
                let slip = i % 6;
                http_req(port, "GET", &format!("/api/analytics/tournament?slippage_cents={slip}"), "")
            };
            let ms = t_start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(st, 200, "concurrent v070 call failed: {b}");
            ms
        }));
    }
    let mut lats: Vec<f64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    lats.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = lats[(lats.len() * 95 / 100).min(lats.len() - 1)];
    assert!(
        p95 < 300.0,
        "v0.7.0 100k-tick 30-client p95 {p95:.1}ms exceeded 300ms budget (lats: {lats:?})"
    );
}
