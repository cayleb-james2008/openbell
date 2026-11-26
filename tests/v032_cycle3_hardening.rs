//! Cycle 3 Wave H1 Hardening Integration Tests (OpenBell v0.32.0).
//!
//! Covers:
//! - E1: `sync_active_markets` rapid churn & duplicates `<= 32` cap + UI pin preservation,
//!   `orderbooks.jsonl` `read_history()` recovery on corrupt/truncated trailing lines,
//!   non-UTF-8 lines, and `> 8 MiB` oversized files, plus bounded WS reconnect backoff
//!   on malformed WebSocket frames.
//! - E2: Fail-closed `RiskRailsState::load` recovery when `risk_rails_runtime.json` is
//!   corrupt/truncated JSON (`killed = true`, `live_armed = false`), `live_executions.jsonl`
//!   corrupt trailing line recovery + `(kind, venue_id)` idempotency + newline repair,
//!   and hard max-page pagination cap (`50`) on `get_fills_since` / `get_settlements`
//!   against infinite-cursor mock loops.
//! - E3: `quarantine_state.json` recovery on corrupt JSON and `NaN`/`Infinity` markout
//!   numbers without losing quarantine status, `sync_from_allocator` guard when all
//!   strategies are quarantined (cleanly empty slots, never `LiveAuto`), and strict
//!   boot invariants (`live_armed: false`, `RunnerMode != LiveAuto`).

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
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

fn iso_z(ts: i64) -> String {
    let days = ts.div_euclid(86_400);
    let secs = ts.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x6745_2301;
    let mut h1: u32 = 0xEFCD_AB89;
    let mut h2: u32 = 0x98BA_DCFE;
    let mut h3: u32 = 0x1032_5476;
    let mut h4: u32 = 0xC3D2_E1F0;
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 72);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in w[..16].iter_mut().enumerate() {
            let off = i * 4;
            *word = u32::from_be_bytes([chunk[off], chunk[off + 1], chunk[off + 2], chunk[off + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1u32),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDCu32),
                _ => (b ^ c ^ d, 0xCA62_C1D6u32),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }
    let mut out = [0u8; 20];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out
}

fn b64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
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
    if !body.is_empty() || !matches!(method, "GET" | "HEAD" | "DELETE") {
        req.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(body);
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).to_string();
    let (hdr, b) = text.split_once("\r\n\r\n").expect("valid HTTP response");
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

