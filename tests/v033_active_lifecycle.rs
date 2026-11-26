//! End-to-end integration tests for OpenBell v0.33.0 Active Quote Lifecycle,
//! Order Amendment (`POST /api/live/orders/{id}/amend`), 1-Click Position Flatten
//! (`POST /api/live/positions/{ticker}/flatten`), TickContext inventory injection,
//! ShadowRunner dynamic maker repricing (cooldown & max_replaces_per_window),
//! TP/SL/exit-before-close intra-window exits, and fail-closed safety invariants.
//!
//! Uses strictly ephemeral local RSA keys and local loopback `TcpListener` mocks.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn iso_z_from_epoch(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let hh = secs / 3600;
    let mm = (secs % 3600) / 60;
    let ss = secs % 60;
    // Civil date from days since 1970-01-01 (Howard Hinnant algorithm)
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let yr = y + if m <= 2 { 1 } else { 0 };
    format!("{yr:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn http_req(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    extra_headers: &[(&str, &str)],
    content_type: Option<&str>,
    body: &str,
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
    if !body.is_empty() || !matches!(method, "GET" | "HEAD") {
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

fn spawn_server(data_dir: &Path, strat_dir: &Path, port: u16, cleanup_dir: Option<PathBuf>) -> ServerGuard {
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::create_dir_all(strat_dir).unwrap();
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

fn extract_spki_bitstring(der: &[u8]) -> Option<&[u8]> {
    fn read_tlv(buf: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let tag = *buf.first()?;
        let b1 = *buf.get(1)?;
        let (len, hdr) = if b1 & 0x80 == 0 {
            (b1 as usize, 2)
        } else {
            let n = (b1 & 0x7f) as usize;
            let mut l = 0usize;
            for i in 0..n {
                l = (l << 8) | (*buf.get(2 + i)? as usize);
            }
            (l, 2 + n)
        };
        Some((tag, buf.get(hdr..hdr + len)?, buf.get(hdr + len..)?))
    }
    let (0x30, seq, _) = read_tlv(der)? else { return None };
    let (0x30, _alg, rest) = read_tlv(seq)? else { return None };
    let (0x03, bits, _) = read_tlv(rest)? else { return None };
    bits.strip_prefix(&[0x00])
}

fn generate_ephemeral_rsa(tmp: &Path) -> (String, Vec<u8>) {
    let priv_path = tmp.join("ephemeral_e1_priv.pem");
    let pub_der_path = tmp.join("ephemeral_e1_pub.der");
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
        .expect("openssl genpkey");
    assert!(st1.success());
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
        .expect("openssl pubout");
    assert!(st2.success());
    (
        std::fs::read_to_string(&priv_path).unwrap(),
        std::fs::read(&pub_der_path).unwrap(),
    )
}

#[derive(Default)]
struct MockVenueState {
    orders: Vec<serde_json::Value>,
    positions: Vec<serde_json::Value>,
    fills: Vec<serde_json::Value>,
    amend_calls: usize,
    decrease_calls: usize,
    placed_orders: Vec<serde_json::Value>,
    next_id: usize,
    auto_fill_new_buy: bool,
    auto_fill_new_sell: bool,
}

fn spawn_mock_kalshi(
    expected_key_id: String,
    pub_der: Vec<u8>,
) -> (u16, Arc<Mutex<MockVenueState>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(MockVenueState::default()));
    let state_bg = state.clone();

    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut req_line = String::new();
            if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
                continue;
            }
            let mut parts = req_line.split_whitespace();
            let method = parts.next().unwrap_or("").to_string();
            let path = parts.next().unwrap_or("").to_string();
            let mut headers = HashMap::new();
            let mut content_len = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    let kl = k.trim().to_ascii_lowercase();
                    let val = v.trim().to_string();
                    if kl == "content-length" {
                        content_len = val.parse().unwrap_or(0);
                    }
                    headers.insert(kl, val);
                }
            }
            let mut body = vec![0u8; content_len];
            if content_len > 0 {
                let _ = reader.read_exact(&mut body);
            }
            let key = headers.get("kalshi-access-key").cloned().unwrap_or_default();
            assert_eq!(key, expected_key_id);
            let ts = headers
                .get("kalshi-access-timestamp")
                .cloned()
                .unwrap_or_default();
            let sig_b64 = headers
                .get("kalshi-access-signature")
                .cloned()
                .unwrap_or_default();
            #[cfg(not(target_os = "windows"))]
            {
                use base64::Engine as _;
                let path_no_q = path.split('?').next().unwrap_or(&path);
                let msg = format!("{ts}{method}{path_no_q}");
                let sig_raw = base64::engine::general_purpose::STANDARD
                    .decode(&sig_b64)
                    .expect("base64 sig");
                let pub_slice = extract_spki_bitstring(&pub_der).unwrap_or(&pub_der);
                let pk = ring::signature::UnparsedPublicKey::new(
                    &ring::signature::RSA_PSS_2048_8192_SHA256,
                    pub_slice,
                );
                pk.verify(msg.as_bytes(), &sig_raw)
                    .expect("valid RSA-PSS signature");
            }
            let _ = (&ts, &sig_b64);

            let path_no_q = path.split('?').next().unwrap_or(&path);
            let mut st = state_bg.lock().unwrap();
            let (status, resp_json) = match (method.as_str(), path_no_q) {
                ("GET", "/trade-api/v2/portfolio/balance") => (
                    200,
                    serde_json::json!({ "balance": 50000, "portfolio_value": 50000 }),
                ),
                ("GET", "/trade-api/v2/portfolio/positions") => (
                    200,
                    serde_json::json!({ "market_positions": st.positions }),
                ),
                ("GET", "/trade-api/v2/portfolio/orders") => (
                    200,
                    serde_json::json!({ "orders": st.orders }),
                ),
                ("GET", "/trade-api/v2/portfolio/fills") => (
                    200,
                    serde_json::json!({ "fills": st.fills }),
                ),
                ("GET", "/trade-api/v2/portfolio/settlements") => (
                    200,
                    serde_json::json!({ "settlements": [] }),
                ),
                ("POST", "/trade-api/v2/portfolio/orders") => {
                    let req_v: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    st.next_id += 1;
                    let oid = format!("kalshi-e1-{}", st.next_id);
                    let action = req_v["action"].as_str().unwrap_or("buy").to_string();
                    let count = req_v["count"].as_i64().unwrap_or(1);
                    let fill_now = (action == "buy" && st.auto_fill_new_buy)
                        || (action == "sell" && st.auto_fill_new_sell);
                    let new_ord = serde_json::json!({
                        "order_id": oid,
                        "client_order_id": req_v["client_order_id"],
                        "ticker": req_v["ticker"],
                        "status": if fill_now { "executed" } else { "resting" },
                        "action": action,
                        "side": req_v["side"],
                        "type": req_v["type"],
                        "yes_price": req_v["yes_price"],
                        "no_price": req_v["no_price"],
                        "initial_count": count,
                        "remaining_count": if fill_now { 0 } else { count },
                        "fill_count": if fill_now { count } else { 0 },
                    });
                    st.placed_orders.push(req_v);
                    st.orders.push(new_ord.clone());
                    (201, serde_json::json!({ "order": new_ord }))
                }
                ("POST", p)
                    if p.starts_with("/trade-api/v2/portfolio/orders/")
                        && p.ends_with("/amend") =>
                {
                    let id = p
                        .trim_start_matches("/trade-api/v2/portfolio/orders/")
                        .trim_end_matches("/amend");
                    let req_v: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    st.amend_calls += 1;
                    if let Some(ord) = st
                        .orders
                        .iter_mut()
                        .find(|o| o["order_id"].as_str() == Some(id))
                    {
                        if let Some(yp) = req_v.get("yes_price").and_then(|v| v.as_i64()) {
                            ord["yes_price"] = serde_json::json!(yp);
                        } else if let Some(np) = req_v.get("no_price").and_then(|v| v.as_i64()) {
                            ord["no_price"] = serde_json::json!(np);
                        } else if let Some(pc) = req_v.get("price_cents").and_then(|v| v.as_i64()) {
                            ord["yes_price"] = serde_json::json!(pc);
                        }
                        if let Some(c) = req_v.get("count").and_then(|v| v.as_i64()) {
                            ord["initial_count"] = serde_json::json!(c);
                            ord["remaining_count"] = serde_json::json!(c);
                        }
                        (200, serde_json::json!({ "order": ord.clone() }))
                    } else {
                        (404, serde_json::json!({ "error": "order not found" }))
                    }
                }
                ("POST", p)
                    if p.starts_with("/trade-api/v2/portfolio/orders/")
                        && p.ends_with("/decrease") =>
                {
                    let id = p
                        .trim_start_matches("/trade-api/v2/portfolio/orders/")
                        .trim_end_matches("/decrease");
                    let req_v: serde_json::Value = serde_json::from_slice(&body).unwrap();
                    st.decrease_calls += 1;
                    let reduce_by = req_v["reduce_by"].as_i64().unwrap_or(0);
                    if let Some(ord) = st
                        .orders
                        .iter_mut()
                        .find(|o| o["order_id"].as_str() == Some(id))
                    {
                        let rem = ord["remaining_count"].as_i64().unwrap_or(0);
                        ord["remaining_count"] = serde_json::json!((rem - reduce_by).max(0));
                        (200, serde_json::json!({ "order": ord.clone() }))
                    } else {
                        (404, serde_json::json!({ "error": "order not found" }))
                    }
                }
                ("DELETE", p) if p.starts_with("/trade-api/v2/portfolio/orders/") => {
                    let id = p.rsplit('/').next().unwrap_or("");
                    for o in &mut st.orders {
                        if o["order_id"].as_str() == Some(id) {
                            o["status"] = serde_json::json!("canceled");
                            o["remaining_count"] = serde_json::json!(0);
                        }
                    }
                    (200, serde_json::json!({ "order_id": id, "status": "canceled" }))
                }
                _ => (404, serde_json::json!({ "error": "not found" })),
            };
            drop(st);
            let body_str = resp_json.to_string();
            let http = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
                body_str.len()
            );
            let _ = s.write_all(http.as_bytes());
        }
    });

    (port, state)
}

