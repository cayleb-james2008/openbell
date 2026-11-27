//! Authenticated Kalshi Trade API v2 Private WebSocket Execution Stream (Wave E3).
//!
//! Strictly READ-ONLY telemetry (`fill`, `market_positions`, `order_group_updates`, `user_orders`).
//! Never submits, amends, or cancels orders.
//!
//! Safety invariants:
//! - Defaults strictly offline when `OPENBELL_PRIVATE_WS_OFFLINE=1` or when Kalshi live
//!   credentials are unconfigured (`!PersistedLiveConfig::is_configured()`).
//! - Connects over `wss://` (via TLS) or loopback-only `ws://` (`OPENBELL_PRIVATE_WS_URL`
//!   for local RFC 6455 integration testing), signing `/trade-api/ws/v2` with
//!   `KalshiLiveClient::sign_ws_handshake_headers`.
//! - Validates RFC 6455 frame bounds, control frames, and UTF-8 payloads.
//! - Any disconnect, error, or sequence gap immediately clears `private_ws_connected`,
//!   wakes `start_background_reconciler` for immediate REST `reconcile_live_venue`, and
//!   preserves fail-closed `is_venue_sync_stale(30s)` gating.

#![allow(dead_code)]

use crate::{kalshi_live, orderbook, risk_rails, tape_hub, App};
use base64::Engine as _;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const KALSHI_PROD_WS: &str = "wss://api.elections.kalshi.com/trade-api/ws/v2";
const KALSHI_DEMO_WS: &str = "wss://demo-api.kalshi.co/trade-api/ws/v2";
const DEFAULT_WS_PATH: &str = "/trade-api/ws/v2";
const MAX_WS_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_WS_CONTINUATION_FRAMES: usize = 128;
const MAX_CONSECUTIVE_MALFORMED_WS_FRAMES: usize = 8;
const KEEPALIVE_PING_SECS: u64 = 15;
const KEEPALIVE_TIMEOUT_SECS: u64 = 30;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Returns `true` when private WebSocket networking is explicitly disabled via env.
pub fn private_ws_offline() -> bool {
    std::env::var("OPENBELL_PRIVATE_WS_OFFLINE")
        .ok()
        .is_some_and(|v| {
            let t = v.trim();
            t == "1" || t.eq_ignore_ascii_case("true")
        })
}

fn is_loopback_host(host: &str) -> bool {
    let lower = host.trim().to_ascii_lowercase();
    matches!(lower.as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

/// Parse and validate a WebSocket URL (`wss://` anywhere, `ws://` loopback-only).
/// Defaults an omitted path to `/trade-api/ws/v2`.
pub fn parse_ws_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let trimmed = url.trim();
    if trimmed.is_empty()
        || trimmed.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("private WS URL is empty or contains invalid characters".to_string());
    }
    let (tls, rest) = if let Some(r) = trimmed.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err("private WS URL must start with wss:// or ws://".to_string());
    };

    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, DEFAULT_WS_PATH.to_string()),
    };

    let (host, port) = if authority.starts_with('[') {
        let Some(end_bracket) = authority.find(']') else {
            return Err("private WS URL has invalid IPv6 host".to_string());
        };
        let h = &authority[..=end_bracket];
        let tail = &authority[end_bracket + 1..];
        if tail.is_empty() {
            (h.to_string(), if tls { 443 } else { 80 })
        } else if let Some(p_str) = tail.strip_prefix(':') {
            let port = p_str
                .parse::<u16>()
                .map_err(|_| "invalid private WS port".to_string())?;
            (h.to_string(), port)
        } else {
            return Err("invalid private WS authority".to_string());
        }
    } else if let Some((h, p_str)) = authority.rsplit_once(':') {
        let port = p_str
            .parse::<u16>()
            .map_err(|_| "invalid private WS port".to_string())?;
        (h.to_string(), port)
    } else {
        (authority.to_string(), if tls { 443 } else { 80 })
    };

    if host.is_empty() {
        return Err("private WS URL missing host".to_string());
    }
    if !tls && !is_loopback_host(&host) {
        return Err(
            "plain ws:// private WS URL is permitted only for loopback addresses; use wss://"
                .to_string(),
        );
    }
    Ok((tls, host, port, path))
}