fn spawn_server_env(
    data_dir: &Path,
    strat_dir: &Path,
    port: u16,
    extra_envs: &[(&str, &str)],
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
        .env("OPENBELL_UPDATE_MODE", "off")
        .env("OPENBELL_NO_OPEN", "1")
        .env("OPENBELL_ALLOWED_HOSTS", "desk.lan")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in extra_envs {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("spawn openbell-rs");
    let guard = ServerGuard {
        child,
        dir: cleanup_dir,
    };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            let host = format!("127.0.0.1:{port}");
            let _ = s.write_all(
                format!("GET /api/home HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
            let mut r = String::new();
            if s.read_to_string(&mut r).is_ok() && r.contains("200 OK") {
                return guard;
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!("openbell-rs did not start on port {port}");
}

fn generate_ephemeral_rsa_pem(dir: &Path) -> String {
    let priv_path = dir.join("ephemeral_c3_priv.pem");
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
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!("wait_until timed out after {timeout:?}");
}

#[test]
fn corrupt_risk_rails_runtime_and_boot_invariants() {
    let dir = std::env::temp_dir().join(format!("ob-v032-rails-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // Seed corrupt/truncated risk_rails_runtime.json AND tampered runner_state.json ("mode": "live_auto")
    std::fs::write(
        data_dir.join("risk_rails_runtime.json"),
        br#"{"utc_day_num":20000,"killed":tru"#,
    )
    .unwrap();
    std::fs::write(
        data_dir.join("runner_state.json"),
        r#"{"mode":"live_auto","tick_interval_ms":250,"cancel_before_close_secs":30,"bankroll_dollars":500.0,"slots":[]}"#,
    )
    .unwrap();

    let pem = generate_ephemeral_rsa_pem(&dir);
    let port = free_port();
    let _srv = spawn_server_env(
        &data_dir,
        &strat_dir,
        port,
        &[("OPENBELL_ORDERBOOK_OFFLINE", "1")],
        Some(dir.clone()),
    );
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // 1. Boot invariants: live_armed MUST be false, killed MUST be true (fail-closed), runner mode != live_auto
    let (st_live, b_live) = http_req(port, "GET", "/api/live/status", &host, &[], None, "");
    assert_eq!(st_live, 200);
    let live_v: serde_json::Value = serde_json::from_str(&b_live).unwrap();
    assert_eq!(live_v["live_armed"], false);
    assert_eq!(live_v["killed"], true, "corrupt runtime MUST fail-closed with killed=true");
    assert!(live_v["kill_reason"].as_str().is_some_and(|s| !s.is_empty()));

    let (st_run, b_run) = http_req(port, "GET", "/api/runner/status", &host, &[], None, "");
    assert_eq!(st_run, 200);
    let run_v: serde_json::Value = serde_json::from_str(&b_run).unwrap();
    assert_eq!(run_v["live_armed"], false);
    assert_ne!(run_v["mode"], "live_auto");
    assert_eq!(run_v["killed"], true);

    // 2. Corrupt backup quarantined and sanitized runtime persisted without live_armed
    let has_corrupt_backup = std::fs::read_dir(&data_dir)
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("risk_rails_runtime.json.corrupt.")
        });
    assert!(has_corrupt_backup, "expected quarantined .corrupt backup");
    let recovered_rt = std::fs::read_to_string(data_dir.join("risk_rails_runtime.json")).unwrap();
    assert!(!recovered_rt.contains("live_armed"));
    let recovered_rt_v: serde_json::Value = serde_json::from_str(&recovered_rt).unwrap();
    assert_eq!(recovered_rt_v["killed"], true);

    // 3. Cannot arm live trading or start live_auto while fail-closed kill latch is active
    let cfg_body = serde_json::json!({
        "api_key_id": "v32-corrupt-test",
        "private_key_pem": pem,
        "environment": "demo",
        "api_base_url": "http://127.0.0.1:9/trade-api/v2"
    })
    .to_string();
    assert_eq!(
        http_req(
            port,
            "POST",
            "/api/live/config",
            &host,
            &csrf,
            Some("application/json"),
            &cfg_body
        )
        .0,
        200
    );
    let (st_arm, b_arm) = http_req(
        port,
        "POST",
        "/api/live/arm",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"confirm_arm":true}"#,
    );
    assert_eq!(st_arm, 400, "arm must refuse while killed from corrupt runtime: {b_arm}");
    let arm_v: serde_json::Value = serde_json::from_str(&b_arm).unwrap();
    assert_eq!(arm_v["status"]["live_armed"], false);
    assert_eq!(arm_v["status"]["killed"], true);
    let (st_start, _) = http_req(
        port,
        "POST",
        "/api/runner/start",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"mode":"live_auto","confirm_auto_live":true}"#,
    );
    assert_eq!(st_start, 403);

    // 4. Explicit reset-kill unlatches killed while keeping live_armed = false
    let (st_rst, b_rst) = http_req(
        port,
        "POST",
        "/api/live/reset-kill",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"confirm_reset_kill":true}"#,
    );
    assert_eq!(st_rst, 200, "reset-kill failed: {b_rst}");
    let rst_v: serde_json::Value = serde_json::from_str(&b_rst).unwrap();
    assert_eq!(rst_v["killed"], false);
    assert_eq!(rst_v["live_armed"], false);
}

#[test]
fn corrupt_live_executions_idempotency_and_pagination_cursor_loop_cap() {
    let dir = std::env::temp_dir().join(format!("ob-v032-exec-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    let now = now_unix();
    let seed_fill = serde_json::json!({
        "kind": "fill",
        "venue_id": "tr-seed-1",
        "ts": now - 60,
        "order_id": "ord-seed-1",
        "client_order_id": "cid-seed-1",
        "trade_id": "tr-seed-1",
        "ticker": "KXBTC15M-C3",
        "side": "yes",
        "action": "buy",
        "count": 2,
        "price_cents": 55,
        "is_taker": false,
        "fee_dollars": 0.01
    })
    .to_string();
    let mut exec_bytes = Vec::new();
    // Valid line + duplicate line + invalid UTF-8 line + truncated trailing JSON WITHOUT newline
    exec_bytes.extend_from_slice(seed_fill.as_bytes());
    exec_bytes.push(b'\n');
    exec_bytes.extend_from_slice(seed_fill.as_bytes());
    exec_bytes.push(b'\n');
    exec_bytes.extend_from_slice(&[0xff, 0xfe, b'{', b'b', b'a', b'd', b'\n']);
    exec_bytes.extend_from_slice(br#"{"kind":"fill","venue_id":"tr-partial"#);
    std::fs::write(data_dir.join("live_executions.jsonl"), &exec_bytes).unwrap();

    // Adversarial mock Kalshi server:
    // - /portfolio/fills returns tr-seed-1 on page 1 + incrementing cursors forever
    // - /portfolio/settlements returns repeating cursor "loop-forever"
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let mock_port = listener.local_addr().unwrap().port();
    let fill_reqs = Arc::new(AtomicUsize::new(0));
    let settle_reqs = Arc::new(AtomicUsize::new(0));
    let fill_reqs_bg = fill_reqs.clone();
    let settle_reqs_bg = settle_reqs.clone();

    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut req_line = String::new();
            if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
                continue;
            }
            let path = req_line.split_whitespace().nth(1).unwrap_or("");
            let path_no_q = path.split_once('?').map_or(path, |(p, _)| p);
            let mut content_len = 0usize;
            loop {
                let mut l = String::new();
                if reader.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                    break;
                }
                if let Some((k, v)) = l.split_once(':') {
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        content_len = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            if content_len > 0 {
                let mut sink = vec![0u8; content_len];
                let _ = reader.read_exact(&mut sink);
            }

            let resp_json = match path_no_q {
                "/trade-api/v2/portfolio/positions" => serde_json::json!({ "market_positions": [] }),
                "/trade-api/v2/portfolio/orders" => serde_json::json!({ "orders": [] }),
                "/trade-api/v2/portfolio/fills" => {
                    let n = fill_reqs_bg.fetch_add(1, Ordering::SeqCst) + 1;
                    if n == 1 {
                        serde_json::json!({
                            "fills": [
                                {
                                    "trade_id": "tr-seed-1",
                                    "order_id": "ord-seed-1",
                                    "ticker": "KXBTC15M-C3",
                                    "side": "yes",
                                    "action": "buy",
                                    "count": 2,
                                    "yes_price": 55,
                                    "ts": now - 60
                                },
                                {
                                    "trade_id": "tr-new-1",
                                    "order_id": "ord-new-1",
                                    "ticker": "KXBTC15M-C3",
                                    "side": "yes",
                                    "action": "buy",
                                    "count": 1,
                                    "yes_price": 56,
                                    "ts": now - 10
                                }
                            ],
                            "cursor": format!("next-{n}")
                        })
                    } else {
                        serde_json::json!({
                            "fills": [],
                            "cursor": format!("next-{n}")
                        })
                    }
                }
                "/trade-api/v2/portfolio/settlements" => {
                    let _ = settle_reqs_bg.fetch_add(1, Ordering::SeqCst);
                    serde_json::json!({
                        "settlements": [{
                            "ticker": "KXBTC15M-C3",
                            "market_result": "yes",
                            "yes_count": 3,
                            "no_count": 0,
                            "yes_total_cost": 166,
                            "no_total_cost": 0,
                            "revenue": 300,
                            "fee_cost_dollars": "0.02"
                        }],
                        "cursor": "loop-forever"
                    })
                }
                _ => serde_json::json!({}),
            };
            let body_str = resp_json.to_string();
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
                body_str.len()
            );
            let _ = s.write_all(http.as_bytes());
            let _ = s.flush();
        }
    });

    let pem = generate_ephemeral_rsa_pem(&dir);
    let port = free_port();
    let srv = spawn_server_env(
        &data_dir,
        &strat_dir,
        port,
        &[("OPENBELL_ORDERBOOK_OFFLINE", "1")],
        None,
    );
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    let cfg_body = serde_json::json!({
        "api_key_id": "v32-pag-key",
        "private_key_pem": pem,
        "environment": "demo",
        "api_base_url": format!("http://127.0.0.1:{mock_port}/trade-api/v2")
    })
    .to_string();
    assert_eq!(
        http_req(
            port,
            "POST",
            "/api/live/config",
            &host,
            &csrf,
            Some("application/json"),
            &cfg_body
        )
        .0,
        200
    );

    // Wait for the background reconcile triggered by /api/live/config to finish, then reset
    // counters so we measure a single POST /api/live/reconcile pass cleanly.
    wait_until(Duration::from_secs(6), || {
        if fill_reqs.load(Ordering::SeqCst) >= 50 && settle_reqs.load(Ordering::SeqCst) >= 2 {
            Some(())
        } else {
            None
        }
    });
    fill_reqs.store(0, Ordering::SeqCst);
    settle_reqs.store(0, Ordering::SeqCst);

    // Trigger reconcile against infinite-cursor mock -> MUST terminate cleanly <= 50 pages!
    let (st_rec, b_rec) = http_req(
        port,
        "POST",
        "/api/live/reconcile",
        &host,
        &csrf,
        Some("application/json"),
        "{}",
    );
    assert_eq!(st_rec, 200, "reconcile failed: {b_rec}");
    assert_eq!(fill_reqs.load(Ordering::SeqCst), 50, "fills pagination must cap at 50 pages");
    assert_eq!(settle_reqs.load(Ordering::SeqCst), 2, "settlements must stop on repeated cursor");

    let rec_v: serde_json::Value = serde_json::from_str(&b_rec).unwrap();
    let execs = rec_v["executions"].as_array().unwrap();
    let seed_count = execs
        .iter()
        .filter(|e| e["kind"] == "fill" && e["venue_id"] == "tr-seed-1")
        .count();
    assert_eq!(seed_count, 1, "tr-seed-1 must be deduplicated across reload + reconcile");
    assert!(
        execs.iter().any(|e| e["kind"] == "fill" && e["venue_id"] == "tr-new-1"),
        "new fill appended cleanly after corrupt partial trailing line"
    );
    assert!(
        execs.iter().any(|e| e["kind"] == "settlement" && e["venue_id"] == "KXBTC15M-C3"),
        "settlement appended cleanly"
    );

    // Restart server and verify all 3 unique records survive reload
    drop(srv);
    let port2 = free_port();
    let _srv2 = spawn_server_env(
        &data_dir,
        &strat_dir,
        port2,
        &[("OPENBELL_ORDERBOOK_OFFLINE", "1")],
        Some(dir.clone()),
    );
    let host2 = format!("127.0.0.1:{port2}");
    let (st_rec2, b_rec2) = http_req(
        port2,
        "POST",
        "/api/live/reconcile",
        &host2,
        &csrf,
        Some("application/json"),
        "{}",
    );
    assert_eq!(st_rec2, 200);
    let rec_v2: serde_json::Value = serde_json::from_str(&b_rec2).unwrap();
    assert_eq!(rec_v2["executions"].as_array().unwrap().len(), 3);
}

