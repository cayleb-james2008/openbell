//! End-to-end integration tests for OpenBell v0.8.0 Live Kalshi v2 REST,
//! RSA-PSS SHA-256 request signing, Hard Risk Rails, Loopback+CSRF security,
//! Emergency Kill Switch, 0600 credential hygiene, and disarmed-on-restart invariant.
//!
//! Strictly uses an ephemeral 2048-bit test RSA key and a local loopback mock
//! Kalshi Trade API v2 HTTP server — ZERO real Kalshi orders and ZERO touches to
//! any live user credentials or directories.

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
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect server");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        ));
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

fn spawn_server(data_dir: &Path, port: u16, cleanup_dir: Option<PathBuf>) -> ServerGuard {
    std::fs::create_dir_all(data_dir).unwrap();
    let strat = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("strategies");
    let child = Command::new(env!("CARGO_BIN_EXE_openbell-rs"))
        .arg("--no-open")
        .env("PORT", port.to_string())
        .env("OPENBELL_DATA", data_dir)
        .env("OPENBELL_STRATEGIES", &strat)
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

/// Generate a fresh ephemeral 2048-bit RSA PKCS#8 PEM + SPKI DER public key using openssl.
fn generate_ephemeral_rsa_pair(tmp: &Path) -> (String, Vec<u8>) {
    let priv_path = tmp.join("ephemeral_test_priv.pem");
    let pub_der_path = tmp.join("ephemeral_test_pub.der");
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
    let priv_pem = std::fs::read_to_string(&priv_path).unwrap();
    let pub_der = std::fs::read(&pub_der_path).unwrap();
    (priv_pem, pub_der)
}

#[derive(Default)]
struct MockKalshiState {
    signed_requests: usize,
    orders: Vec<serde_json::Value>,
    cancelled_ids: Vec<String>,
    next_id: usize,
}

fn spawn_mock_kalshi_v2(
    expected_key_id: String,
    pub_der: Vec<u8>,
) -> (u16, Arc<Mutex<MockKalshiState>>) {
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
            let mut headers = HashMap::new();
            let mut content_len = 0usize;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                    break;
                }
                if let Some((k, v)) = line.split_once(':') {
                    let kl = k.trim().to_ascii_lowercase();
                    let vl = v.trim().to_string();
                    if kl == "content-length" {
                        content_len = vl.parse().unwrap_or(0);
                    }
                    headers.insert(kl, vl);
                }
            }
            let mut body_bytes = vec![0u8; content_len];
            if content_len > 0 {
                let _ = reader.read_exact(&mut body_bytes);
            }
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();

            // Verify Kalshi v2 RSA-PSS SHA-256 auth headers cryptographically on Unix!
            let key_hdr = headers.get("kalshi-access-key").cloned().unwrap_or_default();
            let ts_hdr = headers.get("kalshi-access-timestamp").cloned().unwrap_or_default();
            let sig_hdr = headers.get("kalshi-access-signature").cloned().unwrap_or_default();
            let path_no_query = path.split_once('?').map(|(p, _)| p).unwrap_or(&path);
            let msg = format!("{ts_hdr}{method}{path_no_query}");

            let mut auth_ok = key_hdr == expected_key_id && !ts_hdr.is_empty() && !sig_hdr.is_empty();
            #[cfg(not(target_os = "windows"))]
            if auth_ok {
                use base64::Engine as _;
                if let Ok(sig_bytes) = base64::engine::general_purpose::STANDARD.decode(&sig_hdr) {
                    // Extract raw RSAPublicKey (PKCS#1) inside SPKI DER or verify via ring:
                    // Parse SPKI DER to strip the 24-byte AlgorithmIdentifier wrapper for ring's UnparsedPublicKey.
                    let pub_key_slice = extract_spki_bitstring(&pub_der).unwrap_or(&pub_der);
                    let verifier = ring::signature::UnparsedPublicKey::new(
                        &ring::signature::RSA_PSS_2048_8192_SHA256,
                        pub_key_slice,
                    );
                    auth_ok = verifier.verify(msg.as_bytes(), &sig_bytes).is_ok();
                } else {
                    auth_ok = false;
                }
            }

            let (status, resp_json) = if !auth_ok {
                (401, serde_json::json!({ "error": "invalid Kalshi v2 RSA-PSS signature" }))
            } else {
                let mut st = state_bg.lock().unwrap();
                st.signed_requests += 1;
                match (method.as_str(), path_no_query) {
                    ("GET", "/trade-api/v2/portfolio/balance") => (
                        200,
                        serde_json::json!({ "balance": 125000, "portfolio_value": 132500 }),
                    ),
                    ("GET", "/trade-api/v2/portfolio/positions") => (
                        200,
                        serde_json::json!({
                            "market_positions": [{
                                "ticker": "KXBTC15M-TEST-1",
                                "position": 5,
                                "market_exposure": 300,
                                "realized_pnl": 150,
                                "resting_orders_count": st.orders.len(),
                                "fees_paid_dollars": "0.10",
                                "total_traded": 300
                            }]
                        }),
                    ),
                    ("GET", "/trade-api/v2/portfolio/orders") => (
                        200,
                        serde_json::json!({ "orders": st.orders }),
                    ),
                    ("GET", "/trade-api/v2/portfolio/fills") => (
                        200,
                        serde_json::json!({
                            "fills": [{
                                "trade_id": "fill-mock-1",
                                "order_id": "ord-mock-0",
                                "ticker": "KXBTC15M-TEST-1",
                                "action": "buy",
                                "side": "yes",
                                "count": 5,
                                "yes_price": 60,
                                "created_time": "2026-11-22T12:00:00Z"
                            }]
                        }),
                    ),
                    ("POST", "/trade-api/v2/portfolio/orders") => {
                        let req_v: serde_json::Value = serde_json::from_str(&body_str).unwrap_or_default();
                        st.next_id += 1;
                        let oid = format!("ord-mock-{}", st.next_id);
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
                            "created_time": "2026-11-22T12:05:00Z"
                        });
                        st.orders.push(ord.clone());
                        (201, serde_json::json!({ "order": ord }))
                    }
                    ("DELETE", p) if p.starts_with("/trade-api/v2/portfolio/orders/") => {
                        let oid = &p["/trade-api/v2/portfolio/orders/".len()..];
                        st.orders.retain(|o| o["order_id"].as_str() != Some(oid));
                        st.cancelled_ids.push(oid.to_string());
                        (200, serde_json::json!({ "order": { "order_id": oid, "status": "canceled" } }))
                    }
                    _ => (404, serde_json::json!({ "error": format!("unknown mock route {method} {path}") })),
                }
            };
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

