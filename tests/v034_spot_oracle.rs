//! End-to-end integration tests for OpenBell v0.34.0 (Cycle 4 Wave E2):
//! Real-Time External Crypto Spot Price & Strike Oracle (`SpotFeedStore`,
//! `GET /api/spot/status`, `GET /api/spot/history`, SSE `event: spot`,
//! `Snapshot` strike/spot parsing + legacy JSONL compatibility, `TickContext`
//! spot/strike metrics + `0.0` defaults, loopback mock WS/REST ingestion,
//! 8 MiB rotation, corrupt-line tolerance, CWE-59 symlink rejection, and
//! `live_armed: false` + `runner_mode != live_auto` safety invariants).

use base64::Engine as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
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

fn iso_z_from_epoch(epoch: i64) -> String {
    let days = epoch.div_euclid(86_400);
    let secs = epoch.rem_euclid(86_400);
    let hh = secs / 3600;
    let mm = (secs % 3600) / 60;
    let ss = secs % 60;
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
    extra_headers: &[(&str, &str)],
    content_type: Option<&str>,
    body: &str,
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect server");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut req =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
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

fn spawn_server_with_env(
    data_dir: &Path,
    strat_dir: &Path,
    port: u16,
    cleanup_dir: Option<PathBuf>,
    envs: &[(&str, &str)],
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
        .env("OPENBELL_ORDERBOOK_OFFLINE", "1")
        .env("OPENBELL_NO_OPEN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let child = cmd.spawn().expect("spawn openbell-rs");
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

fn sha1_digest(data: &[u8]) -> [u8; 20] {
    let mut h0: u32 = 0x6745_2301;
    let mut h1: u32 = 0xEFCD_AB89;
    let mut h2: u32 = 0x98BA_DCFE;
    let mut h3: u32 = 0x1032_5476;
    let mut h4: u32 = 0xC3D2_E1F0;
    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, b) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h0, h1, h2, h3, h4);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
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

fn send_ws_text_frame(stream: &mut TcpStream, txt: &str) -> std::io::Result<()> {
    let payload = txt.as_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x81);
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame)?;
    stream.flush()
}

fn assert_safety_invariants(port: u16) {
    let (st_live, body_live) = http_req(port, "GET", "/api/live/status", &[], None, "");
    assert_eq!(st_live, 200);
    let lv: serde_json::Value = serde_json::from_str(&body_live).unwrap();
    assert_eq!(lv["live_armed"], serde_json::json!(false));

    let (st_run, body_run) = http_req(port, "GET", "/api/runner/status", &[], None, "");
    assert_eq!(st_run, 200);
    let rv: serde_json::Value = serde_json::from_str(&body_run).unwrap();
    assert_ne!(
        rv["mode"].as_str().unwrap_or(""),
        "live_auto",
        "must never boot in live_auto"
    );
}