#[test]
fn l2_ticker_churn_cap_read_history_corrupt_oversized_and_ws_malformed_backoff() {
    let dir = std::env::temp_dir().join(format!("ob-v032-l2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // 1. Seed > 8 MiB orderbooks.jsonl with padding, valid target rows, invalid UTF-8 line, and truncated tail
    {
        let mut f = std::fs::File::create(data_dir.join("orderbooks.jsonl")).unwrap();
        let pad = "{\"ts_ms\":1000,\"ticker\":\"KXBTC15M-PAD\",\"seq\":1,\"source\":\"ws\",\"yes_bids\":[[50,10]],\"no_bids\":[[48,10]]}\n";
        let mut written = 0usize;
        while written < 8 * 1024 * 1024 + 64 * 1024 {
            f.write_all(pad.as_bytes()).unwrap();
            written += pad.len();
        }
        writeln!(
            f,
            "{{\"ts_ms\":2000,\"ticker\":\"KXBTC15M-HIST\",\"seq\":11,\"source\":\"ws\",\"yes_bids\":[[55,10]],\"no_bids\":[[42,10]]}}"
        )
        .unwrap();
        f.write_all(&[0xff, 0xfe, b'{', b'b', b'a', b'd', b'\n']).unwrap();
        writeln!(
            f,
            "{{\"ts_ms\":3000,\"ticker\":\"KXBTC15M-HIST\",\"seq\":12,\"source\":\"rest\",\"yes_bids\":[[56,12]],\"no_bids\":[[41,8]]}}"
        )
        .unwrap();
        f.write_all(br#"{"ts_ms":4000,"ticker":"KXBTC15M-HIST","seq":13,"yes_bids":[[57,"#).unwrap();
    }

    // 2. Mock REST /markets returning 45 churning/duplicate markets + mock WS sending malformed frames
    let rest_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let rest_port = rest_listener.local_addr().unwrap().port();
    let collect_round = Arc::new(AtomicUsize::new(0));
    let collect_round_bg = collect_round.clone();
    std::thread::spawn(move || {
        let close_iso = iso_z(now_unix() + 600);
        for mut s in rest_listener.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut req_line = String::new();
            if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
                continue;
            }
            let path = req_line.split_whitespace().nth(1).unwrap_or("");
            loop {
                let mut l = String::new();
                if reader.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                    break;
                }
            }
            let round = collect_round_bg.load(Ordering::SeqCst);
            let body = if path.starts_with("/trade-api/v2/markets?") {
                let mut markets = Vec::new();
                for i in 1..=45usize {
                    let t = format!("KXBTC15M-C{round}M{i:02}");
                    let m = serde_json::json!({
                        "ticker": t,
                        "event_ticker": t,
                        "close_time": close_iso,
                        "status": "open",
                        "yes_bid_dollars": "0.52",
                        "yes_ask_dollars": "0.55",
                        "no_bid_dollars": "0.45",
                        "no_ask_dollars": "0.48",
                        "last_price_dollars": "0.54",
                        "volume": 100
                    });
                    markets.push(m.clone());
                    if i <= 5 {
                        // Inject duplicate markets in API payload to test deduplication
                        markets.push(m);
                    }
                }
                serde_json::json!({ "markets": markets, "cursor": "" }).to_string()
            } else {
                serde_json::json!({ "orderbook": { "yes": [[52, 5]], "no": [[45, 5]] } }).to_string()
            };
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(http.as_bytes());
            let _ = s.flush();
        }
    });

    let ws_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    let ws_connects = Arc::new(AtomicUsize::new(0));
    let ws_connects_bg = ws_connects.clone();
    std::thread::spawn(move || {
        for mut s in ws_listener.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
            let mut reader = BufReader::new(s.try_clone().unwrap());
            let mut req_headers = HashMap::new();
            let mut req_line = String::new();
            if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
                continue;
            }
            loop {
                let mut l = String::new();
                if reader.read_line(&mut l).unwrap_or(0) == 0 || l == "\r\n" {
                    break;
                }
                if let Some((k, v)) = l.split_once(':') {
                    req_headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }
            let Some(ws_key) = req_headers.get("sec-websocket-key") else {
                continue;
            };
            let combined = format!("{ws_key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
            let accept = b64_encode(&sha1_digest(combined.as_bytes()));
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            if s.write_all(resp.as_bytes()).is_err() {
                continue;
            }
            let _ = s.flush();
            ws_connects_bg.fetch_add(1, Ordering::SeqCst);
            // Send malformed WS frame (unknown opcode 0x3) -> client must disconnect and back off exponentially!
            let _ = s.write_all(&[0x83, 0x03, b'b', b'a', b'd']);
            let _ = s.flush();
        }
    });

    // Phase A: Boot with OPENBELL_ORDERBOOK_OFFLINE=1 so REST fallback doesn't rotate the >8MB file
    // before we verify GET /api/orderbook/history recovers valid records around invalid UTF-8 & truncated tail.
    let port_hist = free_port();
    let srv_hist = spawn_server_env(
        &data_dir,
        &strat_dir,
        port_hist,
        &[("OPENBELL_ORDERBOOK_OFFLINE", "1")],
        None,
    );
    let host_hist = format!("127.0.0.1:{port_hist}");
    let (st_hist, b_hist) = http_req(
        port_hist,
        "GET",
        "/api/orderbook/history?ticker=KXBTC15M-HIST&limit=50",
        &host_hist,
        &[],
        None,
        "",
    );
    assert_eq!(st_hist, 200, "history failed: {b_hist}");
    let hist_v: serde_json::Value = serde_json::from_str(&b_hist).unwrap();
    let recs = hist_v["records"].as_array().unwrap();
    assert_eq!(recs.len(), 2);
    assert_eq!(recs[0]["seq"], 11);
    assert_eq!(recs[1]["seq"], 12);
    drop(srv_hist);

    // Phase B: Boot with live mock WS (malformed frames) + mock REST (45 churning/duplicate markets)
    let port = free_port();
    let ws_url = format!("ws://127.0.0.1:{ws_port}/trade-api/ws/v2");
    let rest_base = format!("http://127.0.0.1:{rest_port}/trade-api/v2");
    let _srv = spawn_server_env(
        &data_dir,
        &strat_dir,
        port,
        &[
            ("OPENBELL_WS_URL", &ws_url),
            ("OPENBELL_API_BASE", &rest_base),
            ("OPENBELL_SERIES", "KXBTC15M"),
        ],
        Some(dir.clone()),
    );
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // Pin 2 UI tickers, then run multiple rapid market-churn collect passes
    assert_eq!(
        http_req(
            port,
            "POST",
            "/api/orderbook/subscribe",
            &host,
            &csrf,
            Some("application/json"),
            r#"{"tickers":["KXBTC15M-PIN1","KXETH15M-PIN2"]}"#
        )
        .0,
        200
    );

    for round in 1..=4 {
        collect_round.store(round, Ordering::SeqCst);
        let (st_col, b_col) = http_req(
            port,
            "POST",
            "/api/collect",
            &host,
            &csrf,
            Some("application/json"),
            "{}",
        );
        assert_eq!(st_col, 200, "collect round {round} failed: {b_col}");

        let (st_ss, b_ss) = http_req(
            port,
            "GET",
            "/api/orderbook/stream-status",
            &host,
            &[],
            None,
            "",
        );
        assert_eq!(st_ss, 200);
        let ss: serde_json::Value = serde_json::from_str(&b_ss).unwrap();
        let subs = ss["subscribed_tickers"].as_array().unwrap();
        assert_eq!(subs.len(), 32, "must cap at MAX_SUBSCRIBED_TICKERS=32");
        assert!(subs.iter().any(|t| t == "KXBTC15M-PIN1"));
        assert!(subs.iter().any(|t| t == "KXETH15M-PIN2"));
        assert_eq!(ss["auto_subscribed_count"], 30);
    }

    // Verify WS reconnect attempts against malformed-frame server stay bounded via exponential backoff
    wait_until(Duration::from_secs(4), || {
        if ws_connects.load(Ordering::SeqCst) >= 2 {
            Some(())
        } else {
            None
        }
    });
    std::thread::sleep(Duration::from_millis(2200));
    let attempts = ws_connects.load(Ordering::SeqCst);
    assert!(
        (2..=4).contains(&attempts),
        "WS reconnects on malformed frames must back off exponentially (expected 2..=4 in ~2.5s, got {attempts})"
    );
}

