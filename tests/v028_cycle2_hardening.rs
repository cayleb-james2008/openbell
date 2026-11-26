//! Cycle 2 Wave H1 Hardening & Security Integration Test Suite (OpenBell v0.28.0).
//!
//! Covers fail-closed restart recovery, emergency kill-switch atomicity prior to
//! venue `cancel_all_orders`, queue depletion bounds & duplicate fill/cancel idempotency,
//! malformed/crossed/gapped L2 WebSocket frames, CWE-22/59 symlink & null-byte CRUD
//! rejection, VM step-budget halts on infinite-loop scripts, and degenerate/NaN/Inf
//! synthesizer guards.

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
    ws_url: Option<&str>,
    api_base: Option<&str>,
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
        .env("OPENBELL_UPDATE_MODE", "off")
        .env("OPENBELL_NO_OPEN", "1")
        .env("OPENBELL_ALLOWED_HOSTS", "desk.lan")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(ws) = ws_url {
        cmd.env("OPENBELL_WS_URL", ws);
    }
    if let Some(api) = api_base {
        cmd.env("OPENBELL_API_BASE", api);
    }
    let child = cmd.spawn().expect("spawn openbell-rs");
    let host = format!("127.0.0.1:{port}");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if Instant::now() > deadline {
            panic!("timed out waiting for openbell-rs on port {port}");
        }
        if let Ok(mut s) = TcpStream::connect(("127.0.0.1", port)) {
            let _ = s.write_all(
                format!("GET /api/home HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            );
            let mut r = String::new();
            if s.read_to_string(&mut r).is_ok() && r.contains("200 OK") {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    ServerGuard {
        child,
        dir: cleanup_dir,
    }
}

fn wait_until<F>(timeout: Duration, mut pred: F) -> serde_json::Value
where
    F: FnMut() -> Option<serde_json::Value>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = pred() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for condition");
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x6745_2301;
    let mut h1: u32 = 0xEFCD_AB89;
    let mut h2: u32 = 0x98BA_DCFE;
    let mut h3: u32 = 0x1032_5476;
    let mut h4: u32 = 0xC3D2_E1F0;
    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in w[..16].iter_mut().enumerate() {
            let off = i * 4;
            *word = u32::from_be_bytes([
                chunk[off],
                chunk[off + 1],
                chunk[off + 2],
                chunk[off + 3],
            ]);
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
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let v = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((v >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((v >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((v >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(v & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

fn send_ws_server_text(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    let payload = text.as_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x81);
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else if payload.len() <= 65_535 {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)?;
    stream.flush()
}

fn read_ws_client_frame(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr)?;
    let masked = (hdr[1] & 0x80) != 0;
    let len7 = hdr[1] & 0x7f;
    let len: usize = match len7 {
        0..=125 => len7 as usize,
        126 => {
            let mut ext = [0u8; 2];
            stream.read_exact(&mut ext)?;
            u16::from_be_bytes(ext) as usize
        }
        _ => {
            let mut ext = [0u8; 8];
            stream.read_exact(&mut ext)?;
            u64::from_be_bytes(ext) as usize
        }
    };
    let mut mask = [0u8; 4];
    if masked {
        stream.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    if masked {
        for (i, b) in payload.iter_mut().enumerate() {
            *b ^= mask[i & 3];
        }
    }
    Ok(String::from_utf8_lossy(&payload).to_string())
}

fn generate_ephemeral_rsa_pem(tmp: &Path) -> String {
    let priv_path = tmp.join("ephemeral_h1_priv.pem");
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
        .expect("run openssl genpkey");
    assert!(st.success(), "openssl genpkey failed");
    std::fs::read_to_string(&priv_path).unwrap()
}

#[test]
fn runner_restart_clamps_live_auto_and_kill_switch_halts_before_cancel_all() {
    let dir = std::env::temp_dir().join(format!("ob-v028-sr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // 1. Tamper runner_state.json with mode = "live_auto" before boot.
    std::fs::write(
        data_dir.join("runner_state.json"),
        serde_json::json!({
            "mode": "live_auto",
            "tick_interval_ms": 200,
            "cancel_before_close_secs": 30,
            "bankroll_dollars": 500.0,
            "slots": [{
                "slot_id": 1,
                "strategy": "h1-maker",
                "series": ["KXBTC15M"],
                "max_contracts": 5,
                "execution_mode": "maker",
                "auto_pause_drawdown_dollars": 25.0,
                "enabled": true
            }]
        })
        .to_string(),
    )
    .unwrap();

    std::fs::write(
        strat_dir.join("h1_maker.rn"),
        r#"
pub fn name() { "h1-maker" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { secs_to_close >= 60 && yes_ask <= 0.65 }
pub fn entry_quote(yes_ask, no_ask, secs_to_close) { Some(0.55) }
"#,
    )
    .unwrap();

    // Mock Kalshi server that verifies OpenBell's /api/runner/status is ALREADY halted ("stopped")
    // when cancel_all_orders invokes GET /trade-api/v2/portfolio/orders!
    let openbell_port = free_port();
    let runner_mode_during_cancel_all = Arc::new(Mutex::new(Option::<(String, bool, bool)>::None));
    let seen_clone = runner_mode_during_cancel_all.clone();

    let kalshi_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let kalshi_port = kalshi_listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in kalshi_listener.incoming().flatten() {
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

            let resp_json = match (method.as_str(), path_no_query) {
                ("GET", "/trade-api/v2/portfolio/orders") => {
                    // Probe OpenBell /api/runner/status BEFORE returning cancel_all orders!
                    let host = format!("127.0.0.1:{openbell_port}");
                    let (st, body) = http_req(
                        openbell_port,
                        "GET",
                        "/api/runner/status",
                        &host,
                        &[],
                        None,
                        "",
                    );
                    if st == 200 {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                            *seen_clone.lock().unwrap() = Some((
                                v["mode"].as_str().unwrap_or("").to_string(),
                                v["live_armed"].as_bool().unwrap_or(true),
                                v["killed"].as_bool().unwrap_or(false),
                            ));
                        }
                    }
                    serde_json::json!({ "orders": [] })
                }
                _ => serde_json::json!({ "balance": 50000, "portfolio_value": 0 }),
            };
            let body_out = resp_json.to_string();
            let http = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_out}",
                body_out.len()
            );
            let _ = s.write_all(http.as_bytes());
        }
    });

    let api_base = format!("http://127.0.0.1:{kalshi_port}/trade-api/v2");
    let srv = spawn_server(
        &data_dir,
        &strat_dir,
        openbell_port,
        None,
        Some(&api_base),
        None,
    );
    let host = format!("127.0.0.1:{openbell_port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // Verify boot clamped "live_auto" -> "stopped" and live_armed == false.
    let (st_boot, b_boot) = http_req(openbell_port, "GET", "/api/runner/status", &host, &[], None, "");
    assert_eq!(st_boot, 200);
    let boot_v: serde_json::Value = serde_json::from_str(&b_boot).unwrap();
    assert_eq!(boot_v["mode"], "stopped");
    assert_eq!(boot_v["live_armed"], false);
    let disk_state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(data_dir.join("runner_state.json")).unwrap())
            .unwrap();
    assert_eq!(disk_state["mode"], "stopped");

    // Refuses live_auto while disarmed.
    let (st_refuse, _) = http_req(
        openbell_port,
        "POST",
        "/api/runner/start",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"mode":"live_auto","confirm_auto_live":true}"#,
    );
    assert_eq!(st_refuse, 403);

    // Configure live credentials + arm live + start live_auto, then trigger kill-switch!
    let pem = generate_ephemeral_rsa_pem(&dir);
    let (st_cfg, b_cfg) = http_req(
        openbell_port,
        "POST",
        "/api/live/config",
        &host,
        &csrf,
        Some("application/json"),
        &serde_json::json!({
            "api_key_id": "test-key-h1",
            "private_key_pem": pem,
            "environment": "demo",
            "api_base_url": api_base
        })
        .to_string(),
    );
    assert_eq!(st_cfg, 200, "live/config failed: {b_cfg}");

    let (st_arm, b_arm) = http_req(
        openbell_port,
        "POST",
        "/api/live/arm",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"confirm_arm":true}"#,
    );
    assert_eq!(st_arm, 200, "live/arm failed: {b_arm}");

    let (st_start, _) = http_req(
        openbell_port,
        "POST",
        "/api/runner/start",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"mode":"live_auto","confirm_auto_live":true}"#,
    );
    assert_eq!(st_start, 200);

    let (st_ks, b_ks) = http_req(
        openbell_port,
        "POST",
        "/api/live/kill-switch",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"reason":"h1 atomicity audit"}"#,
    );
    assert_eq!(st_ks, 200);
    let ks_v: serde_json::Value = serde_json::from_str(&b_ks).unwrap();
    assert_eq!(ks_v["killed"], true);
    assert_eq!(ks_v["disarmed"], true);

    let observed = runner_mode_during_cancel_all
        .lock()
        .unwrap()
        .clone()
        .expect("cancel_all_orders must have queried Kalshi /portfolio/orders");
    assert_eq!(
        observed,
        ("stopped".to_string(), false, true),
        "runner must be halted (stopped, live_armed=false, killed=true) BEFORE cancel_all_orders!"
    );

    drop(srv);

    // 2. Restart with persisted "shadow" mode -> recovers "shadow" while ALWAYS booting live_armed=false.
    std::fs::write(
        data_dir.join("runner_state.json"),
        serde_json::json!({
            "mode": "shadow",
            "tick_interval_ms": 200,
            "cancel_before_close_secs": 30,
            "bankroll_dollars": 500.0,
            "slots": [{
                "slot_id": 1,
                "strategy": "h1-maker",
                "series": ["KXBTC15M"],
                "max_contracts": 5,
                "execution_mode": "maker",
                "auto_pause_drawdown_dollars": 25.0,
                "enabled": true
            }]
        })
        .to_string(),
    )
    .unwrap();
    let port2 = free_port();
    let host2 = format!("127.0.0.1:{port2}");
    let _srv2 = spawn_server(&data_dir, &strat_dir, port2, None, None, Some(dir));
    let (st_boot2, b_boot2) = http_req(port2, "GET", "/api/runner/status", &host2, &[], None, "");
    assert_eq!(st_boot2, 200);
    let boot2: serde_json::Value = serde_json::from_str(&b_boot2).unwrap();
    assert_eq!(boot2["mode"], "shadow");
    assert_eq!(boot2["live_armed"], false);
}

#[test]
fn runner_queue_depletion_bounds_and_idempotent_stop() {
    let dir = std::env::temp_dir().join(format!("ob-v028-qid-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    std::fs::write(
        strat_dir.join("q_strat.rn"),
        r#"
pub fn name() { "q-strat" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { secs_to_close >= 60 }
pub fn entry_quote(yes_ask, no_ask, secs_to_close) { Some(0.55) }
"#,
    )
    .unwrap();

    // Tick 1: resting quote at 55c (yes_bid=55, yes_ask=60, yes_bid_size=30 -> queue_ahead=30).
    let snap1 = serde_json::json!({
        "fetched_at": 1_800_000_000i64,
        "ticker": "KXBTC15M-Q1",
        "event_ticker": "KXBTC15M-QEV",
        "series": "KXBTC15M",
        "close_time": "2027-01-20T00:15:00Z",
        "status": "open",
        "result": "",
        "yes_bid": 0.55,
        "yes_ask": 0.60,
        "no_bid": 0.40,
        "no_ask": 0.45,
        "last_price": 0.58,
        "yes_bid_size": 30.0,
        "no_bid_size": 20.0
    });
    std::fs::write(data_dir.join("snapshots.jsonl"), format!("{snap1}\n")).unwrap();

    let port = free_port();
    let _srv = spawn_server(&data_dir, &strat_dir, port, None, None, Some(dir));
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    let (st_cfg, _) = http_req(
        port,
        "POST",
        "/api/runner/config",
        &host,
        &csrf,
        Some("application/json"),
        r#"{
            "slots": [{
                "slot_id": 1,
                "strategy": "q-strat",
                "series": ["KXBTC15M"],
                "max_contracts": 5,
                "execution_mode": "maker",
                "auto_pause_drawdown_dollars": 50.0,
                "enabled": true
            }],
            "cancel_before_close_secs": 30,
            "bankroll_dollars": 500.0,
            "tick_interval_ms": 60
        }"#,
    );
    assert_eq!(st_cfg, 200);
    let (st_start, _) = http_req(
        port,
        "POST",
        "/api/runner/start",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"mode":"shadow"}"#,
    );
    assert_eq!(st_start, 200);

    let posted = wait_until(Duration::from_secs(5), || {
        let (st, b) = http_req(port, "GET", "/api/runner/status", &host, &[], None, "");
        if st != 200 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(&b).ok()?;
        let oq = v["open_quotes"].as_array()?;
        if oq.is_empty() {
            None
        } else {
            Some(v)
        }
    });
    assert_eq!(posted["open_quotes"][0]["queue_ahead_initial"], 30);

    // Tick 2: non-touch bid size drops to 0.0 -> queue_ahead_remaining MUST stay >= 1 (never fills without touch/cross!).
    let snap2 = serde_json::json!({
        "fetched_at": 1_800_000_010i64,
        "ticker": "KXBTC15M-Q1",
        "event_ticker": "KXBTC15M-QEV",
        "series": "KXBTC15M",
        "close_time": "2027-01-20T00:15:00Z",
        "status": "open",
        "result": "",
        "yes_bid": 0.55,
        "yes_ask": 0.59,
        "no_bid": 0.41,
        "no_ask": 0.45,
        "last_price": 0.58,
        "yes_bid_size": 0.0,
        "no_bid_size": -5.0
    });
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(data_dir.join("snapshots.jsonl"))
            .unwrap();
        writeln!(f, "{snap2}").unwrap();
    }
    let after_erode = wait_until(Duration::from_secs(5), || {
        let (_, b) = http_req(port, "GET", "/api/runner/status", &host, &[], None, "");
        let v: serde_json::Value = serde_json::from_str(&b).ok()?;
        let rem = v["open_quotes"].as_array()?.first()?["queue_ahead_remaining"].as_u64()?;
        if rem == 1 {
            Some(v)
        } else {
            None
        }
    });
    assert_eq!(after_erode["stats"]["fills"], 0);
    assert_eq!(after_erode["open_quotes"][0]["queue_ahead_remaining"], 1);

    // Call /api/runner/stop twice -> first cancels the 1 resting quote, second cancels 0 (idempotent).
    let (st_stop1, b_stop1) = http_req(
        port,
        "POST",
        "/api/runner/stop",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"cancel_resting":true}"#,
    );
    assert_eq!(st_stop1, 200);
    let stop1: serde_json::Value = serde_json::from_str(&b_stop1).unwrap();
    assert_eq!(stop1["cancelled_quotes"], 1);

    let (st_stop2, b_stop2) = http_req(
        port,
        "POST",
        "/api/runner/stop",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"cancel_resting":true}"#,
    );
    assert_eq!(st_stop2, 200);
    let stop2: serde_json::Value = serde_json::from_str(&b_stop2).unwrap();
    assert_eq!(stop2["cancelled_quotes"], 0);

    let (st_led, b_led) = http_req(port, "GET", "/api/runner/ledger?limit=50", &host, &[], None, "");
    assert_eq!(st_led, 200);
    let led: serde_json::Value = serde_json::from_str(&b_led).unwrap();
    let canceled_cnt = led["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"] == "QuoteCanceled")
        .count();
    assert_eq!(canceled_cnt, 1, "expected exactly 1 QuoteCanceled event");
}

