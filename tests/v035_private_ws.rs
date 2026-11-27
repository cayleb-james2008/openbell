//! End-to-end integration tests for OpenBell v0.35.0 (Cycle 4 Wave E3):
//! Authenticated Private Venue WebSocket Execution Stream (`src/private_ws.rs`),
//! signed RFC 6455 handshake (`/trade-api/ws/v2`), idempotent `live_executions.jsonl`
//! ingestion, `RiskRailsState` exposure updates, `ShadowRunner::reconcile_live_data`
//! live fill updates, `event: live_exec` SSE delivery, 60s reconciler backstop scaling,
//! immediate REST fallback on disconnect/sequence gap, offline fixture safety, and
//! fail-closed `live_armed: false` / `runner_mode != live_auto` boot invariants.

use base64::Engine as _;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
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

fn http_json(port: u16, method: &str, path: &str, csrf: bool, body: &str) -> (u16, String) {
    let hdrs: Vec<(&str, &str)> = if csrf {
        vec![("X-OpenBell-CSRF", "1")]
    } else {
        vec![]
    };
    let ct = if matches!(method, "GET" | "HEAD" | "DELETE") {
        None
    } else if !body.is_empty() || method == "POST" {
        Some("application/json")
    } else {
        None
    };
    http_req(port, method, path, &hdrs, ct, body)
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
        .env("OPENBELL_AUTO_COLLECT", "0")
        .env("OPENBELL_ORDERBOOK_OFFLINE", "1")
        .env("OPENBELL_SPOT_OFFLINE", "1")
        .env("OPENBELL_UPDATE_MODE", "off")
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
                format!(
                    "GET /api/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
                )
                .as_bytes(),
            );
            let mut buf = String::new();
            if s.read_to_string(&mut buf).is_ok() && buf.contains("200 OK") {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    guard
}

fn generate_ephemeral_rsa_pem(dir: &Path) -> String {
    let priv_path = dir.join("ephemeral_v35_priv.pem");
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

fn ws_accept_key(sec_key: &str) -> String {
    let combined = format!("{}258EAFA5-E914-47DA-95CA-C5AB0DC85B11", sec_key.trim());
    base64::engine::general_purpose::STANDARD.encode(sha1_digest(combined.as_bytes()))
}

fn read_masked_ws_text_frame(stream: &mut TcpStream) -> String {
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr).expect("read ws hdr");
    let masked = (hdr[1] & 0x80) != 0;
    assert!(masked, "client WS frames must be masked per RFC 6455");
    let len7 = hdr[1] & 0x7f;
    let len = match len7 {
        0..=125 => len7 as usize,
        126 => {
            let mut ext = [0u8; 2];
            stream.read_exact(&mut ext).unwrap();
            u16::from_be_bytes(ext) as usize
        }
        _ => {
            let mut ext = [0u8; 8];
            stream.read_exact(&mut ext).unwrap();
            u64::from_be_bytes(ext) as usize
        }
    };
    let mut mask = [0u8; 4];
    stream.read_exact(&mut mask).unwrap();
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload).unwrap();
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i & 3];
    }
    String::from_utf8(payload).expect("UTF-8 ws frame")
}