#[test]
fn corrupt_quarantine_nan_inf_recovery_and_all_quarantined_sync_allocator() {
    let dir = std::env::temp_dir().join(format!("ob-v032-q-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Seed a positive OOS dataset so AutoAllocator has candidates unless quarantined
    let mut tape_buf = String::new();
    let base_epoch: i64 = 1_790_812_800;
    for w in 0..40usize {
        let close_epoch = base_epoch + (w as i64) * 900;
        let close_time = iso_z(close_epoch);
        let btc_ticker = format!("KXBTC15M-C3Q{w:03}");
        let btc_res = if w % 7 != 0 { "yes" } else { "no" };
        tape_buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{btc_ticker}\",\"event_ticker\":\"{btc_ticker}\",\"series\":\"KXBTC15M\",\"close_time\":\"{close_time}\",\"status\":\"open\",\"result\":\"\",\"yes_bid\":0.70,\"yes_ask\":0.72,\"no_bid\":0.28,\"no_ask\":0.30,\"last_price\":0.72,\"yes_bid_size\":400.0,\"no_bid_size\":350.0}}\n",
            close_epoch - 200
        ));
        tape_buf.push_str(&format!(
            "{{\"fetched_at\":{},\"ticker\":\"{btc_ticker}\",\"event_ticker\":\"{btc_ticker}\",\"series\":\"KXBTC15M\",\"close_time\":\"{close_time}\",\"status\":\"finalized\",\"result\":\"{btc_res}\",\"yes_bid\":null,\"yes_ask\":null,\"no_bid\":null,\"no_ask\":null,\"last_price\":null}}\n",
            close_epoch + 60
        ));
    }
    std::fs::write(data_dir.join("snapshots.jsonl"), &tape_buf).unwrap();

    // Server #1: Discover what strategies qualify, then quarantine ALL of them with NaN/Infinity fields!
    let port1 = free_port();
    let srv1 = spawn_server_env(
        &data_dir,
        &strat_dir,
        port1,
        &[("OPENBELL_ORDERBOOK_OFFLINE", "1")],
        None,
    );
    let host1 = format!("127.0.0.1:{port1}");
    let (st_a1, b_a1) = http_req(port1, "GET", "/api/allocator", &host1, &[], None, "");
    assert_eq!(st_a1, 200);
    let a1: serde_json::Value = serde_json::from_str(&b_a1).unwrap();
    let screener_rows = a1["edge_screener"].as_array().unwrap();
    assert!(!screener_rows.is_empty(), "expected screener strategies");
    let mut all_strat_names: Vec<String> = screener_rows
        .iter()
        .map(|w| w["strategy"].as_str().unwrap().to_string())
        .collect();
    // Also read every loaded strategy's `name()` from `strategies/*.rn` so strategies dropped
    // by diversification correlation filters are also quarantined cleanly.
    for entry in std::fs::read_dir(&strat_dir).unwrap().flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "rn") {
            if let Ok(src) = std::fs::read_to_string(&p) {
                if let Some(idx) = src.find("fn name()") {
                    let tail = &src[idx..];
                    if let (Some(q1), Some(rest)) = (tail.find('"'), tail.find('"').map(|i| &tail[i + 1..])) {
                        let _ = q1;
                        if let Some(q2) = rest.find('"') {
                            let sname = rest[..q2].to_string();
                            if !sname.is_empty() && !all_strat_names.contains(&sname) {
                                all_strat_names.push(sname);
                            }
                        }
                    }
                }
            }
        }
    }
    drop(srv1);

    // Write quarantine_state.json containing bare NaN / Infinity / -Infinity markout numbers
    let mut strat_entries_json = Vec::new();
    for name in &all_strat_names {
        strat_entries_json.push(format!(
            r#""{name}": {{
                "status": "quarantined",
                "throttle_mult": Infinity,
                "reason": "corrupt markout recovery test",
                "triggered_at": 1790800000,
                "rolling_fills": 6,
                "rolling_markout_30s_cents": NaN,
                "rolling_fill_rate_pct": -Infinity,
                "rolling_net_pnl_dollars": "NaN"
            }}"#
        ));
    }
    let q_raw = format!(
        "{{\n  \"updated_at\": 1790800000,\n  \"strategies\": {{\n    {}\n  }}\n}}",
        strat_entries_json.join(",\n    ")
    );
    std::fs::write(data_dir.join("quarantine_state.json"), q_raw).unwrap();

    // Server #2: Must recover quarantined strategies cleanly with finite numbers, and sync-allocator must leave slots empty!
    let port2 = free_port();
    let srv2 = spawn_server_env(
        &data_dir,
        &strat_dir,
        port2,
        &[("OPENBELL_ORDERBOOK_OFFLINE", "1")],
        None,
    );
    let host2 = format!("127.0.0.1:{port2}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    let (st_eh, b_eh) = http_req(
        port2,
        "GET",
        "/api/analytics/execution-health",
        &host2,
        &[],
        None,
        "",
    );
    assert_eq!(st_eh, 200, "execution-health failed: {b_eh}");
    let eh: serde_json::Value = serde_json::from_str(&b_eh).unwrap();
    for name in &all_strat_names {
        let entry = &eh["quarantine_state"]["strategies"][name];
        assert_eq!(entry["status"], "quarantined");
        assert_eq!(entry["throttle_mult"], 0.0);
        assert!(entry["rolling_markout_30s_cents"].is_f64());
    }

    let (st_sync, b_sync) = http_req(
        port2,
        "POST",
        "/api/runner/sync-allocator",
        &host2,
        &csrf,
        Some("application/json"),
        "{}",
    );
    assert_eq!(st_sync, 200, "sync-allocator failed: {b_sync}");
    let sync_v: serde_json::Value = serde_json::from_str(&b_sync).unwrap();
    assert!(
        sync_v["slots"].as_array().unwrap().is_empty(),
        "all-quarantined sync-allocator must leave slots empty: {b_sync}"
    );
    assert_eq!(sync_v["status"]["live_armed"], false);
    assert_ne!(sync_v["status"]["mode"], "live_auto");

    // Zero bankroll via HTTP sync-allocator is rejected 400 and never touches LiveAuto
    let (st_zero, _) = http_req(
        port2,
        "POST",
        "/api/runner/sync-allocator",
        &host2,
        &csrf,
        Some("application/json"),
        r#"{"bankroll_dollars":0}"#,
    );
    assert_eq!(st_zero, 400);
    drop(srv2);

    // Server #3: Completely truncated corrupt quarantine_state.json must still boot cleanly (live_armed=false, mode != live_auto)
    std::fs::write(data_dir.join("quarantine_state.json"), r#"{"updated_at":1,"strategies":{"#).unwrap();
    let port3 = free_port();
    let _srv3 = spawn_server_env(
        &data_dir,
        &strat_dir,
        port3,
        &[("OPENBELL_ORDERBOOK_OFFLINE", "1")],
        Some(dir.clone()),
    );
    let host3 = format!("127.0.0.1:{port3}");
    let (st_eh3, _) = http_req(
        port3,
        "GET",
        "/api/analytics/execution-health",
        &host3,
        &[],
        None,
        "",
    );
    assert_eq!(st_eh3, 200);
    let (st_run3, b_run3) = http_req(port3, "GET", "/api/runner/status", &host3, &[], None, "");
    assert_eq!(st_run3, 200);
    let run3: serde_json::Value = serde_json::from_str(&b_run3).unwrap();
    assert_eq!(run3["live_armed"], false);
    assert_ne!(run3["mode"], "live_auto");
}
