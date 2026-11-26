//! End-to-end integration tests for OpenBell v0.31.0 Adverse-Selection Markout
//! Telemetry, Auto-Quarantine Decay Feedback Loop, Persistent Slot Circuit-Breaker,
//! AutoAllocator Decay Demotion, and `POST /api/runner/sync-allocator` Safety Rails.
//!
//! Strictly uses synthetic local tapes, ledgers, and loopback HTTP — ZERO external
//! network or Kalshi calls.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct ServerGuard {
    child: Child,
    cleanup_dir: Option<PathBuf>,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(ref d) = self.cleanup_dir {
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

fn iso_z(epoch: i64) -> String {
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

fn spawn_server(data_dir: &Path, port: u16, cleanup_dir: Option<PathBuf>) -> ServerGuard {
    std::fs::create_dir_all(data_dir).unwrap();
    let strat_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("strategies");
    let child = Command::new(env!("CARGO_BIN_EXE_openbell-rs"))
        .arg("--no-open")
        .env("PORT", port.to_string())
        .env("OPENBELL_DATA", data_dir)
        .env("OPENBELL_STRATEGIES", &strat_dir)
        .env("OPENBELL_COLLECT_SECS", "3600")
        .env("OPENBELL_UPDATE_MODE", "off")
        .env("OPENBELL_NO_OPEN", "1")
        .env("OPENBELL_ORDERBOOK_OFFLINE", "1")
        .env("OPENBELL_ALLOWED_HOSTS", "desk.lan")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn openbell-rs");
    let guard = ServerGuard { child, cleanup_dir };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!("openbell-rs did not start on port {port}");
}

fn wait_until<F>(label: &str, timeout: Duration, mut pred: F) -> serde_json::Value
where
    F: FnMut() -> Result<serde_json::Value, String>,
{
    let start = Instant::now();
    let mut last_err = String::new();
    while start.elapsed() < timeout {
        match pred() {
            Ok(v) => return v,
            Err(e) => last_err = e,
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for expected runner state ({label}): {last_err}");
}

#[test]
fn v031_markout_quarantine_persistence_and_sync_allocator() {
    let dir = std::env::temp_dir().join(format!("ob-v031-mq-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();

    // 1. Seed a 120-window positive OOS dataset with uncorrelated YES (KXBTC15M) and NO (KXETH15M) winners
    let mut tape_buf = String::new();
    let base_epoch: i64 = 1_790_812_800;
    for w in 0..60usize {
        let close_epoch = base_epoch + (w as i64) * 900;
        let close_time = iso_z(close_epoch);

        // KXBTC15M: YES favourite wins 6/7 (no_ask=0.30 so no-favourite does not fire here)
        let btc_ticker = format!("KXBTC15M-MQ{w:03}");
        let btc_res = if w % 7 != 0 { "yes" } else { "no" };
        tape_buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{btc_ticker}\",\"event_ticker\":\"{btc_ticker}\",\"series\":\"KXBTC15M\",\"close_time\":\"{close_time}\",\"status\":\"open\",\"result\":\"\",\"yes_bid\":0.70,\"yes_ask\":0.72,\"no_bid\":0.28,\"no_ask\":0.30,\"last_price\":0.72,\"yes_bid_size\":400.0,\"no_bid_size\":350.0}}\n",
            close_epoch - 200
        ));
        tape_buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{btc_ticker}\",\"event_ticker\":\"{btc_ticker}\",\"series\":\"KXBTC15M\",\"close_time\":\"{close_time}\",\"status\":\"finalized\",\"result\":\"{btc_res}\",\"yes_bid\":null,\"yes_ask\":null,\"no_bid\":null,\"no_ask\":null,\"last_price\":null}}\n",
            close_epoch + 60
        ));

        // KXETH15M: NO favourite wins 6/7 on offset close epochs (uncorrelated with KXBTC15M)
        let eth_close_epoch = close_epoch + 450;
        let eth_close_time = iso_z(eth_close_epoch);
        let eth_ticker = format!("KXETH15M-MQ{w:03}");
        let eth_res = if (w + 3) % 7 != 0 { "no" } else { "yes" };
        tape_buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{eth_ticker}\",\"event_ticker\":\"{eth_ticker}\",\"series\":\"KXETH15M\",\"close_time\":\"{eth_close_time}\",\"status\":\"open\",\"result\":\"\",\"yes_bid\":0.42,\"yes_ask\":0.45,\"no_bid\":0.55,\"no_ask\":0.58,\"last_price\":0.45,\"yes_bid_size\":350.0,\"no_bid_size\":400.0}}\n",
            eth_close_epoch - 450
        ));
        tape_buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{eth_ticker}\",\"event_ticker\":\"{eth_ticker}\",\"series\":\"KXETH15M\",\"close_time\":\"{eth_close_time}\",\"status\":\"finalized\",\"result\":\"{eth_res}\",\"yes_bid\":null,\"yes_ask\":null,\"no_bid\":null,\"no_ask\":null,\"last_price\":null}}\n",
            eth_close_epoch + 60
        ));
    }
    std::fs::write(data.join("snapshots.jsonl"), &tape_buf).unwrap();

    // Server #1: Verify baseline AutoAllocator winners before live decay
    let port1 = free_port();
    let srv1 = spawn_server(&data, port1, None);
    let host1 = format!("127.0.0.1:{port1}");
    let (st_alloc1, b_alloc1) = http_req(port1, "GET", "/api/allocator", &host1, &[], None, "");
    assert_eq!(st_alloc1, 200, "GET /api/allocator failed: {b_alloc1}");
    let alloc1: serde_json::Value = serde_json::from_str(&b_alloc1).unwrap();
    let winners1 = alloc1["active_winners"].as_array().unwrap();
    assert!(winners1.len() >= 2, "expected >=2 baseline winners, got {winners1:?}");
    let decaying_strat = winners1[0]["strategy"].as_str().unwrap().to_string();
    let decaying_side = winners1[0]["side"].as_str().unwrap().to_string();
    let healthy_strat = winners1
        .iter()
        .find(|w| w["strategy"].as_str() != Some(decaying_strat.as_str()))
        .unwrap()["strategy"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(alloc1["live_armed"], false);
    drop(srv1);

    // 2. Seed adverse markout fills in shadow_ledger.jsonl + live_executions.jsonl + orderbooks.jsonl
    let mut ledger_buf = String::new();
    let mut ob_buf = String::new();
    let mut live_exec_buf = String::new();
    let seed_base_ts = base_epoch + 100_000;
    let loss_result = if decaying_side == "yes" { "no" } else { "yes" };

    for i in 0..6i64 {
        let ts = seed_base_ts + i * 300;
        let ticker = format!("KXBTC15M-DECAY{i}");
        let oid = format!("ob-decay-{i}");
        let fill_c: i64 = 60;
        ledger_buf.push_str(&format!(
            "{{\"seq\":{},\"ts\":{},\"event\":\"QuotePosted\",\"mode\":\"shadow\",\"slot_id\":1,\"strategy\":\"{decaying_strat}\",\"ticker\":\"{ticker}\",\"series\":\"KXBTC15M\",\"order_id\":\"{oid}\",\"side\":\"{decaying_side}\",\"execution_mode\":\"maker\",\"limit_cents\":{fill_c},\"contracts\":5}}\n",
            i * 3 + 1,
            ts - 5
        ));
        ledger_buf.push_str(&format!(
            "{{\"seq\":{},\"ts\":{},\"event\":\"OrderFilled\",\"mode\":\"shadow\",\"slot_id\":1,\"strategy\":\"{decaying_strat}\",\"ticker\":\"{ticker}\",\"series\":\"KXBTC15M\",\"order_id\":\"{oid}\",\"side\":\"{decaying_side}\",\"execution_mode\":\"maker\",\"limit_cents\":{fill_c},\"fill_cents\":{fill_c},\"contracts\":5,\"slippage_cents\":0.0,\"fee_dollars\":0.02}}\n",
            i * 3 + 2,
            ts
        ));
        ledger_buf.push_str(&format!(
            "{{\"seq\":{},\"ts\":{},\"event\":\"WindowSettled\",\"mode\":\"shadow\",\"slot_id\":1,\"strategy\":\"{decaying_strat}\",\"ticker\":\"{ticker}\",\"series\":\"KXBTC15M\",\"order_id\":\"{oid}\",\"side\":\"{decaying_side}\",\"execution_mode\":\"maker\",\"limit_cents\":{fill_c},\"fill_cents\":{fill_c},\"contracts\":5,\"fee_dollars\":0.02,\"realized_pnl_dollars\":-3.02,\"result\":\"{loss_result}\"}}\n",
            i * 3 + 3,
            ts + 120
        ));

        // Side-normalized adverse markout: +10s -> -2c, +30s -> -6c, +60s -> -10c
        let (m10_yes, m30_yes, m60_yes) = if decaying_side == "yes" {
            (58.0, 54.0, 50.0)
        } else {
            (42.0, 46.0, 50.0)
        };
        ob_buf.push_str(&format!(
            "{{\"ts_ms\":{},\"ticker\":\"{ticker}\",\"seq\":1,\"source\":\"ws\",\"yes_bids\":[],\"no_bids\":[],\"mid_cents\":{m10_yes}}}\n",
            (ts + 10) * 1000
        ));
        ob_buf.push_str(&format!(
            "{{\"ts_ms\":{},\"ticker\":\"{ticker}\",\"seq\":2,\"source\":\"ws\",\"yes_bids\":[],\"no_bids\":[],\"mid_cents\":{m30_yes}}}\n",
            (ts + 30) * 1000
        ));
        ob_buf.push_str(&format!(
            "{{\"ts_ms\":{},\"ticker\":\"{ticker}\",\"seq\":3,\"source\":\"ws\",\"yes_bids\":[],\"no_bids\":[],\"mid_cents\":{m60_yes}}}\n",
            (ts + 60) * 1000
        ));
    }

    // Also seed a live_executions.jsonl fill for healthy_strat with positive markouts!
    let live_ts = seed_base_ts + 5_000;
    let live_ticker = "KXBTC15M-LIVEOK1";
    let healthy_side = winners1
        .iter()
        .find(|w| w["strategy"] == healthy_strat)
        .unwrap()["side"]
        .as_str()
        .unwrap();
    live_exec_buf.push_str(&format!(
        "{{\"kind\":\"order_submitted\",\"venue_id\":\"ord-live-ok-1\",\"ts\":{},\"order_id\":\"ord-live-ok-1\",\"client_order_id\":\"cid-ok-1\",\"ticker\":\"{live_ticker}\",\"side\":\"{healthy_side}\",\"action\":\"buy\",\"count\":2,\"price_cents\":55,\"is_taker\":false,\"fee_dollars\":0.0}}\n",
        live_ts - 5
    ));
    live_exec_buf.push_str(&format!(
        "{{\"kind\":\"fill\",\"venue_id\":\"tr-live-ok-1\",\"ts\":{live_ts},\"order_id\":\"ord-live-ok-1\",\"client_order_id\":\"cid-ok-1\",\"trade_id\":\"tr-live-ok-1\",\"ticker\":\"{live_ticker}\",\"side\":\"{healthy_side}\",\"action\":\"buy\",\"count\":2,\"price_cents\":55,\"is_taker\":false,\"fee_dollars\":0.01,\"strategy\":\"{healthy_strat}\"}}\n"
    ));
    let (live_m10, live_m30, live_m60) = if healthy_side == "yes" {
        (57.0, 60.0, 63.0)
    } else {
        (43.0, 40.0, 37.0)
    };
    ob_buf.push_str(&format!(
        "{{\"ts_ms\":{},\"ticker\":\"{live_ticker}\",\"seq\":1,\"source\":\"ws\",\"yes_bids\":[],\"no_bids\":[],\"mid_cents\":{live_m10}}}\n",
        (live_ts + 10) * 1000
    ));
    ob_buf.push_str(&format!(
        "{{\"ts_ms\":{},\"ticker\":\"{live_ticker}\",\"seq\":2,\"source\":\"ws\",\"yes_bids\":[],\"no_bids\":[],\"mid_cents\":{live_m30}}}\n",
        (live_ts + 30) * 1000
    ));
    ob_buf.push_str(&format!(
        "{{\"ts_ms\":{},\"ticker\":\"{live_ticker}\",\"seq\":3,\"source\":\"ws\",\"yes_bids\":[],\"no_bids\":[],\"mid_cents\":{live_m60}}}\n",
        (live_ts + 60) * 1000
    ));

    std::fs::write(data.join("shadow_ledger.jsonl"), &ledger_buf).unwrap();
    std::fs::write(data.join("live_executions.jsonl"), &live_exec_buf).unwrap();
    std::fs::write(data.join("orderbooks.jsonl"), &ob_buf).unwrap();

    // Server #2: Verify execution-health, auto-quarantine, AutoAllocator demotion, sync-allocator, and slot pause
    let port2 = free_port();
    let srv2 = spawn_server(&data, port2, None);
    let host2 = format!("127.0.0.1:{port2}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // Non-loopback host must be refused on execution-health
    let (st_lan, _) = http_req(
        port2,
        "GET",
        "/api/analytics/execution-health",
        &format!("desk.lan:{port2}"),
        &[],
        None,
        "",
    );
    assert_eq!(st_lan, 403, "execution-health must refuse non-loopback host");

    let (st_eh, b_eh) = http_req(
        port2,
        "GET",
        "/api/analytics/execution-health",
        &host2,
        &[],
        None,
        "",
    );
    assert_eq!(st_eh, 200, "GET /api/analytics/execution-health failed: {b_eh}");
    let eh: serde_json::Value = serde_json::from_str(&b_eh).unwrap();
    assert_eq!(eh["ok"], true);
    assert_eq!(eh["overall"]["total_fills"], 7);

    let decay_row = eh["strategies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["strategy"] == decaying_strat)
        .expect("decaying strategy present in execution-health");
    assert_eq!(decay_row["status"], "quarantined");
    assert_eq!(decay_row["throttle_mult"], 0.0);
    assert!((decay_row["markout_10s_cents"].as_f64().unwrap() - (-2.0)).abs() < 1e-6);
    assert!((decay_row["markout_30s_cents"].as_f64().unwrap() - (-6.0)).abs() < 1e-6);
    assert!((decay_row["markout_60s_cents"].as_f64().unwrap() - (-10.0)).abs() < 1e-6);
    assert!((decay_row["settlement_markout_cents"].as_f64().unwrap() - (-60.0)).abs() < 1e-6);
    assert_eq!(decay_row["predicted_win_rate_pct"].as_f64().unwrap(), 60.0);
    assert_eq!(decay_row["realized_win_rate_pct"].as_f64().unwrap(), 0.0);
    assert_eq!(decay_row["actual_fill_rate_pct"].as_f64().unwrap(), 100.0);
    assert_eq!(decay_row["slippage_residual_cents"].as_f64().unwrap(), 0.0);

    let healthy_row = eh["strategies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["strategy"] == healthy_strat)
        .expect("healthy live strategy present in execution-health");
    assert_eq!(healthy_row["status"], "active");
    assert!((healthy_row["markout_30s_cents"].as_f64().unwrap() - 5.0).abs() < 1e-6);

    let q_path = data.join("quarantine_state.json");
    assert!(q_path.exists(), "quarantine_state.json must be written");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&q_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "quarantine_state.json must be 0600");
    }

    // AutoAllocator must demote decaying_strat even though its static OOS backtest was #1!
    let (st_alloc2, b_alloc2) = http_req(port2, "GET", "/api/allocator", &host2, &[], None, "");
    assert_eq!(st_alloc2, 200);
    let alloc2: serde_json::Value = serde_json::from_str(&b_alloc2).unwrap();
    assert!(
        !alloc2["active_winners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["strategy"] == decaying_strat),
        "quarantined strategy must be demoted from active_winners"
    );
    assert!(
        alloc2["quarantined"]
            .as_array()
            .unwrap()
            .iter()
            .any(|q| q["strategy"] == decaying_strat),
        "quarantined strategy must appear in allocator quarantined list"
    );
    assert_eq!(alloc2["execution_health"]["quarantined_strategies"], 1);

    // POST /api/runner/sync-allocator: requires CSRF, excludes quarantined, keeps live_armed=false & mode != live_auto
    let (st_sync_nocsrf, _) = http_req(
        port2,
        "POST",
        "/api/runner/sync-allocator",
        &host2,
        &[],
        Some("application/json"),
        "{}",
    );
    assert_eq!(st_sync_nocsrf, 403);

    let (st_sync, b_sync) = http_req(
        port2,
        "POST",
        "/api/runner/sync-allocator",
        &host2,
        &csrf,
        Some("application/json"),
        "{}",
    );
    assert_eq!(st_sync, 200, "POST /api/runner/sync-allocator failed: {b_sync}");
    let sync_v: serde_json::Value = serde_json::from_str(&b_sync).unwrap();
    assert_eq!(sync_v["ok"], true);
    assert_eq!(sync_v["status"]["live_armed"], false);
    assert_ne!(sync_v["status"]["mode"], "live_auto");
    let synced_slots = sync_v["slots"].as_array().unwrap();
    assert!(!synced_slots.is_empty());
    assert!(
        !synced_slots.iter().any(|s| s["strategy"] == decaying_strat),
        "synced runner slots must exclude quarantined strategy"
    );
    for s in synced_slots {
        assert!(
            s["max_contracts"].as_u64().unwrap() <= 25,
            "synced slot max_contracts exceeded risk rail: {s}"
        );
    }

    // Now configure slot 1 (healthy non-quarantined strategy) with tight auto_pause_drawdown_dollars = 0.50
    // AND slot 2 (quarantined decaying_strat) to verify ShadowRunner skips quarantined strategies while tripping slot 1 pause!
    let (pause_strat, pause_loss_res) = if decaying_strat == "maker-spread-capture" {
        ("spot-lead-momentum-yes", "no")
    } else {
        ("maker-spread-capture", "no")
    };
    let pause_cfg = serde_json::json!({
        "slots": [
            {
                "slot_id": 1,
                "strategy": pause_strat,
                "series": ["KXBTC15M"],
                "max_contracts": 5,
                "execution_mode": "taker",
                "auto_pause_drawdown_dollars": 0.50,
                "enabled": true
            },
            {
                "slot_id": 2,
                "strategy": decaying_strat,
                "series": ["KXBTC15M"],
                "max_contracts": 5,
                "execution_mode": "taker",
                "auto_pause_drawdown_dollars": 50.0,
                "enabled": true
            }
        ],
        "cancel_before_close_secs": 30,
        "bankroll_dollars": 500.0,
        "tick_interval_ms": 50
    })
    .to_string();
    assert_eq!(
        http_req(
            port2,
            "POST",
            "/api/runner/config",
            &host2,
            &csrf,
            Some("application/json"),
            &pause_cfg
        )
        .0,
        200
    );
    assert_eq!(
        http_req(
            port2,
            "POST",
            "/api/runner/start",
            &host2,
            &csrf,
            Some("application/json"),
            r#"{"mode":"shadow"}"#
        )
        .0,
        200
    );

    let now_ts = now_unix();
    let close_ep = now_ts + 200;
    let close_iso = iso_z(close_ep);
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(data.join("snapshots.jsonl"))
            .unwrap();
        writeln!(
            f,
            "{{\"fetched_at\":{now_ts},\"ticker\":\"KXBTC15M-PAUSELIVE\",\"event_ticker\":\"KXBTC15M-PL\",\"series\":\"KXBTC15M\",\"close_time\":\"{close_iso}\",\"status\":\"open\",\"result\":\"\",\"yes_bid\":0.55,\"yes_ask\":0.60,\"no_bid\":0.40,\"no_ask\":0.45,\"last_price\":0.58,\"yes_bid_size\":50.0,\"no_bid_size\":50.0}}"
        )
        .unwrap();
    }
    let filled_snap = wait_until("slot1_fill", Duration::from_secs(6), || {
        let (_, b) = http_req(port2, "GET", "/api/runner/status", &host2, &[], None, "");
        let v: serde_json::Value = serde_json::from_str(&b).map_err(|e| e.to_string())?;
        if v["active_slots"][0]["fills"].as_u64().unwrap_or(0) >= 1 {
            Ok(v)
        } else {
            let (_, m) = http_req(port2, "GET", "/api/markets", &host2, &[], None, "");
            let has_pl = m.contains("KXBTC15M-PAUSELIVE");
            let (_, e) = http_req(port2, "GET", "/api/analytics/execution-health", &host2, &[], None, "");
            Err(format!("status={b} has_pl={has_pl} eh={e}"))
        }
    });
    assert_eq!(
        filled_snap["active_slots"][1]["quotes_posted"], 0,
        "quarantined slot 2 must not post quotes"
    );
    assert_eq!(
        filled_snap["active_slots"][1]["fills"], 0,
        "quarantined slot 2 must not fill"
    );
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(data.join("snapshots.jsonl"))
            .unwrap();
        writeln!(
            f,
            "{{\"fetched_at\":{},\"ticker\":\"KXBTC15M-PAUSELIVE\",\"event_ticker\":\"KXBTC15M-PL\",\"series\":\"KXBTC15M\",\"close_time\":\"{close_iso}\",\"status\":\"finalized\",\"result\":\"{pause_loss_res}\",\"yes_bid\":0.01,\"yes_ask\":0.01,\"no_bid\":0.99,\"no_ask\":0.99,\"last_price\":0.01}}",
            now_ts + 5
        )
        .unwrap();
    }
    let paused_snap = wait_until("slot1_pause", Duration::from_secs(6), || {
        let (_, b) = http_req(port2, "GET", "/api/runner/status", &host2, &[], None, "");
        let v: serde_json::Value = serde_json::from_str(&b).map_err(|e| e.to_string())?;
        if v["active_slots"][0]["paused"] == true {
            Ok(v)
        } else {
            Err(b)
        }
    });
    let pause_reason = paused_snap["active_slots"][0]["pause_reason"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(pause_reason.contains("auto-paused"));
    drop(srv2);

    // Server #3: Restart on same data_dir — verify slot `paused` & `pause_reason` AND `quarantine_state.json` survive!
    let port3 = free_port();
    let _srv3 = spawn_server(&data, port3, Some(dir.clone()));
    let host3 = format!("127.0.0.1:{port3}");

    let (st_res3, b_res3) = http_req(port3, "GET", "/api/runner/status", &host3, &[], None, "");
    assert_eq!(st_res3, 200);
    let res3: serde_json::Value = serde_json::from_str(&b_res3).unwrap();
    assert_eq!(res3["live_armed"], false);
    assert_ne!(res3["mode"], "live_auto");
    assert_eq!(
        res3["active_slots"][0]["paused"], true,
        "slot paused MUST survive restart"
    );
    assert_eq!(
        res3["active_slots"][0]["pause_reason"].as_str(),
        Some(pause_reason.as_str())
    );

    let (st_eh3, b_eh3) = http_req(
        port3,
        "GET",
        "/api/analytics/execution-health",
        &host3,
        &[],
        None,
        "",
    );
    assert_eq!(st_eh3, 200);
    let eh3: serde_json::Value = serde_json::from_str(&b_eh3).unwrap();
    assert_eq!(
        eh3["quarantine_state"]["strategies"][&decaying_strat]["status"],
        "quarantined"
    );

    // Quarantine override guards: 403 without CSRF/Origin, 415 without JSON CT, 400 on invalid strategy/action, 200 on reset
    let reset_body = serde_json::json!({
        "strategy": decaying_strat,
        "action": "reset",
        "reason": "operator cleared after inspection"
    })
    .to_string();
    assert_eq!(
        http_req(
            port3,
            "POST",
            "/api/analytics/quarantine/override",
            &host3,
            &[],
            Some("application/json"),
            &reset_body
        )
        .0,
        403
    );
    assert_eq!(
        http_req(
            port3,
            "POST",
            "/api/analytics/quarantine/override",
            &host3,
            &[("X-OpenBell-CSRF", "1"), ("Origin", "http://evil.example")],
            Some("application/json"),
            &reset_body
        )
        .0,
        403
    );
    assert_eq!(
        http_req(
            port3,
            "POST",
            "/api/analytics/quarantine/override",
            &host3,
            &csrf,
            Some("text/plain"),
            &reset_body
        )
        .0,
        415
    );
    assert_eq!(
        http_req(
            port3,
            "POST",
            "/api/analytics/quarantine/override",
            &host3,
            &csrf,
            Some("application/json"),
            r#"{"strategy":"no_such_strategy_xyz","action":"reset"}"#
        )
        .0,
        400
    );
    assert_eq!(
        http_req(
            port3,
            "POST",
            "/api/analytics/quarantine/override",
            &host3,
            &csrf,
            Some("application/json"),
            &serde_json::json!({"strategy": decaying_strat, "action": "bogus"}).to_string()
        )
        .0,
        400
    );

    let (st_ovr, b_ovr) = http_req(
        port3,
        "POST",
        "/api/analytics/quarantine/override",
        &host3,
        &csrf,
        Some("application/json"),
        &reset_body,
    );
    assert_eq!(st_ovr, 200, "valid reset override failed: {b_ovr}");
    let ovr_v: serde_json::Value = serde_json::from_str(&b_ovr).unwrap();
    assert_eq!(ovr_v["ok"], true);
    assert_eq!(ovr_v["entry"]["status"], "active");

    // Allocator promotes it back to active_winners, and sync-allocator still keeps live_armed=false & mode != live_auto
    let (st_alloc3, b_alloc3) = http_req(port3, "GET", "/api/allocator", &host3, &[], None, "");
    assert_eq!(st_alloc3, 200);
    let alloc3: serde_json::Value = serde_json::from_str(&b_alloc3).unwrap();
    assert!(
        alloc3["active_winners"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["strategy"] == decaying_strat),
        "reset strategy should be eligible in active_winners again"
    );

    let (st_sync2, b_sync2) = http_req(
        port3,
        "POST",
        "/api/runner/sync-allocator",
        &host3,
        &csrf,
        Some("application/json"),
        "{}",
    );
    assert_eq!(st_sync2, 200);
    let sync2_v: serde_json::Value = serde_json::from_str(&b_sync2).unwrap();
    assert_eq!(sync2_v["status"]["live_armed"], false);
    assert_ne!(sync2_v["status"]["mode"], "live_auto");

    // Manual quarantine override
    let q_body = serde_json::json!({
        "strategy": decaying_strat,
        "action": "quarantine",
        "reason": "manual safety lock"
    })
    .to_string();
    let (st_q_ovr, b_q_ovr) = http_req(
        port3,
        "POST",
        "/api/analytics/quarantine/override",
        &host3,
        &csrf,
        Some("application/json"),
        &q_body,
    );
    assert_eq!(st_q_ovr, 200);
    let q_ovr_v: serde_json::Value = serde_json::from_str(&b_q_ovr).unwrap();
    assert_eq!(q_ovr_v["entry"]["status"], "quarantined");
    assert_eq!(q_ovr_v["entry"]["throttle_mult"], 0.0);
}
