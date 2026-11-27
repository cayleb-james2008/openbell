//! Cycle 4 Wave H1 Hardening Regression Tests (`v0.36.0` target).
//!
//! Verifies end-to-end hardening across all three Cycle 4 surfaces:
//! 1. Private WS (`src/private_ws.rs`, `live_executions.jsonl`):
//!    - Corrupt/truncated trailing line recovery in `live_executions.jsonl` on startup
//!    - RFC 6455 fragmented text frame with interleaved Ping (`0x9`) -> Pong (`0xA`) reply
//!    - Duplicate `user_order` `canceled` + `market_position` idempotency (no double exposure release)
//!    - Close code `1001` echo + immediate REST fallback (`venue_sync_mode == "rest_poll"`)
//! 2. Quote lifecycle & Risk rails (`src/main.rs`, `src/risk_rails.rs`, `src/shadow_runner.rs`):
//!    - Tampered `risk_rails.json`, `risk_rails_runtime.json`, and `runner_state.json` always boot
//!      `live_armed: false`, `killed: true`, and `runner.mode != "live_auto"`
//!    - Concurrent `POST /api/live/orders/{id}/amend` fail-closed `409` lock (`amend_calls == 1`)
//!    - Concurrent & sequential `POST /api/live/positions/{ticker}/flatten` fail-closed `409` lock
//!      and resting-sell guard (never double-sells)
//! 3. Spot & Strike Oracle (`src/spot_feed.rs`):
//!    - Startup corrupt/truncated `spot_ticks.jsonl` cache recovery
//!    - Outlier price spike rejection + future clock-skew rejection + CWE-59 symlink rejection

use base64::Engine as _;
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

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn http_json(port: u16, method: &str, path: &str, csrf: bool, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect server");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let mut req =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if csrf {
        req.push_str("X-OpenBell-CSRF: 1\r\n");
    }
    if !matches!(method, "GET" | "HEAD" | "DELETE") && (!body.is_empty() || method == "POST") {
        req.push_str("Content-Type: application/json\r\n");
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
    let priv_path = dir.join("ephemeral_h1_priv.pem");
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

fn read_masked_ws_frame(stream: &mut TcpStream) -> std::io::Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 2];
    stream.read_exact(&mut hdr)?;
    let opcode = hdr[0] & 0x0f;
    let masked = (hdr[1] & 0x80) != 0;
    assert!(masked, "client WS frames must be masked per RFC 6455");
    let len7 = hdr[1] & 0x7f;
    let len = match len7 {
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
    stream.read_exact(&mut mask)?;
    let mut payload = vec![0u8; len];
    stream.read_exact(&mut payload)?;
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i & 3];
    }
    Ok((opcode, payload))
}

