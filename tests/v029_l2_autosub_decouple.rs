//! Integration tests for Cycle 3 Gap #1 (v0.29.0):
//! - Active-market L2 WebSocket auto-subscription (<=32 cap + UI pin preservation)
//! - Kalshi WS channels ["orderbook_delta", "ticker", "trade"]
//! - Sub-second ShadowRunner wakeup via notify_tick() + L2 matching & wall-clock pre-close cancel between REST snapshots.jsonl polls
//! - GET /api/orderbook/history & extended stream-status telemetry
//! - Fail-closed live_armed: false and RunnerMode != LiveAuto boot invariant

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

fn iso_from_unix(ts: i64) -> String {
    // Civil calendar conversion (Howard Hinnant algorithm) for UTC ISO-8601.
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
    let hh = secs / 3600;
    let mm = (secs % 3600) / 60;
    let ss = secs % 60;
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
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
    let mut hdr = Vec::with_capacity(10);
    hdr.push(0x81);
    if payload.len() < 126 {
        hdr.push(payload.len() as u8);
    } else if payload.len() <= u16::MAX as usize {
        hdr.push(126);
        hdr.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        hdr.push(127);
        hdr.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    stream.write_all(&hdr)?;
    stream.write_all(payload)?;
    stream.flush()
}

fn read_ws_client_frame(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut h = [0u8; 2];
    stream.read_exact(&mut h)?;
    let masked = (h[1] & 0x80) != 0;
    let mut len = (h[1] & 0x7f) as u64;
    if len == 126 {
        let mut ext = [0u8; 2];
        stream.read_exact(&mut ext)?;
        len = u16::from_be_bytes(ext) as u64;
    } else if len == 127 {
        let mut ext = [0u8; 8];
        stream.read_exact(&mut ext)?;
        len = u64::from_be_bytes(ext);
    }
    let mut mask = [0u8; 4];
    if masked {
        stream.read_exact(&mut mask)?;
    }
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf)?;
    if masked {
        for (i, b) in buf.iter_mut().enumerate() {
            *b ^= mask[i % 4];
        }
    }
    Ok(String::from_utf8_lossy(&buf).to_string())
}

fn http_req(
    port: u16,
    method: &str,
    path: &str,
    host: &str,
    headers: &[(&str, &str)],
    body: &str,
    content_type: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (k, v) in headers {
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
    stream.write_all(req.as_bytes()).unwrap();
    stream.flush().unwrap();
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).to_string();
    let (hdr_part, body_part) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    let status = hdr_part
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);
    (status, body_part.to_string())
}

fn spawn_server(
    data_dir: &Path,
    strat_dir: &Path,
    port: u16,
    ws_url: &str,
    rest_base: &str,
    series: &str,
    cleanup_dir: PathBuf,
) -> ServerGuard {
    let bin = env!("CARGO_BIN_EXE_openbell-rs");
    let child = Command::new(bin)
        .arg("--no-open")
        .env("PORT", port.to_string())
        .env("OPENBELL_DATA", data_dir)
        .env("OPENBELL_STRATEGIES", strat_dir)
        .env("OPENBELL_COLLECT_SECS", "3600")
        .env("OPENBELL_UPDATE_MODE", "off")
        .env("OPENBELL_NO_OPEN", "1")
        .env("OPENBELL_WS_URL", ws_url)
        .env("OPENBELL_API_BASE", rest_base)
        .env("OPENBELL_SERIES", series)
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
        std::thread::sleep(Duration::from_millis(30));
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
        std::thread::sleep(Duration::from_millis(35));
    }
    panic!("timed out waiting for condition");
}

