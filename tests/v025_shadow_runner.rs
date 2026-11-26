//! End-to-end integration tests for OpenBell GAP #1:
//! Tick-Driven Paper/Shadow Orderbook Matching Engine & Fail-Closed Automated Runner.
//!
//! Strictly uses synthetic local snapshots and an ephemeral 2048-bit RSA key +
//! loopback mock Kalshi v2 server — ZERO real Kalshi orders and ZERO touches to
//! user data directories.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct ServerGuard {
    child: Child,
    dir: Option<PathBuf>,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(ref d) = self.dir {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn http_req_full(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    http_req_with_ct(
        port,
        method,
        path,
        host,
        extra_headers,
        body,
        if body.is_empty() {
            None
        } else {
            Some("application/json")
        },
    )
}

fn http_req_with_ct(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
    content_type: Option<&str>,
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect server");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if let Some(ct) = content_type {
        req.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    if !body.is_empty() {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(body);
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

fn spawn_server(
    data_dir: &Path,
    strat_dir: &Path,
    port: u16,
    cleanup_dir: Option<PathBuf>,
) -> ServerGuard {
    std::fs::create_dir_all(data_dir).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_openbell-rs"))
        .arg("--no-open")
        .env("PORT", port.to_string())
        .env("OPENBELL_DATA", data_dir)
        .env("OPENBELL_STRATEGIES", strat_dir)
        .env("OPENBELL_COLLECT_SECS", "3600")
        .env("OPENBELL_UPDATE_MODE", "off")
        .env("OPENBELL_NO_OPEN", "1")
        .env("OPENBELL_ALLOWED_HOSTS", "desk.lan")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn openbell-rs");
    let guard = ServerGuard {
        child,
        dir: cleanup_dir,
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

fn append_snapshot(data_dir: &Path, snap: &serde_json::Value) {
    let path = data_dir.join("snapshots.jsonl");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .unwrap();
    writeln!(f, "{snap}").unwrap();
    f.flush().unwrap();
}

fn wait_until<F>(timeout: Duration, mut pred: F) -> serde_json::Value
where
    F: FnMut() -> Option<serde_json::Value>,
{
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(v) = pred() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for runner state condition");
}

fn read_sse_frame(reader: &mut BufReader<TcpStream>) -> (String, String) {
    let mut ev = String::new();
    let mut data = String::new();
    loop {
        let mut line = String::new();
        let n = reader.read_line(&mut line).expect("read sse line");
        assert!(n > 0, "SSE stream closed unexpectedly");
        let trimmed = line.trim_end_matches(&['\r', '\n'][..]);
        if trimmed.is_empty() {
            if !ev.is_empty() || !data.is_empty() {
                return (ev, data);
            }
            continue;
        }
        if trimmed.starts_with(':') {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("event:") {
            ev = rest.trim().to_string();
        } else if let Some(rest) = trimmed.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.trim());
        }
    }
}

fn generate_ephemeral_rsa_pair(tmp: &Path) -> (String, Vec<u8>) {
    let priv_path = tmp.join("ephemeral_runner_priv.pem");
    let pub_der_path = tmp.join("ephemeral_runner_pub.der");
    let st1 = Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "RSA",
            "-pkeyopt",
            "rsa_keygen_bits:2048",
            "-out",
            priv_path.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run openssl genpkey");
    assert!(st1.success(), "openssl genpkey failed");
    let st2 = Command::new("openssl")
        .args([
            "pkey",
            "-in",
            priv_path.to_str().unwrap(),
            "-pubout",
            "-outform",
            "DER",
            "-out",
            pub_der_path.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run openssl pubout");
    assert!(st2.success(), "openssl pubout failed");
    (
        std::fs::read_to_string(&priv_path).unwrap(),
        std::fs::read(&pub_der_path).unwrap(),
    )
}

#[derive(Default)]
struct MockKalshiState {
    orders: Vec<serde_json::Value>,
    placed_requests: Vec<serde_json::Value>,
    cancelled_ids: Vec<String>,
    next_id: usize,
}

fn spawn_mock_kalshi_v2() -> (u16, Arc<Mutex<MockKalshiState>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(MockKalshiState::default()));
    let state_bg = state.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut s = stream;
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut req_line = String::new();
            if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
                continue;
            }
            let parts: Vec<&str> = req_line.split_whitespace().collect();
            if parts.len() < 2 {
                continue;
            }
            let method = parts[0].to_string();
            let path = parts[1].to_string();
            let path_no_query = path.split_once('?').map(|(p, _)| p).unwrap_or(&path);
            let mut content_len = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        content_len = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body_bytes = vec![0u8; content_len];
            if content_len > 0 {
                let _ = reader.read_exact(&mut body_bytes);
            }
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();

            let mut st = state_bg.lock().unwrap();
            let (status, resp_json) = match (method.as_str(), path_no_query) {
                ("GET", "/trade-api/v2/portfolio/positions") => {
                    (200, serde_json::json!({ "market_positions": [] }))
                }
                ("GET", "/trade-api/v2/portfolio/fills") => {
                    (200, serde_json::json!({ "fills": [] }))
                }
                ("GET", "/trade-api/v2/portfolio/settlements") => {
                    (200, serde_json::json!({ "settlements": [] }))
                }
                ("GET", "/trade-api/v2/portfolio/orders") => {
                    (200, serde_json::json!({ "orders": st.orders }))
                }
                ("POST", "/trade-api/v2/portfolio/orders") => {
                    let req_v: serde_json::Value = serde_json::from_str(&body_str).unwrap_or_default();
                    st.placed_requests.push(req_v.clone());
                    st.next_id += 1;
                    let oid = format!("ord-live-{}", st.next_id);
                    let ord = serde_json::json!({
                        "order_id": oid,
                        "client_order_id": req_v["client_order_id"],
                        "ticker": req_v["ticker"],
                        "status": "resting",
                        "action": req_v["action"],
                        "side": req_v["side"],
                        "type": "limit",
                        "yes_price": req_v["yes_price"],
                        "no_price": req_v["no_price"],
                        "initial_count": req_v["count"],
                        "remaining_count": req_v["count"],
                        "fill_count": 0,
                        "created_time": "2026-11-26T10:00:00Z"
                    });
                    st.orders.push(ord.clone());
                    (201, serde_json::json!({ "order": ord }))
                }
                ("DELETE", p) if p.starts_with("/trade-api/v2/portfolio/orders/") => {
                    let oid = &p["/trade-api/v2/portfolio/orders/".len()..];
                    st.orders.retain(|o| o["order_id"].as_str() != Some(oid));
                    st.cancelled_ids.push(oid.to_string());
                    (
                        200,
                        serde_json::json!({ "order": { "order_id": oid, "status": "canceled" } }),
                    )
                }
                _ => (404, serde_json::json!({ "error": format!("unknown route {method} {path}") })),
            };
            drop(st);
            let body_out = resp_json.to_string();
            let http = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_out}",
                body_out.len()
            );
            let _ = s.write_all(http.as_bytes());
        }
    });

    (port, state)
}

