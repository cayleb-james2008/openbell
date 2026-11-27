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
use std::net::{TcpStream, ToSocketAddrs};
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
const WS_CONNECT_TIMEOUT_SECS: u64 = 5;
const WS_HANDSHAKE_TIMEOUT_SECS: u64 = 5;
const WS_RECONNECT_INITIAL_MS: u64 = 1_000;
const WS_RECONNECT_MAX_MS: u64 = 30_000;

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

/// Parse and validate a WebSocket URL (`wss://` anywhere, `ws://` loopback-only),
/// using `default_path` when the path component is omitted.
pub(crate) fn parse_ws_url_with_default(
    url: &str,
    default_path: &str,
) -> Result<(bool, String, u16, String), String> {
    let trimmed = url.trim();
    if trimmed.is_empty()
        || trimmed.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err("WS URL is empty or contains invalid characters".to_string());
    }
    let (tls, rest) = if let Some(r) = trimmed.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err("WS URL must start with wss:// or ws://".to_string());
    };

    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, default_path.to_string()),
    };

    let (host, port) = if authority.starts_with('[') {
        let Some(end_bracket) = authority.find(']') else {
            return Err("WS URL has invalid IPv6 host".to_string());
        };
        let h = &authority[..=end_bracket];
        let tail = &authority[end_bracket + 1..];
        if tail.is_empty() {
            (h.to_string(), if tls { 443 } else { 80 })
        } else if let Some(p_str) = tail.strip_prefix(':') {
            let port = p_str
                .parse::<u16>()
                .map_err(|_| "invalid WS port".to_string())?;
            (h.to_string(), port)
        } else {
            return Err("invalid WS authority".to_string());
        }
    } else if let Some((h, p_str)) = authority.rsplit_once(':') {
        let port = p_str
            .parse::<u16>()
            .map_err(|_| "invalid WS port".to_string())?;
        (h.to_string(), port)
    } else {
        (authority.to_string(), if tls { 443 } else { 80 })
    };

    if host.is_empty() {
        return Err("WS URL missing host".to_string());
    }
    if !tls && !is_loopback_host(&host) {
        return Err(
            "plain ws:// URL is permitted only for loopback addresses; use wss://".to_string(),
        );
    }
    Ok((tls, host, port, path))
}

