//! End-to-end integration tests for OpenBell v0.30.0 Closed-Loop Live Venue
//! Reconciliation, Persistent Daily Loss / Kill-Switch Runtime State, Stale Venue Sync
//! Fail-Closed Gate, Exposure Release, and `live_executions.jsonl` Idempotency.
//!
//! Strictly uses ephemeral local RSA keys and local loopback `TcpListener` mocks.

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

fn iso_z(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    let h = secs / 3_600;
    let m = (secs % 3_600) / 60;
    let s = secs % 60;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mon = mp + if mp < 10 { 3 } else { -9 };
    let year = y + if mon <= 2 { 1 } else { 0 };
    format!("{year:04}-{mon:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
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
    if !body.is_empty() || !matches!(method, "GET" | "HEAD" | "DELETE") {
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

fn http_json(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    csrf: bool,
    body: &str,
) -> (u16, String) {
    let hdrs: Vec<(&str, &str)> = if csrf {
        vec![("X-OpenBell-CSRF", "1")]
    } else {
        vec![]
    };
    let ct = if matches!(method, "GET" | "HEAD") {
        None
    } else if !body.is_empty() || method == "POST" {
        Some("application/json")
    } else {
        None
    };
    http_req(port, method, path, host, &hdrs, ct, body)
}

fn spawn_server(
    data_dir: &Path,
    strat_dir: &Path,
    port: u16,
    reconcile_secs: Option<u64>,
    cleanup_dir: Option<PathBuf>,
) -> ServerGuard {
    std::fs::create_dir_all(data_dir).unwrap();
    std::fs::create_dir_all(strat_dir).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openbell-rs"));
    cmd.arg("--no-open")
        .env("PORT", port.to_string())
        .env("OPENBELL_DATA", data_dir)
        .env("OPENBELL_STRATEGIES", strat_dir)
        .env("OPENBELL_COLLECT_SECS", "3600")
        .env("OPENBELL_AUTO_COLLECT", "0")
        .env("OPENBELL_ORDERBOOK_OFFLINE", "1")
        .env("OPENBELL_UPDATE_MODE", "off")
        .env("OPENBELL_NO_OPEN", "1")
        .env("OPENBELL_ALLOWED_HOSTS", "desk.lan")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(rs) = reconcile_secs {
        cmd.env("OPENBELL_RECONCILE_SECS", rs.to_string());
    }
    let child = cmd.spawn().expect("spawn openbell-rs");
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

fn generate_ephemeral_rsa_pem(dir: &Path) -> String {
    let priv_path = dir.join("ephemeral_test_priv.pem");
    let st = Command::new("openssl")
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
    assert!(st.success(), "openssl genpkey failed");
    std::fs::read_to_string(&priv_path).unwrap()
}

fn wait_until<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(v) = f() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(35));
    }
    panic!("wait_until timed out after {timeout:?}");
}

#[derive(Default)]
struct MockVenueState {
    positions: Vec<serde_json::Value>,
    orders: Vec<serde_json::Value>,
    fills_page1: Vec<serde_json::Value>,
    fills_page2: Vec<serde_json::Value>,
    settlements: Vec<serde_json::Value>,
    placed_orders: Vec<serde_json::Value>,
    cancelled_ids: Vec<String>,
    fail_settlements_404: bool,
    next_id: usize,
}

fn spawn_mock_kalshi() -> (u16, Arc<Mutex<MockVenueState>>) {
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
            let parts: Vec<&str> = req_line.split_whitespace().collect();
            if parts.len() < 2 {
                continue;
            }
            let method = parts[0].to_string();
            let path = parts[1].to_string();
            let (path_no_query, query) = path.split_once('?').unwrap_or((&path, ""));
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
            assert!(
                headers.contains_key("kalshi-access-signature"),
                "every mock Kalshi request must be signed"
            );
            let mut body_bytes = vec![0u8; content_len];
            if content_len > 0 {
                let _ = reader.read_exact(&mut body_bytes);
            }
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();

            let mut st = state_bg.lock().unwrap();
            let (status, resp_json) = match (method.as_str(), path_no_query) {
                ("GET", "/trade-api/v2/portfolio/positions") => (
                    200,
                    serde_json::json!({ "market_positions": st.positions }),
                ),
                ("GET", "/trade-api/v2/portfolio/orders") => (
                    200,
                    serde_json::json!({ "orders": st.orders }),
                ),
                ("GET", "/trade-api/v2/portfolio/fills") => {
                    if query.contains("cursor=page-2") {
                        (
                            200,
                            serde_json::json!({ "fills": st.fills_page2, "cursor": "" }),
                        )
                    } else if !st.fills_page2.is_empty() {
                        (
                            200,
                            serde_json::json!({ "fills": st.fills_page1, "cursor": "page-2" }),
                        )
                    } else {
                        (
                            200,
                            serde_json::json!({ "fills": st.fills_page1 }),
                        )
                    }
                }
                ("GET", "/trade-api/v2/portfolio/settlements") => {
                    if st.fail_settlements_404 {
                        (404, serde_json::json!({ "error": "settlements not found" }))
                    } else {
                        (
                            200,
                            serde_json::json!({ "settlements": st.settlements }),
                        )
                    }
                }
                ("POST", "/trade-api/v2/portfolio/orders") => {
                    let req_v: serde_json::Value = serde_json::from_str(&body_str).unwrap_or_default();
                    st.placed_orders.push(req_v.clone());
                    st.next_id += 1;
                    let oid = format!("ord-v30-{}", st.next_id);
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
                _ => (404, serde_json::json!({ "error": format!("unknown {method} {path}") })),
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
fn runtime_persistence_reset_kill_exposure_release_and_paginated_reconcile() {
    let dir = std::env::temp_dir().join(format!("ob-v030-rec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    std::fs::write(
        strat_dir.join("rec_maker.rn"),
        r#"
pub fn name() { "rec-maker" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { secs_to_close >= 60 }
pub fn entry_quote(yes_ask, no_ask, secs_to_close) { Some(0.60) }
"#,
    )
    .unwrap();

    let priv_pem = generate_ephemeral_rsa_pem(&dir);
    let (mock_port, mock_state) = spawn_mock_kalshi();
    let mock_base = format!("http://127.0.0.1:{mock_port}/trade-api/v2");

    let port = free_port();
    let srv = spawn_server(&data_dir, &strat_dir, port, Some(20), None);
    let host = format!("127.0.0.1:{port}");

    // 1. Configure + Arm + verify status includes last_venue_sync_epoch & venue_sync_stale.
    let cfg_body = serde_json::json!({
        "api_key_id": "v30-reconcile-key",
        "private_key_pem": priv_pem,
        "environment": "demo",
        "api_base_url": mock_base,
        "rails": {
            "max_order_contracts": 20,
            "max_order_dollars": 25.0,
            "max_daily_loss_dollars": 50.0,
            "max_open_exposure_dollars": 100.0,
            "allowed_ticker_prefix": "KXBTC15M"
        }
    })
    .to_string();
    assert_eq!(http_json(port, "POST", "/api/live/config", &host, true, &cfg_body).0, 200);

    // Wait for initial background reconcile to set last_venue_sync_epoch.
    wait_until(Duration::from_secs(5), || {
        let (st, body) = http_json(port, "GET", "/api/live/status", &host, false, "");
        if st == 200 {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            if v["last_venue_sync_epoch"].is_i64() && v["venue_sync_stale"] == false {
                return Some(());
            }
        }
        None
    });

    assert_eq!(
        http_json(port, "POST", "/api/live/arm", &host, true, r#"{"confirm_arm":true}"#).0,
        200
    );

    // 2. Place a live buy order (5 * 60c = $3.00), then DELETE /api/live/orders/:id releases exposure!
    let (st_o1, b_o1) = http_json(
        port,
        "POST",
        "/api/live/order",
        &host,
        true,
        r#"{"ticker":"KXBTC15M-EXP1","action":"buy","side":"yes","count":5,"price_cents":60}"#,
    );
    assert_eq!(st_o1, 200, "order failed: {b_o1}");
    let o1: serde_json::Value = serde_json::from_str(&b_o1).unwrap();
    let oid1 = o1["order"]["order_id"].as_str().unwrap().to_string();
    assert!((o1["status"]["open_exposure_dollars"].as_f64().unwrap() - 3.0).abs() < 1e-6);
    assert_eq!(o1["status"]["orders_placed_today"], 1);

    let (st_del, b_del) = http_req(
        port,
        "DELETE",
        &format!("/api/live/orders/{oid1}"),
        &host,
        &[("X-OpenBell-CSRF", "1")],
        None,
        "",
    );
    assert_eq!(st_del, 200, "delete order failed: {b_del}");
    let del_v: serde_json::Value = serde_json::from_str(&b_del).unwrap();
    assert!(
        del_v["status"]["open_exposure_dollars"].as_f64().unwrap().abs() < 1e-6,
        "DELETE /api/live/orders/:id must release reserved exposure: {del_v}"
    );

    // 3. Configure live_auto slot, place live order via ShadowRunner, then reconcile partial fills + settlement!
    let runner_cfg = serde_json::json!({
        "slots": [{
            "slot_id": 1,
            "strategy": "rec-maker",
            "series": ["KXBTC15M"],
            "max_contracts": 10,
            "execution_mode": "maker",
            "auto_pause_drawdown_dollars": 50.0,
            "enabled": true
        }],
        "cancel_before_close_secs": 30,
        "bankroll_dollars": 500.0,
        "tick_interval_ms": 60
    })
    .to_string();
    assert_eq!(http_json(port, "POST", "/api/runner/config", &host, true, &runner_cfg).0, 200);
    assert_eq!(
        http_json(
            port,
            "POST",
            "/api/runner/start",
            &host,
            true,
            r#"{"mode":"live_auto","confirm_auto_live":true}"#
        )
        .0,
        200
    );

    let close_ep = now_unix() + 600;
    let snap_row = serde_json::json!({
        "fetched_at": now_unix(),
        "ticker": "KXBTC15M-REC1",
        "event_ticker": "KXBTC15M-EVREC",
        "series": "KXBTC15M",
        "close_time": "2027-01-20T00:15:00Z",
        "status": "open",
        "result": "",
        "yes_bid": 0.54,
        "yes_ask": 0.62,
        "no_bid": 0.38,
        "no_ask": 0.46,
        "last_price": 0.58
    });
    assert!(close_ep > 0);
    std::fs::write(data_dir.join("snapshots.jsonl"), format!("{snap_row}\n")).unwrap();

    let runner_oid = wait_until(Duration::from_secs(5), || {
        let ms = mock_state.lock().unwrap();
        ms.orders
            .iter()
            .find(|o| o["ticker"] == "KXBTC15M-REC1")
            .and_then(|o| o["order_id"].as_str().map(ToString::to_string))
    });

    // Inject paginated fills (partial 4 @ 56c maker + 6 @ 58c taker with $0.14 fee) & losing settlement
    // so utc_day_loss_dollars > 0!
    let now_ts = now_unix();
    {
        let mut ms = mock_state.lock().unwrap();
        for o in &mut ms.orders {
            if o["order_id"] == runner_oid {
                o["status"] = serde_json::json!("executed");
                o["remaining_count"] = serde_json::json!(0);
                o["fill_count"] = serde_json::json!(10);
            }
        }
        ms.fills_page1.push(serde_json::json!({
            "trade_id": "tr-v30-1",
            "order_id": runner_oid,
            "ticker": "KXBTC15M-REC1",
            "action": "buy",
            "side": "yes",
            "count": 4,
            "yes_price": 56,
            "is_taker": false,
            "ts": now_ts
        }));
        ms.fills_page2.push(serde_json::json!({
            "trade_id": "tr-v30-2",
            "order_id": runner_oid,
            "ticker": "KXBTC15M-REC1",
            "action": "buy",
            "side": "yes",
            "count": 6,
            "yes_price": 58,
            "is_taker": true,
            "fee_cost": "0.14",
            "ts": now_ts
        }));
        ms.settlements.push(serde_json::json!({
            "ticker": "KXBTC15M-REC1",
            "market_result": "no",
            "yes_count": 10,
            "yes_total_cost": 572,
            "no_count": 0,
            "no_total_cost": 0,
            "revenue": 0,
            "fee_cost": "0.00",
            "settled_time": iso_z(now_ts)
        }));
    }

    // Security checks on /api/live/reconcile
    assert_eq!(http_json(port, "GET", "/api/live/reconcile", "desk.lan:8472", false, "").0, 403);
    assert_eq!(http_json(port, "POST", "/api/live/reconcile", &host, false, "{}").0, 403);
    assert_eq!(
        http_req(
            port,
            "POST",
            "/api/live/reconcile",
            &host,
            &[("X-OpenBell-CSRF", "1")],
            Some("text/plain"),
            "{}",
        )
        .0,
        415
    );

    // Trigger POST /api/live/reconcile twice to verify idempotency in live_executions.jsonl.
    let (st_rec1, b_rec1) = http_json(port, "POST", "/api/live/reconcile", &host, true, "{}");
    assert_eq!(st_rec1, 200, "reconcile failed: {b_rec1}");
    let (st_rec2, b_rec2) = http_json(port, "POST", "/api/live/reconcile", &host, true, "{}");
    assert_eq!(st_rec2, 200, "reconcile #2 failed: {b_rec2}");

    let rec_v: serde_json::Value = serde_json::from_str(&b_rec2).unwrap();
    assert_eq!(rec_v["ok"], true);
    assert_eq!(rec_v["sync_stale"], false);
    assert!(rec_v["last_sync_epoch"].is_i64());
    // Loss = $5.72 cost + ~$0.017248 maker fee + $0.14 taker fee = ~$5.877248
    let day_loss = rec_v["utc_day_loss_dollars"].as_f64().unwrap();
    assert!(day_loss > 5.80 && day_loss < 5.95, "unexpected utc_day_loss_dollars={day_loss}: {rec_v}");

    let execs = rec_v["executions"].as_array().unwrap();
    let fill_execs: Vec<&serde_json::Value> =
        execs.iter().filter(|e| e["kind"] == "fill").collect();
    assert_eq!(fill_execs.len(), 2, "fills must be idempotent across 2 reconciles: {execs:?}");
    assert!(fill_execs.iter().any(|e| e["price_cents"] == 56 && e["is_taker"] == false));
    assert!(fill_execs.iter().any(|e| e["price_cents"] == 58 && e["is_taker"] == true && e["fee_dollars"] == 0.14));
    assert_eq!(
        execs.iter().filter(|e| e["kind"] == "settlement").count(),
        1,
        "settlement must be idempotent"
    );

    // Check ShadowRunner ledger recorded real fill_cents (56, 58), slippage (-4, -2), fees, & settlement!
    let (_, b_rled) = http_json(port, "GET", "/api/runner/ledger?limit=50", &host, false, "");
    let rled: serde_json::Value = serde_json::from_str(&b_rled).unwrap();
    let r_entries = rled["entries"].as_array().unwrap();
    let r_fills: Vec<&serde_json::Value> = r_entries
        .iter()
        .filter(|e| e["event"] == "OrderFilled" && e["ticker"] == "KXBTC15M-REC1")
        .collect();
    assert_eq!(r_fills.len(), 2, "expected 2 partial live OrderFilled events: {r_entries:?}");
    assert!(r_fills.iter().any(|e| e["fill_cents"] == 56 && e["slippage_cents"] == -4.0));
    assert!(r_fills.iter().any(|e| e["fill_cents"] == 58 && e["fee_dollars"] == 0.14));

    // 4. Trigger kill-switch -> persists killed=true, daily_realized_loss_dollars, orders_placed_today
    //    in risk_rails_runtime.json (NEVER persisting live_armed).
    assert_eq!(
        http_json(
            port,
            "POST",
            "/api/live/kill-switch",
            &host,
            true,
            r#"{"reason":"v030 runtime persistence check"}"#
        )
        .0,
        200
    );
    let rt_path = data_dir.join("risk_rails_runtime.json");
    let rt_raw = std::fs::read_to_string(&rt_path).unwrap();
    assert!(!rt_raw.contains("live_armed"), "risk_rails_runtime.json must NEVER store live_armed");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::metadata(&rt_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(m, 0o600, "risk_rails_runtime.json must be 0600");
        let em = std::fs::metadata(data_dir.join("live_executions.jsonl"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(em, 0o600, "live_executions.jsonl must be 0600");
    }

    // Tamper live_armed: true into runtime JSON -> restart MUST STILL boot live_armed=false!
    drop(srv);
    let mut rt_val: serde_json::Value = serde_json::from_str(&rt_raw).unwrap();
    rt_val["live_armed"] = serde_json::json!(true);
    std::fs::write(&rt_path, rt_val.to_string()).unwrap();

    let port2 = free_port();
    let _srv2 = spawn_server(&data_dir, &strat_dir, port2, Some(20), Some(dir.clone()));
    let host2 = format!("127.0.0.1:{port2}");

    let (st_boot2, b_boot2) = http_json(port2, "GET", "/api/live/status", &host2, false, "");
    assert_eq!(st_boot2, 200);
    let s2: serde_json::Value = serde_json::from_str(&b_boot2).unwrap();
    assert_eq!(s2["live_armed"], false, "MUST boot disarmed even if runtime JSON tampered");
    assert_eq!(s2["killed"], true, "killed latch must survive restart");
    assert_eq!(s2["kill_reason"], "v030 runtime persistence check");
    assert!(s2["orders_placed_today"].as_u64().unwrap() >= 2);
    assert!(s2["daily_realized_loss_dollars"].as_f64().unwrap() > 5.80);

    // Arming while killed is refused.
    assert_eq!(
        http_json(port2, "POST", "/api/live/arm", &host2, true, r#"{"confirm_arm":true}"#).0,
        400
    );

    // 5. POST /api/live/reset-kill requires loopback, CSRF, Content-Type: application/json, and confirm_reset_kill: true!
    assert_eq!(
        http_json(
            port2,
            "POST",
            "/api/live/reset-kill",
            "desk.lan:8472",
            true,
            r#"{"confirm_reset_kill":true}"#
        )
        .0,
        403
    );
    assert_eq!(
        http_json(
            port2,
            "POST",
            "/api/live/reset-kill",
            &host2,
            false,
            r#"{"confirm_reset_kill":true}"#
        )
        .0,
        403
    );
    assert_eq!(
        http_req(
            port2,
            "POST",
            "/api/live/reset-kill",
            &host2,
            &[("X-OpenBell-CSRF", "1")],
            Some("text/plain"),
            r#"{"confirm_reset_kill":true}"#,
        )
        .0,
        415
    );
    assert_eq!(
        http_json(
            port2,
            "POST",
            "/api/live/reset-kill",
            &host2,
            true,
            r#"{"confirm_reset_kill":false}"#
        )
        .0,
        400
    );
    assert_eq!(
        http_json(port2, "POST", "/api/live/reset-kill", &host2, true, "{}").0,
        400
    );
    let (st_rk, b_rk) = http_json(
        port2,
        "POST",
        "/api/live/reset-kill",
        &host2,
        true,
        r#"{"confirm_reset_kill":true}"#,
    );
    assert_eq!(st_rk, 200, "reset-kill failed: {b_rk}");
    let rk: serde_json::Value = serde_json::from_str(&b_rk).unwrap();
    assert_eq!(rk["killed"], false);
    assert_eq!(rk["live_armed"], false, "reset-kill MUST keep live_armed=false");
}

#[test]
fn stale_or_404_venue_sync_blocks_live_auto_fail_closed_while_shadow_works() {
    let dir = std::env::temp_dir().join(format!("ob-v030-stale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    std::fs::write(
        strat_dir.join("stale_maker.rn"),
        r#"
pub fn name() { "stale-maker" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { secs_to_close >= 60 }
pub fn entry_quote(yes_ask, no_ask, secs_to_close) { Some(0.55) }
"#,
    )
    .unwrap();

    // Pre-seed stale runtime (> 30s old) BEFORE boot.
    let utc_day = (now_unix() as u64) / 86_400;
    std::fs::write(
        data_dir.join("risk_rails_runtime.json"),
        serde_json::json!({
            "utc_day_num": utc_day,
            "daily_realized_loss_dollars": 0.0,
            "open_exposure_dollars": 0.0,
            "orders_placed_today": 0,
            "killed": false,
            "kill_reason": null,
            "last_venue_sync_epoch": now_unix() - 120
        })
        .to_string(),
    )
    .unwrap();

    let priv_pem = generate_ephemeral_rsa_pem(&dir);
    let (mock_port, mock_state) = spawn_mock_kalshi();
    mock_state.lock().unwrap().fail_settlements_404 = true;
    let mock_base = format!("http://127.0.0.1:{mock_port}/trade-api/v2");

    let port = free_port();
    let _srv = spawn_server(&data_dir, &strat_dir, port, Some(10), Some(dir.clone()));
    let host = format!("127.0.0.1:{port}");

    let cfg_body = serde_json::json!({
        "api_key_id": "v30-stale-key",
        "private_key_pem": priv_pem,
        "environment": "demo",
        "api_base_url": mock_base
    })
    .to_string();
    assert_eq!(http_json(port, "POST", "/api/live/config", &host, true, &cfg_body).0, 200);
    assert_eq!(
        http_json(port, "POST", "/api/live/arm", &host, true, r#"{"confirm_arm":true}"#).0,
        200
    );

    // Manual reconcile against 404 settlements MUST fail 502 and keep sync_stale = true!
    let (st_rec, _) = http_json(port, "POST", "/api/live/reconcile", &host, true, "{}");
    assert_eq!(st_rec, 502);
    let (_, b_st) = http_json(port, "GET", "/api/live/status", &host, false, "");
    let st_v: serde_json::Value = serde_json::from_str(&b_st).unwrap();
    assert_eq!(st_v["venue_sync_stale"], true);

    // Start live_auto and append an open snapshot -> MUST refuse new live orders fail-closed!
    let runner_cfg = serde_json::json!({
        "slots": [{
            "slot_id": 1,
            "strategy": "stale-maker",
            "series": ["KXBTC15M"],
            "max_contracts": 5,
            "execution_mode": "maker",
            "auto_pause_drawdown_dollars": 50.0,
            "enabled": true
        }],
        "cancel_before_close_secs": 30,
        "bankroll_dollars": 500.0,
        "tick_interval_ms": 50
    })
    .to_string();
    assert_eq!(http_json(port, "POST", "/api/runner/config", &host, true, &runner_cfg).0, 200);
    assert_eq!(
        http_json(
            port,
            "POST",
            "/api/runner/start",
            &host,
            true,
            r#"{"mode":"live_auto","confirm_auto_live":true}"#
        )
        .0,
        200
    );

    let snap_row = serde_json::json!({
        "fetched_at": now_unix(),
        "ticker": "KXBTC15M-STALE1",
        "event_ticker": "KXBTC15M-EVST",
        "series": "KXBTC15M",
        "close_time": "2027-01-20T00:15:00Z",
        "status": "open",
        "result": "",
        "yes_bid": 0.50,
        "yes_ask": 0.60,
        "no_bid": 0.40,
        "no_ask": 0.50,
        "last_price": 0.55
    });
    std::fs::write(data_dir.join("snapshots.jsonl"), format!("{snap_row}\n")).unwrap();

    std::thread::sleep(Duration::from_millis(350));
    assert!(
        mock_state.lock().unwrap().placed_orders.is_empty(),
        "live_auto MUST refuse new orders when venue sync is stale / 404!"
    );

    // Switching to paper Shadow mode continues working normally even while venue sync is failing!
    assert_eq!(
        http_json(
            port,
            "POST",
            "/api/runner/stop",
            &host,
            true,
            r#"{"cancel_resting":false}"#
        )
        .0,
        200
    );
    assert_eq!(
        http_json(
            port,
            "POST",
            "/api/runner/start",
            &host,
            true,
            r#"{"mode":"shadow"}"#
        )
        .0,
        200
    );
    wait_until(Duration::from_secs(5), || {
        let (_, b_led) = http_json(port, "GET", "/api/runner/ledger?limit=20", &host, false, "");
        let v: serde_json::Value = serde_json::from_str(&b_led).unwrap();
        if v["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"] == "QuotePosted" && e["mode"] == "shadow")
        {
            Some(())
        } else {
            None
        }
    });
}