#[test]
fn v034_offline_default_and_safety_invariants() {
    let base = std::env::temp_dir().join(format!("ob-v034-offline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let data_dir = base.join("data");
    let strat_dir = base.join("strategies");
    let port = free_port();
    let _srv = spawn_server_with_env(
        &data_dir,
        &strat_dir,
        port,
        Some(base.clone()),
        &[("OPENBELL_SPOT_OFFLINE", "1")],
    );

    assert_safety_invariants(port);

    let (st, body) = http_req(port, "GET", "/api/spot/status", &[], None, "");
    assert_eq!(st, 200);
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["offline"], serde_json::json!(true));
    let assets = v["assets"].as_array().unwrap();
    assert_eq!(assets.len(), 5);
    let expected = ["BTC", "ETH", "SOL", "XRP", "DOGE"];
    for (idx, exp) in expected.iter().enumerate() {
        assert_eq!(assets[idx]["asset"], serde_json::json!(exp));
        assert!(assets[idx]["price"].is_null());
        assert_eq!(assets[idx]["stale"], serde_json::json!(true));
    }
    assert!(!body.contains("NaN"));
}

#[test]
fn v034_rest_ingestion_history_bounds_rotation_corrupt_and_symlink() {
    let base = std::env::temp_dir().join(format!("ob-v034-rest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let data_dir = base.join("data");
    let strat_dir = base.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();

    // Seed an >8 MiB spot_ticks.jsonl so the first ingested tick triggers 8 MiB rotation,
    // and include corrupt/NaN/negative lines to verify tolerant history parsing.
    let spot_path = data_dir.join("spot_ticks.jsonl");
    let pad_line = "{\"ts_ms\":1700000000000,\"asset\":\"ETH\",\"price\":3100.0,\"source\":\"seed\"}\n";
    let repeats = (8 * 1024 * 1024 / pad_line.len()) + 32;
    {
        let mut f = std::fs::File::create(&spot_path).unwrap();
        for _ in 0..repeats {
            f.write_all(pad_line.as_bytes()).unwrap();
        }
    }

    // Start loopback mock REST server for OPENBELL_SPOT_URL
    let mock_port = free_port();
    let mock_listener = TcpListener::bind(("127.0.0.1", mock_port)).unwrap();
    mock_listener.set_nonblocking(true).unwrap();
    let stop_mock = Arc::new(AtomicBool::new(false));
    let stop_flag = stop_mock.clone();
    let mock_handle = std::thread::spawn(move || {
        let mut step = 0u64;
        while !stop_flag.load(Ordering::Relaxed) {
            match mock_listener.accept() {
                Ok((mut conn, _)) => {
                    let mut buf = [0u8; 1024];
                    let _ = conn.read(&mut buf);
                    step += 1;
                    let btc_px = 95_000.0 + (step as f64) * 25.0;
                    let payload = serde_json::json!({
                        "prices": {
                            "BTC": btc_px,
                            "ETH": 3250.5,
                            "SOL": 192.25,
                            "XRP": 2.48,
                            "DOGE": 0.365
                        }
                    })
                    .to_string();
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        payload.len(),
                        payload
                    );
                    let _ = conn.write_all(resp.as_bytes());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });

    let spot_url = format!("http://127.0.0.1:{mock_port}/spot");
    let port = free_port();
    let _srv = spawn_server_with_env(
        &data_dir,
        &strat_dir,
        port,
        Some(base.clone()),
        &[("OPENBELL_SPOT_OFFLINE", "0"), ("OPENBELL_SPOT_URL", &spot_url)],
    );

    assert_safety_invariants(port);

    // Wait for mock REST ticks to be ingested and rotation to happen.
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut ready = false;
    while Instant::now() < deadline {
        let (st, body) = http_req(port, "GET", "/api/spot/status", &[], None, "");
        if st == 200 {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            if v["offline"] == serde_json::json!(false)
                && v["assets"][0]["price"].as_f64().is_some_and(|p| p >= 95_000.0)
                && data_dir.join("spot_ticks.jsonl.1").exists()
            {
                assert_eq!(v["assets"][0]["source"], serde_json::json!("rest"));
                assert_eq!(v["assets"][0]["stale"], serde_json::json!(false));
                ready = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(ready, "mock REST spot feed did not ingest and rotate in time");

    // Append corrupt and non-finite lines to rotated spot_ticks.jsonl, then query /api/spot/history
    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&spot_path)
            .unwrap();
        writeln!(f, "{{not valid json").unwrap();
        writeln!(
            f,
            "{{\"ts_ms\":1700000001000,\"asset\":\"BTC\",\"price\":null,\"source\":\"bad\"}}"
        )
        .unwrap();
        writeln!(
            f,
            "{{\"ts_ms\":1700000002000,\"asset\":\"BTC\",\"price\":-12.0,\"source\":\"bad\"}}"
        )
        .unwrap();
    }

    let (st_hist, body_hist) =
        http_req(port, "GET", "/api/spot/history?asset=BTC&limit=200", &[], None, "");
    assert_eq!(st_hist, 200);
    let hv: serde_json::Value = serde_json::from_str(&body_hist).unwrap();
    assert_eq!(hv["ok"], serde_json::json!(true));
    assert_eq!(hv["asset"], serde_json::json!("BTC"));
    let ticks = hv["ticks"].as_array().unwrap();
    assert!(!ticks.is_empty());
    for t in ticks {
        assert!(t["price"].as_f64().is_some_and(|p| p.is_finite() && p > 0.0));
    }
    assert!(!body_hist.contains("NaN"));

    // Query bounds checks
    let (st_bad_asset, _) =
        http_req(port, "GET", "/api/spot/history?asset=FOO&limit=10", &[], None, "");
    assert_eq!(st_bad_asset, 400);
    let (st_trav, _) = http_req(
        port,
        "GET",
        "/api/spot/history?asset=../etc&limit=10",
        &[],
        None,
        "",
    );
    assert_eq!(st_trav, 400);
    let (st_clamped, body_clamped) = http_req(
        port,
        "GET",
        "/api/spot/history?asset=BTC&limit=99999",
        &[],
        None,
        "",
    );
    assert_eq!(st_clamped, 200);
    let cv: serde_json::Value = serde_json::from_str(&body_clamped).unwrap();
    assert!(cv["ticks"].as_array().unwrap().len() <= 500);

    // CWE-59 symlink rejection on spot_ticks.jsonl
    #[cfg(unix)]
    {
        stop_mock.store(true, Ordering::Relaxed);
        let outside = base.join("outside_secret.txt");
        std::fs::write(&outside, "DO_NOT_OVERWRITE").unwrap();
        let _ = std::fs::remove_file(&spot_path);
        std::os::unix::fs::symlink(&outside, &spot_path).unwrap();
        let (st_sym, _) =
            http_req(port, "GET", "/api/spot/history?asset=BTC&limit=10", &[], None, "");
        assert_eq!(st_sym, 403, "symlink spot_ticks.jsonl must return 403");
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "DO_NOT_OVERWRITE");
    }

    stop_mock.store(true, Ordering::Relaxed);
    let _ = mock_handle.join();
}

#[test]
fn v034_ws_ingestion_sse_strike_parsing_and_shadow_tick_context() {
    let base = std::env::temp_dir().join(format!("ob-v034-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let data_dir = base.join("data");
    let strat_dir = base.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    let now = now_unix();
    let close_iso = iso_z_from_epoch(now + 600);

    // Seed legacy snapshots.jsonl WITHOUT floor_strike/cap_strike/spot_price to prove backward compat.
    let legacy_snap = serde_json::json!({
        "fetched_at": now - 120,
        "ticker": "KXBTC15M-LEGACY",
        "event_ticker": "KXBTC15M-LEGACY",
        "series": "KXBTC15M",
        "close_time": iso_z_from_epoch(now - 60),
        "status": "settled",
        "result": "yes",
        "yes_bid": 0.55,
        "yes_ask": 0.57,
        "no_bid": 0.43,
        "no_ask": 0.45,
        "last_price": 0.57
    });
    std::fs::write(data_dir.join("snapshots.jsonl"), format!("{legacy_snap}\n")).unwrap();

    // Seed a v2 strategy that relies on spot_price, strike_price, spot_distance_bps,
    // spot_delta_30s_bps, and spot_vol_bps when present, and also supports 0.0 defaults.
    let v2_spot_src = r#"
pub fn name() { "spot-oracle-v2" }
pub fn side() { "yes" }
pub fn describe() { "Reads live spot and strike fields from TickContext." }
pub fn evaluate(ctx) {
    let _v = ctx.spot_vol_bps + ctx.spot_delta_30s_bps;
    if ctx.spot_price > 0.0 && ctx.strike_price > 0.0 && ctx.spot_distance_bps > 10.0 && ctx.secs_to_close >= 60 {
        return Some(#{ enter: true, side: "yes", limit_cents: 58, post_only: true });
    }
    None
}
"#;
    std::fs::write(strat_dir.join("spot-oracle-v2.rn"), v2_spot_src).unwrap();

    // 1. Mock Kalshi REST /markets server returning floor_strike, cap_strike, and optional spot_price.
    let kalshi_port = free_port();
    let kalshi_listener = TcpListener::bind(("127.0.0.1", kalshi_port)).unwrap();
    kalshi_listener.set_nonblocking(true).unwrap();
    let stop_mocks = Arc::new(AtomicBool::new(false));
    let stop_k = stop_mocks.clone();
    let close_for_mock = close_iso.clone();
    let kalshi_handle = std::thread::spawn(move || {
        while !stop_k.load(Ordering::Relaxed) {
            match kalshi_listener.accept() {
                Ok((mut conn, _)) => {
                    let mut buf = [0u8; 2048];
                    let n = conn.read(&mut buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let body = if req.contains("series_ticker=KXBTC15M") && req.contains("status=open") {
                        serde_json::json!({
                            "markets": [{
                                "ticker": "KXBTC15M-ORACLE-LIVE",
                                "event_ticker": "KXBTC15M-ORACLE",
                                "close_time": close_for_mock,
                                "status": "open",
                                "result": "",
                                "yes_bid_dollars": "0.58",
                                "yes_ask_dollars": "0.60",
                                "no_bid_dollars": "0.40",
                                "no_ask_dollars": "0.42",
                                "last_price_dollars": "0.59",
                                "yes_bid_size_fp": "120",
                                "no_bid_size_fp": "90",
                                "floor_strike": 95000.0,
                                "cap_strike": 96000.0
                            }]
                        })
                        .to_string()
                    } else {
                        "{\"markets\":[]}".to_string()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = conn.write_all(resp.as_bytes());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });

    // 2. Mock external Spot WebSocket server speaking RFC 6455.
    let ws_port = free_port();
    let ws_listener = TcpListener::bind(("127.0.0.1", ws_port)).unwrap();
    ws_listener.set_nonblocking(true).unwrap();
    let stop_ws = stop_mocks.clone();
    let ws_handle = std::thread::spawn(move || {
        while !stop_ws.load(Ordering::Relaxed) {
            match ws_listener.accept() {
                Ok((mut stream, _)) => {
                    let mut req_buf = [0u8; 2048];
                    let n = stream.read(&mut req_buf).unwrap_or(0);
                    let req = String::from_utf8_lossy(&req_buf[..n]);
                    let key = req
                        .lines()
                        .find_map(|l| {
                            l.split_once(':').and_then(|(k, v)| {
                                if k.trim().eq_ignore_ascii_case("sec-websocket-key") {
                                    Some(v.trim().to_string())
                                } else {
                                    None
                                }
                            })
                        })
                        .unwrap_or_default();
                    let accept = base64::engine::general_purpose::STANDARD.encode(sha1_digest(
                        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
                    ));
                    let hs = format!(
                        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                    );
                    if stream.write_all(hs.as_bytes()).is_err() {
                        continue;
                    }
                    // Read client subscribe frame briefly, then push ticks.
                    let _ = stream.set_read_timeout(Some(Duration::from_millis(300)));
                    let mut sink = [0u8; 512];
                    let _ = stream.read(&mut sink);
                    let msgs = [
                        r#"{"type":"ticker","product_id":"BTC-USD","price":"95100.00"}"#,
                        r#"{"type":"ticker","product_id":"ETH-USD","price":"3300.50"}"#,
                        r#"{"type":"ticker","product_id":"BTC-USD","price":"95350.00"}"#,
                    ];
                    for m in &msgs {
                        let _ = send_ws_text_frame(&mut stream, m);
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    let mut idx = 0u64;
                    while !stop_ws.load(Ordering::Relaxed) {
                        idx += 1;
                        let px = 95_350.0 + (idx as f64) * 5.0;
                        let tick = format!(
                            "{{\"type\":\"ticker\",\"product_id\":\"BTC-USD\",\"price\":\"{px:.2}\"}}"
                        );
                        if send_ws_text_frame(&mut stream, &tick).is_err() {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(350));
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });

    let kalshi_base = format!("http://127.0.0.1:{kalshi_port}");
    let ws_url = format!("ws://127.0.0.1:{ws_port}/ws");
    let port = free_port();
    let _srv = spawn_server_with_env(
        &data_dir,
        &strat_dir,
        port,
        Some(base.clone()),
        &[
            ("OPENBELL_API_BASE", &kalshi_base),
            ("OPENBELL_SERIES", "KXBTC15M"),
            ("OPENBELL_COLLECT_PACE_MS", "0"),
            ("OPENBELL_SPOT_OFFLINE", "0"),
            ("OPENBELL_SPOT_URL", &ws_url),
        ],
    );

    assert_safety_invariants(port);

    // Connect SSE stream BEFORE waiting / sending more ticks to capture event: spot
    let mut sse = TcpStream::connect(("127.0.0.1", port)).unwrap();
    sse.set_read_timeout(Some(Duration::from_secs(6))).unwrap();
    sse.write_all(
        format!("GET /api/tape/stream HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes(),
    )
    .unwrap();
    let mut sse_reader = BufReader::new(sse);

    // Wait for WS spot ingestion
    let deadline = Instant::now() + Duration::from_secs(12);
    let mut ws_ok = false;
    while Instant::now() < deadline {
        let (st, body) = http_req(port, "GET", "/api/spot/status", &[], None, "");
        if st == 200 {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap();
            let btc = &v["assets"][0];
            if btc["source"] == serde_json::json!("ws")
                && btc["price"].as_f64().is_some_and(|p| p >= 95_300.0)
            {
                ws_ok = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(ws_ok, "WS spot feed did not ingest BTC ticks");

    // Verify SSE delivered at least initial snapshot (and check if spot frame was emitted)
    let mut sse_buf = String::new();
    let sse_deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < sse_deadline {
        let mut line = String::new();
        match sse_reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                sse_buf.push_str(&line);
                if sse_buf.contains("event: spot") {
                    break;
                }
            }
        }
    }
    assert!(sse_buf.contains("event: snapshot"));
    assert!(sse_buf.contains("event: spot"), "expected SSE event: spot frame, got: {sse_buf}");

    // Trigger POST /api/collect -> parses floor_strike=95000, cap_strike=96000 and enriches spot_price
    let (st_col, _) = http_req(
        port,
        "POST",
        "/api/collect",
        &[("X-OpenBell-CSRF", "1")],
        Some("application/json"),
        "{}",
    );
    assert_eq!(st_col, 200);

    let (st_mkts, body_mkts) = http_req(port, "GET", "/api/markets", &[], None, "");
    assert_eq!(st_mkts, 200);
    let mkts: Vec<serde_json::Value> = serde_json::from_str(&body_mkts).unwrap();
    let live_mkt = mkts
        .iter()
        .find(|m| m["ticker"] == serde_json::json!("KXBTC15M-ORACLE-LIVE"))
        .expect("collected market present");
    assert_eq!(live_mkt["floor_strike"], serde_json::json!(95000.0));
    assert_eq!(live_mkt["cap_strike"], serde_json::json!(96000.0));
    assert!(live_mkt["spot_price"].as_f64().is_some_and(|p| p >= 95_300.0));
    assert!(mkts
        .iter()
        .any(|m| m["ticker"] == serde_json::json!("KXBTC15M-LEGACY")));

    // Validate v2 spot strategy via HTTP API (checks 1-bar causal canary)
    let val_req = serde_json::json!({ "source": v2_spot_src }).to_string();
    let (st_val, body_val) = http_req(
        port,
        "POST",
        "/api/strategy/validate",
        &[],
        Some("application/json"),
        &val_req,
    );
    assert_eq!(st_val, 200);
    let vv: serde_json::Value = serde_json::from_str(&body_val).unwrap();
    assert_eq!(vv["valid"], serde_json::json!(true), "validate: {body_val}");
    assert_eq!(vv["paper_only_checked"], serde_json::json!(true), "paper_only: {body_val}");

    // Configure ShadowRunner slot with spot-oracle-v2 in shadow mode and confirm entry triggers on spot/strike distance
    let cfg_req = serde_json::json!({
        "cancel_before_close_secs": 30,
        "bankroll_dollars": 100.0,
        "tick_interval_ms": 80,
        "slots": [{
            "slot_id": 1,
            "strategy": "spot-oracle-v2",
            "series": ["KXBTC15M"],
            "execution_mode": "maker",
            "max_contracts": 5,
            "auto_pause_drawdown_dollars": 50.0,
            "enabled": true
        }]
    })
    .to_string();
    let (st_cfg, body_cfg) = http_req(
        port,
        "POST",
        "/api/runner/config",
        &[("X-OpenBell-CSRF", "1")],
        Some("application/json"),
        &cfg_req,
    );
    assert_eq!(st_cfg, 200, "runner config failed: {body_cfg}");

    let (st_start, body_start) = http_req(
        port,
        "POST",
        "/api/runner/start",
        &[("X-OpenBell-CSRF", "1")],
        Some("application/json"),
        r#"{"mode":"shadow"}"#,
    );
    assert_eq!(st_start, 200, "runner start failed: {body_start}");

    let run_deadline = Instant::now() + Duration::from_secs(10);
    let mut quoted = false;
    while Instant::now() < run_deadline {
        let (st_r, body_r) = http_req(port, "GET", "/api/runner/status", &[], None, "");
        if st_r == 200 {
            let rv: serde_json::Value = serde_json::from_str(&body_r).unwrap();
            assert_ne!(rv["mode"], serde_json::json!("live_auto"));
            if rv["open_quotes"]
                .as_array()
                .is_some_and(|q| q.iter().any(|o| o["ticker"] == "KXBTC15M-ORACLE-LIVE"))
            {
                quoted = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(quoted, "ShadowRunner did not quote on live spot/strike TickContext");
    assert_safety_invariants(port);

    stop_mocks.store(true, Ordering::Relaxed);
    let _ = kalshi_handle.join();
    let _ = ws_handle.join();
}

#[test]
fn v034_legacy_tape_tick_context_defaults_to_zero() {
    let base = std::env::temp_dir().join(format!("ob-v034-legacy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let data_dir = base.join("data");
    let strat_dir = base.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    let now = now_unix();
    let close_iso = iso_z_from_epoch(now - 60);
    let s1 = serde_json::json!({
        "fetched_at": now - 600,
        "ticker": "KXBTC15M-LEG-BT",
        "event_ticker": "KXBTC15M-LEG-BT",
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": "open",
        "result": "",
        "yes_bid": 0.50,
        "yes_ask": 0.52,
        "no_bid": 0.48,
        "no_ask": 0.50,
        "last_price": 0.51
    });
    let s2 = serde_json::json!({
        "fetched_at": now - 300,
        "ticker": "KXBTC15M-LEG-BT",
        "event_ticker": "KXBTC15M-LEG-BT",
        "series": "KXBTC15M",
        "close_time": close_iso,
        "status": "settled",
        "result": "yes",
        "yes_bid": 0.55,
        "yes_ask": 0.57,
        "no_bid": 0.43,
        "no_ask": 0.45,
        "last_price": 0.57
    });
    std::fs::write(data_dir.join("snapshots.jsonl"), format!("{s1}\n{s2}\n")).unwrap();

    let port = free_port();
    let _srv = spawn_server_with_env(
        &data_dir,
        &strat_dir,
        port,
        Some(base.clone()),
        &[("OPENBELL_SPOT_OFFLINE", "1")],
    );

    let legacy_v2_src = r#"
pub fn name() { "legacy-zero-spot-v2" }
pub fn side() { "yes" }
pub fn describe() { "Confirms spot fields default to 0.0 on legacy snapshots." }
pub fn evaluate(ctx) {
    if ctx.spot_price == 0.0 && ctx.strike_price == 0.0 && ctx.spot_distance_bps == 0.0
        && ctx.spot_delta_30s_bps == 0.0 && ctx.spot_vol_bps == 0.0 && ctx.secs_to_close >= 120 {
        return Some(#{ enter: true, side: "yes", limit_cents: 52, post_only: false });
    }
    None
}
"#;
    let req = serde_json::json!({
        "source": legacy_v2_src,
        "dataset": "live"
    })
    .to_string();
    let (st, body) = http_req(
        port,
        "POST",
        "/api/backtest",
        &[],
        Some("application/json"),
        &req,
    );
    assert_eq!(st, 200, "backtest failed: {body}");
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["score"]["windows_settled"], serde_json::json!(1), "backtest: {body}");
    assert_eq!(v["score"]["windows_traded"], serde_json::json!(1), "backtest: {body}");
}
