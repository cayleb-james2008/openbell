//! End-to-end integration tests for OpenBell GAP #2:
//! Live L2 Orderbook Stream (RFC 6455 WS + REST L2 fallback), Book-Walking VWAP & DOM API.
//!
//! Uses strictly local loopback mock WS/REST servers and temp `OPENBELL_DATA` —
//! ZERO real Kalshi network calls and ZERO modifications to user data dirs.

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

fn send_ws_server_text(stream: &mut TcpStream, text: &str) -> std::io::Result<()> {
    let payload = text.as_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x81); // FIN + text
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
    let len7 = (hdr[1] & 0x7f) as usize;
    let len = if len7 == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext)?;
        u16::from_be_bytes(ext) as usize
    } else if len7 == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext)?;
        u64::from_be_bytes(ext) as usize
    } else {
        len7
    };
    let mut mask = [0u8; 4];
    if masked {
        stream.read_exact(&mut mask)?;
    }
    let mut payload = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut payload)?;
        if masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= mask[i & 3];
            }
        }
    }
    Ok(String::from_utf8_lossy(&payload).to_string())
}

fn http_req(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    extra_headers: &[(&str, &str)],
    body: &str,
    content_type: Option<&str>,
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect server");
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
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

fn spawn_server(
    data_dir: &Path,
    strat_dir: &Path,
    port: u16,
    ws_url: &str,
    rest_base: &str,
    cleanup_dir: PathBuf,
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
        .env("OPENBELL_WS_URL", ws_url)
        .env("OPENBELL_API_BASE", rest_base)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn openbell-rs");
    let guard = ServerGuard {
        child,
        dir: Some(cleanup_dir),
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
    panic!("timed out waiting for orderbook condition");
}

#[test]
fn orderbook_l2_ws_delta_resync_rest_fallback_sse_and_security() {
    let dir = std::env::temp_dir().join(format!("ob-v026-l2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // 1. Spawn mock REST L2 server (handles resync & WS-drop fallback).
    let rest_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let rest_port = rest_listener.local_addr().unwrap().port();
    let rest_hits = Arc::new(Mutex::new(0usize));
    let rest_hits_bg = rest_hits.clone();
    std::thread::spawn(move || {
        for mut s in rest_listener.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            *rest_hits_bg.lock().unwrap() += 1;
            let body = serde_json::json!({
                "orderbook": {
                    "yes": [[56, 30], [54, 50]],
                    "no": [[41, 12], [39, 20]]
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

    // 2. Spawn mock RFC 6455 WebSocket server (accepts 1st connection, sends snapshot + delta + gap, then drops so REST fallback engages).
    let ws_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    let drop_ws = Arc::new(Mutex::new(false));
    let drop_ws_bg = drop_ws.clone();
    let send_gap = Arc::new(Mutex::new(false));
    let send_gap_bg = send_gap.clone();
    std::thread::spawn(move || {
        let mut first = true;
        for mut s in ws_listener.incoming().flatten() {
            if !first {
                // Reject subsequent WS reconnects so server stays in REST fallback mode for test verification.
                let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\n\r\n");
                continue;
            }
            first = false;
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
                .expect("WS Sec-WebSocket-Key header");
            let accept = b64_encode(&sha1_digest(
                format!("{ws_key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
            ));
            let upgrade = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            s.write_all(upgrade.as_bytes()).unwrap();
            s.flush().unwrap();

            // Read subscribe command.
            let sub_cmd = read_ws_client_frame(&mut s).unwrap();
            assert!(sub_cmd.contains("KXBTC15M-OB1"), "expected subscribe for KXBTC15M-OB1, got {sub_cmd}");

            // Push snapshot (seq=1) -> yes_bids [[55,20],[53,40]], no_bids [[42,5],[40,10]] (yes_asks [[58,5],[60,10]]).
            let snap_msg = serde_json::json!({
                "type": "orderbook_snapshot",
                "seq": 1,
                "msg": {
                    "market_ticker": "KXBTC15M-OB1",
                    "yes": [[55, 20], [53, 40]],
                    "no": [[42, 5], [40, 10]]
                }
            })
            .to_string();
            send_ws_server_text(&mut s, &snap_msg).unwrap();

            // Wait >260ms so SSE 4Hz throttle allows delta frame emission too, or send delta seq=2 immediately.
            std::thread::sleep(Duration::from_millis(280));
            let delta_msg = serde_json::json!({
                "type": "orderbook_delta",
                "seq": 2,
                "msg": {
                    "market_ticker": "KXBTC15M-OB1",
                    "side": "no",
                    "price": 42,
                    "delta": 5
                }
            })
            .to_string();
            send_ws_server_text(&mut s, &delta_msg).unwrap();

            // Wait for test to trigger sequence gap, then drop WS.
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(10) {
                if *send_gap_bg.lock().unwrap() {
                    let gap_msg = serde_json::json!({
                        "type": "orderbook_delta",
                        "seq": 5,
                        "msg": {
                            "market_ticker": "KXBTC15M-OB1",
                            "side": "yes",
                            "price": 55,
                            "delta": 1
                        }
                    })
                    .to_string();
                    let _ = send_ws_server_text(&mut s, &gap_msg);
                    *send_gap_bg.lock().unwrap() = false;
                }
                if *drop_ws_bg.lock().unwrap() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        }
    });

    let port = free_port();
    let ws_url = format!("ws://127.0.0.1:{ws_port}/trade-api/ws/v2");
    let rest_base = format!("http://127.0.0.1:{rest_port}/trade-api/v2");
    let _srv = spawn_server(&data_dir, &strat_dir, port, &ws_url, &rest_base, dir.clone());
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // 3. Check /api/home includes orderbook_l2: true and live_armed: false.
    let (st_home, b_home) = http_req(port, "GET", "/api/home", &host, &[], "", None);
    assert_eq!(st_home, 200);
    let home: serde_json::Value = serde_json::from_str(&b_home).unwrap();
    assert_eq!(home["orderbook_l2"], true);
    assert_eq!(home["live"]["live_armed"], false);
    assert!(home["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "orderbook_l2"));

    // 4. Empty book + invalid ticker checks.
    let (st_empty, b_empty) = http_req(
        port,
        "GET",
        "/api/orderbook?ticker=KXBTC15M-EMPTY",
        &host,
        &[],
        "",
        None,
    );
    assert_eq!(st_empty, 200);
    let empty_v: serde_json::Value = serde_json::from_str(&b_empty).unwrap();
    assert_eq!(empty_v["source"], "empty");
    assert_eq!(empty_v["stale"], true);
    assert!(empty_v["yes_bids"].as_array().unwrap().is_empty());

    let (st_bad_q, _) = http_req(
        port,
        "GET",
        "/api/orderbook?ticker=bad/ticker",
        &host,
        &[],
        "",
        None,
    );
    assert_eq!(st_bad_q, 400);

    // 5. Subscribe security checks: non-loopback 403, missing CSRF 403, bad ticker 400, >16 tickers 400.
    let (st_lan, _) = http_req(
        port,
        "POST",
        "/api/orderbook/subscribe",
        "desk.lan:8472",
        &csrf,
        r#"{"ticker":"KXBTC15M-OB1"}"#,
        Some("application/json"),
    );
    assert_eq!(st_lan, 403);

    let (st_nocsrf, _) = http_req(
        port,
        "POST",
        "/api/orderbook/subscribe",
        &host,
        &[],
        r#"{"ticker":"KXBTC15M-OB1"}"#,
        Some("application/json"),
    );
    assert_eq!(st_nocsrf, 403);

    let (st_bad_sub, _) = http_req(
        port,
        "POST",
        "/api/orderbook/subscribe",
        &host,
        &csrf,
        r#"{"ticker":"bad/ticker"}"#,
        Some("application/json"),
    );
    assert_eq!(st_bad_sub, 400);

    let too_many: Vec<String> = (1..=17).map(|i| format!("KXBTC15M-M{i}")).collect();
    let (st_17, _) = http_req(
        port,
        "POST",
        "/api/orderbook/subscribe",
        &host,
        &csrf,
        &serde_json::json!({ "tickers": too_many }).to_string(),
        Some("application/json"),
    );
    assert_eq!(st_17, 400);

    // 6. Connect SSE client to /api/stream (verifying /api/stream alias & event: orderbook).
    let mut sse_stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    sse_stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    sse_stream
        .write_all(
            format!("GET /api/stream HTTP/1.1\r\nHost: {host}\r\nAccept: text/event-stream\r\n\r\n")
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

    // 7. Subscribe to KXBTC15M-OB1 -> triggers WS handshake, snapshot, delta, and SSE orderbook frame!
    let (st_sub, b_sub) = http_req(
        port,
        "POST",
        "/api/orderbook/subscribe",
        &host,
        &csrf,
        r#"{"ticker":"KXBTC15M-OB1"}"#,
        Some("application/json"),
    );
    assert_eq!(st_sub, 200, "subscribe failed: {b_sub}");

    let mut ob_sse_seen = false;
    for _ in 0..8 {
        let (ev, data) = read_sse_frame(&mut sse_reader);
        if ev == "orderbook" {
            let v: serde_json::Value = serde_json::from_str(&data).unwrap();
            if v["ticker"] == "KXBTC15M-OB1" {
                ob_sse_seen = true;
                break;
            }
        }
    }
    assert!(ob_sse_seen, "expected event: orderbook SSE frame on /api/stream");

    // 8. Verify GET /api/orderbook & GET /api/orderbook/stream-status reflect WS snapshot + seq 2 delta.
    let ob_v = wait_until(Duration::from_secs(5), || {
        let (code, body) = http_req(
            port,
            "GET",
            "/api/orderbook?ticker=KXBTC15M-OB1",
            &host,
            &[],
            "",
            None,
        );
        if code != 200 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        if v["last_seq"] == 2 {
            Some(v)
        } else {
            None
        }
    });
    assert_eq!(ob_v["source"], "ws");
    assert_eq!(ob_v["stale"], false);
    assert_eq!(ob_v["best_yes_bid"], 55);
    assert_eq!(ob_v["best_yes_ask"], 58);
    assert_eq!(ob_v["spread_cents"], 3);
    assert!((ob_v["mid_cents"].as_f64().unwrap() - 56.5).abs() < 1e-9);
    // After seq=2 delta (+5 on NO 42c), YES ask 58c has 10 contracts so vwap_10 == 58.0!
    assert!((ob_v["vwap_10"].as_f64().unwrap() - 58.0).abs() < 1e-9);
    assert!(!ob_v["queue_by_level"].as_array().unwrap().is_empty());

    let (st_ss, b_ss) = http_req(
        port,
        "GET",
        "/api/orderbook/stream-status",
        &host,
        &[],
        "",
        None,
    );
    assert_eq!(st_ss, 200);
    let ss: serde_json::Value = serde_json::from_str(&b_ss).unwrap();
    assert_eq!(ss["ws_connected"], true);
    assert_eq!(ss["mode"], "ws");
    assert!(ss["delta_count"].as_u64().unwrap() >= 1);

    // 9. Trigger sequence gap -> resync_count increments & REST resync updates book -> then drop WS and verify REST fallback mode.
    *send_gap.lock().unwrap() = true;
    wait_until(Duration::from_secs(5), || {
        let (_, b) = http_req(
            port,
            "GET",
            "/api/orderbook/stream-status",
            &host,
            &[],
            "",
            None,
        );
        let v: serde_json::Value = serde_json::from_str(&b).ok()?;
        if v["resync_count"].as_u64().unwrap_or(0) >= 1 {
            Some(v)
        } else {
            None
        }
    });

    *drop_ws.lock().unwrap() = true;
    let rest_status = wait_until(Duration::from_secs(6), || {
        let (_, b) = http_req(
            port,
            "GET",
            "/api/orderbook/stream-status",
            &host,
            &[],
            "",
            None,
        );
        let v: serde_json::Value = serde_json::from_str(&b).ok()?;
        if v["ws_connected"] == false && v["mode"] == "rest" {
            Some(v)
        } else {
            None
        }
    });
    assert_eq!(rest_status["mode"], "rest");
    assert!(*rest_hits.lock().unwrap() >= 1);

    let (_, b_after_rest) = http_req(
        port,
        "GET",
        "/api/orderbook?ticker=KXBTC15M-OB1",
        &host,
        &[],
        "",
        None,
    );
    let ob_rest: serde_json::Value = serde_json::from_str(&b_after_rest).unwrap();
    assert_eq!(ob_rest["source"], "rest");
    assert_eq!(ob_rest["best_yes_bid"], 56);
    assert_eq!(ob_rest["best_yes_ask"], 59);
}