#[test]
fn v029_autosub_channels_stream_status_and_history_security() {
    let dir = std::env::temp_dir().join(format!("ob-v029-autosub-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // 1. Mock REST server serving 35 open KXBTC15M markets + L2 orderbooks.
    let rest_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let rest_port = rest_listener.local_addr().unwrap().port();
    let base_close = now_unix() + 600;
    std::thread::spawn(move || {
        for mut s in rest_listener.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
            let mut reader = BufReader::new(&mut s);
            let mut req_line = String::new();
            let _ = reader.read_line(&mut req_line);
            loop {
                let mut hdr = String::new();
                if reader.read_line(&mut hdr).unwrap_or(0) == 0 || hdr == "\r\n" {
                    break;
                }
            }
            let path = req_line.split_whitespace().nth(1).unwrap_or("/");
            let body = if path.contains("/markets?") {
                if path.contains("status=open") {
                    let markets: Vec<serde_json::Value> = (1..=35)
                        .map(|i| {
                            serde_json::json!({
                                "ticker": format!("KXBTC15M-M{i:02}"),
                                "event_ticker": format!("KXBTC15M-EV{i:02}"),
                                "close_time": iso_from_unix(base_close + (i as i64) * 60),
                                "status": "open",
                                "result": "",
                                "yes_bid_dollars": "0.54",
                                "yes_ask_dollars": "0.58",
                                "no_bid_dollars": "0.42",
                                "no_ask_dollars": "0.46",
                                "last_price_dollars": "0.55",
                                "yes_bid_size_fp": "20",
                                "no_bid_size_fp": "15"
                            })
                        })
                        .collect();
                    serde_json::json!({ "markets": markets }).to_string()
                } else {
                    serde_json::json!({ "markets": [] }).to_string()
                }
            } else {
                serde_json::json!({
                    "orderbook": {
                        "yes": [[54, 20]],
                        "no": [[42, 15]]
                    }
                })
                .to_string()
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });

    // 2. Mock WS server recording subscribe payloads & pushing snapshot + delta + ticker + trade.
    let ws_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    let captured_subs: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let captured_subs_bg = captured_subs.clone();
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

            let sub_raw = read_ws_client_frame(&mut s).unwrap();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&sub_raw) {
                captured_subs_bg.lock().unwrap().push(v);
            }

            let snap1 = serde_json::json!({
                "type": "orderbook_snapshot",
                "seq": 1,
                "msg": {
                    "market_ticker": "KXBTC15M-M01",
                    "yes": [[55, 25], [53, 30]],
                    "no": [[42, 12], [40, 20]]
                }
            })
            .to_string();
            send_ws_server_text(&mut s, &snap1).unwrap();

            let delta2 = serde_json::json!({
                "type": "orderbook_delta",
                "seq": 2,
                "msg": {
                    "market_ticker": "KXBTC15M-M01",
                    "side": "yes",
                    "price": 55,
                    "delta": 5
                }
            })
            .to_string();
            send_ws_server_text(&mut s, &delta2).unwrap();

            let ticker_msg = serde_json::json!({
                "type": "ticker",
                "msg": {
                    "market_ticker": "KXBTC15M-M01",
                    "price": 56
                }
            })
            .to_string();
            send_ws_server_text(&mut s, &ticker_msg).unwrap();

            let trade_msg = serde_json::json!({
                "type": "trade",
                "msg": {
                    "market_ticker": "KXBTC15M-M01",
                    "yes_price": 57,
                    "count": 8
                }
            })
            .to_string();
            send_ws_server_text(&mut s, &trade_msg).unwrap();

            // Also send second snapshot so orderbooks.jsonl has multiple history entries.
            let snap2 = serde_json::json!({
                "type": "orderbook_snapshot",
                "seq": 3,
                "msg": {
                    "market_ticker": "KXBTC15M-M01",
                    "yes": [[56, 18], [53, 30]],
                    "no": [[41, 10], [40, 20]]
                }
            })
            .to_string();
            send_ws_server_text(&mut s, &snap2).unwrap();

            // Keep connection alive briefly to capture any subscription updates.
            let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(6) {
                if let Ok(next_cmd) = read_ws_client_frame(&mut s) {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&next_cmd) {
                        captured_subs_bg.lock().unwrap().push(v);
                    }
                }
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
        &ws_url,
        &rest_base,
        "KXBTC15M",
        dir.clone(),
    );
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // 3. Boot invariants: ALWAYS live_armed: false and RunnerMode != LiveAuto.
    let (st_home, b_home) = http_req(port, "GET", "/api/home", &host, &[], "", None);
    assert_eq!(st_home, 200);
    let home: serde_json::Value = serde_json::from_str(&b_home).unwrap();
    assert_eq!(home["live"]["live_armed"], false);

    let (st_run, b_run) = http_req(port, "GET", "/api/runner/status", &host, &[], "", None);
    assert_eq!(st_run, 200);
    let run_v: serde_json::Value = serde_json::from_str(&b_run).unwrap();
    assert_eq!(run_v["live_armed"], false);
    assert_ne!(run_v["mode"], "live_auto");

    // 4. Wait for startup discovery auto-subscription (<=32 cap), WS channels, trade telemetry & active_l2_tickers.
    let ss = wait_until(Duration::from_secs(8), || {
        let (code, body) = http_req(
            port,
            "GET",
            "/api/orderbook/stream-status",
            &host,
            &[],
            "",
            None,
        );
        if code != 200 {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(&body).ok()?;
        let subs = v["subscribed_tickers"].as_array()?;
        let active_l2 = v["active_l2_tickers"].as_array()?;
        if subs.len() == 32
            && v["auto_subscribed_count"] == 32
            && v["last_trade_ms"].as_i64().is_some()
            && active_l2.iter().any(|t| t == "KXBTC15M-M01")
        {
            Some(v)
        } else {
            None
        }
    });
    assert_eq!(ss["subscribed_tickers"].as_array().unwrap().len(), 32);
    assert_eq!(ss["auto_subscribed_count"], 32);
    assert!(ss["tape_sync_rate_hz"].as_f64().unwrap_or(0.0) > 0.0);

    // Verify WS subscribe command used ["orderbook_delta", "ticker", "trade"].
    let subs_seen = captured_subs.lock().unwrap().clone();
    assert!(!subs_seen.is_empty(), "expected WS subscribe command");
    assert_eq!(
        subs_seen[0]["params"]["channels"],
        serde_json::json!(["orderbook_delta", "ticker", "trade"])
    );

    // 5. Pin a UI ticker and trigger POST /api/collect -> pin must be preserved alongside 31 active markets (total 32).
    let (st_pin, b_pin) = http_req(
        port,
        "POST",
        "/api/orderbook/subscribe",
        &host,
        &csrf,
        r#"{"ticker":"KXETH15M-UIPIN"}"#,
        Some("application/json"),
    );
    assert_eq!(st_pin, 200, "pin failed: {b_pin}");

    let (st_col, b_col) = http_req(
        port,
        "POST",
        "/api/collect",
        &host,
        &csrf,
        "{}",
        Some("application/json"),
    );
    assert_eq!(st_col, 200, "collect failed: {b_col}");

    let (_, b_ss2) = http_req(
        port,
        "GET",
        "/api/orderbook/stream-status",
        &host,
        &[],
        "",
        None,
    );
    let ss2: serde_json::Value = serde_json::from_str(&b_ss2).unwrap();
    let subs2 = ss2["subscribed_tickers"].as_array().unwrap();
    assert_eq!(subs2.len(), 32);
    assert_eq!(ss2["auto_subscribed_count"], 31);
    assert!(
        subs2.iter().any(|t| t == "KXETH15M-UIPIN"),
        "UI-pinned ticker must be preserved across sync_active_markets"
    );
    assert!(
        subs2.iter().any(|t| t == "KXBTC15M-M01"),
        "highest-priority open market must remain subscribed"
    );

    // 6. Verify GET /api/orderbook/history?ticker=KXBTC15M-M01&limit=10 and input validation.
    let (st_hist, b_hist) = http_req(
        port,
        "GET",
        "/api/orderbook/history?ticker=KXBTC15M-M01&limit=10",
        &host,
        &[],
        "",
        None,
    );
    assert_eq!(st_hist, 200, "history failed: {b_hist}");
    let hist_v: serde_json::Value = serde_json::from_str(&b_hist).unwrap();
    assert_eq!(hist_v["ok"], true);
    assert_eq!(hist_v["ticker"], "KXBTC15M-M01");
    let recs = hist_v["records"].as_array().unwrap();
    assert!(recs.len() >= 2, "expected >=2 persisted orderbooks.jsonl records, got {recs:?}");
    assert_eq!(recs.last().unwrap()["best_yes_bid"], 56);
    assert_eq!(recs.last().unwrap()["best_yes_ask"], 59);

    let (st_bad, _) = http_req(
        port,
        "GET",
        "/api/orderbook/history?ticker=../etc/passwd&limit=5",
        &host,
        &[],
        "",
        None,
    );
    assert_eq!(st_bad, 400);
}

#[test]
fn v029_runner_wakeup_l2_fill_and_preclose_cancel_between_rest_polls() {
    let dir = std::env::temp_dir().join(format!("ob-v029-decouple-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // Strategy that posts a resting maker bid at 57c whenever yes_ask == 60c and secs_to_close >= 8.
    std::fs::write(
        strat_dir.join("maker_decouple.rn"),
        r#"
pub fn name() { "maker-decouple" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { secs_to_close >= 8 }
pub fn entry_quote(yes_ask, no_ask, secs_to_close) { Some(0.57) }
"#,
    )
    .unwrap();

    // Mock REST server returns no open markets so only our two seeded snapshots exist,
    // and returns a wide non-crossing L2 book for REST fallback if called.
    let rest_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let rest_port = rest_listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in rest_listener.incoming().flatten() {
            let _ = s.set_read_timeout(Some(Duration::from_secs(3)));
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let req = String::from_utf8_lossy(&buf);
            let body = if req.contains("/markets?") {
                serde_json::json!({ "markets": [] }).to_string()
            } else {
                serde_json::json!({
                    "orderbook": {
                        "yes": [[55, 20]],
                        "no": [[40, 20]]
                    }
                })
                .to_string()
            };
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });

    // Mock WS server controlled by test stages:
    // Stage 1: pushes crossing/trade L2 update for KXBTC15M-L2FILL (waking runner immediately!).
    // Stage 2: pushes another L2 delta after wall clock advances past cancel_before_close_secs on KXBTC15M-PRECLOSE.
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

            // Initial non-crossing L2 snapshot for KXBTC15M-L2FILL (yes_bid=55c, yes_ask=60c).
            let init_snap = serde_json::json!({
                "type": "orderbook_snapshot",
                "seq": 1,
                "msg": {
                    "market_ticker": "KXBTC15M-L2FILL",
                    "yes": [[55, 20]],
                    "no": [[40, 20]]
                }
            })
            .to_string();
            let _ = send_ws_server_text(&mut s, &init_snap);

            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(15) {
                let cur = *stage_bg.lock().unwrap();
                if cur == 1 {
                    // Wait >260ms from initial snapshot so <=4Hz throttle permits immediate wakeup.
                    std::thread::sleep(Duration::from_millis(280));
                    // Push NO bid at 44c -> YES ask = 56c < 57c (crosses resting 57c maker bid) + trade at 56c!
                    let cross_delta = serde_json::json!({
                        "type": "orderbook_delta",
                        "seq": 2,
                        "msg": {
                            "market_ticker": "KXBTC15M-L2FILL",
                            "side": "no",
                            "price": 44,
                            "delta": 10
                        }
                    })
                    .to_string();
                    let _ = send_ws_server_text(&mut s, &cross_delta);
                    let trade = serde_json::json!({
                        "type": "trade",
                        "msg": {
                            "market_ticker": "KXBTC15M-L2FILL",
                            "yes_price": 56,
                            "count": 10
                        }
                    })
                    .to_string();
                    let _ = send_ws_server_text(&mut s, &trade);
                    *stage_bg.lock().unwrap() = 2;
                } else if cur == 3 {
                    // Wake runner immediately via a WS delta on L2FILL so pre-close cancel is checked instantly.
                    std::thread::sleep(Duration::from_millis(280));
                    let wake_delta = serde_json::json!({
                        "type": "orderbook_delta",
                        "seq": 3,
                        "msg": {
                            "market_ticker": "KXBTC15M-L2FILL",
                            "side": "yes",
                            "price": 55,
                            "delta": 1
                        }
                    })
                    .to_string();
                    let _ = send_ws_server_text(&mut s, &wake_delta);
                    *stage_bg.lock().unwrap() = 4;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
    });

    // Seed snapshots.jsonl with ONE snapshot per market and NEVER append again.
    let t0 = now_unix();
    let snap_fill = serde_json::json!({
        "fetched_at": t0,
        "ticker": "KXBTC15M-L2FILL",
        "event_ticker": "KXBTC15M-EVF",
        "series": "KXBTC15M",
        "close_time": iso_from_unix(t0 + 300),
        "status": "open",
        "result": "",
        "yes_bid": 0.55,
        "yes_ask": 0.60,
        "no_bid": 0.40,
        "no_ask": 0.45,
        "last_price": 0.58,
        "yes_bid_size": 20.0,
        "no_bid_size": 20.0
    });
    let snap_preclose = serde_json::json!({
        "fetched_at": t0,
        "ticker": "KXBTC15M-PRECLOSE",
        "event_ticker": "KXBTC15M-EVP",
        "series": "KXBTC15M",
        "close_time": iso_from_unix(t0 + 11),
        "status": "open",
        "result": "",
        "yes_bid": 0.55,
        "yes_ask": 0.60,
        "no_bid": 0.40,
        "no_ask": 0.45,
        "last_price": 0.58,
        "yes_bid_size": 20.0,
        "no_bid_size": 20.0
    });
    std::fs::write(
        data_dir.join("snapshots.jsonl"),
        format!("{snap_fill}\n{snap_preclose}\n"),
    )
    .unwrap();

    let port = free_port();
    let ws_url = format!("ws://127.0.0.1:{ws_port}/trade-api/ws/v2");
    let rest_base = format!("http://127.0.0.1:{rest_port}/trade-api/v2");
    let _srv = spawn_server(
        &data_dir,
        &strat_dir,
        port,
        &ws_url,
        &rest_base,
        "KXBTC15M",
        dir.clone(),
    );
    let host = format!("127.0.0.1:{port}");
    let csrf = [("X-OpenBell-CSRF", "1")];

    // Configure ShadowRunner with a LONG 5000ms tick_interval_ms to prove notify_tick() wakes it sub-second!
    let cfg_body = serde_json::json!({
        "slots": [{
            "slot_id": 1,
            "strategy": "maker-decouple",
            "series": ["KXBTC15M"],
            "max_contracts": 5,
            "execution_mode": "maker",
            "auto_pause_drawdown_dollars": 50.0,
            "enabled": true
        }],
        "cancel_before_close_secs": 8,
        "bankroll_dollars": 500.0,
        "tick_interval_ms": 5000
    })
    .to_string();
    let (st_cfg, b_cfg) = http_req(
        port,
        "POST",
        "/api/runner/config",
        &host,
        &csrf,
        &cfg_body,
        Some("application/json"),
    );
    assert_eq!(st_cfg, 200, "config failed: {b_cfg}");

    let (st_start, b_start) = http_req(
        port,
        "POST",
        "/api/runner/start",
        &host,
        &csrf,
        r#"{"mode":"shadow"}"#,
        Some("application/json"),
    );
    assert_eq!(st_start, 200, "start failed: {b_start}");

    // Wait for both resting maker quotes to post from the initial snapshot.
    let start_wait = Instant::now();
    loop {
        let (code, b) = http_req(port, "GET", "/api/runner/status", &host, &[], "", None);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&b) {
            if let Some(oq) = v["open_quotes"].as_array() {
                if oq.iter().any(|q| q["ticker"] == "KXBTC15M-L2FILL")
                    && oq.iter().any(|q| q["ticker"] == "KXBTC15M-PRECLOSE")
                {
                    break;
                }
            }
        }
        if start_wait.elapsed() >= Duration::from_secs(4) {
            let (_, sb) = http_req(port, "GET", "/api/strategies", &host, &[], "", None);
            let (_, lb) = http_req(port, "GET", "/api/runner/ledger?limit=10", &host, &[], "", None);
            let (_, ob) = http_req(port, "GET", "/api/orderbook?ticker=KXBTC15M-L2FILL", &host, &[], "", None);
            panic!("timed out waiting for open_quotes: code={code} body={b} strategies={sb} ledger={lb} ob={ob}");
        }
        std::thread::sleep(Duration::from_millis(35));
    }

    // Pin KXBTC15M-L2FILL so WS delivers initial snapshot; wait until book is fresh.
    let (st_sub, _) = http_req(
        port,
        "POST",
        "/api/orderbook/subscribe",
        &host,
        &csrf,
        r#"{"ticker":"KXBTC15M-L2FILL"}"#,
        Some("application/json"),
    );
    assert_eq!(st_sub, 200);
    wait_until(Duration::from_secs(4), || {
        let (_, b) = http_req(
            port,
            "GET",
            "/api/orderbook?ticker=KXBTC15M-L2FILL",
            &host,
            &[],
            "",
            None,
        );
        let v: serde_json::Value = serde_json::from_str(&b).ok()?;
        if v["source"] == "ws" && v["last_seq"] == 1 {
            Some(v)
        } else {
            None
        }
    });

    // Stage 1: Trigger crossing L2 delta + trade WITHOUT touching snapshots.jsonl.
    // Even with tick_interval_ms = 5000, notify_tick() must wake runner and fill in < 2.0s!
    let trigger_at = Instant::now();
    *stage.lock().unwrap() = 1;
    let filled_status = wait_until(Duration::from_millis(2_000), || {
        let (_, b) = http_req(port, "GET", "/api/runner/status", &host, &[], "", None);
        let v: serde_json::Value = serde_json::from_str(&b).ok()?;
        if v["stats"]["fills"].as_u64().unwrap_or(0) >= 1
            && !v["open_quotes"]
                .as_array()?
                .iter()
                .any(|q| q["ticker"] == "KXBTC15M-L2FILL")
        {
            Some(v)
        } else {
            None
        }
    });
    assert!(
        trigger_at.elapsed() < Duration::from_millis(2_000),
        "expected sub-2s wakeup fill on 5000ms tick interval"
    );
    assert_eq!(filled_status["stats"]["fills"], 1);

    // Stage 2: Wait until wall-clock reaches (t0 + 11) - 8 = t0 + 3s, then trigger notify_tick() via WS
    // and assert KXBTC15M-PRECLOSE is canceled immediately with zero new snapshots.jsonl rows!
    while now_unix() < (t0 + 4) {
        std::thread::sleep(Duration::from_millis(100));
    }
    *stage.lock().unwrap() = 3;
    wait_until(Duration::from_millis(2_500), || {
        let (_, b) = http_req(port, "GET", "/api/runner/status", &host, &[], "", None);
        let v: serde_json::Value = serde_json::from_str(&b).ok()?;
        if !v["open_quotes"]
            .as_array()?
            .iter()
            .any(|q| q["ticker"] == "KXBTC15M-PRECLOSE")
        {
            Some(v)
        } else {
            None
        }
    });

    let (_, b_led) = http_req(
        port,
        "GET",
        "/api/runner/ledger?limit=20",
        &host,
        &[],
        "",
        None,
    );
    let led: serde_json::Value = serde_json::from_str(&b_led).unwrap();
    let entries = led["entries"].as_array().unwrap();
    assert!(
        entries
            .iter()
            .any(|e| e["event"] == "OrderFilled" && e["ticker"] == "KXBTC15M-L2FILL"),
        "expected OrderFilled on KXBTC15M-L2FILL without new REST snapshots"
    );
    assert!(
        entries.iter().any(|e| e["event"] == "QuoteCanceled"
            && e["ticker"] == "KXBTC15M-PRECLOSE"
            && e["reason"] == "cancel_before_close"),
        "expected QuoteCanceled cancel_before_close on KXBTC15M-PRECLOSE via wall clock"
    );
}