fn send_ws_frame(stream: &mut TcpStream, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push(0x80 | (opcode & 0x0f));
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

#[derive(Default)]
struct MockRestState {
    positions: Vec<serde_json::Value>,
    orders: Vec<serde_json::Value>,
    fills: Vec<serde_json::Value>,
    settlements: Vec<serde_json::Value>,
    next_id: usize,
    reconcile_calls: AtomicUsize,
}

fn spawn_mock_rest_kalshi() -> (u16, Arc<Mutex<MockRestState>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(MockRestState::default()));
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
            let path_no_query = path.split('?').next().unwrap_or(&path);
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
            let body_str = String::from_utf8_lossy(&body_bytes).to_string();

            let mut st = state_bg.lock().unwrap();
            let (status, resp_json) = match (method.as_str(), path_no_query) {
                ("GET", "/trade-api/v2/portfolio/positions") => (
                    200,
                    serde_json::json!({ "market_positions": st.positions }),
                ),
                ("GET", "/trade-api/v2/portfolio/orders") => {
                    (200, serde_json::json!({ "orders": st.orders }))
                }
                ("GET", "/trade-api/v2/portfolio/fills") => {
                    (200, serde_json::json!({ "fills": st.fills }))
                }
                ("GET", "/trade-api/v2/portfolio/settlements") => {
                    st.reconcile_calls.fetch_add(1, Ordering::SeqCst);
                    (200, serde_json::json!({ "settlements": st.settlements }))
                }
                ("POST", "/trade-api/v2/portfolio/orders") => {
                    let req_v: serde_json::Value =
                        serde_json::from_str(&body_str).unwrap_or_default();
                    st.next_id += 1;
                    let oid = format!("ord-v35-{}", st.next_id);
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
                        "created_time": "2026-11-26T12:00:00Z"
                    });
                    st.orders.push(ord.clone());
                    (201, serde_json::json!({ "order": ord }))
                }
                _ => (404, serde_json::json!({ "error": "not found" })),
            };
            drop(st);
            let out = resp_json.to_string();
            let http = format!(
                "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}",
                out.len()
            );
            let _ = s.write_all(http.as_bytes());
        }
    });

    (port, state)
}

enum WsMockCmd {
    SendText(String),
    Close,
}

struct WsHandshakeInfo {
    path: String,
    key_id: String,
    timestamp: String,
    signature_len: usize,
    channels: Vec<String>,
}

struct MockWsHandle {
    port: u16,
    tx: mpsc::Sender<WsMockCmd>,
    handshake: Arc<Mutex<Option<WsHandshakeInfo>>>,
    accept_count: Arc<AtomicUsize>,
}

fn spawn_mock_private_ws(accept_new: Arc<AtomicBool>) -> MockWsHandle {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel::<WsMockCmd>();
    let rx = Arc::new(Mutex::new(rx));
    let handshake_info = Arc::new(Mutex::new(None));
    let handshake_bg = handshake_info.clone();
    let accept_count = Arc::new(AtomicUsize::new(0));
    let count_bg = accept_count.clone();

    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            count_bg.fetch_add(1, Ordering::SeqCst);
            if !accept_new.load(Ordering::SeqCst) {
                continue;
            }
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let mut buf = Vec::with_capacity(2048);
            let mut tmp = [0u8; 1024];
            let hdr_end = loop {
                let n = stream.read(&mut tmp).unwrap_or(0);
                if n == 0 {
                    break None;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break Some(pos);
                }
            };
            let Some(pos) = hdr_end else {
                continue;
            };
            let hdr_str = String::from_utf8_lossy(&buf[..pos]).to_string();
            let mut lines = hdr_str.split("\r\n");
            let req_line = lines.next().unwrap_or("");
            let path = req_line
                .split_whitespace()
                .nth(1)
                .unwrap_or("")
                .to_string();
            let mut hdrs = HashMap::new();
            for l in lines {
                if let Some((k, v)) = l.split_once(':') {
                    hdrs.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                }
            }
            let sec_key = hdrs
                .get("sec-websocket-key")
                .cloned()
                .expect("Sec-WebSocket-Key required");
            let key_id = hdrs
                .get("kalshi-access-key")
                .cloned()
                .expect("KALSHI-ACCESS-KEY required");
            let timestamp = hdrs
                .get("kalshi-access-timestamp")
                .cloned()
                .expect("KALSHI-ACCESS-TIMESTAMP required");
            let sig_b64 = hdrs
                .get("kalshi-access-signature")
                .cloned()
                .expect("KALSHI-ACCESS-SIGNATURE required");
            let sig_bytes = base64::engine::general_purpose::STANDARD
                .decode(sig_b64.as_bytes())
                .expect("valid base64 RSA-PSS signature");

            let accept = ws_accept_key(&sec_key);
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\n\
                 Upgrade: websocket\r\n\
                 Connection: Upgrade\r\n\
                 Sec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();

            let sub_txt = read_masked_ws_text_frame(&mut stream);
            let sub_v: serde_json::Value = serde_json::from_str(&sub_txt).unwrap();
            let channels: Vec<String> = sub_v["params"]["channels"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|v| v.as_str().map(ToString::to_string))
                .collect();

            *handshake_bg.lock().unwrap() = Some(WsHandshakeInfo {
                path,
                key_id,
                timestamp,
                signature_len: sig_bytes.len(),
                channels,
            });

            let sub_ack = serde_json::json!({
                "type": "subscribed",
                "id": 1,
                "msg": { "channel": "fill", "sid": 1 }
            });
            let _ = send_ws_frame(&mut stream, 0x1, sub_ack.to_string().as_bytes());

            let rx_guard = rx.lock().unwrap();
            while let Ok(cmd) = rx_guard.recv_timeout(Duration::from_secs(30)) {
                match cmd {
                    WsMockCmd::SendText(txt) => {
                        if send_ws_frame(&mut stream, 0x1, txt.as_bytes()).is_err() {
                            break;
                        }
                    }
                    WsMockCmd::Close => {
                        let _ = send_ws_frame(&mut stream, 0x8, &[]);
                        break;
                    }
                }
            }
        }
    });

    MockWsHandle {
        port,
        tx,
        handshake: handshake_info,
        accept_count,
    }
}