/// Resolve the target authenticated private WebSocket URL for `cfg`, or `None`
/// when offline, unconfigured, or pointing at a plain-HTTP REST mock without `OPENBELL_PRIVATE_WS_URL`.
pub fn resolve_private_ws_url(cfg: &kalshi_live::PersistedLiveConfig) -> Option<String> {
    if private_ws_offline() || !cfg.is_configured() {
        return None;
    }
    if let Ok(raw) = std::env::var("OPENBELL_PRIVATE_WS_URL") {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return parse_ws_url(trimmed).ok().map(|_| trimmed.to_string());
        }
    }
    let rest_base = std::env::var("OPENBELL_API_BASE")
        .ok()
        .and_then(|s| kalshi_live::validate_api_base_url(&s).ok())
        .unwrap_or_else(|| cfg.effective_base_url());
    if rest_base.starts_with("https://") {
        Some(match cfg.environment {
            kalshi_live::KalshiEnv::Prod => KALSHI_PROD_WS.to_string(),
            kalshi_live::KalshiEnv::Demo => KALSHI_DEMO_WS.to_string(),
        })
    } else {
        None
    }
}

enum WsTransport {
    Plain(TcpStream),
    TlsProc {
        child: Child,
        stdin: std::process::ChildStdin,
        stdout: std::process::ChildStdout,
    },
}

impl WsTransport {
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            WsTransport::Plain(s) => s.write_all(buf),
            WsTransport::TlsProc { stdin, .. } => stdin.write_all(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            WsTransport::Plain(s) => s.flush(),
            WsTransport::TlsProc { stdin, .. } => stdin.flush(),
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            WsTransport::Plain(s) => s.read(buf),
            WsTransport::TlsProc { stdout, .. } => stdout.read(buf),
        }
    }
}

impl Drop for WsTransport {
    fn drop(&mut self) {
        if let WsTransport::TlsProc { child, .. } = self {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

struct WsConnection {
    io: WsTransport,
    leftover: Vec<u8>,
    offset: usize,
}

impl WsConnection {
    fn read_exact_bytes(&mut self, dst: &mut [u8]) -> std::io::Result<()> {
        let mut filled = 0usize;
        while filled < dst.len() {
            if self.offset < self.leftover.len() {
                let avail = self.leftover.len() - self.offset;
                let take = avail.min(dst.len() - filled);
                dst[filled..filled + take]
                    .copy_from_slice(&self.leftover[self.offset..self.offset + take]);
                self.offset += take;
                filled += take;
                continue;
            }
            let n = self.io.read(&mut dst[filled..])?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "websocket EOF",
                ));
            }
            filled += n;
        }
        Ok(())
    }

    fn send_masked_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | (opcode & 0x0f));
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | (len as u8));
        } else if len <= 65_535 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mut mask = [0u8; 4];
        let _ = getrandom::getrandom(&mut mask);
        frame.extend_from_slice(&mask);
        for (i, &b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i & 3]);
        }
        self.io
            .write_all(&frame)
            .and_then(|_| self.io.flush())
            .map_err(|e| format!("private WS write failed: {e}"))
    }

    fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.send_masked_frame(0x1, text.as_bytes())
    }

    fn send_ping(&mut self) -> Result<(), String> {
        self.send_masked_frame(0x9, b"ob-ping")
    }

    fn read_frame(&mut self) -> std::io::Result<(u8, Vec<u8>)> {
        let mut full_payload = Vec::new();
        let mut first_opcode = 0u8;
        let mut fragments = 0usize;
        loop {
            fragments += 1;
            if fragments > MAX_WS_CONTINUATION_FRAMES {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws too many continuation frames",
                ));
            }
            let mut hdr = [0u8; 2];
            self.read_exact_bytes(&mut hdr)?;
            let fin = (hdr[0] & 0x80) != 0;
            let opcode = hdr[0] & 0x0f;
            if !matches!(opcode, 0x0..=0x2 | 0x8..=0xA) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws unknown opcode",
                ));
            }
            if opcode == 0 && first_opcode == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws unexpected continuation frame",
                ));
            }
            if first_opcode != 0 && opcode != 0 && opcode < 0x8 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws interleaved data frame during continuation",
                ));
            }
            if first_opcode == 0 && opcode != 0 && opcode < 0x8 {
                first_opcode = opcode;
            }
            let masked = (hdr[1] & 0x80) != 0;
            let len7 = hdr[1] & 0x7f;
            if opcode >= 0x8 && (!fin || len7 > 125) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws invalid control frame",
                ));
            }
            let payload_len: usize = match len7 {
                0..=125 => len7 as usize,
                126 => {
                    let mut ext = [0u8; 2];
                    self.read_exact_bytes(&mut ext)?;
                    u16::from_be_bytes(ext) as usize
                }
                _ => {
                    let mut ext = [0u8; 8];
                    self.read_exact_bytes(&mut ext)?;
                    let v = u64::from_be_bytes(ext);
                    if v > MAX_WS_FRAME_BYTES as u64 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "ws frame too large",
                        ));
                    }
                    v as usize
                }
            };
            if payload_len > MAX_WS_FRAME_BYTES
                || full_payload
                    .len()
                    .checked_add(payload_len)
                    .is_none_or(|sum| sum > MAX_WS_FRAME_BYTES)
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws payload exceeds limit",
                ));
            }
            let mut mask_key = [0u8; 4];
            if masked {
                self.read_exact_bytes(&mut mask_key)?;
            }
            let start = full_payload.len();
            full_payload.resize(start + payload_len, 0);
            if payload_len > 0 {
                self.read_exact_bytes(&mut full_payload[start..])?;
                if masked {
                    for (i, b) in full_payload[start..].iter_mut().enumerate() {
                        *b ^= mask_key[i & 3];
                    }
                }
            }
            if opcode == 0x9 {
                let _ = self.send_masked_frame(0xA, &full_payload[start..]);
                full_payload.truncate(start);
                return Ok((0x9, Vec::new()));
            }
            if opcode == 0xA {
                full_payload.truncate(start);
                return Ok((0xA, Vec::new()));
            }
            if opcode == 0x8 {
                return Ok((0x8, full_payload));
            }
            if fin {
                return Ok((first_opcode, full_payload));
            }
        }
    }
}

