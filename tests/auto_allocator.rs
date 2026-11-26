//! End-to-end integration tests for OpenBell Autonomous Kalshi Edge & Auto-Allocator:
//! - 16 builtin strategies (existing 12 + 4 maker/momentum/fade shapes)
//! - `GET /api/allocator` (default Quarter-Kelly + Maker Post-Only, causal IS->OOS replay)
//! - `POST /api/allocator/plan` (Half-Kelly, Equal Winners, Taker vs Maker, Heat Caps, Quarantine, $500/day verdict)
//! - `POST /api/live/order` with `"post_only": true` (`dry_run: true`) verifying JSON `"post_only":true`
//! - Same-site / Origin / Content-Type / missing dataset guards and `live_armed: false` boot invariant

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

fn http_req_full(
    port: u16,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect server");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
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
        .env("OPENBELL_ORDERBOOK_OFFLINE", "1")
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
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("openbell-rs did not start on port {port}");
}

#[test]
fn allocator_endpoints_post_only_live_order_and_quarantine_work_end_to_end() {
    let dir = std::env::temp_dir().join(format!("ob-auto-alloc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();

    // Multi-regime 100-window settled tape where maker/momentum YES winners have positive EV
    // and contrarian NO fade strategies lose OOS.
    let mut buf = String::new();
    let base_epoch: i64 = 1_790_812_800;
    for w in 0..100usize {
        let ticker = format!("KXBTC15M-AA{w:03}");
        let close_epoch = base_epoch + (w as i64) * 900;
        let close_time = format!(
            "2026-10-{:02}T{:02}:{:02}:00Z",
            1 + (w / 96),
            (w / 4) % 24,
            (w % 4) * 15
        );
        let result = if w % 7 != 0 { "yes" } else { "no" };

        buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{ticker}\",\"event_ticker\":\"{ticker}\",\"series\":\"KXBTC15M\",\"close_time\":\"{close_time}\",\"status\":\"open\",\"result\":\"\",\"yes_bid\":0.78,\"yes_ask\":0.80,\"no_bid\":0.20,\"no_ask\":0.22,\"last_price\":0.80,\"yes_bid_size\":300.0,\"no_bid_size\":250.0}}\n",
            close_epoch - 480
        ));
        buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{ticker}\",\"event_ticker\":\"{ticker}\",\"series\":\"KXBTC15M\",\"close_time\":\"{close_time}\",\"status\":\"open\",\"result\":\"\",\"yes_bid\":0.55,\"yes_ask\":0.58,\"no_bid\":0.42,\"no_ask\":0.45,\"last_price\":0.58,\"yes_bid_size\":400.0,\"no_bid_size\":350.0}}\n",
            close_epoch - 300
        ));
        buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{ticker}\",\"event_ticker\":\"{ticker}\",\"series\":\"KXBTC15M\",\"close_time\":\"{close_time}\",\"status\":\"open\",\"result\":\"\",\"yes_bid\":0.66,\"yes_ask\":0.68,\"no_bid\":0.32,\"no_ask\":0.34,\"last_price\":0.68,\"yes_bid_size\":420.0,\"no_bid_size\":380.0}}\n",
            close_epoch - 240
        ));
        buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{ticker}\",\"event_ticker\":\"{ticker}\",\"series\":\"KXBTC15M\",\"close_time\":\"{close_time}\",\"status\":\"finalized\",\"result\":\"{result}\",\"yes_bid\":null,\"yes_ask\":null,\"no_bid\":null,\"no_ask\":null,\"last_price\":null}}\n",
            close_epoch + 60
        ));
    }
    std::fs::write(data.join("snapshots.jsonl"), &buf).unwrap();

    // Secondary 0-winner losing tape
    let mut lose_buf = String::new();
    for w in 0..50usize {
        let ticker = format!("KXETH15M-LOSE{w:03}");
        let close_epoch = base_epoch + (w as i64) * 900;
        let close_time = format!(
            "2026-10-{:02}T{:02}:{:02}:00Z",
            1 + (w / 96),
            (w / 4) % 24,
            (w % 4) * 15
        );
        let result = if w % 5 == 0 { "yes" } else { "no" };
        lose_buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{ticker}\",\"event_ticker\":\"{ticker}\",\"series\":\"KXETH15M\",\"close_time\":\"{close_time}\",\"status\":\"open\",\"result\":\"\",\"yes_bid\":0.63,\"yes_ask\":0.65,\"no_bid\":0.35,\"no_ask\":0.37,\"last_price\":0.65}}\n",
            close_epoch - 650
        ));
        lose_buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{ticker}\",\"event_ticker\":\"{ticker}\",\"series\":\"KXETH15M\",\"close_time\":\"{close_time}\",\"status\":\"finalized\",\"result\":\"{result}\",\"yes_bid\":null,\"yes_ask\":null,\"no_bid\":null,\"no_ask\":null,\"last_price\":null}}\n",
            close_epoch + 60
        ));
    }
    std::fs::write(data.join("losing-snapshots.jsonl"), &lose_buf).unwrap();

    let port = free_port();
    let _srv = spawn_server(&dir, port);

    // 1. GET /api/allocator
    let (st_get, body_get) = http_req_full(port, "GET", "/api/allocator", &[], "");
    assert_eq!(st_get, 200, "GET /api/allocator failed: {body_get}");
    let rep: serde_json::Value = serde_json::from_str(&body_get).unwrap();
    assert_eq!(rep["execution_mode"], "maker");
    assert_eq!(rep["sizing_mode"], "quarter_kelly");
    assert_eq!(rep["live_armed"], false);
    assert_eq!(rep["windows_settled_total"], 100);
    assert_eq!(rep["windows_settled_oos"], 40);
    assert_eq!(rep["comparison"]["selection_basis"], "causal_is_to_oos");
    assert_eq!(rep["daily_target"]["target_daily_profit_dollars"], 500.0);
    assert_eq!(rep["daily_target"]["verdict"], "NO_GO_PAPER");

    let winners = rep["active_winners"].as_array().unwrap();
    let quarantined = rep["quarantined"].as_array().unwrap();
    assert_eq!(winners.len() + quarantined.len(), 16);
    assert!(!winners.is_empty());
    assert!(!quarantined.is_empty());
    assert!(rep["total_allocated_pct"].as_f64().unwrap() <= 40.0 + 1e-9);
    assert!(
        (rep["total_allocated_pct"].as_f64().unwrap()
            + rep["cash_reserve_pct"].as_f64().unwrap()
            - 100.0)
            .abs()
            < 1e-6
    );
    for w in winners {
        assert_eq!(w["status"], "ACTIVE_WINNER");
        assert!(w["allocated_pct"].as_f64().unwrap() <= 15.0 + 1e-9);
        let tf = w["taker_fee_cents"].as_f64().unwrap();
        let mf = w["maker_fee_cents"].as_f64().unwrap();
        assert!((tf - 4.0 * mf).abs() < 1e-6);
    }
    for intent in rep["planned_intents"].as_array().unwrap() {
        assert_eq!(intent["post_only"], true);
        assert_eq!(intent["rail_ready"], false);
    }
    assert!(rep["comparison"]["fee_savings_dollars"].as_f64().unwrap() > 0.0);
    assert!(!rep["comparison"]["equity_curve"].as_array().unwrap().is_empty());

    // 2. POST /api/allocator/plan
    let plan_req = serde_json::json!({
        "sizing_mode": "half_kelly",
        "execution_mode": "taker",
        "maker_fill_rate": 0.9,
        "min_oos_trades": 5,
        "max_slot_risk_pct": 12.0,
        "max_total_heat_pct": 25.0,
        "slippage_cents": 1.0,
        "starting_bankroll": 500.0
    })
    .to_string();
    let (st_post1, b_post1) = http_req_full(port, "POST", "/api/allocator/plan", &[], &plan_req);
    let (st_post2, b_post2) = http_req_full(port, "POST", "/api/allocator/plan", &[], &plan_req);
    assert_eq!(st_post1, 200, "POST /api/allocator/plan failed: {b_post1}");
    assert_eq!(st_post2, 200, "POST /api/allocator/plan #2 failed: {b_post2}");
    assert_eq!(b_post1, b_post2, "allocator plan must be 100% deterministic");
    let plan: serde_json::Value = serde_json::from_str(&b_post1).unwrap();
    assert_eq!(plan["execution_mode"], "taker");
    assert_eq!(plan["sizing_mode"], "half_kelly");
    assert_eq!(plan["starting_bankroll"], 500.0);
    assert!(plan["total_allocated_pct"].as_f64().unwrap() <= 25.0 + 1e-9);
    for intent in plan["planned_intents"].as_array().unwrap() {
        assert_eq!(intent["post_only"], false);
    }

    // 3. 0-winner dataset -> 100% cash protection
    let (st_zero, b_zero) = http_req_full(port, "GET", "/api/allocator?dataset=losing", &[], "");
    assert_eq!(st_zero, 200, "GET /api/allocator?dataset=losing failed: {b_zero}");
    let zero_rep: serde_json::Value = serde_json::from_str(&b_zero).unwrap();
    assert!(zero_rep["active_winners"].as_array().unwrap().is_empty());
    assert_eq!(zero_rep["total_allocated_pct"], 0.0);
    assert_eq!(zero_rep["cash_reserve_pct"], 100.0);
    assert_eq!(zero_rep["comparison"]["auto_allocator_end_equity"], 100.0);
    assert!(zero_rep["comparison"]["equity_delta_dollars"].as_f64().unwrap() >= 0.0);

    // 4. Guards
    let (st_404, _) = http_req_full(port, "GET", "/api/allocator?dataset=missing", &[], "");
    assert_eq!(st_404, 404);
    let (st_400, _) = http_req_full(port, "POST", "/api/allocator/plan", &[], "{not-json");
    assert_eq!(st_400, 400);
    let (st_403, _) = http_req_full(
        port,
        "POST",
        "/api/allocator/plan",
        &[("Origin", "http://evil.example")],
        "{}",
    );
    assert_eq!(st_403, 403);

    // 5. POST /api/live/order with dry_run:true + post_only:true
    let csrf = [("X-OpenBell-CSRF", "1")];
    let dry_maker = r#"{"ticker":"KXBTC15M-TEST-1","action":"buy","side":"yes","count":3,"price_cents":58,"dry_run":true,"post_only":true}"#;
    let (st_dry_m, b_dry_m) = http_req_full(port, "POST", "/api/live/order", &csrf, dry_maker);
    assert_eq!(st_dry_m, 200, "dry_run maker order failed: {b_dry_m}");
    let jm: serde_json::Value = serde_json::from_str(&b_dry_m).unwrap();
    assert_eq!(jm["ok"], true);
    assert_eq!(jm["order_request"]["post_only"], true);

    let dry_taker = r#"{"ticker":"KXBTC15M-TEST-1","action":"buy","side":"yes","count":3,"price_cents":58,"dry_run":true}"#;
    let (st_dry_t, b_dry_t) = http_req_full(port, "POST", "/api/live/order", &csrf, dry_taker);
    assert_eq!(st_dry_t, 200, "dry_run taker order failed: {b_dry_t}");
    let jt: serde_json::Value = serde_json::from_str(&b_dry_t).unwrap();
    assert!(jt["order_request"].get("post_only").is_none());

    let bad_po = r#"{"ticker":"KXBTC15M-TEST-1","action":"buy","side":"yes","count":3,"price_cents":58,"dry_run":true,"post_only":"yes"}"#;
    let (st_bad_po, _) = http_req_full(port, "POST", "/api/live/order", &csrf, bad_po);
    assert_eq!(st_bad_po, 400);
}