fn spawn_sse_collector(port: u16) -> Arc<Mutex<Vec<(String, serde_json::Value)>>> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let events_bg = events.clone();

    std::thread::spawn(move || {
        let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect sse");
        let _ = s.set_read_timeout(Some(Duration::from_secs(20)));
        let req = format!(
            "GET /api/tape/stream HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: text/event-stream\r\n\r\n"
        );
        s.write_all(req.as_bytes()).unwrap();
        let mut reader = BufReader::new(s);
        // Skip HTTP headers.
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                break;
            }
        }
        let mut cur_event = String::new();
        let mut cur_data = String::new();
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    if trimmed.is_empty() {
                        if !cur_data.is_empty() {
                            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&cur_data) {
                                let ev = if cur_event.is_empty() {
                                    "tape".to_string()
                                } else {
                                    cur_event.clone()
                                };
                                events_bg.lock().unwrap().push((ev, v));
                            }
                        }
                        cur_event.clear();
                        cur_data.clear();
                    } else if let Some(ev) = trimmed.strip_prefix("event:") {
                        cur_event = ev.trim().to_string();
                    } else if let Some(d) = trimmed.strip_prefix("data:") {
                        cur_data.push_str(d.trim());
                    }
                }
            }
        }
    });

    events
}

#[test]
fn private_ws_signed_handshake_streaming_idempotent_sse_and_rest_fallback() {
    let dir = std::env::temp_dir().join(format!("ob-v035-ws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    std::fs::write(
        strat_dir.join("ws_maker.rn"),
        r#"
pub fn name() { "ws-maker" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { secs_to_close >= 60 }
pub fn entry_quote(yes_ask, no_ask, secs_to_close) { Some(0.60) }
"#,
    )
    .unwrap();

    let priv_pem = generate_ephemeral_rsa_pem(&dir);
    let (rest_port, rest_state) = spawn_mock_rest_kalshi();
    let mock_rest_base = format!("http://127.0.0.1:{rest_port}/trade-api/v2");

    let accept_ws = Arc::new(AtomicBool::new(true));
    let mock_ws = spawn_mock_private_ws(accept_ws.clone());
    let ws_port = mock_ws.port;
    let ws_tx = mock_ws.tx;
    let ws_handshake = mock_ws.handshake;
    let ws_url = format!("ws://127.0.0.1:{ws_port}/trade-api/ws/v2");

    let port = free_port();
    let _srv = spawn_server_with_env(
        &data_dir,
        &strat_dir,
        port,
        Some(dir.clone()),
        &[
            ("OPENBELL_PRIVATE_WS_URL", &ws_url),
            ("OPENBELL_RECONCILE_SECS", "1"),
        ],
    );

    // 1. Verify boot invariants before configuring live credentials.
    let (st_boot, b_boot) = http_json(port, "GET", "/api/live/status", false, "");
    assert_eq!(st_boot, 200);
    let boot_v: serde_json::Value = serde_json::from_str(&b_boot).unwrap();
    assert_eq!(boot_v["live_armed"], false);
    assert_eq!(boot_v["configured"], false);
    assert_eq!(boot_v["private_ws_connected"], false);
    assert_eq!(boot_v["venue_sync_mode"], "offline");

    let (st_run, b_run) = http_json(port, "GET", "/api/runner/status", false, "");
    assert_eq!(st_run, 200);
    let run_v: serde_json::Value = serde_json::from_str(&b_run).unwrap();
    assert_ne!(run_v["mode"], "live_auto");
    assert_eq!(run_v["live_armed"], false);

    let sse_events = spawn_sse_collector(port);
    wait_until(Duration::from_secs(5), || {
        let evs = sse_events.lock().unwrap();
        evs.iter().any(|(ev, _)| ev == "snapshot").then_some(())
    });

    // 2. Configure Kalshi live credentials -> triggers signed private WS handshake.
    let cfg_body = serde_json::json!({
        "api_key_id": "v35-private-ws-key",
        "private_key_pem": priv_pem,
        "environment": "demo",
        "api_base_url": mock_rest_base,
        "rails": {
            "max_order_contracts": 25,
            "max_order_dollars": 25.0,
            "max_daily_loss_dollars": 50.0,
            "max_open_exposure_dollars": 100.0,
            "allowed_ticker_prefix": "KXBTC15M"
        }
    })
    .to_string();
    assert_eq!(
        http_json(port, "POST", "/api/live/config", true, &cfg_body).0,
        200
    );

    wait_until(Duration::from_secs(5), || {
        let (st, b) = http_json(port, "GET", "/api/live/status", false, "");
        if st == 200 {
            let v: serde_json::Value = serde_json::from_str(&b).unwrap();
            if v["private_ws_connected"] == true
                && v["venue_sync_mode"] == "ws_stream"
                && v["venue_sync_stale"] == false
            {
                return Some(());
            }
        }
        None
    });

    // Verify signed handshake headers & subscribed channels.
    let hs = ws_handshake
        .lock()
        .unwrap()
        .take()
        .expect("private WS handshake recorded");
    assert_eq!(hs.path, "/trade-api/ws/v2");
    assert_eq!(hs.key_id, "v35-private-ws-key");
    assert!(hs.timestamp.parse::<u64>().is_ok());
    assert_eq!(hs.signature_len, 256, "2048-bit RSA-PSS signature expected");
    assert!(hs.channels.contains(&"fill".to_string()));
    assert!(hs.channels.contains(&"market_positions".to_string()));
    assert!(hs.channels.contains(&"order_group_updates".to_string()));
    assert!(hs.channels.contains(&"user_orders".to_string()));

    // 3. Verify 60s reconciler backstop scaling (OPENBELL_RECONCILE_SECS=1 must NOT poll every 1s).
    std::thread::sleep(Duration::from_millis(250));
    let baseline_reconcile = rest_state
        .lock()
        .unwrap()
        .reconcile_calls
        .load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(2_200));
    let after_idle_reconcile = rest_state
        .lock()
        .unwrap()
        .reconcile_calls
        .load(Ordering::SeqCst);
    assert_eq!(
        after_idle_reconcile, baseline_reconcile,
        "reconciler must scale to 60s backstop while private_ws_connected == true"
    );

    // 4. Arm live trading, start LiveAuto runner, and also place an operator buy order.
    assert_eq!(
        http_json(port, "POST", "/api/live/arm", true, r#"{"confirm_arm":true}"#).0,
        200
    );

    let runner_cfg = serde_json::json!({
        "slots": [{
            "slot_id": 1,
            "strategy": "ws-maker",
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
    assert_eq!(
        http_json(port, "POST", "/api/runner/config", true, &runner_cfg).0,
        200
    );
    assert_eq!(
        http_json(
            port,
            "POST",
            "/api/runner/start",
            true,
            r#"{"mode":"live_auto","confirm_auto_live":true}"#
        )
        .0,
        200
    );

    let snap_row = serde_json::json!({
        "fetched_at": now_unix(),
        "ticker": "KXBTC15M-WSRUN",
        "event_ticker": "KXBTC15M-EVWS",
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
    std::fs::write(data_dir.join("snapshots.jsonl"), format!("{snap_row}\n")).unwrap();

    let runner_oid = wait_until(Duration::from_secs(5), || {
        let rs = rest_state.lock().unwrap();
        rs.orders
            .iter()
            .find(|o| o["ticker"] == "KXBTC15M-WSRUN")
            .and_then(|o| o["order_id"].as_str().map(ToString::to_string))
    });

    // Place a separate operator buy order (5 @ 60c = $3.00).
    let (st_o1, b_o1) = http_json(
        port,
        "POST",
        "/api/live/order",
        true,
        r#"{"ticker":"KXBTC15M-WSOP","action":"buy","side":"yes","count":5,"price_cents":60}"#,
    );
    assert_eq!(st_o1, 200, "order failed: {b_o1}");
    let o1: serde_json::Value = serde_json::from_str(&b_o1).unwrap();
    let op_oid = o1["order"]["order_id"].as_str().unwrap().to_string();

    // 5. Stream WS `fill` for runner order -> triggers ShadowRunner::reconcile_live_data + live_exec SSE!
    let fill_runner = serde_json::json!({
        "type": "fill",
        "sid": 1,
        "seq": 1,
        "msg": {
            "trade_id": "tr-ws-run-1",
            "order_id": runner_oid,
            "market_ticker": "KXBTC15M-WSRUN",
            "action": "buy",
            "side": "yes",
            "count": 10,
            "yes_price": 57,
            "is_taker": false,
            "ts": now_unix()
        }
    });
    ws_tx.send(WsMockCmd::SendText(fill_runner.to_string())).unwrap();

    // Send duplicate trade_id -> must remain idempotent in live_executions.jsonl!
    let fill_runner_dup = serde_json::json!({
        "type": "fill",
        "sid": 1,
        "seq": 2,
        "msg": {
            "trade_id": "tr-ws-run-1",
            "order_id": runner_oid,
            "market_ticker": "KXBTC15M-WSRUN",
            "action": "buy",
            "side": "yes",
            "count": 10,
            "yes_price": 57,
            "is_taker": false,
            "ts": now_unix()
        }
    });
    ws_tx
        .send(WsMockCmd::SendText(fill_runner_dup.to_string()))
        .unwrap();

    wait_until(Duration::from_secs(5), || {
        let evs = sse_events.lock().unwrap();
        evs.iter()
            .any(|(ev, v)| ev == "live_exec" && v["trade_id"] == "tr-ws-run-1")
            .then_some(())
    });

    wait_until(Duration::from_secs(5), || {
        let (_, b_rled) = http_json(port, "GET", "/api/runner/ledger?limit=20", false, "");
        let v: serde_json::Value = serde_json::from_str(&b_rled).unwrap();
        v["entries"]
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .find(|e| e["event"] == "OrderFilled" && e["ticker"] == "KXBTC15M-WSRUN")
                    .cloned()
            })
            .filter(|e| e["fill_cents"] == 57)
    });

    // 6. Stream `user_order` cancel for operator order -> releases $3.00 reserved exposure + SSE live_exec.
    let (_, b_pre_cancel) = http_json(port, "GET", "/api/live/status", false, "");
    let pre_exp = serde_json::from_str::<serde_json::Value>(&b_pre_cancel).unwrap()
        ["open_exposure_dollars"]
        .as_f64()
        .unwrap();

    let ord_cancel = serde_json::json!({
        "type": "user_order",
        "sid": 1,
        "seq": 3,
        "msg": {
            "order_id": op_oid,
            "ticker": "KXBTC15M-WSOP",
            "status": "canceled",
            "action": "buy",
            "side": "yes",
            "type": "limit",
            "yes_price": 60,
            "initial_count": 5,
            "remaining_count": 5,
            "fill_count": 0
        }
    });
    ws_tx.send(WsMockCmd::SendText(ord_cancel.to_string())).unwrap();

    wait_until(Duration::from_secs(5), || {
        let evs = sse_events.lock().unwrap();
        evs.iter()
            .any(|(ev, v)| {
                ev == "live_exec"
                    && v["event_kind"] == "order"
                    && v["order_id"] == op_oid
                    && v["status"] == "canceled"
            })
            .then_some(())
    });

    let (_, b_post_cancel) = http_json(port, "GET", "/api/live/status", false, "");
    let post_cancel_v: serde_json::Value = serde_json::from_str(&b_post_cancel).unwrap();
    let post_exp = post_cancel_v["open_exposure_dollars"].as_f64().unwrap();
    assert!(
        (pre_exp - post_exp - 3.0).abs() < 1e-6,
        "canceled order over WS must release $3.00 exposure: pre={pre_exp} post={post_exp}"
    );
    assert_eq!(post_cancel_v["private_ws_connected"], true);
    assert!(post_cancel_v["private_ws_last_event_epoch"].is_i64());
    assert_eq!(post_cancel_v["venue_sync_mode"], "ws_stream");

    // 7. Stream `market_position` frame -> updates exposure and daily realized loss + SSE live_exec.
    let pos_msg = serde_json::json!({
        "type": "market_position",
        "sid": 1,
        "seq": 4,
        "msg": {
            "ticker": "KXBTC15M-WSRUN",
            "position": 0,
            "market_exposure_dollars": "0.00",
            "realized_pnl_dollars": "-1.25",
            "resting_orders_count": 0
        }
    });
    ws_tx.send(WsMockCmd::SendText(pos_msg.to_string())).unwrap();

    wait_until(Duration::from_secs(5), || {
        let evs = sse_events.lock().unwrap();
        evs.iter()
            .any(|(ev, v)| ev == "live_exec" && v["event_kind"] == "position")
            .then_some(())
    });

    wait_until(Duration::from_secs(5), || {
        let (_, b_st) = http_json(port, "GET", "/api/live/status", false, "");
        let v: serde_json::Value = serde_json::from_str(&b_st).unwrap();
        if v["open_exposure_dollars"].as_f64().unwrap().abs() < 1e-6
            && v["daily_realized_loss_dollars"].as_f64().unwrap() >= 1.249
        {
            Some(())
        } else {
            None
        }
    });

    // Verify live_executions.jsonl idempotency (exactly 1 fill for tr-ws-run-1).
    let (_, b_rec) = http_json(port, "GET", "/api/live/reconcile", false, "");
    let rec_v: serde_json::Value = serde_json::from_str(&b_rec).unwrap();
    let execs = rec_v["executions"].as_array().unwrap();
    let run_fills = execs
        .iter()
        .filter(|e| e["kind"] == "fill" && e["venue_id"] == "tr-ws-run-1")
        .count();
    assert_eq!(run_fills, 1, "WS fill must be idempotent in live_executions.jsonl");
    assert!(
        execs
            .iter()
            .any(|e| e["kind"] == "order_canceled" && e["order_id"] == op_oid),
        "WS order cancel must be recorded in live_executions.jsonl"
    );

    // 8. Sequence gap (`seq: 6` skipping `5`) triggers immediate REST reconcile!
    let pre_gap_calls = rest_state
        .lock()
        .unwrap()
        .reconcile_calls
        .load(Ordering::SeqCst);
    let gap_msg = serde_json::json!({
        "type": "heartbeat",
        "sid": 1,
        "seq": 6
    });
    ws_tx.send(WsMockCmd::SendText(gap_msg.to_string())).unwrap();
    wait_until(Duration::from_secs(5), || {
        let cur = rest_state
            .lock()
            .unwrap()
            .reconcile_calls
            .load(Ordering::SeqCst);
        (cur > pre_gap_calls).then_some(())
    });

    // 9. Disconnect WS (reject reconnects) -> immediate REST fallback + status flips to rest_poll.
    accept_ws.store(false, Ordering::SeqCst);
    let pre_disc_calls = rest_state
        .lock()
        .unwrap()
        .reconcile_calls
        .load(Ordering::SeqCst);
    ws_tx.send(WsMockCmd::Close).unwrap();

    wait_until(Duration::from_secs(5), || {
        let (_, b_st) = http_json(port, "GET", "/api/live/status", false, "");
        let v: serde_json::Value = serde_json::from_str(&b_st).unwrap();
        let calls = rest_state
            .lock()
            .unwrap()
            .reconcile_calls
            .load(Ordering::SeqCst);
        if v["private_ws_connected"] == false
            && v["venue_sync_mode"] == "rest_poll"
            && calls > pre_disc_calls
        {
            Some(())
        } else {
            None
        }
    });
}

#[test]
fn private_ws_offline_fixture_safety_and_boot_invariants() {
    let dir = std::env::temp_dir().join(format!("ob-v035-off-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    let priv_pem = generate_ephemeral_rsa_pem(&dir);
    let (rest_port, _) = spawn_mock_rest_kalshi();
    let mock_rest_base = format!("http://127.0.0.1:{rest_port}/trade-api/v2");

    // A. Unconfigured server even WITH OPENBELL_PRIVATE_WS_URL makes zero WS handshakes.
    let accept_ws = Arc::new(AtomicBool::new(true));
    let mock_ws_a = spawn_mock_private_ws(accept_ws.clone());
    let ws_url_a = format!("ws://127.0.0.1:{}/trade-api/ws/v2", mock_ws_a.port);
    let port_a = free_port();
    let srv_a = spawn_server_with_env(
        &data_dir,
        &strat_dir,
        port_a,
        None,
        &[("OPENBELL_PRIVATE_WS_URL", &ws_url_a)],
    );
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        mock_ws_a.accept_count.load(Ordering::SeqCst),
        0,
        "unconfigured server must never initiate private WS handshake"
    );
    let (_, b_a) = http_json(port_a, "GET", "/api/live/status", false, "");
    let va: serde_json::Value = serde_json::from_str(&b_a).unwrap();
    assert_eq!(va["private_ws_connected"], false);
    assert_eq!(va["venue_sync_mode"], "offline");
    assert_eq!(va["live_armed"], false);
    let (_, br_a) = http_json(port_a, "GET", "/api/runner/status", false, "");
    let vra: serde_json::Value = serde_json::from_str(&br_a).unwrap();
    assert_ne!(vra["mode"], "live_auto");
    drop(srv_a);

    // B. Configured server with OPENBELL_PRIVATE_WS_OFFLINE=1 makes zero WS handshakes.
    let mock_ws_b = spawn_mock_private_ws(accept_ws);
    let ws_url_b = format!("ws://127.0.0.1:{}/trade-api/ws/v2", mock_ws_b.port);
    let port_b = free_port();
    let _srv_b = spawn_server_with_env(
        &data_dir,
        &strat_dir,
        port_b,
        Some(dir.clone()),
        &[
            ("OPENBELL_PRIVATE_WS_OFFLINE", "1"),
            ("OPENBELL_PRIVATE_WS_URL", &ws_url_b),
        ],
    );
    let cfg_body = serde_json::json!({
        "api_key_id": "v35-offline-key",
        "private_key_pem": priv_pem,
        "environment": "demo",
        "api_base_url": mock_rest_base,
    })
    .to_string();
    assert_eq!(
        http_json(port_b, "POST", "/api/live/config", true, &cfg_body).0,
        200
    );
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        mock_ws_b.accept_count.load(Ordering::SeqCst),
        0,
        "OPENBELL_PRIVATE_WS_OFFLINE=1 must skip all WS handshakes"
    );
    let (_, b_b) = http_json(port_b, "GET", "/api/live/status", false, "");
    let vb: serde_json::Value = serde_json::from_str(&b_b).unwrap();
    assert_eq!(vb["private_ws_connected"], false);
    assert_eq!(vb["live_armed"], false);
}