#[test]
fn live_amend_and_flatten_fail_closed_risk_rails_and_ledger() {
    let tmp = std::env::temp_dir().join(format!("ob-v033-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let data_dir = tmp.join("data");
    let strat_dir = tmp.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    let (priv_pem, pub_der) = generate_ephemeral_rsa(&tmp);
    let key_path = tmp.join("kalshi_e1.pem");
    std::fs::write(&key_path, &priv_pem).unwrap();

    let (kalshi_port, kalshi_state) = spawn_mock_kalshi("e1-test-key".to_string(), pub_der);
    let port = free_port();
    let mut guard = spawn_server(&data_dir, &strat_dir, port, Some(tmp.clone()));

    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // 1. Boots disarmed by default
    let (code, body) = http_req(port, "GET", "/api/live/status", &host, &[], None, "");
    assert_eq!(code, 200);
    let st0: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(st0["live_armed"], false);
    assert_eq!(st0["killed"], false);

    // 2. Configure live client + risk rails
    let cfg_body = serde_json::json!({
        "api_key_id": "e1-test-key",
        "private_key_path": key_path.to_str().unwrap(),
        "environment": "demo",
        "api_base_url": format!("http://127.0.0.1:{kalshi_port}/trade-api/v2"),
        "rails": {
            "max_order_contracts": 20,
            "max_order_dollars": 10.0,
            "max_daily_loss_dollars": 25.0,
            "max_open_exposure_dollars": 9.0,
            "allowed_ticker_prefix": "KXBTC15M-"
        }
    })
    .to_string();
    let (code, b) = http_req(
        port,
        "POST",
        "/api/live/config",
        &host,
        &csrf,
        Some("application/json"),
        &cfg_body,
    );
    assert_eq!(code, 200, "config failed: {b}");

    // 3. Transport boundary: non-loopback, bad CSRF, missing Content-Type, disarmed -> fail closed
    let (code, _) = http_req(
        port,
        "POST",
        "/api/live/orders/ord-1/amend",
        "desk.lan",
        &csrf,
        Some("application/json"),
        r#"{"price_cents":55}"#,
    );
    assert_eq!(code, 403, "non-loopback amend must return 403");

    let (code, _) = http_req(
        port,
        "POST",
        "/api/live/orders/ord-1/amend",
        &host,
        &[("X-OpenBell-CSRF", "0")],
        Some("application/json"),
        r#"{"price_cents":55}"#,
    );
    assert_eq!(code, 403, "CSRF != 1 amend must return 403");

    let (code, _) = http_req(
        port,
        "POST",
        "/api/live/positions/KXBTC15M-E1/flatten",
        &host,
        &csrf,
        None,
        "",
    );
    assert_eq!(code, 415, "missing Content-Type flatten must return 415");

    let (code, b) = http_req(
        port,
        "POST",
        "/api/live/orders/ord-1/amend",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"price_cents":55}"#,
    );
    assert_eq!(code, 403, "disarmed amend must return 403: {b}");

    let (code, b) = http_req(
        port,
        "POST",
        "/api/live/positions/KXBTC15M-E1/flatten",
        &host,
        &csrf,
        Some("application/json"),
        "{}",
    );
    assert_eq!(code, 403, "disarmed flatten must return 403: {b}");

    // 4. Arm live trading -> flatten still fails closed 403 because venue sync is stale!
    let (code, b) = http_req(
        port,
        "POST",
        "/api/live/arm",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"confirm_arm":true}"#,
    );
    assert_eq!(code, 200, "arm failed: {b}");

    let (code, b) = http_req(
        port,
        "POST",
        "/api/live/positions/KXBTC15M-E1/flatten",
        &host,
        &csrf,
        Some("application/json"),
        "{}",
    );
    assert_eq!(code, 403, "stale venue sync flatten must return 403: {b}");

    // 5. Reconcile so venue sync is fresh
    let (code, b) = http_req(
        port,
        "POST",
        "/api/live/reconcile",
        &host,
        &csrf,
        Some("application/json"),
        "{}",
    );
    assert_eq!(code, 200, "reconcile failed: {b}");

    // 6. Flatten zero open position -> 404
    let (code, b) = http_req(
        port,
        "POST",
        "/api/live/positions/KXBTC15M-ZERO/flatten",
        &host,
        &csrf,
        Some("application/json"),
        "{}",
    );
    assert_eq!(code, 404, "zero-position flatten must return 404: {b}");

    // 7. Place live buy order: 10 @ 50c ($5.00 reserved exposure)
    let (code, b) = http_req(
        port,
        "POST",
        "/api/live/order",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"ticker":"KXBTC15M-E1","action":"buy","side":"yes","count":10,"price_cents":50,"post_only":true}"#,
    );
    assert_eq!(code, 200, "place_order failed: {b}");
    let placed: serde_json::Value = serde_json::from_str(&b).unwrap();
    let order_id = placed["order"]["order_id"].as_str().unwrap().to_string();
    assert!((placed["status"]["open_exposure_dollars"].as_f64().unwrap() - 5.0).abs() < 1e-6);

    // 8. Upward amend within max_open_exposure ($9.00): 10 @ 75c ($7.50) -> 200
    let (code, b) = http_req(
        port,
        "POST",
        &format!("/api/live/orders/{order_id}/amend"),
        &host,
        &csrf,
        Some("application/json"),
        r#"{"price_cents":75}"#,
    );
    assert_eq!(code, 200, "valid upward amend failed: {b}");
    let am1: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(am1["order"]["yes_price"], 75);
    assert!(
        (am1["status"]["open_exposure_dollars"].as_f64().unwrap() - 7.5).abs() < 1e-6,
        "expected $7.50 open exposure without double counting: {am1}"
    );

    // 9. Second amend to 80c ($8.00) -> 200 and appends a second order_amended record
    let (code, b) = http_req(
        port,
        "POST",
        &format!("/api/live/orders/{order_id}/amend"),
        &host,
        &csrf,
        Some("application/json"),
        r#"{"price_cents":80,"count":10}"#,
    );
    assert_eq!(code, 200, "second amend failed: {b}");

    // 10. Upward amend exceeding max_open_exposure ($9.00): 12 @ 80c ($9.60) -> 403 fail closed
    let amends_before = kalshi_state.lock().unwrap().amend_calls;
    let (code, b) = http_req(
        port,
        "POST",
        &format!("/api/live/orders/{order_id}/amend"),
        &host,
        &csrf,
        Some("application/json"),
        r#"{"price_cents":80,"count":12}"#,
    );
    assert_eq!(code, 403, "over-exposure upward amend must fail closed 403: {b}");
    assert_eq!(
        kalshi_state.lock().unwrap().amend_calls,
        amends_before,
        "rejected amend must never hit Kalshi"
    );

    // Verify both order_amended entries were appended to live_executions.jsonl
    let exec_raw = std::fs::read_to_string(data_dir.join("live_executions.jsonl")).unwrap();
    let amended_rows: Vec<serde_json::Value> = exec_raw
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| v["kind"] == "order_amended" && v["order_id"] == order_id)
        .collect();
    assert_eq!(amended_rows.len(), 2, "expected 2 order_amended rows: {exec_raw}");

    // 11. Seed open venue position and test 1-click flatten
    kalshi_state.lock().unwrap().positions.push(serde_json::json!({
        "ticker": "KXBTC15M-E1",
        "position": 10,
        "market_exposure": 800,
        "realized_pnl": 0,
        "resting_orders_count": 0,
        "fees_paid": 10,
        "total_traded": 800
    }));
    // Side mismatch -> 400
    let (code, b) = http_req(
        port,
        "POST",
        "/api/live/positions/KXBTC15M-E1/flatten",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"side":"no","price_cents":70}"#,
    );
    assert_eq!(code, 400, "wrong-side flatten must return 400: {b}");

    // Matching flatten -> 200, routes action: "sell" order
    let (code, b) = http_req(
        port,
        "POST",
        "/api/live/positions/KXBTC15M-E1/flatten",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"price_cents":78,"post_only":false}"#,
    );
    assert_eq!(code, 200, "flatten failed: {b}");
    let fl_v: serde_json::Value = serde_json::from_str(&b).unwrap();
    assert_eq!(fl_v["flattened_contracts"], 10);
    assert_eq!(fl_v["side"], "yes");
    assert_eq!(fl_v["order"]["action"], "sell");
    assert_eq!(fl_v["order"]["yes_price"], 78);

    // 12. Kill switch engaged -> 403 on both amend & flatten
    let (code, _) = http_req(
        port,
        "POST",
        "/api/live/kill-switch",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"reason":"e1_test_halt"}"#,
    );
    assert_eq!(code, 200);
    let (code, _) = http_req(
        port,
        "POST",
        &format!("/api/live/orders/{order_id}/amend"),
        &host,
        &csrf,
        Some("application/json"),
        r#"{"price_cents":70}"#,
    );
    assert_eq!(code, 403, "killed amend must return 403");
    let (code, _) = http_req(
        port,
        "POST",
        "/api/live/positions/KXBTC15M-E1/flatten",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"price_cents":70}"#,
    );
    assert_eq!(code, 403, "killed flatten must return 403");

    // 13. Restart invariant: live_armed is ALWAYS false on reboot
    let _ = guard.child.kill();
    let _ = guard.child.wait();
    let port2 = free_port();
    let _guard2 = spawn_server(&data_dir, &strat_dir, port2, Some(tmp));
    let host2 = format!("127.0.0.1:{port2}");
    let (code, b2) = http_req(port2, "GET", "/api/live/status", &host2, &[], None, "");
    assert_eq!(code, 200);
    let st_reboot: serde_json::Value = serde_json::from_str(&b2).unwrap();
    assert_eq!(st_reboot["live_armed"], false);
    assert_eq!(st_reboot["killed"], true);
}