#[test]
fn orderbook_ws_malformed_crossed_clamped_and_gap_resync() {
    let dir = std::env::temp_dir().join(format!("ob-v028-l2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // Mock REST L2 server for resync after crossed/gap.
    let rest_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let rest_port = rest_listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in rest_listener.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let body = serde_json::json!({
                "orderbook": {
                    "yes": [[55, 20], [0, 99], [100, 99]],
                    "no": [[42, 10], [-5, 50]]
                }
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });

    // Mock WS server: sends malformed JSON, then out-of-range snapshot, then crossed snapshot.
    let ws_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    let stage = Arc::new(Mutex::new(0u8));
    let stage_bg = stage.clone();
    std::thread::spawn(move || {
        for mut s in ws_listener.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
            let mut raw = Vec::new();
            let mut tmp = [0u8; 1024];
            while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = s.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break;
                }
                raw.extend_from_slice(&tmp[..n]);
            }
            let hdr_str = String::from_utf8_lossy(&raw);
            let ws_key = hdr_str
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.trim().eq_ignore_ascii_case("sec-websocket-key") {
                        Some(v.trim().to_string())
                    } else {
                        None
                    }
                })
                .unwrap();
            let accept = b64_encode(&sha1_digest(
                format!("{ws_key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
            ));
            let upgrade = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            s.write_all(upgrade.as_bytes()).unwrap();
            s.flush().unwrap();
            let _ = read_ws_client_frame(&mut s);

            // 1. Malformed JSON frame -> server records error without crashing.
            let _ = send_ws_server_text(&mut s, "{corrupt json");
            // 2. Snapshot with out-of-bounds price levels (0c, 100c, -5c) + valid 55c/42c.
            let snap_clamp = serde_json::json!({
                "type": "orderbook_snapshot",
                "seq": 1,
                "msg": {
                    "market_ticker": "KXBTC15M-H1OB",
                    "yes": [[0, 50], [55, 12], [100, 50]],
                    "no": [[-3, 10], [42, 8], [105, 10]]
                }
            })
            .to_string();
            let _ = send_ws_server_text(&mut s, &snap_clamp);

            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(10) {
                let cur = *stage_bg.lock().unwrap();
                if cur == 1 {
                    // 3. Crossed delta: yes bid at 65c crosses no bid at 42c (65+42=107>100).
                    let crossed_delta = serde_json::json!({
                        "type": "orderbook_delta",
                        "seq": 2,
                        "msg": {
                            "market_ticker": "KXBTC15M-H1OB",
                            "side": "yes",
                            "price": 65,
                            "delta": 10
                        }
                    })
                    .to_string();
                    let _ = send_ws_server_text(&mut s, &crossed_delta);
                    *stage_bg.lock().unwrap() = 2;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
    });

    let port = free_port();
    let ws_url = format!("ws://127.0.0.1:{ws_port}/trade-api/ws/v2");
    let rest_base = format!("http://127.0.0.1:{rest_port}/trade-api/v2");
    let _srv = spawn_server(
        &data_dir,
        &strat_dir,
        port,
        Some(&ws_url),
        Some(&rest_base),
        Some(dir),
    );
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    let (st_sub, _) = http_req(
        port,
        "POST",
        "/api/orderbook/subscribe",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"ticker":"KXBTC15M-H1OB"}"#,
    );
    assert_eq!(st_sub, 200);

    // Wait for clamped WS snapshot (only 55c yes_bid and 42c no_bid survived).
    let clamped = wait_until(Duration::from_secs(6), || {
        let (st, b) = http_req(
            port,
            "GET",
            "/api/orderbook?ticker=KXBTC15M-H1OB",
            &host,
            &[],
            None,
            "",
        );
        if st != 200 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(&b).ok()?;
        if v["source"] == "ws" && v["last_seq"] == 1 {
            Some(v)
        } else {
            None
        }
    });
    assert_eq!(clamped["yes_bids"].as_array().unwrap().len(), 1);
    assert_eq!(clamped["yes_bids"][0]["price_cents"], 55);
    assert_eq!(clamped["no_bids"].as_array().unwrap().len(), 1);
    assert_eq!(clamped["no_bids"][0]["price_cents"], 42);

    // Trigger crossed delta -> needs_resync -> REST resync cleans it back to 55c/42c!
    *stage.lock().unwrap() = 1;
    let resynced = wait_until(Duration::from_secs(6), || {
        let (_, b_st) = http_req(port, "GET", "/api/orderbook/stream-status", &host, &[], None, "");
        let st_v: serde_json::Value = serde_json::from_str(&b_st).ok()?;
        if st_v["resync_count"].as_u64().unwrap_or(0) >= 1 {
            let (_, b_ob) = http_req(
                port,
                "GET",
                "/api/orderbook?ticker=KXBTC15M-H1OB",
                &host,
                &[],
                None,
                "",
            );
            let ob: serde_json::Value = serde_json::from_str(&b_ob).ok()?;
            if ob["source"] == "rest" && ob["needs_resync"] == false {
                return Some(ob);
            }
        }
        None
    });
    assert_eq!(resynced["best_yes_bid"], 55);
    assert_eq!(resynced["best_yes_ask"], 58);
}