fn connect_private_ws(
    url: &str,
    client: &kalshi_live::KalshiLiveClient,
) -> Result<WsConnection, String> {
    let (tls, host, port, path) = parse_ws_url(url)?;
    let auth_headers = client.sign_ws_handshake_headers(Some(&path))?;

    let mut io = if !tls {
        let addr = format!("{host}:{port}");
        let stream = TcpStream::connect(&addr)
            .map_err(|e| format!("private WS TCP connect failed: {e}"))?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        WsTransport::Plain(stream)
    } else {
        let target = format!("{host}:{port}");
        let mut child = Command::new("openssl")
            .args([
                "s_client",
                "-quiet",
                "-servername",
                &host,
                "-connect",
                &target,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("private WS TLS transport unavailable: {e}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "private WS TLS stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "private WS TLS stdout unavailable".to_string())?;
        WsTransport::TlsProc {
            child,
            stdin,
            stdout,
        }
    };

    let mut key_raw = [0u8; 16];
    let _ = getrandom::getrandom(&mut key_raw);
    let ws_key = base64::engine::general_purpose::STANDARD.encode(key_raw);
    let expected_accept = orderbook::compute_ws_accept_key(&ws_key);

    let host_header = if (tls && port == 443) || (!tls && port == 80) {
        host.clone()
    } else {
        format!("{host}:{port}")
    };
    let mut req = format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host_header}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {ws_key}\r\n\
         Sec-WebSocket-Version: 13\r\n"
    );
    for (k, v) in auth_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    io.write_all(req.as_bytes())
        .and_then(|_| io.flush())
        .map_err(|e| format!("private WS handshake write failed: {e}"))?;

    let mut resp_buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    let hdr_end = loop {
        if resp_buf.len() > 16_384 {
            return Err("private WS handshake response too large".to_string());
        }
        let n = io
            .read(&mut tmp)
            .map_err(|e| format!("private WS handshake read failed: {e}"))?;
        if n == 0 {
            return Err("private WS handshake closed before 101".to_string());
        }
        resp_buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = resp_buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };

    let hdr_str = std::str::from_utf8(&resp_buf[..hdr_end])
        .map_err(|_| "private WS handshake response not UTF-8".to_string())?;
    let mut lines = hdr_str.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    if !status_line.contains(" 101 ") {
        return Err(format!("private WS upgrade rejected: {status_line}"));
    }
    let mut accept_ok = false;
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("sec-websocket-accept")
                && v.trim() == expected_accept
            {
                accept_ok = true;
            }
        }
    }
    if !accept_ok {
        return Err("private WS Sec-WebSocket-Accept mismatch".to_string());
    }

    let leftover = resp_buf[hdr_end + 4..].to_vec();
    Ok(WsConnection {
        io,
        leftover,
        offset: 0,
    })
}

/// Tracks monotonic `seq` per subscription `sid` to detect missed WS frames.
#[derive(Default)]
pub struct SeqTracker {
    by_sid: HashMap<i64, u64>,
}

