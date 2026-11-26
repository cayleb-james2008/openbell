//! End-to-end integration tests for OpenBell v0.27.0 (GAP #3):
//! - Stateful Feature-Rich Strategy Runtime v2 (`evaluate(ctx)` + `TickContext`)
//! - Backward-compatible v1 (`allow_entry`) + v2 dispatch across scoring & tournament
//! - 1-bar causal lookahead canary verification
//! - Atomic CWE-22/59-confined strategy CRUD (`POST /api/strategies/save`,
//!   `DELETE /api/strategies/{name}`) + traversal/symlink rejection + CSRF/loopback/CT guards
//! - Deterministic Chronological IS->OOS BH-FDR & Deflated Sharpe Edge Synthesizer
//!   (`POST /api/research/synthesize`)
//! - `live_armed: false` boot invariant preserved.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
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
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
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

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if Instant::now() > deadline {
            panic!("server on port {port} did not become ready in time");
        }
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            let _ = s.write_all(
                format!("GET /api/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
            let mut buf = String::new();
            if s.read_to_string(&mut buf).is_ok() && buf.contains("200 OK") {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    guard
}

fn iso_utc(epoch: i64) -> String {
    // Simple civil-from-days UTC formatter matching OpenBell's ISO-Z format.
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = mp + if mp < 10 { 3 } else { -9 };
    let yr = y + if mo <= 2 { 1 } else { 0 };
    format!("{yr:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

fn write_synthetic_tape(data_dir: &Path, num_windows: usize) {
    let mut lines = String::new();
    let base = 1_795_700_000i64;
    for w in 0..num_windows {
        let close_ep = base + ((w as i64) + 1) * 900;
        let close_str = iso_utc(close_ep);
        let series = if w % 2 == 0 { "KXBTC15M" } else { "KXETH15M" };
        let ticker = format!("{series}-V027-{w:03}");
        // Alternate price bands across windows so library strategies don't have 1.00 correlation
        // with every synthesized candidate, while keeping positive momentum/imbalance signal.
        let won_yes = (w % 5) != 0;
        let result = if won_yes { "yes" } else { "no" };
        let ticks = if w % 2 == 0 {
            [
                (close_ep - 500, 0.48, 0.50, 0.50, 0.52, 40.0, 40.0, "open", ""),
                (close_ep - 420, 0.53, 0.56, 0.44, 0.47, 95.0, 20.0, "open", ""),
                (close_ep - 300, 0.56, 0.58, 0.42, 0.44, 110.0, 18.0, "open", ""),
                (close_ep - 180, 0.52, 0.54, 0.46, 0.48, 80.0, 25.0, "settled", result),
            ]
        } else {
            [
                (close_ep - 500, 0.36, 0.38, 0.62, 0.64, 40.0, 40.0, "open", ""),
                (close_ep - 420, 0.41, 0.44, 0.56, 0.59, 95.0, 20.0, "open", ""),
                (close_ep - 300, 0.44, 0.46, 0.54, 0.56, 110.0, 18.0, "open", ""),
                (close_ep - 180, 0.40, 0.42, 0.58, 0.60, 80.0, 25.0, "settled", result),
            ]
        };
        for (ts, yb, ya, nb, na, ysz, nsz, status, res) in ticks {
            let line = serde_json::json!({
                "fetched_at": ts,
                "ticker": ticker,
                "event_ticker": format!("{series}-V027"),
                "series": series,
                "close_time": close_str,
                "status": status,
                "result": res,
                "yes_bid": yb,
                "yes_ask": ya,
                "no_bid": nb,
                "no_ask": na,
                "last_price": ya,
                "yes_bid_size": ysz,
                "no_bid_size": nsz
            });
            lines.push_str(&line.to_string());
            lines.push('\n');
        }
    }
    std::fs::write(data_dir.join("snapshots.jsonl"), lines).unwrap();
}

#[test]
fn end_to_end_v2_runtime_crud_security_and_synthesizer() {
    let base = std::env::temp_dir().join(format!("openbell-v027-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let data_dir = base.join("data");
    let strat_dir = base.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    write_synthetic_tape(&data_dir, 45);

    // Seed one classic v1 strategy (selective band 0.56..=0.70) AND one v2 strategy.
    let v1_src = r#"
pub fn name() { "v1-compat-yes" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) {
    yes_ask >= 0.56 && yes_ask <= 0.70 && secs_to_close <= 600 && secs_to_close >= 120
}
"#;
    let v2_map_src = r#"
pub fn name() { "v2-imbalance-map" }
pub fn side() { "yes" }
pub fn describe() { "V2 strategy returning direct map decision using TickContext." }
pub fn evaluate(ctx) {
    if ctx.secs_to_close <= 500 && ctx.secs_to_close >= 120
        && ctx.book_imbalance >= 0.30
        && ctx.delta_60s_cents >= 2.0
        && ctx.tick_index >= 1
        && ctx.high_yes_ask >= ctx.open_yes_ask
        && ctx.low_yes_ask <= ctx.yes_ask
        && ctx.window_vol_cents >= 0.0
        && ctx.pav_yes_prob > 0.0
        && ctx.series != ""
    {
        let lc = (ctx.yes_ask * 100.0).round() as i64;
        return #{ enter: true, side: "yes", limit_cents: lc, post_only: false };
    }
    #{ enter: false }
}
"#;
    std::fs::write(strat_dir.join("v1_compat_yes.rn"), v1_src).unwrap();
    std::fs::write(strat_dir.join("v2_imbalance_map.rn"), v2_map_src).unwrap();

    let port = free_port();
    let _guard = spawn_server(&data_dir, &strat_dir, port, Some(base.clone()));
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // 1. Verify live_armed: false boot invariant + new capabilities.
    let (st_home, body_home) = http_req(port, "GET", "/api/home", &host, &[], None, "");
    assert_eq!(st_home, 200);
    let home: serde_json::Value = serde_json::from_str(&body_home).unwrap();
    assert_eq!(home["live"]["live_armed"], false, "MUST boot disarmed");
    assert_eq!(home["library_errors"].as_array().unwrap().len(), 0);
    let caps = home["capabilities"].as_array().unwrap();
    assert!(caps.iter().any(|c| c == "strategy_v2"));
    assert!(caps.iter().any(|c| c == "synthesizer"));

    // 2. Verify v1 and v2 both score cleanly on /api/strategies & /api/analytics/tournament.
    let (st_strats, body_strats) = http_req(port, "GET", "/api/strategies", &host, &[], None, "");
    assert_eq!(st_strats, 200);
    let strats_json: serde_json::Value = serde_json::from_str(&body_strats).unwrap();
    let strat_rows = strats_json.as_array().unwrap();
    assert!(
        strat_rows.iter().any(|r| r["strategy"] == "v1-compat-yes" && r["windows_traded"].as_u64().unwrap() > 0),
        "v1-compat-yes missing or 0 trades: {body_strats}"
    );
    assert!(
        strat_rows.iter().any(|r| r["strategy"] == "v2-imbalance-map" && r["windows_traded"].as_u64().unwrap() > 0),
        "v2-imbalance-map missing or 0 trades: {body_strats}"
    );

    let (st_tour, body_tour) = http_req(port, "GET", "/api/analytics/tournament", &host, &[], None, "");
    assert_eq!(st_tour, 200);
    let tour: serde_json::Value = serde_json::from_str(&body_tour).unwrap();
    let rankings = tour["rankings"].as_array().unwrap();
    assert!(rankings.iter().any(|r| r["strategy"] == "v1-compat-yes"));
    assert!(rankings.iter().any(|r| r["strategy"] == "v2-imbalance-map"));

    // 3. Validate + Backtest a v2 Option-returning script in sandbox.
    let v2_opt_src = r#"
pub fn name() { "v2-option-canary" }
pub fn side() { "yes" }
pub fn describe() { "V2 strategy returning Option using multi-bar causal features." }
pub fn evaluate(ctx) {
    let spread = ctx.spread_cents;
    if ctx.secs_to_close <= 500 && ctx.secs_to_close >= 120
        && spread <= 6.0
        && ctx.delta_60s_cents >= 3.0
        && (ctx.yes_ask - ctx.prev_yes_ask) * 100.0 >= 0.0
        && ctx.pav_ev_yes_cents > -20.0
        && ctx.pav_ev_no_cents < 50.0
        && ctx.yes_bid_size > ctx.no_bid_size
    {
        let lc = (ctx.yes_ask * 100.0).round() as i64;
        return Some(#{ enter: true, side: "yes", limit_cents: lc, post_only: true });
    }
    None
}
"#;
    let val_payload = serde_json::json!({ "source": v2_opt_src }).to_string();
    let (st_val, body_val) = http_req(
        port,
        "POST",
        "/api/strategy/validate",
        &host,
        &[],
        Some("application/json"),
        &val_payload,
    );
    assert_eq!(st_val, 200);
    let val_json: serde_json::Value = serde_json::from_str(&body_val).unwrap();
    assert_eq!(val_json["valid"], true, "v2 validate failed: {body_val}");
    assert_eq!(val_json["strategy_name"], "v2-option-canary");

    let (st_bt, body_bt) = http_req(
        port,
        "POST",
        "/api/backtest",
        &host,
        &[],
        Some("application/json"),
        &val_payload,
    );
    assert_eq!(st_bt, 200);
    let bt_json: serde_json::Value = serde_json::from_str(&body_bt).unwrap();
    assert!(bt_json["score"]["windows_traded"].as_u64().unwrap() > 0, "v2 backtest trades: {body_bt}");

    // 4. Security guards: non-loopback 403, missing/bad CSRF 403, wrong Content-Type 415.
    let save_body = serde_json::json!({
        "name": "v2_option_canary",
        "source": v2_opt_src
    })
    .to_string();

    for (method, path, body_s) in [
        ("POST", "/api/strategies/save", save_body.as_str()),
        ("DELETE", "/api/strategies/v2_option_canary", ""),
        ("POST", "/api/research/synthesize", "{}"),
    ] {
        // Non-loopback host (even in OPENBELL_ALLOWED_HOSTS!) -> 403
        let (st_lan, _) = http_req(
            port,
            method,
            path,
            &format!("desk.lan:{port}"),
            &csrf,
            Some("application/json"),
            body_s,
        );
        assert_eq!(st_lan, 403, "expected 403 for non-loopback on {method} {path}");

        // Missing CSRF -> 403
        let (st_nocsrf, _) = http_req(
            port,
            method,
            path,
            &host,
            &[],
            Some("application/json"),
            body_s,
        );
        assert_eq!(st_nocsrf, 403, "expected 403 for missing CSRF on {method} {path}");

        // Wrong CSRF value -> 403
        let (st_badcsrf, _) = http_req(
            port,
            method,
            path,
            &host,
            &[("X-OpenBell-CSRF", "0")],
            Some("application/json"),
            body_s,
        );
        assert_eq!(st_badcsrf, 403, "expected 403 for CSRF!=1 on {method} {path}");

        // Non-JSON Content-Type -> 415
        let (st_ct, _) = http_req(
            port,
            method,
            path,
            &host,
            &csrf,
            Some("text/plain"),
            body_s,
        );
        assert_eq!(st_ct, 415, "expected 415 for text/plain on {method} {path}");
    }

    // 5. CWE-22/59 Path traversal, symlink, and non-paper source rejection.
    for bad_name in ["../escape", "sub/dir", "..%2Fescape", ".hidden"] {
        let bad_save = serde_json::json!({ "name": bad_name, "source": v2_opt_src }).to_string();
        let (st_bad, _) = http_req(
            port,
            "POST",
            "/api/strategies/save",
            &host,
            &csrf,
            Some("application/json"),
            &bad_save,
        );
        assert!(st_bad == 400 || st_bad == 403, "save traversal must fail for {bad_name}, got {st_bad}");

        let (st_del, _) = http_req(
            port,
            "DELETE",
            &format!("/api/strategies/{bad_name}"),
            &host,
            &csrf,
            Some("application/json"),
            "",
        );
        assert!(st_del == 400 || st_del == 403, "delete traversal must fail for {bad_name}, got {st_del}");
    }
    assert!(!base.join("escape.rn").exists(), "traversal must never write outside strategies_dir");

    // Non-paper source rejected on save.
    let bad_src = r#"
pub fn name() { "evil-net" }
pub fn side() { "yes" }
pub fn evaluate(ctx) { let _ = "https://api.kalshi.com/orders"; None }
"#;
    let bad_src_payload = serde_json::json!({ "name": "evil_net", "source": bad_src }).to_string();
    let (st_np, _) = http_req(
        port,
        "POST",
        "/api/strategies/save",
        &host,
        &csrf,
        Some("application/json"),
        &bad_src_payload,
    );
    assert_eq!(st_np, 400);
    assert!(!strat_dir.join("evil_net.rn").exists());

    // Symlink confinement rejection (Unix).
    #[cfg(unix)]
    {
        let outside = base.join("outside.rn");
        std::fs::write(&outside, v1_src).unwrap();
        let sym = strat_dir.join("symlink_trap.rn");
        std::os::unix::fs::symlink(&outside, &sym).unwrap();
        let sym_save = serde_json::json!({ "name": "symlink_trap", "source": v2_opt_src }).to_string();
        let (st_sym_save, _) = http_req(
            port,
            "POST",
            "/api/strategies/save",
            &host,
            &csrf,
            Some("application/json"),
            &sym_save,
        );
        assert_eq!(st_sym_save, 403, "refuse overwriting symlink");
        let (st_sym_del, _) = http_req(
            port,
            "DELETE",
            "/api/strategies/symlink_trap",
            &host,
            &csrf,
            Some("application/json"),
            "",
        );
        assert_eq!(st_sym_del, 403, "refuse deleting symlink");
        assert!(outside.exists(), "outside target untouched");
        let _ = std::fs::remove_file(&sym);
    }

    // 6. Save valid v2 strategy -> verify disk + GET /api/strategies + DELETE -> verify 404.
    let (st_save, body_save) = http_req(
        port,
        "POST",
        "/api/strategies/save",
        &host,
        &csrf,
        Some("application/json"),
        &save_body,
    );
    assert_eq!(st_save, 200, "save failed: {body_save}");
    let save_json: serde_json::Value = serde_json::from_str(&body_save).unwrap();
    assert_eq!(save_json["ok"], true);
    assert_eq!(save_json["version"], "v2");
    assert_eq!(save_json["causal_canary_passed"], true);
    assert!(strat_dir.join("v2_option_canary.rn").exists());

    let (st_after_save, body_after_save) =
        http_req(port, "GET", "/api/strategies", &host, &[], None, "");
    assert_eq!(st_after_save, 200);
    let after_rows: serde_json::Value = serde_json::from_str(&body_after_save).unwrap();
    assert!(after_rows.as_array().unwrap().iter().any(|r| r["strategy"] == "v2-option-canary"));

    let (st_del, body_del) = http_req(
        port,
        "DELETE",
        "/api/strategies/v2-option-canary",
        &host,
        &csrf,
        Some("application/json"),
        "",
    );
    assert_eq!(st_del, 200, "delete failed: {body_del}");
    assert!(!strat_dir.join("v2_option_canary.rn").exists());

    let (st_del_again, _) = http_req(
        port,
        "DELETE",
        "/api/strategies/v2-option-canary",
        &host,
        &csrf,
        Some("application/json"),
        "",
    );
    assert_eq!(st_del_again, 404);

    // 7. Deterministic Chronological IS->OOS BH-FDR Synthesizer.
    let synth_req = serde_json::json!({
        "family": "all",
        "execution_mode": "maker",
        "fdr_alpha": 0.25,
        "max_candidates": 6,
        "min_is_trades": 3,
        "min_oos_trades": 3,
        "min_deflated_sharpe": 0.50,
        "max_corr": 1.0
    })
    .to_string();

    let (st_s1, body_s1) = http_req(
        port,
        "POST",
        "/api/research/synthesize",
        &host,
        &csrf,
        Some("application/json"),
        &synth_req,
    );
    assert_eq!(st_s1, 200, "synthesize failed: {body_s1}");
    let (st_s2, body_s2) = http_req(
        port,
        "POST",
        "/api/research/synthesize",
        &host,
        &csrf,
        Some("application/json"),
        &synth_req,
    );
    assert_eq!(st_s2, 200);
    let mut s_json: serde_json::Value = serde_json::from_str(&body_s1).unwrap();
    let mut s2_json: serde_json::Value = serde_json::from_str(&body_s2).unwrap();
    s_json["elapsed_ms"] = serde_json::json!(0);
    s2_json["elapsed_ms"] = serde_json::json!(0);
    assert_eq!(s_json, s2_json, "synthesizer MUST be deterministic across runs");

    assert_eq!(s_json["ok"], true);
    assert!(s_json["windows_settled_is"].as_u64().unwrap() > 0);
    assert!(s_json["windows_settled_oos"].as_u64().unwrap() > 0);
    assert!(s_json["total_hypotheses_tested"].as_u64().unwrap() > 0);
    assert!(s_json["survivors_count"].as_u64().unwrap() > 0, "expected survivors: {body_s1}");
    let cands = s_json["candidates"].as_array().unwrap();
    assert!(!cands.is_empty(), "expected synthesized candidates: {body_s1}");
    assert_eq!(cands[0]["passed_all_gates"], true);

    // Save the top synthesized candidate straight into the strategy library!
    let top = &cands[0];
    let top_src = top["source"].as_str().unwrap();
    let top_name = top["name"].as_str().unwrap();
    let save_synth = serde_json::json!({
        "name": top_name,
        "source": top_src,
    })
    .to_string();
    let (st_save_synth, body_save_synth) = http_req(
        port,
        "POST",
        "/api/strategies/save",
        &host,
        &csrf,
        Some("application/json"),
        &save_synth,
    );
    assert_eq!(st_save_synth, 200, "saving synthesized strategy failed: {body_save_synth}");

    // Confirm all three families and taker mode work, and bad family returns 400.
    for fam in ["momentum_microstructure", "mean_reversion_pav", "maker_imbalance"] {
        let req = serde_json::json!({ "family": fam, "execution_mode": "taker" }).to_string();
        let (st_f, _) = http_req(
            port,
            "POST",
            "/api/research/synthesize",
            &host,
            &csrf,
            Some("application/json"),
            &req,
        );
        assert_eq!(st_f, 200, "family {fam} taker failed");
    }
    let bad_fam = serde_json::json!({ "family": "nonexistent_family" }).to_string();
    let (st_bad_fam, _) = http_req(
        port,
        "POST",
        "/api/research/synthesize",
        &host,
        &csrf,
        Some("application/json"),
        &bad_fam,
    );
    assert_eq!(st_bad_fam, 400);

    // Final invariant check: live_armed stays false.
    let (st_home2, body_home2) = http_req(port, "GET", "/api/home", &host, &[], None, "");
    assert_eq!(st_home2, 200);
    let home2: serde_json::Value = serde_json::from_str(&body_home2).unwrap();
    assert_eq!(home2["live"]["live_armed"], false);
}