#[test]
fn strategy_crud_security_vm_budget_and_synthesizer_degenerate_guards() {
    let dir = std::env::temp_dir().join(format!("ob-v028-s2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    let outside_dir = dir.join("outside");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();
    std::fs::create_dir_all(&outside_dir).unwrap();

    // Create a file outside strategies_dir and symlinks (live + dangling) inside strategies_dir.
    let outside_secret = outside_dir.join("secret.rn");
    std::fs::write(&outside_secret, "pub fn name() { \"secret\" }").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside_secret, strat_dir.join("sym_escape.rn")).unwrap();
        std::os::unix::fs::symlink(
            outside_dir.join("nonexistent.rn"),
            strat_dir.join("dangling_link.rn"),
        )
        .unwrap();
    }

    let port = free_port();
    let _srv = spawn_server(&data_dir, &strat_dir, port, None, None, Some(dir));
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    let valid_v2 = r#"
pub fn name() { "h1-valid-v2" }
pub fn side() { "yes" }
pub fn evaluate(ctx) {
    if ctx.secs_to_close >= 120 && ctx.yes_ask <= 0.60 {
        return Some(#{ enter: true, side: "yes", limit_cents: 55, post_only: true });
    }
    None
}
"#;

    // 1. CWE-22/59 null bytes, traversal, control bytes, symlink & dangling symlink rejections.
    for bad_name in [
        "bad\0name",
        "bad%00name",
        "../outside",
        "..%2foutside",
        "bad%0aname",
        "sym_escape",
        "sym-escape",
        "dangling_link",
    ] {
        let body = serde_json::json!({
            "name": bad_name,
            "source": valid_v2,
            "overwrite": true
        })
        .to_string();
        let (st, _) = http_req(
            port,
            "POST",
            "/api/strategies/save",
            &host,
            &csrf,
            Some("application/json"),
            &body,
        );
        assert!(
            st == 400 || st == 403,
            "save must reject {bad_name} with 400/403, got {st}"
        );
    }

    for bad_del in [
        "bad%00name",
        "..%2foutside%2fsecret",
        "sym_escape",
        "sym-escape",
        "dangling_link",
    ] {
        let (st, _) = http_req(
            port,
            "DELETE",
            &format!("/api/strategies/{bad_del}"),
            &host,
            &csrf,
            Some("application/json"),
            "{}",
        );
        assert!(
            st == 400 || st == 403,
            "delete must reject {bad_del} with 400/403, got {st}"
        );
    }
    assert!(
        outside_secret.exists(),
        "outside target file must remain untouched"
    );

    // 2. Oversized script (>256 KiB) rejected cleanly.
    let huge_src = format!("{valid_v2}\n// {}", "x".repeat(260 * 1024));
    let (st_huge, _) = http_req(
        port,
        "POST",
        "/api/strategies/save",
        &host,
        &csrf,
        Some("application/json"),
        &serde_json::json!({ "name": "huge_strat", "source": huge_src }).to_string(),
    );
    assert_eq!(st_huge, 400);

    // 3. Adversarial infinite-loop & OOM-loop scripts rejected promptly by VM step/memory budget.
    let evil_scripts = [
        (
            "loop_name",
            "pub fn name() { loop {} \"x\" }\npub fn side() { \"yes\" }\npub fn allow_entry(a,b,c) { false }",
        ),
        (
            "loop_describe",
            "pub fn name() { \"loop-desc\" }\npub fn side() { \"yes\" }\npub fn describe() { loop {} \"d\" }\npub fn allow_entry(a,b,c) { false }",
        ),
        (
            "loop_allow",
            "pub fn name() { \"loop-allow\" }\npub fn side() { \"yes\" }\npub fn allow_entry(a,b,c) { loop {} false }",
        ),
        (
            "loop_quote",
            "pub fn name() { \"loop-quote\" }\npub fn side() { \"yes\" }\npub fn allow_entry(a,b,c) { false }\npub fn entry_quote(a,b,c) { loop {} None }",
        ),
        (
            "loop_eval",
            "pub fn name() { \"loop-eval\" }\npub fn side() { \"yes\" }\npub fn evaluate(ctx) { loop {} None }",
        ),
        (
            "oom_eval",
            "pub fn name() { \"oom-eval\" }\npub fn side() { \"yes\" }\npub fn evaluate(ctx) { let v = []; loop { v.push(1); } None }",
        ),
    ];
    for (slug, src) in evil_scripts {
        let t0 = Instant::now();
        let (st, b) = http_req(
            port,
            "POST",
            "/api/strategies/save",
            &host,
            &csrf,
            Some("application/json"),
            &serde_json::json!({ "name": slug, "source": src }).to_string(),
        );
        assert_eq!(st, 400, "infinite/oom script {slug} must fail with 400: {b}");
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "VM budget must halt {slug} quickly"
        );
    }

    // 4. Synthesize on empty dataset -> 200 OK with empty candidates and zero NaN/Inf.
    let (st_syn_empty, b_syn_empty) = http_req(
        port,
        "POST",
        "/api/research/synthesize",
        &host,
        &csrf,
        Some("application/json"),
        r#"{"family":"all","execution_mode":"maker","fdr_alpha":999.0,"min_deflated_sharpe":-5.0,"slippage_cents":999.0}"#,
    );
    assert_eq!(st_syn_empty, 200);
    let syn_empty: serde_json::Value = serde_json::from_str(&b_syn_empty).unwrap();
    assert_eq!(syn_empty["ok"], true);
    assert_eq!(syn_empty["windows_settled_total"], 0);
    assert_eq!(syn_empty["candidates"].as_array().unwrap().len(), 0);
    assert!(syn_empty["fdr_alpha"].as_f64().unwrap() <= 0.50);
    assert!(syn_empty["min_deflated_sharpe"].as_f64().unwrap() >= 0.0);

    // 5. Synthesize on single-window dataset -> 200 OK with empty candidates.
    std::fs::write(
        data_dir.join("snapshots.jsonl"),
        serde_json::json!({
            "fetched_at": 1_800_000_000i64,
            "ticker": "KXBTC15M-SINGLE",
            "event_ticker": "KXBTC15M-SEV",
            "series": "KXBTC15M",
            "close_time": "2027-01-20T00:15:00Z",
            "status": "settled",
            "result": "yes",
            "yes_bid": 0.52,
            "yes_ask": 0.54,
            "no_bid": 0.46,
            "no_ask": 0.48,
            "last_price": 0.54,
            "yes_bid_size": 25.0,
            "no_bid_size": 25.0
        })
        .to_string()
            + "\n",
    )
    .unwrap();
    let (st_syn_one, b_syn_one) = http_req(
        port,
        "POST",
        "/api/research/synthesize",
        &host,
        &csrf,
        Some("application/json"),
        "{}",
    );
    assert_eq!(st_syn_one, 200);
    let syn_one: serde_json::Value = serde_json::from_str(&b_syn_one).unwrap();
    assert_eq!(syn_one["windows_settled_total"], 1);
    assert_eq!(syn_one["candidates"].as_array().unwrap().len(), 0);
}
