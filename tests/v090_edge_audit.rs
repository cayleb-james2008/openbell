//! Integration tests for OpenBell v0.9.0:
//! - 1-click Full Quant Edge Audit (`GET /api/analytics/edge-audit`)
//! - Causal signal-bar truncation & 1-bar lookahead canary across strategies & tournament
//! - HTTP Content-Length & dataset query-parameter security hardening

use serde_json::Value;
use std::io::{Read, Write};
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

fn spawn_server(dir: &Path, port: u16) -> ServerGuard {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let strat = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("strategies");
    let child = Command::new(env!("CARGO_BIN_EXE_openbell-rs"))
        .arg("--no-open")
        .env("PORT", port.to_string())
        .env("OPENBELL_DATA", &data)
        .env("OPENBELL_STRATEGIES", &strat)
        .env("OPENBELL_COLLECT_SECS", "3600")
        .env("OPENBELL_UPDATE_MODE", "off")
        .env("OPENBELL_NO_OPEN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn openbell-rs");
    let guard = ServerGuard {
        child,
        dir: dir.to_path_buf(),
    };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!("openbell-rs did not start on port {port}");
}

fn iso_epoch(epoch: i64) -> String {
    // Simple UTC ISO formatter for test timestamps around 2026-11-22 (days from 1970-01-01).
    let days = epoch.div_euclid(86400);
    let secs = epoch.rem_euclid(86400);
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn snap_line(ticker: &str, fetched_at: i64, close_iso: &str, result: &str, ya: Option<f64>, na: Option<f64>) -> String {
    serde_json::json!({
        "fetched_at": fetched_at,
        "ticker": ticker,
        "event_ticker": ticker,
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": if result.is_empty() { "open" } else { "settled" },
        "result": result,
        "yes_bid": ya.map(|v| (v - 0.02).max(0.01)),
        "yes_ask": ya,
        "no_bid": na.map(|v| (v - 0.02).max(0.01)),
        "no_ask": na,
        "last_price": ya,
        "yes_bid_size": 120.0,
        "no_bid_size": 120.0,
    })
    .to_string()
}

#[test]
fn edge_audit_endpoint_and_security_hardening_work_end_to_end() {
    let dir = std::env::temp_dir().join(format!("ob-v090-audit-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let fixture_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend/e2e/fixtures/data/fixture-snapshots.jsonl");
    std::fs::copy(&fixture_src, data.join("fixture-snapshots.jsonl")).unwrap();

    let port = free_port();
    let _guard = spawn_server(&dir, port);

    // 1. Edge audit endpoint returns complete 5-stage Go/No-Go scorecard on fixture tape.
    let (st, body) = http_req(port, "GET", "/api/analytics/edge-audit?dataset=fixture&slippage_cents=1", "");
    assert_eq!(st, 200, "edge-audit status: {body}");
    assert!(!body.contains("NaN") && !body.contains("Infinity"));
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["dataset"], "fixture");
    assert_eq!(v["verdict"], "NO_GO", "fixture tape must never emit GO");
    assert_eq!(v["funnel"].as_array().unwrap().len(), 5);
    assert_eq!(v["readiness"]["gates"].as_array().unwrap().len(), 5);
    assert_eq!(v["strategies_total"].as_u64().unwrap(), 16);

    // 1b. Verify gzip compression, strong ETag / 304 Not Modified, Vary, and security headers on `/`.
    {
        let mut sg = TcpStream::connect(("127.0.0.1", port)).unwrap();
        sg.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sg.write_all(
            format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept-Encoding: gzip\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut raw_bytes = Vec::new();
        sg.read_to_end(&mut raw_bytes).unwrap();
        let split = raw_bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        let hdr = String::from_utf8_lossy(&raw_bytes[..split]).to_string();
        let gz_body = &raw_bytes[split + 4..];
        assert!(hdr.starts_with("HTTP/1.1 200"));
        assert!(hdr.to_ascii_lowercase().contains("content-encoding: gzip"));
        assert!(hdr.to_ascii_lowercase().contains("vary: accept-encoding"));
        assert!(hdr.to_ascii_lowercase().contains("x-content-type-options: nosniff"));
        assert!(hdr.to_ascii_lowercase().contains("x-frame-options: deny"));
        assert!(hdr.to_ascii_lowercase().contains("referrer-policy: no-referrer"));
        assert!(gz_body.len() < 600_000, "gzipped index must be slim, got {}", gz_body.len());

        let etag_line = hdr
            .lines()
            .find(|l| l.to_ascii_lowercase().starts_with("etag:"))
            .expect("ETag header present");
        let etag_val = etag_line.split_once(':').unwrap().1.trim();

        let mut s304 = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s304.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        s304.write_all(
            format!("GET / HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nIf-None-Match: {etag_val}\r\nConnection: close\r\n\r\n")
                .as_bytes(),
        )
        .unwrap();
        let mut raw_304 = String::new();
        s304.read_to_string(&mut raw_304).unwrap();
        assert!(raw_304.starts_with("HTTP/1.1 304"), "expected 304 Not Modified, got {raw_304}");
    }

    // 2. Security: `notdataset=fixture` must NOT match `dataset=fixture`.
    let (st_sub, body_sub) = http_req(port, "GET", "/api/markets?notdataset=fixture", "");
    assert_eq!(st_sub, 200);
    assert_eq!(body_sub, "[]", "live tape is empty; substring param must not hijack dataset");

    // 3. Security: malformed Content-Length must be rejected with 400, not coerced to 0.
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(
        format!(
            "POST /api/collect HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: notanumber\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    assert!(raw.starts_with("HTTP/1.1 400"), "malformed Content-Length must return 400, got: {raw}");
}

#[test]
fn causal_signal_bar_truncation_and_1bar_lookahead_canary_via_http() {
    let base_epoch = 1_765_000_000i64;
    let mut full_lines = Vec::new();
    let mut cut_lines = Vec::new();
    let mut canary_cut_lines = Vec::new();

    for i in 0..20 {
        let ce = base_epoch + i * 900;
        let ci = iso_epoch(ce);
        let tk = format!("KXBTC15M-CAUSAL-{i:02}");
        let res = if i % 4 == 0 { "no" } else { "yes" };

        // Bar 0 (t=-450s): outside favourite-late (<=300s) -> no entry
        let b0 = snap_line(&tk, ce - 450, &ci, "", Some(0.92), Some(0.10));
        // Bar 1 (t=-240s): SIGNAL BAR for favourite-late (yes_ask=0.78 <= 0.85, 61 <= 240 <= 300)
        let b1_signal = snap_line(&tk, ce - 240, &ci, "", Some(0.78), Some(0.24));
        // Bar 1 for canary (t=-240s): yes_ask=0.91 -> does NOT trigger entry
        let b1_no_signal = snap_line(&tk, ce - 240, &ci, "", Some(0.91), Some(0.11));
        // Bar 2 (t=-120s): future bar AFTER signal bar
        let b2_future = snap_line(&tk, ce - 120, &ci, "", Some(0.55), Some(0.47));
        // Bar 3 (t=+30s): settled result
        let b3_settle = snap_line(&tk, ce + 30, &ci, res, None, None);

        full_lines.extend([b0.clone(), b1_signal.clone(), b2_future, b3_settle.clone()]);
        cut_lines.extend([b0.clone(), b1_signal, b3_settle.clone()]);
        canary_cut_lines.extend([b0, b1_no_signal, b3_settle]);
    }

    let dir = std::env::temp_dir().join(format!("ob-v090-causal-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("snapshots.jsonl"), full_lines.join("\n") + "\n").unwrap();
    std::fs::write(data.join("cut-snapshots.jsonl"), cut_lines.join("\n") + "\n").unwrap();
    std::fs::write(data.join("canary-snapshots.jsonl"), canary_cut_lines.join("\n") + "\n").unwrap();

    let port = free_port();
    let _guard = spawn_server(&dir, port);

    let (st1, full_tour) = http_req(port, "GET", "/api/analytics/tournament?slippage_cents=1", "");
    let (st2, cut_tour) = http_req(port, "GET", "/api/analytics/tournament?dataset=cut&slippage_cents=1", "");
    let (st3, canary_tour) = http_req(port, "GET", "/api/analytics/tournament?dataset=canary&slippage_cents=1", "");
    assert_eq!((st1, st2, st3), (200, 200, 200));

    let v_full: Value = serde_json::from_str(&full_tour).unwrap();
    let v_cut: Value = serde_json::from_str(&cut_tour).unwrap();
    let v_canary: Value = serde_json::from_str(&canary_tour).unwrap();

    let find_row = |v: &Value, name: &str| -> Value {
        v["rankings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["strategy"] == name)
            .unwrap()
            .clone()
    };

    let fl_full = find_row(&v_full, "favourite-late");
    let fl_cut = find_row(&v_cut, "favourite-late");
    assert_eq!(fl_full["total_trades"], 20);
    assert_eq!(
        fl_full["oos_net_expectancy_cents"], fl_cut["oos_net_expectancy_cents"],
        "truncating future bars after signal bar must yield identical OOS expectancy"
    );
    assert_eq!(fl_full["oos_trades"], fl_cut["oos_trades"]);

    // 1-bar lookahead canary: with signal bar withheld, cutting before the future bar MUST have 0 trades.
    let fl_canary = find_row(&v_canary, "favourite-late");
    assert_eq!(
        fl_canary["total_trades"], 0,
        "1-bar lookahead canary failed: strategy entered without seeing future bar"
    );
}

#[cfg(unix)]
#[test]
fn cwe59_symlinks_on_tape_and_strategy_paths_are_rejected() {
    use std::os::unix::fs::symlink;

    let base = std::env::temp_dir().join(format!("ob-v090-cwe59-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let data = base.join("data");
    let strats = base.join("strategies");
    let outside = base.join("outside");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&strats).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let outside_tape = outside.join("outside_canary_tape.jsonl");
    std::fs::write(
        &outside_tape,
        "{\"ticker\":\"CANARY_OUTSIDE_SECRET_DO_NOT_READ\",\"fetched_at\":1765000000,\"close_time\":\"2025-12-06T00:00:00Z\",\"yes_bid\":0.50,\"yes_ask\":0.51}\n",
    )
    .unwrap();
    symlink(&outside_tape, data.join("escape-outside-snapshots.jsonl")).unwrap();

    let outside_strat = outside.join("evil_strategy.rn");
    std::fs::write(
        &outside_strat,
        "// CANARY_OUTSIDE_STRATEGY_DO_NOT_READ\npub fn strategy_name() { \"CANARY_EVIL\" }\npub fn side() { \"yes\" }\npub fn allow_entry(_t, _c, _yb, _ya, _nb, _na, _lp, _ttc) { false }\n",
    )
    .unwrap();
    symlink(&outside_strat, strats.join("evil_strategy.rn")).unwrap();

    let port = free_port();
    let _guard = spawn_server(&base, port);

    for path in [
        "/api/markets?dataset=escape-outside",
        "/api/markets/history?dataset=escape-outside&ticker=CANARY_OUTSIDE_SECRET_DO_NOT_READ",
        "/api/strategies?dataset=escape-outside",
        "/api/analytics/edge-audit?dataset=escape-outside",
    ] {
        let (st, body) = http_req(port, "GET", path, "");
        assert_eq!(st, 403, "{path} must reject symlinked dataset with 403, got {st}: {body}");
        assert!(
            !body.contains("CANARY_OUTSIDE_SECRET_DO_NOT_READ"),
            "outside canary leaked in {path}: {body}"
        );
    }

    let (st_src, b_src) = http_req(port, "GET", "/api/strategy/source?name=CANARY_EVIL", "");
    assert_eq!(st_src, 404, "symlinked strategy must not be loaded or exposed: {st_src} {b_src}");
    assert!(!b_src.contains("CANARY_OUTSIDE_STRATEGY_DO_NOT_READ"));

    let _ = std::fs::remove_dir_all(&base);
}