#[test]
fn shadow_runner_reprice_cap_sl_exit_before_close_and_reboot_invariant() {
    let tmp = std::env::temp_dir().join(format!("ob-v033-runner-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let data_dir = tmp.join("data");
    let strat_dir = tmp.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // Create a v2 strategy that inspects ctx.slot_position_contracts and ctx.slot_resting_cents
    let v2_src = r#"
pub fn name() { "e1-active-v2" }
pub fn side() { "yes" }
pub fn describe() { "Active quote v2 strategy using TickContext slot inventory." }
pub fn evaluate(ctx) {
    if ctx.slot_position_contracts > 0 {
        return None;
    }
    let bid_c = (ctx.yes_bid * 100.0).round() as i64;
    if bid_c >= 1 && bid_c <= 98 {
        return Some(#{ enter: true, side: "yes", limit_cents: bid_c, post_only: true });
    }
    None
}
"#;
    let close_ts = now_unix() + 900;
    let close_iso = iso_z_from_epoch(close_ts);
    let t0 = now_unix();
    let snap1 = serde_json::json!({
        "fetched_at": t0,
        "ticker": "KXBTC15M-SR1",
        "event_ticker": "KXBTC15M-SR1EV",
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": "open",
        "result": "",
        "yes_bid": 0.45,
        "yes_ask": 0.55,
        "no_bid": 0.45,
        "no_ask": 0.55,
        "last_price": 0.50,
        "yes_bid_size": 10.0,
        "no_bid_size": 10.0
    });
    std::fs::write(data_dir.join("snapshots.jsonl"), format!("{snap1}\n")).unwrap();

    let port = free_port();
    let mut guard = spawn_server(&data_dir, &strat_dir, port, Some(tmp.clone()));
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // Save v2 strategy through /api/strategies/save (runs causal canary with stateless TickContext defaults!)
    let save_body = serde_json::json!({
        "slug": "e1_active_v2",
        "source": v2_src
    })
    .to_string();
    let (code, b) = http_req(
        port,
        "POST",
        "/api/strategies/save",
        &host,
        &csrf,
        Some("application/json"),
        &save_body,
    );
    assert_eq!(code, 200, "save v2 strategy failed: {b}");

    // Configure runner with max_replaces_per_window=1, cooldown=0, stop_loss_cents=10
    let cfg_body = serde_json::json!({
        "cancel_before_close_secs": 30,
        "bankroll_dollars": 500.0,
        "tick_interval_ms": 80,
        "slots": [{
            "slot_id": 1,
            "strategy": "e1-active-v2",
            "series": ["KXBTC15M"],
            "max_contracts": 4,
            "execution_mode": "maker",
            "auto_pause_drawdown_dollars": 50.0,
            "enabled": true,
            "reprice_cooldown_secs": 0,
            "max_replaces_per_window": 1,
            "take_profit_cents": 20,
            "stop_loss_cents": 10,
            "exit_before_close_secs": 60
        }]
    })
    .to_string();
    let (code, b) = http_req(
        port,
        "POST",
        "/api/runner/config",
        &host,
        &csrf,
        Some("application/json"),
        &cfg_body,
    );
    assert_eq!(code, 200, "runner config failed: {b}");

    let (code, b) = http_req(
        port,
        "POST",
        "/api/runner/start",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"mode":"shadow"}"#,
    );
    assert_eq!(code, 200, "runner start failed: {b}");

    // Wait for initial resting quote @ 45c
    let mut posted_ok = false;
    for _ in 0..40 {
        let (c, sb) = http_req(port, "GET", "/api/runner/status", &host, &[], None, "");
        assert_eq!(c, 200);
        let sv: serde_json::Value = serde_json::from_str(&sb).unwrap();
        if sv["open_quotes"].as_array().is_some_and(|a| !a.is_empty()) {
            assert_eq!(sv["open_quotes"][0]["limit_cents"], 45);
            assert_eq!(sv["open_quotes"][0]["action"], "buy");
            assert_eq!(sv["open_quotes"][0]["replace_count"], 0);
            posted_ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    assert!(posted_ok, "expected resting quote @ 45c");

    // Append snap2 (yes_bid=0.47) -> reprices once to 47c (replace_count=1)
    let snap2 = serde_json::json!({
        "fetched_at": t0 + 5,
        "ticker": "KXBTC15M-SR1",
        "event_ticker": "KXBTC15M-SR1EV",
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": "open",
        "result": "",
        "yes_bid": 0.47,
        "yes_ask": 0.55,
        "no_bid": 0.45,
        "no_ask": 0.53,
        "last_price": 0.50,
        "yes_bid_size": 10.0,
        "no_bid_size": 10.0
    });
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(data_dir.join("snapshots.jsonl"))
        .unwrap();
    writeln!(f, "{snap2}").unwrap();
    f.flush().unwrap();

    let mut repriced_ok = false;
    for _ in 0..40 {
        let (_, sb) = http_req(port, "GET", "/api/runner/status", &host, &[], None, "");
        let sv: serde_json::Value = serde_json::from_str(&sb).unwrap();
        if sv["open_quotes"]
            .as_array()
            .and_then(|a| a.first())
            .is_some_and(|q| q["limit_cents"] == 47 && q["replace_count"] == 1)
        {
            repriced_ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    assert!(repriced_ok, "expected quote reprice to 47c with replace_count=1");

    // Append snap3 (yes_bid=0.49) -> max_replaces_per_window=1 already reached -> MUST stay at 47c!
    let snap3 = serde_json::json!({
        "fetched_at": t0 + 10,
        "ticker": "KXBTC15M-SR1",
        "event_ticker": "KXBTC15M-SR1EV",
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": "open",
        "result": "",
        "yes_bid": 0.49,
        "yes_ask": 0.55,
        "no_bid": 0.45,
        "no_ask": 0.51,
        "last_price": 0.50,
        "yes_bid_size": 10.0,
        "no_bid_size": 10.0
    });
    writeln!(f, "{snap3}").unwrap();
    f.flush().unwrap();
    std::thread::sleep(Duration::from_millis(220));
    let (_, sb) = http_req(port, "GET", "/api/runner/status", &host, &[], None, "");
    let sv: serde_json::Value = serde_json::from_str(&sb).unwrap();
    assert_eq!(sv["open_quotes"][0]["limit_cents"], 47);
    assert_eq!(sv["open_quotes"][0]["replace_count"], 1);

    // Append snap4 (yes_ask=0.46) -> strict cross fills at 47c; then snap5 (yes_bid=0.35, -12c <= -10c SL) -> stop_loss exit!
    let snap4 = serde_json::json!({
        "fetched_at": t0 + 15,
        "ticker": "KXBTC15M-SR1",
        "event_ticker": "KXBTC15M-SR1EV",
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": "open",
        "result": "",
        "yes_bid": 0.45,
        "yes_ask": 0.46,
        "no_bid": 0.54,
        "no_ask": 0.55,
        "last_price": 0.46,
        "yes_bid_size": 10.0,
        "no_bid_size": 10.0
    });
    writeln!(f, "{snap4}").unwrap();
    f.flush().unwrap();
    let mut filled_ok = false;
    for _ in 0..40 {
        let (_, sb) = http_req(port, "GET", "/api/runner/status", &host, &[], None, "");
        let sv: serde_json::Value = serde_json::from_str(&sb).unwrap();
        if sv["active_slots"][0]["filled_positions_count"] == 1 {
            filled_ok = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    assert!(filled_ok, "expected maker fill at 47c");

    let snap5 = serde_json::json!({
        "fetched_at": t0 + 20,
        "ticker": "KXBTC15M-SR1",
        "event_ticker": "KXBTC15M-SR1EV",
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": "open",
        "result": "",
        "yes_bid": 0.35,
        "yes_ask": 0.38,
        "no_bid": 0.62,
        "no_ask": 0.65,
        "last_price": 0.35,
        "yes_bid_size": 10.0,
        "no_bid_size": 10.0
    });
    writeln!(f, "{snap5}").unwrap();
    f.flush().unwrap();

    let mut sl_exited = false;
    for _ in 0..40 {
        let (_, lb) = http_req(port, "GET", "/api/runner/ledger?limit=50", &host, &[], None, "");
        let lv: serde_json::Value = serde_json::from_str(&lb).unwrap();
        if lv["entries"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|e| e["event"] == "PositionExited" && e["reason"] == "stop_loss"))
        {
            sl_exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    assert!(sl_exited, "expected PositionExited(stop_loss) in runner ledger");

    // Second window: verify exit_before_close_secs triggers PositionExited("exit_before_close")
    let close_ts2 = t0 + 120;
    let close_iso2 = iso_z_from_epoch(close_ts2);
    let snap_ebc1 = serde_json::json!({
        "fetched_at": t0 + 25,
        "ticker": "KXBTC15M-EBC",
        "event_ticker": "KXBTC15M-EBCEV",
        "series": "KXBTC15M",
        "close_time": close_iso2,
        "status": "open",
        "result": "",
        "yes_bid": 0.50,
        "yes_ask": 0.50,
        "no_bid": 0.50,
        "no_ask": 0.51,
        "last_price": 0.50,
        "yes_bid_size": 0.0,
        "no_bid_size": 10.0
    });
    writeln!(f, "{snap_ebc1}").unwrap();
    f.flush().unwrap();
    // Quote @ 49c (clamped below 50c ask); cross at 48c then jump within 45s of close_ts2
    std::thread::sleep(Duration::from_millis(160));
    let snap_ebc_fill = serde_json::json!({
        "fetched_at": t0 + 30,
        "ticker": "KXBTC15M-EBC",
        "event_ticker": "KXBTC15M-EBCEV",
        "series": "KXBTC15M",
        "close_time": close_iso2,
        "status": "open",
        "result": "",
        "yes_bid": 0.47,
        "yes_ask": 0.48,
        "no_bid": 0.52,
        "no_ask": 0.53,
        "last_price": 0.48,
        "yes_bid_size": 10.0,
        "no_bid_size": 10.0
    });
    writeln!(f, "{snap_ebc_fill}").unwrap();
    f.flush().unwrap();
    std::thread::sleep(Duration::from_millis(160));
    let snap_ebc_exit = serde_json::json!({
        "fetched_at": close_ts2 - 45,
        "ticker": "KXBTC15M-EBC",
        "event_ticker": "KXBTC15M-EBCEV",
        "series": "KXBTC15M",
        "close_time": close_iso2,
        "status": "open",
        "result": "",
        "yes_bid": 0.50,
        "yes_ask": 0.52,
        "no_bid": 0.48,
        "no_ask": 0.50,
        "last_price": 0.50,
        "yes_bid_size": 10.0,
        "no_bid_size": 10.0
    });
    writeln!(f, "{snap_ebc_exit}").unwrap();
    f.flush().unwrap();

    let mut ebc_exited = false;
    for _ in 0..40 {
        let (_, lb) = http_req(port, "GET", "/api/runner/ledger?limit=50", &host, &[], None, "");
        let lv: serde_json::Value = serde_json::from_str(&lb).unwrap();
        if lv["entries"].as_array().is_some_and(|arr| {
            arr.iter()
                .any(|e| e["event"] == "PositionExited" && e["ticker"] == "KXBTC15M-EBC" && e["reason"] == "exit_before_close")
        }) {
            ebc_exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    assert!(ebc_exited, "expected PositionExited(exit_before_close) on KXBTC15M-EBC");

    // Tamper runner_state.json mode="live_auto" and restart -> MUST clamp to stopped & live_armed=false
    let _ = guard.child.kill();
    let _ = guard.child.wait();
    let rs_path = data_dir.join("runner_state.json");
    let mut rs_v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&rs_path).unwrap()).unwrap();
    rs_v["mode"] = serde_json::json!("live_auto");
    std::fs::write(&rs_path, serde_json::to_string_pretty(&rs_v).unwrap()).unwrap();

    let port2 = free_port();
    let _guard2 = spawn_server(&data_dir, &strat_dir, port2, Some(tmp));
    let host2 = format!("127.0.0.1:{port2}");
    let (code, sb2) = http_req(port2, "GET", "/api/runner/status", &host2, &[], None, "");
    assert_eq!(code, 200);
    let sv2: serde_json::Value = serde_json::from_str(&sb2).unwrap();
    assert_eq!(sv2["mode"], "stopped");
    assert_eq!(sv2["live_armed"], false);
}

#[test]
fn live_auto_runner_reprice_and_tp_exit_through_risk_rails() {
    let tmp = std::env::temp_dir().join(format!("ob-v033-liveauto-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let data_dir = tmp.join("data");
    let strat_dir = tmp.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    let (priv_pem, pub_der) = generate_ephemeral_rsa(&tmp);
    let key_path = tmp.join("kalshi_la.pem");
    std::fs::write(&key_path, &priv_pem).unwrap();
    let (kalshi_port, kalshi_state) = spawn_mock_kalshi("la-test-key".to_string(), pub_der);
    kalshi_state.lock().unwrap().auto_fill_new_sell = true;

    std::fs::write(
        strat_dir.join("la_maker.rn"),
        r#"
pub fn name() { "la-maker" }
pub fn side() { "yes" }
pub fn describe() { "Live-auto maker quoting yes_bid." }
pub fn evaluate(ctx) {
    if ctx.slot_position_contracts > 0 {
        return None;
    }
    let bid_c = (ctx.yes_bid * 100.0).round() as i64;
    if bid_c >= 1 && bid_c <= 98 {
        return Some(#{ enter: true, side: "yes", limit_cents: bid_c, post_only: true });
    }
    None
}
"#,
    )
    .unwrap();

    let t0 = now_unix();
    let close_iso = iso_z_from_epoch(t0 + 900);
    let snap1 = serde_json::json!({
        "fetched_at": t0,
        "ticker": "KXBTC15M-LA1",
        "event_ticker": "KXBTC15M-LA1EV",
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": "open",
        "result": "",
        "yes_bid": 0.45,
        "yes_ask": 0.55,
        "no_bid": 0.45,
        "no_ask": 0.55,
        "last_price": 0.50,
        "yes_bid_size": 10.0,
        "no_bid_size": 10.0
    });
    std::fs::write(data_dir.join("snapshots.jsonl"), format!("{snap1}\n")).unwrap();

    let port = free_port();
    let _guard = spawn_server(&data_dir, &strat_dir, port, Some(tmp));
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    let cfg_body = serde_json::json!({
        "api_key_id": "la-test-key",
        "private_key_path": key_path.to_str().unwrap(),
        "environment": "demo",
        "api_base_url": format!("http://127.0.0.1:{kalshi_port}/trade-api/v2"),
        "rails": {
            "max_order_contracts": 10,
            "max_order_dollars": 10.0,
            "max_daily_loss_dollars": 25.0,
            "max_open_exposure_dollars": 20.0,
            "allowed_ticker_prefix": "KXBTC15M-"
        }
    })
    .to_string();
    assert_eq!(
        http_req(port, "POST", "/api/live/config", &host, &csrf, Some("application/json"), &cfg_body).0,
        200
    );
    assert_eq!(
        http_req(port, "POST", "/api/live/reconcile", &host, &csrf, Some("application/json"), "{}").0,
        200
    );
    assert_eq!(
        http_req(port, "POST", "/api/live/arm", &host, &csrf, Some("application/json"), r#"{"confirm_arm":true}"#).0,
        200
    );

    let r_cfg = serde_json::json!({
        "cancel_before_close_secs": 30,
        "bankroll_dollars": 500.0,
        "tick_interval_ms": 80,
        "slots": [{
            "slot_id": 1,
            "strategy": "la-maker",
            "series": ["KXBTC15M"],
            "max_contracts": 3,
            "execution_mode": "maker",
            "auto_pause_drawdown_dollars": 50.0,
            "enabled": true,
            "reprice_cooldown_secs": 0,
            "max_replaces_per_window": 2,
            "take_profit_cents": 15,
            "stop_loss_cents": 15,
            "exit_before_close_secs": 0
        }]
    })
    .to_string();
    assert_eq!(
        http_req(port, "POST", "/api/runner/config", &host, &csrf, Some("application/json"), &r_cfg).0,
        200
    );
    assert_eq!(
        http_req(port, "POST", "/api/runner/start", &host, &csrf, Some("application/json"), r#"{"mode":"live_auto","confirm_auto_live":true}"#).0,
        200
    );

    // Wait for live resting order @ 45c
    let mut live_oid = String::new();
    for _ in 0..40 {
        let (_, sb) = http_req(port, "GET", "/api/runner/status", &host, &[], None, "");
        let sv: serde_json::Value = serde_json::from_str(&sb).unwrap();
        if let Some(q) = sv["open_quotes"].as_array().and_then(|a| a.first()) {
            if q["mode"] == "live" && q["limit_cents"] == 45 {
                live_oid = q["live_order_id"].as_str().unwrap().to_string();
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    assert!(!live_oid.is_empty(), "expected live_auto quote @ 45c");

    // Append snap2 -> yes_bid=0.48 -> live_auto reprices via Kalshi /amend!
    let snap2 = serde_json::json!({
        "fetched_at": t0 + 5,
        "ticker": "KXBTC15M-LA1",
        "event_ticker": "KXBTC15M-LA1EV",
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": "open",
        "result": "",
        "yes_bid": 0.48,
        "yes_ask": 0.55,
        "no_bid": 0.45,
        "no_ask": 0.52,
        "last_price": 0.50,
        "yes_bid_size": 10.0,
        "no_bid_size": 10.0
    });
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(data_dir.join("snapshots.jsonl"))
        .unwrap();
    writeln!(f, "{snap2}").unwrap();
    f.flush().unwrap();

    let mut live_amended = false;
    for _ in 0..40 {
        let (_, sb) = http_req(port, "GET", "/api/runner/status", &host, &[], None, "");
        let sv: serde_json::Value = serde_json::from_str(&sb).unwrap();
        if sv["open_quotes"]
            .as_array()
            .and_then(|a| a.first())
            .is_some_and(|q| q["limit_cents"] == 48 && q["replace_count"] == 1)
        {
            live_amended = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    assert!(live_amended, "expected live_auto order amend to 48c");
    assert_eq!(kalshi_state.lock().unwrap().amend_calls, 1);

    // Simulate venue fill on live_oid and reconcile
    {
        let mut ks = kalshi_state.lock().unwrap();
        for o in &mut ks.orders {
            if o["order_id"].as_str() == Some(&live_oid) {
                o["status"] = serde_json::json!("executed");
                o["remaining_count"] = serde_json::json!(0);
                o["fill_count"] = serde_json::json!(3);
            }
        }
        ks.fills.push(serde_json::json!({
            "trade_id": "tr-la-1",
            "order_id": live_oid,
            "ticker": "KXBTC15M-LA1",
            "action": "buy",
            "side": "yes",
            "count": 3,
            "yes_price": 48,
            "is_taker": false,
            "ts": t0 + 8
        }));
    }
    assert_eq!(
        http_req(port, "POST", "/api/live/reconcile", &host, &csrf, Some("application/json"), "{}").0,
        200
    );

    // Append snap3 -> yes_bid=0.65 (+17c >= 15c TP) -> live_auto routes sell exit order!
    let snap3 = serde_json::json!({
        "fetched_at": t0 + 12,
        "ticker": "KXBTC15M-LA1",
        "event_ticker": "KXBTC15M-LA1EV",
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": "open",
        "result": "",
        "yes_bid": 0.65,
        "yes_ask": 0.68,
        "no_bid": 0.32,
        "no_ask": 0.35,
        "last_price": 0.65,
        "yes_bid_size": 10.0,
        "no_bid_size": 10.0
    });
    writeln!(f, "{snap3}").unwrap();
    f.flush().unwrap();

    let mut live_exited = false;
    for _ in 0..40 {
        let (_, lb) = http_req(port, "GET", "/api/runner/ledger?limit=50", &host, &[], None, "");
        let lv: serde_json::Value = serde_json::from_str(&lb).unwrap();
        if lv["entries"].as_array().is_some_and(|arr| {
            arr.iter()
                .any(|e| e["event"] == "PositionExited" && e["mode"] == "live" && e["reason"] == "take_profit")
        }) {
            live_exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(60));
    }
    assert!(live_exited, "expected live_auto PositionExited(take_profit)");
    assert!(
        kalshi_state
            .lock()
            .unwrap()
            .placed_orders
            .iter()
            .any(|o| o["action"] == "sell" && o["ticker"] == "KXBTC15M-LA1"),
        "expected closing sell order routed to mock Kalshi"
    );
}