fn send_ws_raw(stream: &mut TcpStream, fin: bool, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.push((if fin { 0x80 } else { 0 }) | (opcode & 0x0f));
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
struct MockRestH1 {
    positions: Vec<serde_json::Value>,
    orders: Vec<serde_json::Value>,
    fills: Vec<serde_json::Value>,
    next_id: usize,
    amend_calls: usize,
    sell_orders_placed: usize,
    reconcile_calls: AtomicUsize,
    amend_delay_ms: u64,
    positions_delay_ms: u64,
}

fn spawn_mock_rest_h1() -> (u16, Arc<Mutex<MockRestH1>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let state = Arc::new(Mutex::new(MockRestH1::default()));
    let state_bg = state.clone();

    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let st_conn = state_bg.clone();
            std::thread::spawn(move || {
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let mut reader = BufReader::new(s.try_clone().unwrap());
                let mut req_line = String::new();
                if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
                    return;
                }
                let parts: Vec<&str> = req_line.split_whitespace().collect();
                if parts.len() < 2 {
                    return;
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

                let (pos_delay, amend_delay) = {
                    let st = st_conn.lock().unwrap();
                    (st.positions_delay_ms, st.amend_delay_ms)
                };
                if method == "GET" && path_no_query == "/trade-api/v2/portfolio/positions" && pos_delay > 0 {
                    std::thread::sleep(Duration::from_millis(pos_delay));
                }
                if method == "POST" && path_no_query.ends_with("/amend") && amend_delay > 0 {
                    std::thread::sleep(Duration::from_millis(amend_delay));
                }

                let mut st = st_conn.lock().unwrap();
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
                        (200, serde_json::json!({ "settlements": [] }))
                    }
                    ("POST", "/trade-api/v2/portfolio/orders") => {
                        let req_v: serde_json::Value =
                            serde_json::from_slice(&body_bytes).unwrap_or_default();
                        st.next_id += 1;
                        let oid = format!("ord-h1-{}", st.next_id);
                        if req_v["action"] == "sell" {
                            st.sell_orders_placed += 1;
                        }
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
                    ("POST", p)
                        if p.starts_with("/trade-api/v2/portfolio/orders/")
                            && p.ends_with("/amend") =>
                    {
                        let id = p
                            .trim_start_matches("/trade-api/v2/portfolio/orders/")
                            .trim_end_matches("/amend");
                        let req_v: serde_json::Value =
                            serde_json::from_slice(&body_bytes).unwrap_or_default();
                        st.amend_calls += 1;
                        if let Some(ord) = st
                            .orders
                            .iter_mut()
                            .find(|o| o["order_id"].as_str() == Some(id))
                        {
                            if let Some(yp) = req_v.get("yes_price").and_then(|v| v.as_i64()) {
                                ord["yes_price"] = serde_json::json!(yp);
                            }
                            if let Some(c) = req_v.get("count").and_then(|v| v.as_i64()) {
                                ord["initial_count"] = serde_json::json!(c);
                                ord["remaining_count"] = serde_json::json!(c);
                            }
                            (200, serde_json::json!({ "order": ord.clone() }))
                        } else {
                            (404, serde_json::json!({ "error": "order not found" }))
                        }
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
            });
        }
    });

    (port, state)
}