/// Parse and validate a private WebSocket URL (`wss://` anywhere, `ws://` loopback-only).
/// Defaults an omitted path to `/trade-api/ws/v2`.
pub fn parse_ws_url(url: &str) -> Result<(bool, String, u16, String), String> {
    parse_ws_url_with_default(url, DEFAULT_WS_PATH)
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

pub(crate) enum WsTransport {
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

/// Validates RFC 6455 section 7.4 close status codes allowed on the wire.
pub(crate) fn is_valid_ws_close_code(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999)
}

/// Formats a received RFC 6455 close frame payload `(code, reason)` into a safe diagnostic string.
pub(crate) fn parse_ws_close_info(payload: &[u8]) -> (Option<u16>, String) {
    if payload.len() < 2 {
        return (None, String::new());
    }
    let code = u16::from_be_bytes([payload[0], payload[1]]);
    let reason = if payload.len() > 2 {
        String::from_utf8_lossy(&payload[2..]).trim().to_string()
    } else {
        String::new()
    };
    (Some(code), reason)
}

/// Computes `(next_base_backoff_ms, jittered_delay_ms)` for bounded exponential WS reconnects.
pub fn next_ws_reconnect_delay_ms(current_backoff_ms: u64, had_valid_message: bool) -> (u64, u64) {
    let next_base = if had_valid_message {
        WS_RECONNECT_INITIAL_MS
    } else {
        current_backoff_ms
            .max(WS_RECONNECT_INITIAL_MS)
            .saturating_mul(2)
            .min(WS_RECONNECT_MAX_MS)
    };
    let mut rand_bytes = [0u8; 8];
    let _ = getrandom::getrandom(&mut rand_bytes);
    let rand_u64 = u64::from_le_bytes(rand_bytes);
    // Symmetric +/-12.5% bounded jitter around `next_base` (clamped 250ms..=30s).
    let span = (next_base / 4).max(1);
    let half = span / 2;
    let offset = rand_u64 % (span + 1);
    let jittered = next_base
        .saturating_sub(half)
        .saturating_add(offset)
        .clamp(250, WS_RECONNECT_MAX_MS);
    (next_base, jittered)
}

pub(crate) struct WsConnection {
    io: WsTransport,
    leftover: Vec<u8>,
    offset: usize,
    frag_opcode: u8,
    frag_buf: Vec<u8>,
    frag_count: usize,
    max_frame_bytes: usize,
    max_continuation_frames: usize,
}

impl WsConnection {
    pub(crate) fn from_plain_stream(
        stream: TcpStream,
        leftover: Vec<u8>,
        max_frame_bytes: usize,
        max_continuation_frames: usize,
    ) -> Self {
        Self {
            io: WsTransport::Plain(stream),
            leftover,
            offset: 0,
            frag_opcode: 0,
            frag_buf: Vec::new(),
            frag_count: 0,
            max_frame_bytes: max_frame_bytes.max(1024),
            max_continuation_frames: max_continuation_frames.max(1),
        }
    }

    fn ensure_buffered(&mut self, cursor: usize, need: usize) -> std::io::Result<()> {
        let mut tmp = [0u8; 4096];
        while self.leftover.len().saturating_sub(cursor) < need {
            let n = self.io.read(&mut tmp)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "websocket EOF",
                ));
            }
            self.leftover.extend_from_slice(&tmp[..n]);
        }
        Ok(())
    }

    pub(crate) fn send_masked_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        let op = opcode & 0x0f;
        if op >= 0x8 && payload.len() > 125 {
            return Err("WS control frame payload exceeds 125 bytes".to_string());
        }
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | op);
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
            .map_err(|e| format!("WS write failed: {e}"))
    }

    pub(crate) fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.send_masked_frame(0x1, text.as_bytes())
    }

    pub(crate) fn send_ping(&mut self) -> Result<(), String> {
        self.send_masked_frame(0x9, b"ob-ping")
    }

    /// Reads one complete WS frame (reassembling fragmented data frames and handling
    /// interleaved RFC 6455 Ping/Pong/Close control frames without losing partial reads
    /// across `WouldBlock`/`TimedOut`).
    pub(crate) fn read_frame(&mut self) -> std::io::Result<(u8, Vec<u8>)> {
        loop {
            if self.offset >= self.leftover.len() {
                self.leftover.clear();
                self.offset = 0;
            } else if self.offset > 4096 {
                self.leftover.drain(..self.offset);
                self.offset = 0;
            }

            let mut cursor = self.offset;
            self.ensure_buffered(cursor, 2)?;
            let hdr = [self.leftover[cursor], self.leftover[cursor + 1]];
            cursor += 2;

            let fin = (hdr[0] & 0x80) != 0;
            if (hdr[0] & 0x70) != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws reserved bits set",
                ));
            }
            let opcode = hdr[0] & 0x0f;
            if !matches!(opcode, 0x0..=0x2 | 0x8..=0xA) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws unknown opcode",
                ));
            }
            if opcode == 0 && self.frag_opcode == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws unexpected continuation frame",
                ));
            }
            if self.frag_opcode != 0 && opcode != 0 && opcode < 0x8 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws interleaved data frame during continuation",
                ));
            }

            let masked = (hdr[1] & 0x80) != 0;
            if masked {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws masked frame from server",
                ));
            }
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
                    self.ensure_buffered(cursor, 2)?;
                    let len = u16::from_be_bytes([
                        self.leftover[cursor],
                        self.leftover[cursor + 1],
                    ]) as usize;
                    cursor += 2;
                    len
                }
                _ => {
                    self.ensure_buffered(cursor, 8)?;
                    let mut ext = [0u8; 8];
                    ext.copy_from_slice(&self.leftover[cursor..cursor + 8]);
                    cursor += 8;
                    let v = u64::from_be_bytes(ext);
                    if (v & (1u64 << 63)) != 0 || v > self.max_frame_bytes as u64 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "ws frame too large",
                        ));
                    }
                    v as usize
                }
            };

            if opcode < 0x8 {
                if self.frag_count + 1 > self.max_continuation_frames {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "ws too many continuation frames",
                    ));
                }
                if payload_len > self.max_frame_bytes
                    || self
                        .frag_buf
                        .len()
                        .checked_add(payload_len)
                        .is_none_or(|sum| sum > self.max_frame_bytes)
                {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "ws payload exceeds limit",
                    ));
                }
            }

            self.ensure_buffered(cursor, payload_len)?;
            let payload_start = cursor;
            cursor += payload_len;
            // Commit frame offset only once the full frame payload is present.
            self.offset = cursor;

            if opcode == 0x9 {
                let ping_payload = self.leftover[payload_start..cursor].to_vec();
                let _ = self.send_masked_frame(0xA, &ping_payload);
                return Ok((0x9, ping_payload));
            }
            if opcode == 0xA {
                let pong_payload = self.leftover[payload_start..cursor].to_vec();
                return Ok((0xA, pong_payload));
            }
            if opcode == 0x8 {
                let close_payload = self.leftover[payload_start..cursor].to_vec();
                self.frag_opcode = 0;
                self.frag_buf.clear();
                self.frag_count = 0;
                if close_payload.len() == 1 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "ws invalid 1-byte close payload",
                    ));
                }
                if close_payload.len() >= 2 {
                    let code = u16::from_be_bytes([close_payload[0], close_payload[1]]);
                    if !is_valid_ws_close_code(code) {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("ws invalid close code {code}"),
                        ));
                    }
                    if close_payload.len() > 2 && std::str::from_utf8(&close_payload[2..]).is_err()
                    {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "ws close reason is not valid UTF-8",
                        ));
                    }
                    let _ = self.send_masked_frame(0x8, &close_payload[..2]);
                } else {
                    let _ = self.send_masked_frame(0x8, &1000u16.to_be_bytes());
                }
                return Ok((0x8, close_payload));
            }

            self.frag_count += 1;
            if self.frag_opcode == 0 {
                self.frag_opcode = opcode;
            }
            self.frag_buf
                .extend_from_slice(&self.leftover[payload_start..cursor]);
            if fin {
                let out_opcode = self.frag_opcode;
                let out_payload = std::mem::take(&mut self.frag_buf);
                self.frag_opcode = 0;
                self.frag_count = 0;
                return Ok((out_opcode, out_payload));
            }
        }
    }
}