impl SeqTracker {
    /// Returns `true` when `seq` skips ahead of `prev + 1` (sequence gap).
    pub fn check(&mut self, sid: i64, seq: u64) -> bool {
        let gap = match self.by_sid.get(&sid) {
            Some(&prev) => seq > prev.saturating_add(1),
            None => false,
        };
        if let Some(prev) = self.by_sid.get_mut(&sid) {
            if seq > *prev {
                *prev = seq;
            }
        } else {
            self.by_sid.insert(sid, seq);
        }
        gap
    }
}

fn order_price_cents(o: &kalshi_live::KalshiOrder) -> u8 {
    if o.side.eq_ignore_ascii_case("no") {
        o.no_price.or_else(|| o.yes_price.map(|p| 100 - p))
    } else {
        o.yes_price.or_else(|| o.no_price.map(|p| 100 - p))
    }
    .unwrap_or(50)
    .clamp(1, 99) as u8
}

fn recompute_live_exposure_and_loss(st: &mut crate::LiveDeskManager) -> (f64, f64) {
    let pos_exposure: f64 = st
        .ws_positions
        .values()
        .map(|p| p.market_exposure_dollars.abs())
        .filter(|v| v.is_finite())
        .sum();
    let resting_buy_exposure: f64 = st
        .reserved_order_exposure
        .values()
        .copied()
        .filter(|v| v.is_finite() && *v >= 0.0)
        .sum();
    let open_exposure = pos_exposure + resting_buy_exposure;

    let today = risk_rails::current_utc_day();
    let bd = st.executions.utc_day_breakdown(today);
    let pos_loss: f64 = st
        .ws_positions
        .values()
        .filter(|p| p.realized_pnl_dollars.is_finite() && p.realized_pnl_dollars < 0.0)
        .map(|p| -p.realized_pnl_dollars)
        .sum();
    let prev_same_day_loss = if st.rails.utc_day_num == today {
        st.rails.daily_realized_loss_dollars
    } else {
        0.0
    };
    let effective_loss = bd
        .realized_loss_dollars
        .max(pos_loss)
        .max(prev_same_day_loss);
    (open_exposure, effective_loss)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn publish_live_exec_sse(
    event_kind: &str,
    source: &str,
    ticker: Option<&str>,
    order_id: Option<&str>,
    trade_id: Option<&str>,
    status: Option<&str>,
    count: Option<i64>,
    price_cents: Option<u8>,
    position: Option<i64>,
    snap: &risk_rails::LiveStatusSnapshot,
    ts: i64,
) {
    let payload = serde_json::json!({
        "type": "live_exec",
        "source": source,
        "event_kind": event_kind,
        "ticker": ticker,
        "order_id": order_id,
        "trade_id": trade_id,
        "status": status,
        "count": count,
        "price_cents": price_cents,
        "position": position,
        "open_exposure_dollars": snap.open_exposure_dollars,
        "daily_realized_loss_dollars": snap.daily_realized_loss_dollars,
        "private_ws_connected": snap.private_ws_connected,
        "venue_sync_mode": snap.venue_sync_mode,
        "ts": ts,
    });
    tape_hub().publish_event("live_exec", payload.to_string());
}

fn ingest_fill_event(app: &App, payload: &serde_json::Value, now: i64) -> Result<(), String> {
    let Some(fill) = kalshi_live::parse_kalshi_fill(payload) else {
        return Err("private WS fill missing ticker/order_id".to_string());
    };
    let _ = app
        .runner
        .reconcile_live_data(&[], std::slice::from_ref(&fill), &[], now);
    app.runner.notify_tick();

    let payload_cid = payload
        .get("client_order_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);

    let (inserted, snap, rec) = {
        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        let cid = payload_cid.or_else(|| st.executions.client_order_id_for(&fill.order_id));
        let rec = risk_rails::execution_record_from_fill(&fill, cid, now);
        let inserted = st.executions.append_idempotent(rec.clone());
        st.private_ws_connected = true;
        st.private_ws_last_event_epoch = Some(now);
        st.connected = true;
        st.rails.last_error = None;

        if inserted {
            let fill_notional =
                (fill.count.max(0) as f64) * (rec.price_cents as f64 / 100.0);
            if fill.action.eq_ignore_ascii_case("sell") {
                st.rails.release_exposure(fill_notional);
                if let Some(pos) = st.ws_positions.get_mut(&fill.ticker) {
                    pos.position = (pos.position.abs() - fill.count.max(0)).max(0);
                    pos.market_exposure_dollars =
                        (pos.market_exposure_dollars - fill_notional).max(0.0);
                    pos.market_exposure_cents =
                        (pos.market_exposure_dollars * 100.0).round() as i64;
                }
            } else {
                let reserved_consumed = {
                    let ord_info = st.executions.latest_order_info(&fill.order_id);
                    if let Some(rem_res) = st.reserved_order_exposure.get_mut(&fill.order_id) {
                        let expected_consume = match ord_info {
                            Some((_, _, _, _, orig_cnt)) if orig_cnt > 0 && fill.count >= orig_cnt => *rem_res,
                            Some((_, _, _, orig_px, _)) if orig_px > 0 => {
                                (fill.count.max(0) as f64) * (orig_px as f64 / 100.0)
                            }
                            _ => fill_notional,
                        };
                        let take = (*rem_res).min(expected_consume);
                        *rem_res = (*rem_res - take).max(0.0);
                        let left = *rem_res;
                        if left <= 1e-6 {
                            st.reserved_order_exposure.remove(&fill.order_id);
                        }
                        take
                    } else {
                        0.0
                    }
                };
                let delta = fill_notional - reserved_consumed;
                if delta.abs() > 1e-9 {
                    st.rails.open_exposure_dollars =
                        (st.rails.open_exposure_dollars + delta).max(0.0);
                }
                let entry =
                    st.ws_positions
                        .entry(fill.ticker.clone())
                        .or_insert_with(|| kalshi_live::KalshiPosition {
                            ticker: fill.ticker.clone(),
                            position: 0,
                            market_exposure_cents: 0,
                            market_exposure_dollars: 0.0,
                            realized_pnl_cents: 0,
                            realized_pnl_dollars: 0.0,
                            resting_orders_count: 0,
                            fees_paid_dollars: 0.0,
                            total_traded_dollars: 0.0,
                        });
                entry.position += fill.count.max(0);
                entry.market_exposure_dollars += fill_notional;
                entry.market_exposure_cents =
                    (entry.market_exposure_dollars * 100.0).round() as i64;
            }
        }
        let open_exp = st.rails.open_exposure_dollars;
        let loss = st.rails.daily_realized_loss_dollars;
        st.rails.record_venue_sync(now, open_exp, loss);
        let snap = st.status_snapshot();
        (inserted, snap, rec)
    };

    if inserted {
        app.invalidate_live_hot();
        publish_live_exec_sse(
            "fill",
            "private_ws",
            Some(&rec.ticker),
            Some(&rec.order_id),
            rec.trade_id.as_deref(),
            None,
            Some(rec.count),
            Some(rec.price_cents),
            None,
            &snap,
            now,
        );
    }
    Ok(())
}

fn ingest_order_event(app: &App, payload: &serde_json::Value, now: i64) -> Result<(), String> {
    let Some(ord) = kalshi_live::parse_kalshi_order(payload) else {
        return Err("private WS order missing order_id".to_string());
    };
    let _ = app
        .runner
        .reconcile_live_data(std::slice::from_ref(&ord), &[], &[], now);
    app.runner.notify_tick();

    let price_c = order_price_cents(&ord);
    let snap = {
        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        st.private_ws_connected = true;
        st.private_ws_last_event_epoch = Some(now);
        st.connected = true;
        st.rails.last_error = None;

        if ord.status.eq_ignore_ascii_case("canceled")
            || ord.status.eq_ignore_ascii_case("cancelled")
        {
            let removed = st.reserved_order_exposure.remove(&ord.order_id);
            let released = if let Some(r) = removed {
                r
            } else if ord.action.eq_ignore_ascii_case("buy") {
                if ord.remaining_count > 0 {
                    (ord.remaining_count as f64) * (price_c as f64 / 100.0)
                } else {
                    st.executions
                        .submitted_notional_for(&ord.order_id)
                        .unwrap_or(0.0)
                }
            } else {
                0.0
            };
            st.rails.release_exposure(released);
            let rec = risk_rails::execution_record_from_canceled_order(&ord, now);
            st.executions.append_idempotent(rec);
        } else if ord.status.eq_ignore_ascii_case("executed") || ord.remaining_count <= 0 {
            st.reserved_order_exposure.remove(&ord.order_id);
        } else if ord.status.eq_ignore_ascii_case("resting")
            && ord.action.eq_ignore_ascii_case("buy")
            && ord.remaining_count > 0
        {
            let rem_notional = (ord.remaining_count as f64) * (price_c as f64 / 100.0);
            if let Some(prev) = st
                .reserved_order_exposure
                .insert(ord.order_id.clone(), rem_notional)
            {
                let diff = rem_notional - prev;
                st.rails.open_exposure_dollars =
                    (st.rails.open_exposure_dollars + diff).max(0.0);
            }
        }

        let open_exp = st.rails.open_exposure_dollars;
        let loss = st.rails.daily_realized_loss_dollars;
        st.rails.record_venue_sync(now, open_exp, loss);
        st.status_snapshot()
    };

    app.invalidate_live_hot();
    publish_live_exec_sse(
        "order",
        "private_ws",
        Some(&ord.ticker),
        Some(&ord.order_id),
        None,
        Some(&ord.status),
        Some(ord.remaining_count),
        Some(price_c),
        None,
        &snap,
        now,
    );
    Ok(())
}

fn ingest_position_event(app: &App, payload: &serde_json::Value, now: i64) -> Result<(), String> {
    let Some(pos) = kalshi_live::parse_kalshi_position(payload) else {
        return Err("private WS position missing ticker".to_string());
    };
    app.runner.notify_tick();

    let snap = {
        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        st.private_ws_connected = true;
        st.private_ws_last_event_epoch = Some(now);
        st.connected = true;
        st.rails.last_error = None;
        st.ws_positions.insert(pos.ticker.clone(), pos.clone());
        let (open_exp, eff_loss) = recompute_live_exposure_and_loss(&mut st);
        st.rails.record_venue_sync(now, open_exp, eff_loss);
        st.status_snapshot()
    };

    app.invalidate_live_hot();
    publish_live_exec_sse(
        "position",
        "private_ws",
        Some(&pos.ticker),
        None,
        None,
        None,
        None,
        None,
        Some(pos.position),
        &snap,
        now,
    );
    Ok(())
}

/// Ingest a single JSON text frame received from Kalshi's private WebSocket stream.
pub(crate) fn ingest_private_ws_message(
    app: &App,
    text: &str,
    seq_tracker: &mut SeqTracker,
) -> Result<(), String> {
    let val: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("private WS JSON parse error: {e}"))?;
    let now = now_unix();

    let sid = val.get("sid").and_then(|v| v.as_i64()).unwrap_or(0);
    if let Some(seq) = val.get("seq").and_then(|v| v.as_u64()) {
        if seq_tracker.check(sid, seq) {
            {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.rails.last_error = Some(format!(
                    "private WS sequence gap detected (sid={sid}, seq={seq}); triggering REST reconcile"
                ));
            }
            app.notify_reconcile_immediate();
        }
    }

    let msg_type = val
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let payload = val.get("msg").or_else(|| val.get("data")).unwrap_or(&val);

    match msg_type.as_str() {
        "fill" => ingest_fill_event(app, payload, now),
        "user_order" | "user_orders" | "order" => ingest_order_event(app, payload, now),
        "market_position" | "market_positions" | "position" => {
            ingest_position_event(app, payload, now)
        }
        "order_group_update" | "order_group_updates" => {
            let triggered = payload
                .get("event_type")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.eq_ignore_ascii_case("triggered"));
            let snap = {
                let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                st.private_ws_connected = true;
                st.private_ws_last_event_epoch = Some(now);
                st.connected = true;
                st.status_snapshot()
            };
            if triggered {
                app.notify_reconcile_immediate();
            }
            app.runner.notify_tick();
            app.invalidate_live_hot();
            publish_live_exec_sse(
                "order_group",
                "private_ws",
                None,
                None,
                None,
                payload.get("event_type").and_then(|v| v.as_str()),
                None,
                None,
                None,
                &snap,
                now,
            );
            Ok(())
        }
        "error" => {
            let err_msg = payload
                .get("msg")
                .or_else(|| payload.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("private WS stream error")
                .to_string();
            let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
            st.rails.last_error = Some(err_msg);
            Ok(())
        }
        "subscribed" | "ok" | "unsubscribed" | "heartbeat" => {
            let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
            st.private_ws_connected = true;
            st.private_ws_last_event_epoch = Some(now);
            Ok(())
        }
        _ => Ok(()),
    }
}

fn mark_ws_disconnected(app: &App, err: Option<String>, trigger_reconcile: bool) {
    {
        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        st.private_ws_connected = false;
        if let Some(e) = err {
            st.rails.last_error = Some(e);
        }
    }
    app.invalidate_live_hot();
    if trigger_reconcile {
        app.notify_reconcile_immediate();
    }
}

fn wait_private_ws_or_timeout(app: &App, last_seen_seq: u64, dur: Option<Duration>) {
    let (lock, cv) = &*app.private_ws_wake;
    let mut seq = lock.lock().unwrap_or_else(|p| p.into_inner());
    if *seq != last_seen_seq {
        return;
    }
    match dur {
        None => {
            while *seq == last_seen_seq {
                seq = cv.wait(seq).unwrap_or_else(|p| p.into_inner());
            }
        }
        Some(d) => {
            let _ = cv.wait_timeout(seq, d);
        }
    }
}

/// Start the background authenticated Kalshi Trade API v2 Private WebSocket worker.
/// Parks idle at 0% CPU / 0 network when offline or unconfigured.
pub fn start_background_private_ws(app: Arc<App>) {
    std::thread::spawn(move || {
        let mut backoff_ms = 1_000u64;
        let mut next_attempt_ms = 0i64;

        loop {
            let (cfg_opt, ws_url_opt, last_seen_wake) = {
                let (lock, _) = &*app.private_ws_wake;
                let wake_seq = *lock.lock().unwrap_or_else(|p| p.into_inner());
                let st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                if st.config.is_configured() {
                    let url = resolve_private_ws_url(&st.config);
                    (Some(st.config.clone()), url, wake_seq)
                } else {
                    (None, None, wake_seq)
                }
            };

            let (Some(cfg), Some(ws_url)) = (cfg_opt, ws_url_opt) else {
                mark_ws_disconnected(&app, None, false);
                wait_private_ws_or_timeout(&app, last_seen_wake, None);
                backoff_ms = 1_000;
                next_attempt_ms = 0;
                continue;
            };

            let now = now_ms();
            if now < next_attempt_ms {
                let wait_dur = Duration::from_millis((next_attempt_ms - now).max(20) as u64);
                wait_private_ws_or_timeout(&app, last_seen_wake, Some(wait_dur));
                continue;
            }

            let client = match kalshi_live::KalshiLiveClient::from_config(&cfg) {
                Ok(c) => c,
                Err(e) => {
                    mark_ws_disconnected(&app, Some(e), false);
                    backoff_ms = orderbook::next_ws_reconnect_backoff_ms(backoff_ms, false);
                    next_attempt_ms = now_ms() + backoff_ms as i64;
                    continue;
                }
            };

            match connect_private_ws(&ws_url, &client) {
                Ok(mut conn) => {
                    let mut session_valid_msg = false;
                    let mut consecutive_bad_frames = 0usize;
                    let mut seq_tracker = SeqTracker::default();
                    let mut last_rx = Instant::now();
                    let mut last_ping_sent: Option<Instant> = None;

                    {
                        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        st.private_ws_connected = true;
                        st.rails.last_error = None;
                    }
                    app.invalidate_live_hot();
                    app.notify_reconcile_reschedule();

                    let sub_cmd = serde_json::json!({
                        "id": 1,
                        "cmd": "subscribe",
                        "params": {
                            "channels": [
                                "fill",
                                "market_positions",
                                "order_group_updates",
                                "user_orders"
                            ]
                        }
                    });
                    if let Err(e) = conn.send_text(&sub_cmd.to_string()) {
                        mark_ws_disconnected(&app, Some(e), true);
                    } else {
                        loop {
                            let wake_now = {
                                let (lock, _) = &*app.private_ws_wake;
                                *lock.lock().unwrap_or_else(|p| p.into_inner())
                            };
                            if wake_now != last_seen_wake || private_ws_offline() {
                                mark_ws_disconnected(&app, None, false);
                                break;
                            }

                            if last_rx.elapsed() >= Duration::from_secs(KEEPALIVE_TIMEOUT_SECS) {
                                mark_ws_disconnected(
                                    &app,
                                    Some("private WS keepalive timeout".to_string()),
                                    true,
                                );
                                break;
                            }
                            if last_rx.elapsed() >= Duration::from_secs(KEEPALIVE_PING_SECS)
                                && last_ping_sent
                                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(KEEPALIVE_PING_SECS))
                            {
                                last_ping_sent = Some(Instant::now());
                                if let Err(e) = conn.send_ping() {
                                    mark_ws_disconnected(&app, Some(e), true);
                                    break;
                                }
                            }

                            match conn.read_frame() {
                                Ok((0x1, payload)) => {
                                    last_rx = Instant::now();
                                    match std::str::from_utf8(&payload) {
                                        Ok(txt) => {
                                            if let Err(e) = ingest_private_ws_message(
                                                &app,
                                                txt,
                                                &mut seq_tracker,
                                            ) {
                                                consecutive_bad_frames += 1;
                                                if consecutive_bad_frames
                                                    >= MAX_CONSECUTIVE_MALFORMED_WS_FRAMES
                                                {
                                                    mark_ws_disconnected(
                                                        &app,
                                                        Some(format!(
                                                            "private WS malformed frame limit exceeded: {e}"
                                                        )),
                                                        true,
                                                    );
                                                    break;
                                                }
                                                let mut st = app
                                                    .live_desk
                                                    .lock()
                                                    .unwrap_or_else(|p| p.into_inner());
                                                st.rails.last_error = Some(e);
                                            } else {
                                                session_valid_msg = true;
                                                consecutive_bad_frames = 0;
                                                backoff_ms = 1_000;
                                            }
                                        }
                                        Err(_) => {
                                            consecutive_bad_frames += 1;
                                            if consecutive_bad_frames
                                                >= MAX_CONSECUTIVE_MALFORMED_WS_FRAMES
                                            {
                                                mark_ws_disconnected(
                                                    &app,
                                                    Some(
                                                        "private WS invalid UTF-8 frame limit exceeded"
                                                            .to_string(),
                                                    ),
                                                    true,
                                                );
                                                break;
                                            }
                                            let mut st = app
                                                .live_desk
                                                .lock()
                                                .unwrap_or_else(|p| p.into_inner());
                                            st.rails.last_error = Some(
                                                "invalid UTF-8 in private WS text frame"
                                                    .to_string(),
                                            );
                                        }
                                    }
                                }
                                Ok((0x9, _)) => {
                                    last_rx = Instant::now();
                                    session_valid_msg = true;
                                }
                                Ok((0xA, _)) => {
                                    last_rx = Instant::now();
                                    session_valid_msg = true;
                                    let mut st =
                                        app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                                    if st.rails.last_venue_sync_epoch.is_some() {
                                        let ts = now_unix();
                                        let open_exp = st.rails.open_exposure_dollars;
                                        let loss = st.rails.daily_realized_loss_dollars;
                                        st.rails.record_venue_sync(ts, open_exp, loss);
                                    }
                                }
                                Ok((0x8, _)) => {
                                    mark_ws_disconnected(
                                        &app,
                                        Some("private WS closed by remote".to_string()),
                                        true,
                                    );
                                    break;
                                }
                                Ok(_) => {
                                    last_rx = Instant::now();
                                }
                                Err(ref e)
                                    if matches!(
                                        e.kind(),
                                        std::io::ErrorKind::WouldBlock
                                            | std::io::ErrorKind::TimedOut
                                    ) =>
                                {
                                    continue;
                                }
                                Err(e) => {
                                    mark_ws_disconnected(
                                        &app,
                                        Some(format!("private WS stream error: {e}")),
                                        true,
                                    );
                                    break;
                                }
                            }
                        }
                    }
                    backoff_ms =
                        orderbook::next_ws_reconnect_backoff_ms(backoff_ms, session_valid_msg);
                    next_attempt_ms = now_ms() + backoff_ms as i64;
                }
                Err(e) => {
                    mark_ws_disconnected(&app, Some(e), true);
                    next_attempt_ms = now_ms() + backoff_ms as i64;
                    backoff_ms = orderbook::next_ws_reconnect_backoff_ms(backoff_ms, false);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ws_url_enforces_loopback_on_plain_ws() {
        assert!(parse_ws_url("ws://127.0.0.1:8080/trade-api/ws/v2").is_ok());
        assert!(parse_ws_url("ws://localhost:9000").unwrap().3 == "/trade-api/ws/v2");
        assert!(parse_ws_url("ws://[::1]:9000/trade-api/ws/v2").is_ok());
        assert!(parse_ws_url("ws://api.elections.kalshi.com/trade-api/ws/v2").is_err());
        assert!(parse_ws_url("wss://api.elections.kalshi.com/trade-api/ws/v2").is_ok());
        assert!(parse_ws_url("http://127.0.0.1:8080").is_err());
    }

    #[test]
    fn resolve_private_ws_url_skips_when_unconfigured() {
        let cfg = kalshi_live::PersistedLiveConfig::default();
        assert!(resolve_private_ws_url(&cfg).is_none());
    }

    #[test]
    fn seq_tracker_detects_gaps() {
        let mut tracker = SeqTracker::default();
        assert!(!tracker.check(1, 1));
        assert!(!tracker.check(1, 2));
        assert!(tracker.check(1, 4));
        assert!(!tracker.check(1, 5));
        assert!(!tracker.check(2, 1));
    }
}