#[test]
fn h1_private_ws_fragmented_interleaved_ping_close_corrupt_ledger_and_rest_fallback() {
    let dir = std::env::temp_dir().join(format!("ob-h1-pws-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // Seed live_executions.jsonl with 1 valid record + corrupt truncated trailing line without '\n'
    let seed_rec = serde_json::json!({
        "kind": "fill",
        "venue_id": "tr-seed-h1",
        "ts": now_unix() - 120,
        "order_id": "ord-seed-1",
        "client_order_id": null,
        "trade_id": "tr-seed-h1",
        "ticker": "KXBTC15M-H1SEED",
        "side": "yes",
        "action": "buy",
        "count": 2,
        "price_cents": 50,
        "is_taker": false,
        "fee_dollars": 0.01
    });
    let mut seed_bytes = format!("{seed_rec}\n").into_bytes();
    seed_bytes.extend_from_slice(br#"{"kind":"fill","venue_id":"tr-truncated"#);
    std::fs::write(data_dir.join("live_executions.jsonl"), &seed_bytes).unwrap();

    let priv_pem = generate_ephemeral_rsa_pem(&dir);
    let (rest_port, rest_state) = spawn_mock_rest_h1();
    let mock_rest_base = format!("http://127.0.0.1:{rest_port}/trade-api/v2");

    // Mock WS server that speaks fragmented frames + interleaved ping + close code 1001
    let ws_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    let accept_ws = Arc::new(AtomicBool::new(true));
    let accept_bg = accept_ws.clone();
    let pong_received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let pong_bg = pong_received.clone();
    let close_echo_received = Arc::new(Mutex::new(None::<u16>));
    let close_bg = close_echo_received.clone();
    let (cmd_tx, cmd_rx) = mpsc::channel::<String>();
    let cmd_rx = Arc::new(Mutex::new(cmd_rx));

    std::thread::spawn(move || {
        for mut stream in ws_listener.incoming().flatten() {
            if !accept_bg.load(Ordering::SeqCst) {
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
            let Some(pos) = hdr_end else { continue };
            let hdr_str = String::from_utf8_lossy(&buf[..pos]);
            let sec_key = hdr_str
                .lines()
                .find_map(|l| {
                    l.split_once(':').and_then(|(k, v)| {
                        k.trim()
                            .eq_ignore_ascii_case("sec-websocket-key")
                            .then(|| v.trim().to_string())
                    })
                })
                .unwrap();
            let accept = ws_accept_key(&sec_key);
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
            let (op, _) = read_masked_ws_frame(&mut stream).unwrap();
            assert_eq!(op, 0x1);

            let ack = serde_json::json!({"type": "subscribed", "id": 1, "msg": {"channel": "fill", "sid": 1}}).to_string();
            send_ws_raw(&mut stream, true, 0x1, ack.as_bytes()).unwrap();

            let rx_g = cmd_rx.lock().unwrap();
            while let Ok(cmd) = rx_g.recv_timeout(Duration::from_secs(15)) {
                if cmd == "CLOSE_1001" {
                    let mut close_body = 1001u16.to_be_bytes().to_vec();
                    close_body.extend_from_slice(b"going away");
                    send_ws_raw(&mut stream, true, 0x8, &close_body).unwrap();
                    if let Ok((0x8, cp)) = read_masked_ws_frame(&mut stream) {
                        if cp.len() >= 2 {
                            *close_bg.lock().unwrap() =
                                Some(u16::from_be_bytes([cp[0], cp[1]]));
                        }
                    }
                    break;
                } else if let Some(frag_json) = cmd.strip_prefix("FRAG_PING:") {
                    let b = frag_json.as_bytes();
                    let mid = b.len() / 2;
                    send_ws_raw(&mut stream, false, 0x1, &b[..mid]).unwrap();
                    send_ws_raw(&mut stream, true, 0x9, b"h1-ping").unwrap();
                    if let Ok((0xA, p)) = read_masked_ws_frame(&mut stream) {
                        pong_bg.lock().unwrap().push(p);
                    }
                    send_ws_raw(&mut stream, true, 0x0, &b[mid..]).unwrap();
                } else {
                    send_ws_raw(&mut stream, true, 0x1, cmd.as_bytes()).unwrap();
                }
            }
        }
    });

    let ws_url = format!("ws://127.0.0.1:{ws_port}/trade-api/ws/v2");
    let port = free_port();
    let _srv = spawn_server_with_env(
        &data_dir,
        &strat_dir,
        port,
        Some(dir.clone()),
        &[
            ("OPENBELL_SPOT_OFFLINE", "1"),
            ("OPENBELL_PRIVATE_WS_URL", &ws_url),
            ("OPENBELL_RECONCILE_SECS", "1"),
        ],
    );

    // 1. Verify corrupt trailing line was truncated cleanly on startup and seeded record survived
    let (st_rec0, b_rec0) = http_json(port, "GET", "/api/live/reconcile", false, "");
    assert_eq!(st_rec0, 200);
    let rec0: serde_json::Value = serde_json::from_str(&b_rec0).unwrap();
    assert_eq!(rec0["executions"].as_array().unwrap().len(), 1);
    assert_eq!(rec0["executions"][0]["venue_id"], "tr-seed-h1");

    // 2. Configure live credentials & connect WS
    let cfg_body = serde_json::json!({
        "api_key_id": "h1-pws-key",
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
    assert_eq!(http_json(port, "POST", "/api/live/config", true, &cfg_body).0, 200);
    wait_until(Duration::from_secs(5), || {
        let (_, b) = http_json(port, "GET", "/api/live/status", false, "");
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        (v["private_ws_connected"] == true).then_some(())
    });

    // 3. Arm + place 2 resting orders ($4.00 and $3.00)
    assert_eq!(http_json(port, "POST", "/api/live/arm", true, r#"{"confirm_arm":true}"#).0, 200);
    let (_, b1) = http_json(
        port,
        "POST",
        "/api/live/order",
        true,
        r#"{"ticker":"KXBTC15M-H1A","action":"buy","side":"yes","count":8,"price_cents":50}"#,
    );
    let oid1 = serde_json::from_str::<serde_json::Value>(&b1).unwrap()["order"]["order_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (_, b2) = http_json(
        port,
        "POST",
        "/api/live/order",
        true,
        r#"{"ticker":"KXBTC15M-H1B","action":"buy","side":"yes","count":6,"price_cents":50}"#,
    );
    let oid2 = serde_json::from_str::<serde_json::Value>(&b2).unwrap()["order"]["order_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 4. Send fragmented WS fill (with interleaved Ping) for oid2
    let frag_fill = serde_json::json!({
        "type": "fill",
        "sid": 1,
        "seq": 1,
        "msg": {
            "trade_id": "tr-h1-frag",
            "order_id": oid2,
            "market_ticker": "KXBTC15M-H1B",
            "action": "buy",
            "side": "yes",
            "count": 6,
            "yes_price": 50,
            "ts": now_unix()
        }
    })
    .to_string();
    cmd_tx.send(format!("FRAG_PING:{frag_fill}")).unwrap();

    wait_until(Duration::from_secs(5), || {
        let (_, b) = http_json(port, "GET", "/api/live/reconcile", false, "");
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        v["executions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["venue_id"] == "tr-h1-frag")
            .then_some(())
    });
    assert!(
        pong_received
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.as_slice() == b"h1-ping"),
        "expected Pong reply carrying mid-fragment Ping payload"
    );

    // 5. Send duplicate user_order cancel for oid1 -> must release $4.00 ONCE, preserving $3.00!
    let cancel_ev = serde_json::json!({
        "type": "user_order",
        "sid": 1,
        "seq": 2,
        "msg": {
            "order_id": oid1,
            "ticker": "KXBTC15M-H1A",
            "status": "canceled",
            "action": "buy",
            "side": "yes",
            "yes_price": 50,
            "initial_count": 8,
            "remaining_count": 8,
            "fill_count": 0
        }
    })
    .to_string();
    cmd_tx.send(cancel_ev.clone()).unwrap();
    cmd_tx.send(cancel_ev).unwrap();

    wait_until(Duration::from_secs(5), || {
        let (_, b) = http_json(port, "GET", "/api/live/status", false, "");
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        let exp = v["open_exposure_dollars"].as_f64().unwrap();
        ((exp - 3.0).abs() < 1e-6).then_some(())
    });
    std::thread::sleep(Duration::from_millis(150));
    let (_, b_after_dup) = http_json(port, "GET", "/api/live/status", false, "");
    let exp_after_dup = serde_json::from_str::<serde_json::Value>(&b_after_dup).unwrap()
        ["open_exposure_dollars"]
        .as_f64()
        .unwrap();
    assert!(
        (exp_after_dup - 3.0).abs() < 1e-6,
        "duplicate WS cancel must not double-release exposure: got {exp_after_dup}"
    );

    // 6. Send Close 1001 -> server echoes 1001, drops WS, and falls back immediately to REST poll
    accept_ws.store(false, Ordering::SeqCst);
    let pre_close_calls = rest_state
        .lock()
        .unwrap()
        .reconcile_calls
        .load(Ordering::SeqCst);
    cmd_tx.send("CLOSE_1001".to_string()).unwrap();

    wait_until(Duration::from_secs(5), || {
        let (_, b) = http_json(port, "GET", "/api/live/status", false, "");
        let v: serde_json::Value = serde_json::from_str(&b).unwrap();
        let calls = rest_state
            .lock()
            .unwrap()
            .reconcile_calls
            .load(Ordering::SeqCst);
        (v["private_ws_connected"] == false
            && v["venue_sync_mode"] == "rest_poll"
            && calls > pre_close_calls)
            .then_some(())
    });
    assert_eq!(*close_echo_received.lock().unwrap(), Some(1001));
}

#[test]
fn h1_concurrent_amend_flatten_fail_closed_and_tamper_boot_invariants() {
    let dir = std::env::temp_dir().join(format!("ob-h1-rails-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    // Seed tampered disk state BEFORE boot:
    // - risk_rails.json with live_armed: true and huge contracts
    // - risk_rails_runtime.json with live_armed: true, killed: false
    // - runner_state.json with mode: "live_auto"
    std::fs::write(
        data_dir.join("risk_rails.json"),
        r#"{"max_order_contracts":50000,"max_order_dollars":50.0,"max_daily_loss_dollars":50.0,"max_open_exposure_dollars":100.0,"allowed_ticker_prefix":"KX","live_armed":true}"#,
    )
    .unwrap();
    std::fs::write(
        data_dir.join("risk_rails_runtime.json"),
        r#"{"utc_day_num":20000,"daily_realized_loss_dollars":5.0,"open_exposure_dollars":0.0,"orders_placed_today":1,"killed":false,"live_armed":true}"#,
    )
    .unwrap();
    std::fs::write(
        data_dir.join("runner_state.json"),
        r#"{"mode":"live_auto","tick_interval_ms":200,"cancel_before_close_secs":30,"bankroll_dollars":500.0,"slots":[],"slot_states":[]}"#,
    )
    .unwrap();

    let priv_pem = generate_ephemeral_rsa_pem(&dir);
    let (rest_port, rest_state) = spawn_mock_rest_h1();
    let mock_rest_base = format!("http://127.0.0.1:{rest_port}/trade-api/v2");

    let port = free_port();
    let _srv = spawn_server_with_env(
        &data_dir,
        &strat_dir,
        port,
        Some(dir.clone()),
        &[
            ("OPENBELL_SPOT_OFFLINE", "1"),
            ("OPENBELL_PRIVATE_WS_OFFLINE", "1"),
        ],
    );

    // 1. Tampered disk state MUST boot disarmed, killed=true, and runner_mode != live_auto
    let (st_ls, b_ls) = http_json(port, "GET", "/api/live/status", false, "");
    assert_eq!(st_ls, 200);
    let lv: serde_json::Value = serde_json::from_str(&b_ls).unwrap();
    assert_eq!(lv["live_armed"], false);
    assert_eq!(lv["killed"], true);

    let (st_rs, b_rs) = http_json(port, "GET", "/api/runner/status", false, "");
    assert_eq!(st_rs, 200);
    let rv: serde_json::Value = serde_json::from_str(&b_rs).unwrap();
    assert_ne!(rv["mode"], "live_auto");

    let (st_ord_blocked, _) = http_json(
        port,
        "POST",
        "/api/live/order",
        true,
        r#"{"ticker":"KXBTC15M-BLK","action":"buy","side":"yes","count":1,"price_cents":50}"#,
    );
    assert_eq!(st_ord_blocked, 403);

    // 2. Configure clean rails, reset kill, reconcile, arm, and place 1 order
    let cfg_body = serde_json::json!({
        "api_key_id": "h1-conc-key",
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
    assert_eq!(http_json(port, "POST", "/api/live/config", true, &cfg_body).0, 200);
    assert_eq!(
        http_json(port, "POST", "/api/live/reset-kill", true, r#"{"confirm_reset_kill":true}"#).0,
        200
    );
    assert_eq!(http_json(port, "POST", "/api/live/reconcile", true, "{}").0, 200);
    assert_eq!(http_json(port, "POST", "/api/live/arm", true, r#"{"confirm_arm":true}"#).0, 200);

    let (st_o, b_o) = http_json(
        port,
        "POST",
        "/api/live/order",
        true,
        r#"{"ticker":"KXBTC15M-CONC","action":"buy","side":"yes","count":10,"price_cents":50}"#,
    );
    assert_eq!(st_o, 200, "order failed: {b_o}");
    let oid = serde_json::from_str::<serde_json::Value>(&b_o).unwrap()["order"]["order_id"]
        .as_str()
        .unwrap()
        .to_string();

    // 3. Concurrent amend on the same order_id -> one 200, one fail-closed 409, amend_calls == 1
    rest_state.lock().unwrap().amend_delay_ms = 250;
    let oid_a = oid.clone();
    let oid_b = oid;
    let h1 = std::thread::spawn(move || {
        http_json(
            port,
            "POST",
            &format!("/api/live/orders/{oid_a}/amend"),
            true,
            r#"{"price_cents":55}"#,
        )
    });
    std::thread::sleep(Duration::from_millis(40));
    let h2 = std::thread::spawn(move || {
        http_json(
            port,
            "POST",
            &format!("/api/live/orders/{oid_b}/amend"),
            true,
            r#"{"price_cents":60}"#,
        )
    });
    let (c1, _) = h1.join().unwrap();
    let (c2, _) = h2.join().unwrap();
    let mut amend_codes = [c1, c2];
    amend_codes.sort_unstable();
    assert_eq!(amend_codes, [200, 409], "expected one 200 and one 409, got {amend_codes:?}");
    assert_eq!(rest_state.lock().unwrap().amend_calls, 1);

    // 4. Concurrent flatten on same ticker + subsequent flatten while sell order rests -> 409 fail-closed!
    {
        let mut st = rest_state.lock().unwrap();
        st.amend_delay_ms = 0;
        st.positions_delay_ms = 250;
        st.positions.push(serde_json::json!({
            "ticker": "KXBTC15M-CONC",
            "position": 10,
            "market_exposure": 550,
            "realized_pnl": 0,
            "resting_orders_count": 0,
            "fees_paid": 0,
            "total_traded": 550
        }));
    }
    let f1 = std::thread::spawn(move || {
        http_json(
            port,
            "POST",
            "/api/live/positions/KXBTC15M-CONC/flatten",
            true,
            r#"{"price_cents":54}"#,
        )
    });
    std::thread::sleep(Duration::from_millis(40));
    let f2 = std::thread::spawn(move || {
        http_json(
            port,
            "POST",
            "/api/live/positions/KXBTC15M-CONC/flatten",
            true,
            r#"{"price_cents":54}"#,
        )
    });
    let (fc1, _) = f1.join().unwrap();
    let (fc2, _) = f2.join().unwrap();
    let mut flat_codes = [fc1, fc2];
    flat_codes.sort_unstable();
    assert_eq!(flat_codes, [200, 409], "expected one 200 and one 409, got {flat_codes:?}");
    assert_eq!(rest_state.lock().unwrap().sell_orders_placed, 1);

    // Sequential flatten while the resting sell order is still open must ALSO fail closed 409!
    let (fc3, b3) = http_json(
        port,
        "POST",
        "/api/live/positions/KXBTC15M-CONC/flatten",
        true,
        r#"{"price_cents":54}"#,
    );
    assert_eq!(fc3, 409, "sequential duplicate flatten must fail closed 409: {b3}");
    assert_eq!(rest_state.lock().unwrap().sell_orders_placed, 1);
}

#[test]
fn h1_spot_oracle_corrupt_cache_outlier_clock_skew_and_symlink_safety() {
    let dir = std::env::temp_dir().join(format!("ob-h1-spot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data_dir = dir.join("data");
    let strat_dir = dir.join("strategies");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&strat_dir).unwrap();

    let t0 = now_ms();
    let spot_path = data_dir.join("spot_ticks.jsonl");
    let valid = format!(
        "{{\"ts_ms\":{},\"asset\":\"BTC\",\"price\":95000.0,\"source\":\"seed\"}}\n",
        t0 - 1_500
    );
    let mut bytes = valid.into_bytes();
    // Corrupt truncated trailing line without newline
    bytes.extend_from_slice(br#"{"ts_ms":170000,"asset":"BTC","price":"#);
    std::fs::write(&spot_path, &bytes).unwrap();

    // Mock spot WS sending: 1) excessive future clock skew tick, 2) outlier 100x spike tick, 3) valid tick
    let ws_listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let ws_port = ws_listener.local_addr().unwrap().port();
    let stop_ws = Arc::new(AtomicBool::new(false));
    let stop_bg = stop_ws.clone();
    std::thread::spawn(move || {
        while !stop_bg.load(Ordering::Relaxed) {
            if let Ok((mut stream, _)) = ws_listener.accept() {
                let mut req_buf = [0u8; 2048];
                let n = stream.read(&mut req_buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&req_buf[..n]);
                let key = req
                    .lines()
                    .find_map(|l| {
                        l.split_once(':').and_then(|(k, v)| {
                            k.trim()
                                .eq_ignore_ascii_case("sec-websocket-key")
                                .then(|| v.trim().to_string())
                        })
                    })
                    .unwrap_or_default();
                let accept = ws_accept_key(&key);
                let hs = format!(
                    "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
                );
                if stream.write_all(hs.as_bytes()).is_err() {
                    continue;
                }
                let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
                let mut sink = [0u8; 512];
                let _ = stream.read(&mut sink);

                let future_bad = serde_json::json!({
                    "type": "ticker",
                    "product_id": "BTC-USD",
                    "price": "99999.00",
                    "ts_ms": now_ms() + 120_000
                })
                .to_string();
                let spike_bad = serde_json::json!({
                    "type": "ticker",
                    "product_id": "BTC-USD",
                    "price": "9500000.00"
                })
                .to_string();
                let valid_ok = serde_json::json!({
                    "type": "ticker",
                    "product_id": "BTC-USD",
                    "price": "95125.50"
                })
                .to_string();
                let _ = send_ws_raw(&mut stream, true, 0x1, future_bad.as_bytes());
                std::thread::sleep(Duration::from_millis(80));
                let _ = send_ws_raw(&mut stream, true, 0x1, spike_bad.as_bytes());
                std::thread::sleep(Duration::from_millis(80));
                let _ = send_ws_raw(&mut stream, true, 0x1, valid_ok.as_bytes());
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    });

    let ws_url = format!("ws://127.0.0.1:{ws_port}/ws");
    let port = free_port();
    let _srv = spawn_server_with_env(
        &data_dir,
        &strat_dir,
        port,
        Some(dir.clone()),
        &[
            ("OPENBELL_PRIVATE_WS_OFFLINE", "1"),
            ("OPENBELL_SPOT_OFFLINE", "0"),
            ("OPENBELL_SPOT_URL", &ws_url),
        ],
    );

    // Boot invariants
    let (_, b_live) = http_json(port, "GET", "/api/live/status", false, "");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&b_live).unwrap()["live_armed"],
        false
    );
    let (_, b_run) = http_json(port, "GET", "/api/runner/status", false, "");
    assert_ne!(
        serde_json::from_str::<serde_json::Value>(&b_run).unwrap()["mode"],
        "live_auto"
    );

    // Wait for valid_ok tick (95125.50), ensuring spike_bad (9.5M) & future_bad (99999) were rejected!
    wait_until(Duration::from_secs(6), || {
        let (st, b) = http_json(port, "GET", "/api/spot/status", false, "");
        if st == 200 {
            let v: serde_json::Value = serde_json::from_str(&b).unwrap();
            let btc = &v["assets"][0];
            if btc["price"].as_f64().is_some_and(|p| (p - 95_125.5).abs() < 0.01) {
                return Some(());
            }
        }
        None
    });

    let (st_hist, b_hist) = http_json(port, "GET", "/api/spot/history?asset=BTC&limit=50", false, "");
    assert_eq!(st_hist, 200);
    let hv: serde_json::Value = serde_json::from_str(&b_hist).unwrap();
    let ticks = hv["ticks"].as_array().unwrap();
    assert_eq!(ticks.len(), 2, "expected seed + valid_ok ticks cleanly recovered: {b_hist}");
    assert!(ticks.iter().all(|t| t["price"].as_f64().unwrap() < 96_000.0));

    // CWE-59 symlink check
    #[cfg(unix)]
    {
        stop_ws.store(true, Ordering::Relaxed);
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, "PROTECTED").unwrap();
        std::fs::remove_file(&spot_path).unwrap();
        std::os::unix::fs::symlink(&outside, &spot_path).unwrap();
        let (st_sym, _) = http_json(port, "GET", "/api/spot/history?asset=BTC&limit=10", false, "");
        assert_eq!(st_sym, 403);
        assert_eq!(std::fs::read_to_string(&outside).unwrap(), "PROTECTED");
    }

    stop_ws.store(true, Ordering::Relaxed);
}