pub(crate) fn connect_ws_with_headers(
    url: &str,
    default_path: &str,
    extra_headers: &[(String, String)],
    max_frame_bytes: usize,
    max_continuation_frames: usize,
    label: &str,
) -> Result<WsConnection, String> {
    let (tls, host, port, path) = parse_ws_url_with_default(url, default_path)?;

    let mut io = if !tls {
        let addr_str = format!("{host}:{port}");
        let addrs: Vec<_> = addr_str
            .to_socket_addrs()
            .map_err(|e| format!("{label} DNS resolve failed: {e}"))?
            .collect();
        let mut last_err = None;
        let mut connected = None;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, Duration::from_secs(WS_CONNECT_TIMEOUT_SECS)) {
                Ok(s) => {
                    connected = Some(s);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let stream = connected.ok_or_else(|| {
            format!(
                "{label} TCP connect failed: {}",
                last_err
                    .map(|e| e.to_string())
                    .unwrap_or_else(|| "no resolved addresses".to_string())
            )
        })?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        WsTransport::Plain(stream)
    } else {
        let clean_host = host.trim_start_matches('[').trim_end_matches(']');
        let target = format!("{host}:{port}");
        let mut child = Command::new("openssl")
            .args([
                "s_client",
                "-quiet",
                "-servername",
                clean_host,
                "-connect",
                &target,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("{label} TLS transport unavailable: {e}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{label} TLS stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("{label} TLS stdout unavailable"))?;
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
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    io.write_all(req.as_bytes())
        .and_then(|_| io.flush())
        .map_err(|e| format!("{label} handshake write failed: {e}"))?;

    let mut resp_buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    let deadline = Instant::now() + Duration::from_secs(WS_HANDSHAKE_TIMEOUT_SECS);
    let hdr_end = loop {
        if resp_buf.len() > 16_384 {
            return Err(format!("{label} handshake response too large"));
        }
        match io.read(&mut tmp) {
            Ok(0) => return Err(format!("{label} handshake closed before 101")),
            Ok(n) => {
                resp_buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = resp_buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
            }
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(format!("{label} handshake timed out"));
                }
                continue;
            }
            Err(e) => return Err(format!("{label} handshake read failed: {e}")),
        }
    };

    let hdr_str = std::str::from_utf8(&resp_buf[..hdr_end])
        .map_err(|_| format!("{label} handshake response not UTF-8"))?;
    let mut lines = hdr_str.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    if status_line.split_whitespace().nth(1) != Some("101") {
        return Err(format!("{label} upgrade rejected: {status_line}"));
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
        return Err(format!("{label} Sec-WebSocket-Accept mismatch"));
    }

    let leftover = resp_buf[hdr_end + 4..].to_vec();
    Ok(WsConnection {
        io,
        leftover,
        offset: 0,
        frag_opcode: 0,
        frag_buf: Vec::new(),
        frag_count: 0,
        max_frame_bytes: max_frame_bytes.max(1024),
        max_continuation_frames: max_continuation_frames.max(1),
    })
}

fn connect_private_ws(
    url: &str,
    client: &kalshi_live::KalshiLiveClient,
) -> Result<WsConnection, String> {
    let (_, _, _, path) = parse_ws_url(url)?;
    let auth_headers = client.sign_ws_handshake_headers(Some(&path))?;
    connect_ws_with_headers(
        url,
        DEFAULT_WS_PATH,
        &auth_headers,
        MAX_WS_FRAME_BYTES,
        MAX_WS_CONTINUATION_FRAMES,
        "private WS",
    )
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
    if fill.count <= 0 {
        return Err("private WS fill has non-positive count".to_string());
    }

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
            let signed_delta = if fill.side.eq_ignore_ascii_case("no") {
                -fill.count
            } else {
                fill.count
            };
            if fill.action.eq_ignore_ascii_case("sell") {
                st.rails.release_exposure(fill_notional);
                if let Some(pos) = st.ws_positions.get_mut(&fill.ticker) {
                    pos.position -= signed_delta;
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
                entry.position += signed_delta;
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
        let _ = app
            .runner
            .reconcile_live_data(&[], std::slice::from_ref(&fill), &[], now);
        app.runner.notify_tick();
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

fn is_terminal_order_status(status_lc: &str) -> bool {
    matches!(status_lc, "canceled" | "cancelled" | "executed")
}

fn ingest_order_event(app: &App, payload: &serde_json::Value, now: i64) -> Result<(), String> {
    let Some(ord) = kalshi_live::parse_kalshi_order(payload) else {
        return Err("private WS order missing order_id".to_string());
    };
    let status_lc = ord.status.trim().to_ascii_lowercase();
    let price_c = order_price_cents(&ord);

    let (should_emit, snap) = {
        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        st.private_ws_connected = true;
        st.private_ws_last_event_epoch = Some(now);
        st.connected = true;
        st.rails.last_error = None;

        let prev = st.ws_orders.get(&ord.order_id).cloned();
        let prev_terminal = prev
            .as_ref()
            .is_some_and(|(s, _, _)| is_terminal_order_status(s));
        let exact_dup = prev
            .as_ref()
            .is_some_and(|(s, rem, px)| s == &status_lc && *rem == ord.remaining_count && *px == price_c);
        let stale_after_terminal = prev_terminal && !is_terminal_order_status(&status_lc);

        let mut changed = !exact_dup && !stale_after_terminal;
        if !exact_dup && !stale_after_terminal {
            if status_lc == "canceled" || status_lc == "cancelled" {
                let removed = st.reserved_order_exposure.remove(&ord.order_id);
                let rec = risk_rails::execution_record_from_canceled_order(&ord, now);
                let inserted = st.executions.append_idempotent(rec);
                let released = if let Some(r) = removed {
                    r
                } else if inserted && ord.action.eq_ignore_ascii_case("buy") {
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
                if released > 0.0 {
                    st.rails.release_exposure(released);
                }
                changed = inserted || removed.is_some() || !prev_terminal;
            } else if status_lc == "executed" || ord.remaining_count <= 0 {
                st.reserved_order_exposure.remove(&ord.order_id);
            } else if status_lc == "resting"
                && ord.action.eq_ignore_ascii_case("buy")
                && ord.remaining_count > 0
            {
                let rem_notional = (ord.remaining_count as f64) * (price_c as f64 / 100.0);
                if let Some(prev_res) = st
                    .reserved_order_exposure
                    .insert(ord.order_id.clone(), rem_notional)
                {
                    let diff = rem_notional - prev_res;
                    st.rails.open_exposure_dollars =
                        (st.rails.open_exposure_dollars + diff).max(0.0);
                }
            }
            if st.ws_orders.len() > 20_000 {
                st.ws_orders.retain(|_, (s, _, _)| !is_terminal_order_status(s));
            }
            st.ws_orders
                .insert(ord.order_id.clone(), (status_lc.clone(), ord.remaining_count, price_c));
        }

        let open_exp = st.rails.open_exposure_dollars;
        let loss = st.rails.daily_realized_loss_dollars;
        st.rails.record_venue_sync(now, open_exp, loss);
        (changed, st.status_snapshot())
    };

    if should_emit {
        let _ = app
            .runner
            .reconcile_live_data(std::slice::from_ref(&ord), &[], &[], now);
        app.runner.notify_tick();
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
    }
    Ok(())
}

fn position_unchanged(
    prev: &kalshi_live::KalshiPosition,
    next: &kalshi_live::KalshiPosition,
) -> bool {
    prev.position == next.position
        && prev.market_exposure_cents == next.market_exposure_cents
        && prev.realized_pnl_cents == next.realized_pnl_cents
        && prev.resting_orders_count == next.resting_orders_count
        && (prev.market_exposure_dollars - next.market_exposure_dollars).abs() <= 1e-6
        && (prev.realized_pnl_dollars - next.realized_pnl_dollars).abs() <= 1e-6
        && (prev.fees_paid_dollars - next.fees_paid_dollars).abs() <= 1e-6
        && (prev.total_traded_dollars - next.total_traded_dollars).abs() <= 1e-6
}

fn ingest_position_event(app: &App, payload: &serde_json::Value, now: i64) -> Result<(), String> {
    let Some(pos) = kalshi_live::parse_kalshi_position(payload) else {
        return Err("private WS position missing ticker".to_string());
    };

    let (changed, snap) = {
        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        st.private_ws_connected = true;
        st.private_ws_last_event_epoch = Some(now);
        st.connected = true;
        st.rails.last_error = None;
        let changed = st
            .ws_positions
            .get(&pos.ticker)
            .is_none_or(|prev| !position_unchanged(prev, &pos));
        if changed {
            st.ws_positions.insert(pos.ticker.clone(), pos.clone());
            let (open_exp, eff_loss) = recompute_live_exposure_and_loss(&mut st);
            st.rails.record_venue_sync(now, open_exp, eff_loss);
        } else {
            let open_exp = st.rails.open_exposure_dollars;
            let loss = st.rails.daily_realized_loss_dollars;
            st.rails.record_venue_sync(now, open_exp, loss);
        }
        (changed, st.status_snapshot())
    };

    if changed {
        app.runner.notify_tick();
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
    }
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
    let (was_connected, snap) = {
        let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        let was = st.private_ws_connected;
        st.private_ws_connected = false;
        if let Some(e) = err {
            st.rails.last_error = Some(e);
        }
        (was, st.status_snapshot())
    };
    app.invalidate_live_hot();
    if was_connected && trigger_reconcile {
        publish_live_exec_sse(
            "ws_disconnected",
            "private_ws",
            None,
            None,
            None,
            Some("disconnected"),
            None,
            None,
            None,
            &snap,
            now_unix(),
        );
    }
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
        let mut backoff_ms = WS_RECONNECT_INITIAL_MS;
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
                backoff_ms = WS_RECONNECT_INITIAL_MS;
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
                    let (next_b, delay) = next_ws_reconnect_delay_ms(backoff_ms, false);
                    backoff_ms = next_b;
                    next_attempt_ms = now_ms() + delay as i64;
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
                        {
                            let mut st = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                            st.private_ws_connected = true;
                            st.rails.last_error = None;
                        }
                        app.invalidate_live_hot();
                        app.notify_reconcile_reschedule();

                        loop {
                            let wake_now = {
                                let (lock, _) = &*app.private_ws_wake;
                                *lock.lock().unwrap_or_else(|p| p.into_inner())
                            };
                            if wake_now != last_seen_wake || private_ws_offline() {
                                let _ = conn.send_masked_frame(0x8, &1000u16.to_be_bytes());
                                mark_ws_disconnected(&app, None, false);
                                break;
                            }

                            if last_rx.elapsed() >= Duration::from_secs(KEEPALIVE_TIMEOUT_SECS) {
                                let _ = conn.send_masked_frame(0x8, &1001u16.to_be_bytes());
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
                                                backoff_ms = WS_RECONNECT_INITIAL_MS;
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
                                Ok((0x8, close_payload)) => {
                                    let msg = match parse_ws_close_info(&close_payload) {
                                        (Some(code), reason) if !reason.is_empty() => format!(
                                            "private WS closed by remote (code {code}: {reason})"
                                        ),
                                        (Some(code), _) => {
                                            format!("private WS closed by remote (code {code})")
                                        }
                                        (None, _) => "private WS closed by remote".to_string(),
                                    };
                                    mark_ws_disconnected(&app, Some(msg), true);
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
                    let (next_b, delay) = next_ws_reconnect_delay_ms(backoff_ms, session_valid_msg);
                    backoff_ms = next_b;
                    next_attempt_ms = now_ms() + delay as i64;
                }
                Err(e) => {
                    mark_ws_disconnected(&app, Some(e), true);
                    let (next_b, delay) = next_ws_reconnect_delay_ms(backoff_ms, false);
                    backoff_ms = next_b;
                    next_attempt_ms = now_ms() + delay as i64;
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

    fn raw_ws_frame(fin: bool, rsv: u8, opcode: u8, masked: bool, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.push((if fin { 0x80 } else { 0 }) | (rsv & 0x70) | (opcode & 0x0f));
        let mask_bit = if masked { 0x80 } else { 0 };
        if payload.len() < 126 {
            out.push(mask_bit | (payload.len() as u8));
        } else if payload.len() <= 65_535 {
            out.push(mask_bit | 126);
            out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            out.push(mask_bit | 127);
            out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        if masked {
            let key = [0x11, 0x22, 0x33, 0x44];
            out.extend_from_slice(&key);
            for (i, &b) in payload.iter().enumerate() {
                out.push(b ^ key[i & 3]);
            }
        } else {
            out.extend_from_slice(payload);
        }
        out
    }

    #[test]
    fn rfc6455_fragmentation_interleaved_ping_timeout_resume_and_close_codes() {
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            // 1. Fragmented text with multi-byte UTF-8 ("€" = E2 82 AC) split across fragments,
            //    with interleaved Ping (0x9) and Pong (0xA) in between!
            let f1 = raw_ws_frame(false, 0, 0x1, false, b"{\"msg\":\"\xe2");
            let ping = raw_ws_frame(true, 0, 0x9, false, b"mid-frag");
            let f2 = raw_ws_frame(false, 0, 0x0, false, b"\x82");
            let pong = raw_ws_frame(true, 0, 0xA, false, b"unsolicited");
            let f3 = raw_ws_frame(true, 0, 0x0, false, b"\xac\"}");

            s.write_all(&f1).unwrap();
            s.write_all(&ping).unwrap();
            s.flush().unwrap();

            // Verify client replies with masked Pong carrying "mid-frag"
            let mut hdr = [0u8; 2];
            s.read_exact(&mut hdr).unwrap();
            assert_eq!(hdr[0] & 0x0f, 0xA);
            assert_ne!(hdr[1] & 0x80, 0);
            let plen = (hdr[1] & 0x7f) as usize;
            let mut m = [0u8; 4];
            s.read_exact(&mut m).unwrap();
            let mut pbuf = vec![0u8; plen];
            s.read_exact(&mut pbuf).unwrap();
            for (i, b) in pbuf.iter_mut().enumerate() {
                *b ^= m[i & 3];
            }
            assert_eq!(&pbuf, b"mid-frag");

            s.write_all(&f2).unwrap();
            s.write_all(&pong).unwrap();
            // Send partial f3 (header + 2 bytes), pause > read_timeout so client hits TimedOut, then send rest!
            s.write_all(&f3[..3]).unwrap();
            s.flush().unwrap();
            std::thread::sleep(Duration::from_millis(120));
            s.write_all(&f3[3..]).unwrap();
            // Follow with Close 1001 "going away"
            let mut close_body = 1001u16.to_be_bytes().to_vec();
            close_body.extend_from_slice(b"going away");
            let close_f = raw_ws_frame(true, 0, 0x8, false, &close_body);
            s.write_all(&close_f).unwrap();
            s.flush().unwrap();

            // Expect echoed Close 1001
            let mut chdr = [0u8; 2];
            s.read_exact(&mut chdr).unwrap();
            assert_eq!(chdr[0] & 0x0f, 0x8);
        });

        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let mut conn = WsConnection::from_plain_stream(stream, Vec::new(), 4096, 16);

        let (op1, _) = conn.read_frame().unwrap();
        assert_eq!(op1, 0x9);
        let (op2, _) = conn.read_frame().unwrap();
        assert_eq!(op2, 0xA);

        // Next read_frame may hit TimedOut on partial f3, then succeed on retry without losing bytes!
        let (op3, msg_bytes) = loop {
            match conn.read_frame() {
                Ok(v) => break v,
                Err(ref e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    continue;
                }
                Err(e) => panic!("unexpected frame error: {e}"),
            }
        };
        assert_eq!(op3, 0x1);
        assert_eq!(std::str::from_utf8(&msg_bytes).unwrap(), "{\"msg\":\"€\"}");

        let (op4, close_p) = conn.read_frame().unwrap();
        assert_eq!(op4, 0x8);
        assert_eq!(parse_ws_close_info(&close_p), (Some(1001), "going away".to_string()));
        srv.join().unwrap();
    }

    #[test]
    fn rfc6455_rejects_oversized_rsv_masked_server_and_bad_close_codes() {
        use std::net::TcpListener;

        let cases: Vec<Vec<u8>> = vec![
            // RSV1 bit set
            raw_ws_frame(true, 0x40, 0x1, false, b"hi"),
            // Masked server-to-client frame
            raw_ws_frame(true, 0, 0x1, true, b"hi"),
            // Oversized frame > 2048 cap
            raw_ws_frame(true, 0, 0x1, false, &vec![b'a'; 2049]),
            // 1-byte close payload
            raw_ws_frame(true, 0, 0x8, false, &[0x03]),
            // Forbidden close code 1005
            raw_ws_frame(true, 0, 0x8, false, &1005u16.to_be_bytes()),
            // Invalid close code 999
            raw_ws_frame(true, 0, 0x8, false, &999u16.to_be_bytes()),
        ];

        for bad in cases {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let srv = std::thread::spawn(move || {
                let (mut s, _) = listener.accept().unwrap();
                let _ = s.write_all(&bad);
                let _ = s.flush();
            });
            let stream = TcpStream::connect(addr).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut conn = WsConnection::from_plain_stream(stream, Vec::new(), 2048, 8);
            let err = conn.read_frame().unwrap_err();
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            srv.join().unwrap();
        }
    }

    #[test]
    fn reconnect_backoff_with_jitter_is_bounded_and_non_constant() {
        let mut delays = Vec::new();
        for _ in 0..16 {
            let (base, d) = next_ws_reconnect_delay_ms(4_000, false);
            assert_eq!(base, 8_000);
            assert!((7_000..=9_000).contains(&d), "delay {d} out of jitter bounds");
            delays.push(d);
        }
        let min_d = *delays.iter().min().unwrap();
        let max_d = *delays.iter().max().unwrap();
        assert!(max_d > min_d, "expected jitter variation, got {delays:?}");
        let (reset_b, reset_d) = next_ws_reconnect_delay_ms(16_000, true);
        assert_eq!(reset_b, 1_000);
        assert!((875..=1_125).contains(&reset_d));
    }

    #[test]
    fn duplicate_fill_order_cancel_and_position_are_idempotent() {
        let dir = std::env::temp_dir().join(format!("ob-pws-dedup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data_dir = dir.join("data");
        let strat_dir = dir.join("strats");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&strat_dir).unwrap();

        let app = App::new(data_dir.clone(), strat_dir, 500.0, 3600);
        {
            let mut st = app.live_desk.lock().unwrap();
            st.rails.arm(true, true).unwrap();
            // Seed two resting buy orders: ord-A ($4.00) and ord-B ($3.00) -> total $7.00
            st.rails.record_submitted_order("buy", 4.0);
            st.reserved_order_exposure.insert("ord-A".to_string(), 4.0);
            st.rails.record_submitted_order("buy", 3.0);
            st.reserved_order_exposure.insert("ord-B".to_string(), 3.0);
        }

        let mut tracker = SeqTracker::default();
        // Duplicate cancel of ord-A must release $4.00 ONCE, leaving ord-B's $3.00 intact!
        let cancel_msg = serde_json::json!({
            "type": "user_order",
            "sid": 1,
            "seq": 1,
            "msg": {
                "order_id": "ord-A",
                "ticker": "KXBTC15M-DEDUP",
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
        ingest_private_ws_message(&app, &cancel_msg, &mut tracker).unwrap();
        ingest_private_ws_message(&app, &cancel_msg, &mut tracker).unwrap();

        {
            let st = app.live_desk.lock().unwrap();
            assert!((st.rails.open_exposure_dollars - 3.0).abs() < 1e-6);
            assert_eq!(
                st.executions
                    .records()
                    .iter()
                    .filter(|r| r.kind == "order_canceled" && r.order_id == "ord-A")
                    .count(),
                1
            );
        }

        // Duplicate NO-side fill must update signed position ONCE (-5 for NO buy).
        let fill_msg = serde_json::json!({
            "type": "fill",
            "sid": 1,
            "seq": 2,
            "msg": {
                "trade_id": "tr-no-1",
                "order_id": "ord-B",
                "market_ticker": "KXBTC15M-DEDUP",
                "action": "buy",
                "side": "no",
                "count": 5,
                "no_price": 60,
                "ts": now_unix()
            }
        })
        .to_string();
        ingest_private_ws_message(&app, &fill_msg, &mut tracker).unwrap();
        ingest_private_ws_message(&app, &fill_msg, &mut tracker).unwrap();

        {
            let st = app.live_desk.lock().unwrap();
            assert_eq!(st.ws_positions["KXBTC15M-DEDUP"].position, -5);
            assert_eq!(
                st.executions
                    .records()
                    .iter()
                    .filter(|r| r.kind == "fill" && r.venue_id == "tr-no-1")
                    .count(),
                1
            );
        }

        // Duplicate market_position message stays idempotent.
        let pos_msg = serde_json::json!({
            "type": "market_position",
            "sid": 1,
            "seq": 3,
            "msg": {
                "ticker": "KXBTC15M-DEDUP",
                "position": -5,
                "market_exposure_dollars": "3.00",
                "realized_pnl_dollars": "-0.50",
                "resting_orders_count": 0
            }
        })
        .to_string();
        ingest_private_ws_message(&app, &pos_msg, &mut tracker).unwrap();
        ingest_private_ws_message(&app, &pos_msg, &mut tracker).unwrap();
        {
            let st = app.live_desk.lock().unwrap();
            assert!((st.rails.daily_realized_loss_dollars - 0.50).abs() < 1e-6);
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