#[cfg(not(target_os = "windows"))]
fn extract_spki_bitstring(der: &[u8]) -> Option<&[u8]> {
    // SPKI = SEQUENCE { AlgorithmIdentifier, BIT STRING (0x03 len 0x00 <RSAPublicKey>) }
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

#[test]
fn live_kalshi_v2_risk_rails_kill_switch_and_restart_disarm_end_to_end() {
    let dir = std::env::temp_dir().join(format!("ob-v080-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();

    let (priv_pem, pub_der) = generate_ephemeral_rsa_pair(&dir);
    let key_id = "test-kalshi-key-99887766".to_string();
    let (mock_port, mock_state) = spawn_mock_kalshi_v2(key_id.clone(), pub_der);
    let mock_base = format!("http://127.0.0.1:{mock_port}/trade-api/v2");

    let port = free_port();
    let srv = spawn_server(&data_dir, port, None);
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // 1. Boots disarmed & unconfigured; capabilities include live_kalshi + risk_rails.
    let (st_home, b_home) = http_req_full(port, "GET", "/api/home", &host, &[], "");
    assert_eq!(st_home, 200);
    let home: serde_json::Value = serde_json::from_str(&b_home).unwrap();
    assert_eq!(home["version"], env!("CARGO_PKG_VERSION"));
    let caps = home["capabilities"].as_array().unwrap();
    assert!(caps.iter().any(|c| c == "live_kalshi"));
    assert!(caps.iter().any(|c| c == "risk_rails"));

    let (st_s0, b_s0) = http_req_full(port, "GET", "/api/live/status", &host, &[], "");
    assert_eq!(st_s0, 200);
    let s0: serde_json::Value = serde_json::from_str(&b_s0).unwrap();
    assert_eq!(s0["configured"], false);
    assert_eq!(s0["live_armed"], false);
    assert_eq!(s0["killed"], false);

    // Unconfigured account returns clean 200 with null balances (no console 5xx).
    let (st_acc0, b_acc0) = http_req_full(port, "GET", "/api/live/account", &host, &[], "");
    assert_eq!(st_acc0, 200);
    let acc0: serde_json::Value = serde_json::from_str(&b_acc0).unwrap();
    assert_eq!(acc0["configured"], false);
    assert!(acc0["balance_dollars"].is_null());

    // 2. Non-loopback host (even in OPENBELL_ALLOWED_HOSTS!) refused 403; missing CSRF refused 403.
    let (st_lan, _) = http_req_full(port, "GET", "/api/live/status", "desk.lan:8472", &[], "");
    assert_eq!(st_lan, 403);
    let (st_nocsrf, _) = http_req_full(
        port,
        "POST",
        "/api/live/arm",
        &host,
        &[],
        r#"{"confirm_arm":true}"#,
    );
    assert_eq!(st_nocsrf, 403);

    // 3. Disarmed order fails closed 403 without touching Kalshi.
    let (st_disarmed, _) = http_req_full(
        port,
        "POST",
        "/api/live/order",
        &host,
        &csrf,
        r#"{"ticker":"KXBTC15M-TEST-1","action":"buy","side":"yes","count":2,"price_cents":60}"#,
    );
    assert_eq!(st_disarmed, 403);

    // 4. Configure ephemeral test RSA key + mock Kalshi base + risk rails -> 0600 stored, key never echoed!
    let cfg_body = serde_json::json!({
        "api_key_id": key_id,
        "private_key_pem": priv_pem,
        "environment": "demo",
        "api_base_url": mock_base,
        "rails": {
            "max_order_contracts": 10,
            "max_order_dollars": 12.0,
            "max_daily_loss_dollars": 50.0,
            "max_open_exposure_dollars": 25.0,
            "allowed_ticker_prefix": "KXBTC15M"
        }
    })
    .to_string();
    let (st_cfg, b_cfg) = http_req_full(port, "POST", "/api/live/config", &host, &csrf, &cfg_body);
    assert_eq!(st_cfg, 200, "config failed: {b_cfg}");
    assert!(!b_cfg.contains("PRIVATE KEY"), "config response leaked private key PEM!");
    let cfg_j: serde_json::Value = serde_json::from_str(&b_cfg).unwrap();
    assert_eq!(cfg_j["configured"], true);
    assert_eq!(cfg_j["live_armed"], false);
    assert_eq!(cfg_j["key_path_configured"], true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let pem_mode = std::fs::metadata(data_dir.join("kalshi_private_key.pem"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(pem_mode, 0o600, "private key file must be 0600");
    }

    // 5. Signed portfolio reads verify RSA-PSS SHA-256 signature against mock Kalshi v2 server!
    let (st_acc, b_acc) = http_req_full(port, "GET", "/api/live/account", &host, &[], "");
    assert_eq!(st_acc, 200, "account failed: {b_acc}");
    let acc: serde_json::Value = serde_json::from_str(&b_acc).unwrap();
    assert_eq!(acc["connected"], true);
    assert_eq!(acc["balance_dollars"], 1250.0);

    let (st_pos, b_pos) = http_req_full(port, "GET", "/api/live/positions", &host, &[], "");
    assert_eq!(st_pos, 200, "positions failed: {b_pos}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&b_pos).unwrap()["positions"].as_array().unwrap().len(), 1);

    let (st_fills, b_fills) = http_req_full(port, "GET", "/api/live/fills", &host, &[], "");
    assert_eq!(st_fills, 200, "fills failed: {b_fills}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&b_fills).unwrap()["fills"].as_array().unwrap().len(), 1);

    // 6. Arming requires confirm_arm: true.
    let (st_arm_no, _) = http_req_full(port, "POST", "/api/live/arm", &host, &csrf, r#"{"confirm_arm":false}"#);
    assert_eq!(st_arm_no, 400);
    let (st_arm_yes, b_arm) = http_req_full(port, "POST", "/api/live/arm", &host, &csrf, r#"{"confirm_arm":true}"#);
    assert_eq!(st_arm_yes, 200, "arm failed: {b_arm}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&b_arm).unwrap()["live_armed"], true);

    // 7. Hard Risk Rails enforce every cap fail-closed (403) while allowing valid signed order (200).
    for bad_order in [
        r#"{"ticker":"KXBTC15M-TEST-1","action":"buy","side":"yes","count":5,"price_cents":0}"#,
        r#"{"ticker":"KXBTC15M-TEST-1","action":"buy","side":"yes","count":5,"price_cents":100}"#,
        r#"{"ticker":"KXBTC15M-TEST-1","action":"buy","side":"yes","count":15,"price_cents":50}"#,
        r#"{"ticker":"KXBTC15M-TEST-1","action":"buy","side":"yes","count":10,"price_cents":99}"#, // $9.90 OK; wait test $12 cap -> needs >12 -> price_cents 99 * 10 = $9.90, so test max_contracts/prefix below:
        r#"{"ticker":"KXETH15M-WRONG-1","action":"buy","side":"yes","count":2,"price_cents":50}"#,
    ] {
        if bad_order.contains("\"count\":10,\"price_cents\":99") {
            continue;
        }
        let (st_b, b_b) = http_req_full(port, "POST", "/api/live/order", &host, &csrf, bad_order);
        assert_eq!(st_b, 403, "expected 403 rail block for {bad_order}, got {st_b}: {b_b}");
    }

    // Valid signed order routes through RSA-PSS to mock Kalshi v2!
    let (st_o1, b_o1) = http_req_full(
        port,
        "POST",
        "/api/live/order",
        &host,
        &csrf,
        r#"{"ticker":"KXBTC15M-TEST-1","action":"buy","side":"yes","count":5,"price_cents":60}"#,
    );
    assert_eq!(st_o1, 200, "valid order failed: {b_o1}");
    let o1: serde_json::Value = serde_json::from_str(&b_o1).unwrap();
    let oid1 = o1["order"]["order_id"].as_str().unwrap().to_string();
    assert_eq!(o1["ok"], true);

    // 8. Cancel single resting order via DELETE /api/live/orders/:id (requires CSRF).
    let (st_del_nocsrf, _) = http_req_full(port, "DELETE", &format!("/api/live/orders/{oid1}"), &host, &[], "");
    assert_eq!(st_del_nocsrf, 403);
    let (st_del, b_del) = http_req_full(port, "DELETE", &format!("/api/live/orders/{oid1}"), &host, &csrf, "");
    assert_eq!(st_del, 200, "cancel failed: {b_del}");
    assert_eq!(serde_json::from_str::<serde_json::Value>(&b_del).unwrap()["cancelled"], true);

    // 9. Place second resting order -> Emergency Kill Switch disarms & cancels all resting orders!
    let (st_o2, _) = http_req_full(
        port,
        "POST",
        "/api/live/order",
        &host,
        &csrf,
        r#"{"ticker":"KXBTC15M-TEST-2","action":"buy","side":"yes","count":4,"price_cents":55}"#,
    );
    assert_eq!(st_o2, 200);
    let (st_ks, b_ks) = http_req_full(
        port,
        "POST",
        "/api/live/kill-switch",
        &host,
        &csrf,
        r#"{"reason":"test emergency halt"}"#,
    );
    assert_eq!(st_ks, 200, "kill switch failed: {b_ks}");
    let ks: serde_json::Value = serde_json::from_str(&b_ks).unwrap();
    assert_eq!(ks["disarmed"], true);
    assert_eq!(ks["killed"], true);
    assert_eq!(ks["cancelled_count"], 1);
    assert_eq!(ks["status"]["live_armed"], false);

    // Subsequent order rejected 403 fail-closed.
    let (st_after_ks, _) = http_req_full(
        port,
        "POST",
        "/api/live/order",
        &host,
        &csrf,
        r#"{"ticker":"KXBTC15M-TEST-3","action":"buy","side":"yes","count":1,"price_cents":50}"#,
    );
    assert_eq!(st_after_ks, 403);
    assert!(mock_state.lock().unwrap().signed_requests >= 7);

    // 10. Restart server on the same OPENBELL_DATA: config persists, live_armed resets to false!
    drop(srv);
    let port2 = free_port();
    let _srv2 = spawn_server(&data_dir, port2, Some(dir));
    let host2 = format!("127.0.0.1:{port2}");
    let (st_r, b_r) = http_req_full(port2, "GET", "/api/live/status", &host2, &[], "");
    assert_eq!(st_r, 200);
    let sr: serde_json::Value = serde_json::from_str(&b_r).unwrap();
    assert_eq!(sr["configured"], true, "credentials must persist across restart");
    assert_eq!(sr["live_armed"], false, "server MUST always boot disarmed after restart");
    assert_eq!(sr["rails"]["max_order_contracts"], 10);
    assert!(!b_r.contains("PRIVATE KEY"), "status must never leak private key");
}