#[test]
fn shadow_runner_matching_ledger_circuit_breaker_and_fail_closed_live_auto() {
    let dir = std::env::temp_dir().join(format!("ob-v025-runner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // Copy maker_spread_capture.rn and add a custom queue-joining test strategy using entry_quote().
    let repo_strat = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("strategies");
    std::fs::copy(
        repo_strat.join("maker_spread_capture.rn"),
        strat_dir.join("maker_spread_capture.rn"),
    )
    .unwrap();
    std::fs::write(
        strat_dir.join("queue_join_test.rn"),
        r#"
pub fn name() { "queue-join-test" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) {
    secs_to_close >= 60 && secs_to_close <= 800 && yes_ask >= 0.50 && yes_ask <= 0.70
}
pub fn entry_quote(yes_ask, no_ask, secs_to_close) {
    if allow_entry(yes_ask, no_ask, secs_to_close) {
        let yb = 1.0 - no_ask;
        Some((yb * 100.0).round() / 100.0)
    } else {
        None
    }
}
"#,
    )
    .unwrap();

    // Tamper runner_state.json BEFORE boot with "mode": "live_auto" to prove boot safety invariant!
    std::fs::write(
        data_dir.join("runner_state.json"),
        r#"{"mode":"live_auto","tick_interval_ms":80,"cancel_before_close_secs":30,"bankroll_dollars":500.0,"slots":[]}"#,
    )
    .unwrap();

    let port = free_port();
    let srv = spawn_server(&data_dir, &strat_dir, port, None);
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // 1. Verify boot invariants: live_armed=false, mode!="live_auto" (coerced to "stopped"), capability advertised.
    let (st_home, b_home) = http_req_full(port, "GET", "/api/home", &host, &[], "");
    assert_eq!(st_home, 200);
    let home: serde_json::Value = serde_json::from_str(&b_home).unwrap();
    assert!(home["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "shadow_runner"));

    let (st_rs, b_rs) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
    assert_eq!(st_rs, 200);
    let rs0: serde_json::Value = serde_json::from_str(&b_rs).unwrap();
    assert_eq!(rs0["mode"], "stopped");
    assert_eq!(rs0["live_armed"], false);
    assert_eq!(rs0["killed"], false);

    // 2. Security guards: non-loopback 403, missing CSRF 403, non-JSON Content-Type 415, unconfirmed/disarmed live_auto 403.
    let (st_lan, _) = http_req_full(port, "GET", "/api/runner/status", "desk.lan:8472", &[], "");
    assert_eq!(st_lan, 403);
    let (st_nocsrf, _) = http_req_full(
        port,
        "POST",
        "/api/runner/start",
        &host,
        &[],
        r#"{"mode":"shadow"}"#,
    );
    assert_eq!(st_nocsrf, 403);
    let (st_ct, _) = http_req_with_ct(
        port,
        "POST",
        "/api/runner/start",
        &host,
        &csrf,
        r#"{"mode":"shadow"}"#,
        Some("text/plain"),
    );
    assert_eq!(st_ct, 415);
    let (st_live_disarmed, _) = http_req_full(
        port,
        "POST",
        "/api/runner/start",
        &host,
        &csrf,
        r#"{"mode":"live_auto","confirm_auto_live":true}"#,
    );
    assert_eq!(st_live_disarmed, 403);

    // 3. Connect SSE client to /api/tape/stream to capture `event: runner` frames.
    let mut sse_stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    sse_stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    sse_stream
        .write_all(
            format!(
                "GET /api/tape/stream HTTP/1.1\r\nHost: {host}\r\nAccept: text/event-stream\r\n\r\n"
            )
            .as_bytes(),
        )
        .unwrap();
    let mut sse_reader = BufReader::new(sse_stream);
    loop {
        let mut line = String::new();
        sse_reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
    }
    let (init_ev, _) = read_sse_frame(&mut sse_reader);
    assert_eq!(init_ev, "snapshot");

    // 4. Configure slots: Slot 1 = maker-spread-capture (price-improving entry_quote -> 57c), Slot 2 = queue-join-test (joins 55c bid with queue=25).
    let cfg_body = serde_json::json!({
        "slots": [
            {
                "slot_id": 1,
                "strategy": "maker-spread-capture",
                "series": ["KXBTC15M"],
                "max_contracts": 5,
                "execution_mode": "maker",
                "auto_pause_drawdown_dollars": 1.50,
                "enabled": true
            },
            {
                "slot_id": 2,
                "strategy": "queue-join-test",
                "series": ["KXETH15M"],
                "max_contracts": 5,
                "execution_mode": "maker",
                "auto_pause_drawdown_dollars": 10.0,
                "enabled": true
            }
        ],
        "cancel_before_close_secs": 30,
        "bankroll_dollars": 500.0,
        "tick_interval_ms": 60
    })
    .to_string();
    let (st_rcfg, b_rcfg) = http_req_full(port, "POST", "/api/runner/config", &host, &csrf, &cfg_body);
    assert_eq!(st_rcfg, 200, "runner config failed: {b_rcfg}");

    let (st_start, b_start) = http_req_full(
        port,
        "POST",
        "/api/runner/start",
        &host,
        &csrf,
        r#"{"mode":"shadow"}"#,
    );
    assert_eq!(st_start, 200, "runner start failed: {b_start}");

    // 5. Tick 1 on KXBTC15M-WIN1: yes_ask=0.60, no_ask=0.45 => entry_quote = 0.57 (57c), resting maker!
    fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
        let y = if m <= 2 { y - 1 } else { y };
        let era = if y >= 0 { y } else { y - 399 } / 400;
        let yoe = y - era * 400;
        let mp = (m + 9) % 12;
        let doy = (153 * mp + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146097 + doe - 719468
    }
    let close_iso = "2026-11-26T12:15:00Z";
    let close_ep = days_from_civil(2026, 11, 26) * 86400 + 12 * 3600 + 15 * 60;

    append_snapshot(
        &data_dir,
        &serde_json::json!({
            "fetched_at": close_ep - 300,
            "ticker": "KXBTC15M-WIN1",
            "event_ticker": "KXBTC15M-EV1",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.55,
            "yes_ask": 0.60,
            "no_bid": 0.40,
            "no_ask": 0.45,
            "last_price": 0.58,
            "yes_bid_size": 20.0,
            "no_bid_size": 20.0
        }),
    );

    let st1 = wait_until(Duration::from_secs(5), || {
        let (code, body) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
        if code != 200 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        if v["open_quotes"].as_array()?.iter().any(|q| q["ticker"] == "KXBTC15M-WIN1") {
            Some(v)
        } else {
            None
        }
    });
    let q1 = st1["open_quotes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["ticker"] == "KXBTC15M-WIN1")
        .unwrap();
    assert_eq!(q1["limit_cents"], 57, "entry_quote() must rest at 57c, not 60c ask");
    assert_eq!(q1["queue_ahead_initial"], 0);

    // 6. Test queue-ahead depletion on KXETH15M-Q1 (joining 55c bid with 25 contracts ahead).
    append_snapshot(
        &data_dir,
        &serde_json::json!({
            "fetched_at": close_ep - 300,
            "ticker": "KXETH15M-Q1",
            "event_ticker": "KXETH15M-EVQ1",
            "series": "KXETH15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.55,
            "yes_ask": 0.60,
            "no_bid": 0.40,
            "no_ask": 0.45,
            "last_price": 0.58,
            "yes_bid_size": 25.0,
            "no_bid_size": 20.0
        }),
    );
    let st_q = wait_until(Duration::from_secs(5), || {
        let (_, body) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        if v["open_quotes"].as_array()?.iter().any(|q| q["ticker"] == "KXETH15M-Q1") {
            Some(v)
        } else {
            None
        }
    });
    let qq = st_q["open_quotes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["ticker"] == "KXETH15M-Q1")
        .unwrap();
    assert_eq!(qq["limit_cents"], 55);
    assert_eq!(qq["queue_ahead_initial"], 25);
    assert_eq!(qq["queue_ahead_remaining"], 25);

    // Tick 2 on KXETH15M-Q1: bid depth shrinks from 25 -> 12 without crossing -> queue_ahead_remaining drops!
    append_snapshot(
        &data_dir,
        &serde_json::json!({
            "fetched_at": close_ep - 280,
            "ticker": "KXETH15M-Q1",
            "event_ticker": "KXETH15M-EVQ1",
            "series": "KXETH15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.55,
            "yes_ask": 0.59,
            "no_bid": 0.41,
            "no_ask": 0.45,
            "last_price": 0.57,
            "yes_bid_size": 12.0,
            "no_bid_size": 20.0
        }),
    );
    wait_until(Duration::from_secs(5), || {
        let (_, body) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        let found = v["open_quotes"]
            .as_array()?
            .iter()
            .find(|q| q["ticker"] == "KXETH15M-Q1")?;
        if found["queue_ahead_remaining"] == 12 {
            Some(v)
        } else {
            None
        }
    });

    // Tick 3 on KXETH15M-Q1: pre-close cancel at secs_to_close = 15 <= cancel_before_close_secs (30)!
    append_snapshot(
        &data_dir,
        &serde_json::json!({
            "fetched_at": close_ep - 15,
            "ticker": "KXETH15M-Q1",
            "event_ticker": "KXETH15M-EVQ1",
            "series": "KXETH15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.55,
            "yes_ask": 0.59,
            "no_bid": 0.41,
            "no_ask": 0.45,
            "last_price": 0.57,
            "yes_bid_size": 12.0,
            "no_bid_size": 20.0
        }),
    );
    wait_until(Duration::from_secs(5), || {
        let (_, body) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        if !v["open_quotes"].as_array()?.iter().any(|q| q["ticker"] == "KXETH15M-Q1") {
            Some(v)
        } else {
            None
        }
    });

    // 7. Tick 2 on KXBTC15M-WIN1: ask touches 0.57 -> fills resting 57c maker quote!
    append_snapshot(
        &data_dir,
        &serde_json::json!({
            "fetched_at": close_ep - 240,
            "ticker": "KXBTC15M-WIN1",
            "event_ticker": "KXBTC15M-EV1",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.55,
            "yes_ask": 0.57,
            "no_bid": 0.43,
            "no_ask": 0.45,
            "last_price": 0.57,
            "yes_bid_size": 20.0,
            "no_bid_size": 20.0
        }),
    );
    wait_until(Duration::from_secs(5), || {
        let (_, body) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        if v["stats"]["fills"].as_u64().unwrap_or(0) >= 1
            && !v["open_quotes"].as_array()?.iter().any(|q| q["ticker"] == "KXBTC15M-WIN1")
        {
            Some(v)
        } else {
            None
        }
    });

    // 8. Tick 3 on KXBTC15M-WIN1: settles YES -> positive realized PnL!
    append_snapshot(
        &data_dir,
        &serde_json::json!({
            "fetched_at": close_ep + 5,
            "ticker": "KXBTC15M-WIN1",
            "event_ticker": "KXBTC15M-EV1",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "finalized",
            "result": "yes",
            "yes_bid": 0.99,
            "yes_ask": 0.99,
            "no_bid": 0.01,
            "no_ask": 0.01,
            "last_price": 0.99
        }),
    );
    let st_win = wait_until(Duration::from_secs(5), || {
        let (_, body) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        if v["stats"]["realized_pnl_dollars"].as_f64().unwrap_or(0.0) > 1.0 {
            Some(v)
        } else {
            None
        }
    });
    let pnl_after_win = st_win["stats"]["realized_pnl_dollars"].as_f64().unwrap();
    assert!(pnl_after_win > 2.0, "expected > $2 realized profit on 5x57c->100c, got {pnl_after_win}");

    // 9. Trigger auto_pause circuit breaker on Slot 1 by entering & losing on KXBTC15M-LOSE1 (drawdown >= $1.50).
    append_snapshot(
        &data_dir,
        &serde_json::json!({
            "fetched_at": close_ep - 300,
            "ticker": "KXBTC15M-LOSE1",
            "event_ticker": "KXBTC15M-EV2",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.55,
            "yes_ask": 0.60,
            "no_bid": 0.40,
            "no_ask": 0.45,
            "last_price": 0.58
        }),
    );
    wait_until(Duration::from_secs(5), || {
        let (_, body) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        if v["open_quotes"].as_array()?.iter().any(|q| q["ticker"] == "KXBTC15M-LOSE1") {
            Some(v)
        } else {
            None
        }
    });
    append_snapshot(
        &data_dir,
        &serde_json::json!({
            "fetched_at": close_ep - 240,
            "ticker": "KXBTC15M-LOSE1",
            "event_ticker": "KXBTC15M-EV2",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.54,
            "yes_ask": 0.56, // strict cross < 57c
            "no_bid": 0.44,
            "no_ask": 0.46,
            "last_price": 0.56
        }),
    );
    wait_until(Duration::from_secs(5), || {
        let (_, body) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        if v["stats"]["fills"].as_u64().unwrap_or(0) >= 2 {
            Some(v)
        } else {
            None
        }
    });
    append_snapshot(
        &data_dir,
        &serde_json::json!({
            "fetched_at": close_ep + 5,
            "ticker": "KXBTC15M-LOSE1",
            "event_ticker": "KXBTC15M-EV2",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "finalized",
            "result": "no",
            "yes_bid": 0.01,
            "yes_ask": 0.01,
            "no_bid": 0.99,
            "no_ask": 0.99,
            "last_price": 0.01
        }),
    );
    let st_paused = wait_until(Duration::from_secs(5), || {
        let (_, body) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        let slot1 = v["active_slots"].as_array()?.iter().find(|s| s["slot_id"] == 1)?;
        if slot1["paused"] == true {
            Some(v)
        } else {
            None
        }
    });
    assert_eq!(st_paused["active_slots"][0]["paused"], true);

    // Verify ledger contains QuotePosted, QuoteCanceled, OrderFilled, WindowSettled, SlotPaused.
    let (st_led, b_led) = http_req_full(
        port,
        "GET",
        "/api/runner/ledger?limit=200&mode=shadow",
        &host,
        &[],
        "",
    );
    assert_eq!(st_led, 200);
    let led: serde_json::Value = serde_json::from_str(&b_led).unwrap();
    let entries = led["entries"].as_array().unwrap();
    for ev_name in [
        "QuotePosted",
        "QuoteCanceled",
        "OrderFilled",
        "WindowSettled",
        "SlotPaused",
    ] {
        assert!(
            entries.iter().any(|e| e["event"] == ev_name),
            "ledger missing {ev_name}: {b_led}"
        );
    }

    // Check SSE stream received `event: runner` frames!
    let mut saw_runner_sse = HashMap::<String, bool>::new();
    for _ in 0..20 {
        let (ev, data) = read_sse_frame(&mut sse_reader);
        if ev == "runner" {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(t) = v["type"].as_str() {
                    saw_runner_sse.insert(t.to_string(), true);
                }
            }
            if saw_runner_sse.contains_key("slot_auto_paused")
                && saw_runner_sse.contains_key("window_settled")
            {
                break;
            }
        }
    }
    assert!(saw_runner_sse.contains_key("quote_posted"));
    assert!(saw_runner_sse.contains_key("quote_filled"));
    assert!(saw_runner_sse.contains_key("quote_canceled"));
    assert!(saw_runner_sse.contains_key("window_settled"));
    assert!(saw_runner_sse.contains_key("slot_auto_paused"));
    drop(sse_reader);

    // 10. Live-auto mode + emergency kill switch atomic halt!
    let (priv_pem, _pub_der) = generate_ephemeral_rsa_pair(&dir);
    let (mock_port, mock_state) = spawn_mock_kalshi_v2();
    let mock_base = format!("http://127.0.0.1:{mock_port}/trade-api/v2");

    let live_cfg = serde_json::json!({
        "api_key_id": "test-runner-key",
        "private_key_pem": priv_pem,
        "environment": "demo",
        "api_base_url": mock_base,
        "rails": {
            "max_order_contracts": 10,
            "max_order_dollars": 25.0,
            "max_daily_loss_dollars": 50.0,
            "max_open_exposure_dollars": 100.0,
            "allowed_ticker_prefix": "KXBTC15M,KXETH15M"
        }
    })
    .to_string();
    assert_eq!(
        http_req_full(port, "POST", "/api/live/config", &host, &csrf, &live_cfg).0,
        200
    );
    assert_eq!(
        http_req_full(
            port,
            "POST",
            "/api/live/arm",
            &host,
            &csrf,
            r#"{"confirm_arm":true}"#
        )
        .0,
        200
    );
    // Unconfirmed live_auto still rejected 403 even when live_armed=true!
    assert_eq!(
        http_req_full(
            port,
            "POST",
            "/api/runner/start",
            &host,
            &csrf,
            r#"{"mode":"live_auto","confirm_auto_live":false}"#
        )
        .0,
        403
    );

    // Re-enable Slot 1 (clears pause) and start live_auto with confirm_auto_live=true!
    let cfg_live_slot = serde_json::json!({
        "slots": [{
            "slot_id": 1,
            "strategy": "maker-spread-capture",
            "series": ["KXBTC15M"],
            "max_contracts": 3,
            "execution_mode": "maker",
            "auto_pause_drawdown_dollars": 50.0,
            "enabled": true
        }],
        "cancel_before_close_secs": 30,
        "bankroll_dollars": 500.0,
        "tick_interval_ms": 60
    })
    .to_string();
    assert_eq!(
        http_req_full(port, "POST", "/api/runner/config", &host, &csrf, &cfg_live_slot).0,
        200
    );
    let (st_la, b_la) = http_req_full(
        port,
        "POST",
        "/api/runner/start",
        &host,
        &csrf,
        r#"{"mode":"live_auto","confirm_auto_live":true}"#,
    );
    assert_eq!(st_la, 200, "live_auto start failed: {b_la}");

    // Verify runner_state.json NEVER persists "live_auto"!
    let disk_state = std::fs::read_to_string(data_dir.join("runner_state.json")).unwrap();
    assert!(
        !disk_state.contains("live_auto"),
        "runner_state.json must NEVER persist live_auto: {disk_state}"
    );

    // Append open tick -> live_auto routes signed post_only maker order to mock Kalshi v2!
    append_snapshot(
        &data_dir,
        &serde_json::json!({
            "fetched_at": close_ep - 300,
            "ticker": "KXBTC15M-LIVE1",
            "event_ticker": "KXBTC15M-EVL1",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.55,
            "yes_ask": 0.60,
            "no_bid": 0.40,
            "no_ask": 0.45,
            "last_price": 0.58
        }),
    );
    wait_until(Duration::from_secs(5), || {
        if !mock_state.lock().unwrap().placed_requests.is_empty() {
            Some(serde_json::json!(true))
        } else {
            None
        }
    });
    {
        let ms = mock_state.lock().unwrap();
        assert_eq!(ms.placed_requests[0]["post_only"], true);
        assert_eq!(ms.placed_requests[0]["yes_price"], 57);
    }

    // Call POST /api/live/kill-switch -> atomically halts runner AND cancels resting live order!
    let (st_ks, b_ks) = http_req_full(
        port,
        "POST",
        "/api/live/kill-switch",
        &host,
        &csrf,
        r#"{"reason":"panic halt test"}"#,
    );
    assert_eq!(st_ks, 200, "kill-switch failed: {b_ks}");
    assert_eq!(mock_state.lock().unwrap().cancelled_ids.len(), 1);

    let (_, b_after_ks) = http_req_full(port, "GET", "/api/runner/status", &host, &[], "");
    let rs_after_ks: serde_json::Value = serde_json::from_str(&b_after_ks).unwrap();
    assert_eq!(rs_after_ks["mode"], "stopped");
    assert_eq!(rs_after_ks["live_armed"], false);
    assert_eq!(rs_after_ks["killed"], true);

    // 11. Restart server -> reloads shadow_ledger.jsonl, boots disarmed and never live_auto!
    drop(srv);
    let port2 = free_port();
    let _srv2 = spawn_server(&data_dir, &strat_dir, port2, Some(dir));
    let host2 = format!("127.0.0.1:{port2}");
    let (_, b_rs2) = http_req_full(port2, "GET", "/api/runner/status", &host2, &[], "");
    let rs2: serde_json::Value = serde_json::from_str(&b_rs2).unwrap();
    assert_eq!(rs2["live_armed"], false);
    assert_ne!(rs2["mode"], "live_auto");
    assert_eq!(rs2["mode"], "stopped");

    let (_, b_led2) = http_req_full(port2, "GET", "/api/runner/ledger?limit=200", &host2, &[], "");
    let led2: serde_json::Value = serde_json::from_str(&b_led2).unwrap();
    assert!(
        led2["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "WindowSettled" && e["ticker"] == "KXBTC15M-WIN1"),
        "persisted shadow_ledger.jsonl must reload across restarts"
    );
}
