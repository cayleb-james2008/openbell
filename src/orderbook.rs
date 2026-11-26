//! Live L2 Orderbook Depth, Kalshi v2 WebSocket + REST Poller & Book-Walking Sizing (GAP #2).
//!
//! Maintains thread-safe per-ticker Kalshi binary orderbooks (`yes_bids`, `no_bids`),
//! derives normalized `yes_asks` (`100 - no_bid`) and `no_asks` (`100 - yes_bid`),
//! computes exact book-walking VWAP fill price / slippage / queue-ahead depth,
//! tracks sequence numbers with clean resync flags on gaps, and streams live updates
//! via pure-Rust RFC 6455 WebSocket (`wss://.../trade-api/ws/v2`) with exponential
//! backoff fallback to unauthenticated REST L2 polling (`GET /markets/{ticker}/orderbook`).

#![allow(dead_code)]

use crate::{kalshi_live, tape_hub, App};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const MAX_SUBSCRIBED_TICKERS: usize = 16;
pub const DEFAULT_STALE_AFTER_MS: i64 = 15_000;
const SSE_THROTTLE_MS: i64 = 250; // <= 4 Hz per ticker
const REST_POLL_INTERVAL_MS: u64 = 1_500;
const WS_RECONNECT_MAX_MS: u64 = 30_000;
const MAX_WS_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_WS_CONTINUATION_FRAMES: usize = 128;
const MAX_CACHED_BOOKS: usize = 128;
const MAX_ORDERBOOKS_JSONL_BYTES: u64 = 8 * 1024 * 1024;
const MAX_LEVEL_QTY: u64 = 1_000_000_000;
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const KALSHI_PROD_WS: &str = "wss://api.elections.kalshi.com/trade-api/ws/v2";
const KALSHI_DEMO_WS: &str = "wss://demo-api.kalshi.co/trade-api/ws/v2";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderbookLevel {
    pub price_cents: u8,
    pub qty: u64,
    pub cumulative_qty: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueLevelSummary {
    pub price_cents: u8,
    pub yes_bid_qty: u64,
    pub no_bid_qty: u64,
    pub yes_queue_ahead: u64,
    pub no_queue_ahead: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookWalkResult {
    pub ticker: String,
    pub side: String,
    pub requested_qty: u64,
    pub fillable_qty: u64,
    pub fully_fillable: bool,
    pub best_price_cents: u8,
    pub worst_level_cents: u8,
    pub vwap_cents: f64,
    pub slippage_cents: f64,
    pub levels_touched: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookSnapshotPayload {
    pub ok: bool,
    pub ticker: String,
    pub yes_bids: Vec<OrderbookLevel>,
    pub no_bids: Vec<OrderbookLevel>,
    pub yes_asks: Vec<OrderbookLevel>,
    pub no_asks: Vec<OrderbookLevel>,
    pub best_yes_bid: Option<u8>,
    pub best_yes_ask: Option<u8>,
    pub spread_cents: Option<i32>,
    pub mid_cents: Option<f64>,
    pub microprice_cents: Option<f64>,
    pub vwap_10: Option<f64>,
    pub imbalance_top3: f64,
    pub last_seq: u64,
    pub updated_ms: i64,
    pub source: String,
    pub stale: bool,
    pub needs_resync: bool,
    pub queue_by_level: Vec<QueueLevelSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderbookStreamStatus {
    pub ws_connected: bool,
    pub mode: String,
    pub subscribed_tickers: Vec<String>,
    pub active_books: usize,
    pub stale_books: usize,
    pub last_error: Option<String>,
    pub delta_count: u64,
    pub resync_count: u64,
    pub walk_fallback_count: u64,
}

#[derive(Debug, Clone)]
struct TickerBook {
    yes_bids: BTreeMap<u8, u64>,
    no_bids: BTreeMap<u8, u64>,
    last_seq: u64,
    updated_ms: i64,
    source: String,
    needs_resync: bool,
    has_snapshot: bool,
    last_trade_cents: Option<u8>,
    last_trade_size: Option<u64>,
    last_sse_emit_ms: i64,
}

impl Default for TickerBook {
    fn default() -> Self {
        Self {
            yes_bids: BTreeMap::new(),
            no_bids: BTreeMap::new(),
            last_seq: 0,
            updated_ms: 0,
            source: "empty".to_string(),
            needs_resync: false,
            has_snapshot: false,
            last_trade_cents: None,
            last_trade_size: None,
            last_sse_emit_ms: 0,
        }
    }
}

impl TickerBook {
    fn is_crossed(&self) -> bool {
        let best_yes = self.yes_bids.keys().next_back().copied();
        let best_no = self.no_bids.keys().next_back().copied();
        match (best_yes, best_no) {
            (Some(y), Some(n)) => (y as u16) + (n as u16) > 100,
            _ => false,
        }
    }

    fn is_stale(&self, now: i64, stale_after_ms: i64) -> bool {
        if self.source == "empty" || !self.has_snapshot || self.updated_ms <= 0 || self.needs_resync {
            return true;
        }
        now.saturating_sub(self.updated_ms) > stale_after_ms
    }

    fn build_bids_desc(map: &BTreeMap<u8, u64>) -> Vec<OrderbookLevel> {
        let mut out = Vec::with_capacity(map.len());
        let mut cum = 0u64;
        for (&price_cents, &qty) in map.iter().rev() {
            if qty == 0 {
                continue;
            }
            cum = cum.saturating_add(qty);
            out.push(OrderbookLevel {
                price_cents,
                qty,
                cumulative_qty: cum,
            });
        }
        out
    }

    fn build_asks_from_opposite_bids(opposite: &BTreeMap<u8, u64>) -> Vec<OrderbookLevel> {
        // Opposite bid at price `p` in 1..=99 is an ask on this side at `100 - p`.
        // Iterating `opposite` in descending order gives ascending ask prices!
        let mut out = Vec::with_capacity(opposite.len());
        let mut cum = 0u64;
        for (&opp_bid_p, &qty) in opposite.iter().rev() {
            if qty == 0 || !(1..=99).contains(&opp_bid_p) {
                continue;
            }
            let ask_p = 100u8.saturating_sub(opp_bid_p);
            if !(1..=99).contains(&ask_p) {
                continue;
            }
            cum = cum.saturating_add(qty);
            out.push(OrderbookLevel {
                price_cents: ask_p,
                qty,
                cumulative_qty: cum,
            });
        }
        out
    }

    fn walk_asks(&self, ticker: &str, side: &str, qty: u64) -> Option<BookWalkResult> {
        if qty == 0 {
            return None;
        }
        let asks = if side == "yes" {
            Self::build_asks_from_opposite_bids(&self.no_bids)
        } else if side == "no" {
            Self::build_asks_from_opposite_bids(&self.yes_bids)
        } else {
            return None;
        };
        let best = asks.first()?;
        let best_price_cents = best.price_cents;

        let mut remaining = qty;
        let mut filled = 0u64;
        let mut cost_sum = 0.0f64;
        let mut worst_level_cents = best_price_cents;
        let mut levels_touched = 0usize;

        for lvl in &asks {
            if remaining == 0 {
                break;
            }
            let take = remaining.min(lvl.qty);
            if take > 0 {
                filled = filled.saturating_add(take);
                remaining = remaining.saturating_sub(take);
                cost_sum += (take as f64) * (lvl.price_cents as f64);
                worst_level_cents = lvl.price_cents;
                levels_touched += 1;
            }
        }
        if filled == 0 {
            return None;
        }
        let vwap_cents = cost_sum / (filled as f64);
        let slippage_cents = (vwap_cents - (best_price_cents as f64)).max(0.0);
        Some(BookWalkResult {
            ticker: ticker.to_string(),
            side: side.to_string(),
            requested_qty: qty,
            fillable_qty: filled,
            fully_fillable: filled >= qty,
            best_price_cents,
            worst_level_cents,
            vwap_cents,
            slippage_cents,
            levels_touched,
        })
    }

    fn queue_ahead_at(&self, side: &str, price_cents: u8) -> u64 {
        let map = if side == "yes" {
            &self.yes_bids
        } else if side == "no" {
            &self.no_bids
        } else {
            return 0;
        };
        map.range(price_cents..=99)
            .map(|(_, &q)| q)
            .fold(0u64, |acc, q| acc.saturating_add(q))
    }

    fn to_payload(&self, ticker: &str, now: i64, stale_after_ms: i64) -> OrderbookSnapshotPayload {
        let yes_bids = Self::build_bids_desc(&self.yes_bids);
        let no_bids = Self::build_bids_desc(&self.no_bids);
        let yes_asks = Self::build_asks_from_opposite_bids(&self.no_bids);
        let no_asks = Self::build_asks_from_opposite_bids(&self.yes_bids);

        let best_yes_bid = yes_bids.first().map(|l| l.price_cents);
        let best_yes_ask = yes_asks.first().map(|l| l.price_cents);
        let spread_cents = match (best_yes_bid, best_yes_ask) {
            (Some(b), Some(a)) => Some((a as i32) - (b as i32)),
            _ => None,
        };
        let mid_cents = match (best_yes_bid, best_yes_ask) {
            (Some(b), Some(a)) => Some(((b as f64) + (a as f64)) * 0.5),
            _ => None,
        };
        let microprice_cents = match (yes_bids.first(), yes_asks.first()) {
            (Some(b), Some(a)) => {
                let denom = b.qty.saturating_add(a.qty) as f64;
                if denom > 0.0 {
                    Some(
                        ((b.price_cents as f64) * (a.qty as f64)
                            + (a.price_cents as f64) * (b.qty as f64))
                            / denom,
                    )
                } else {
                    mid_cents
                }
            }
            _ => None,
        };
        let vwap_10 = if yes_asks.is_empty() {
            None
        } else {
            let mut rem = 10u64;
            let mut filled = 0u64;
            let mut cost = 0.0f64;
            for lvl in &yes_asks {
                if rem == 0 {
                    break;
                }
                let take = rem.min(lvl.qty);
                if take > 0 {
                    filled = filled.saturating_add(take);
                    rem = rem.saturating_sub(take);
                    cost += (take as f64) * (lvl.price_cents as f64);
                }
            }
            if filled > 0 {
                Some(cost / (filled as f64))
            } else {
                None
            }
        };

        let top3_yes: u64 = yes_bids
            .iter()
            .take(3)
            .map(|l| l.qty)
            .fold(0u64, u64::saturating_add);
        let top3_no: u64 = no_bids
            .iter()
            .take(3)
            .map(|l| l.qty)
            .fold(0u64, u64::saturating_add);
        let top3_tot = (top3_yes as f64) + (top3_no as f64);
        let imbalance_top3 = if top3_tot > 0.0 {
            ((top3_yes as f64) - (top3_no as f64)) / top3_tot
        } else {
            0.0
        };

        // O(L) queue_by_level scan in descending price order using cumulative bid sums.
        let mut all_prices = BTreeSet::new();
        for &p in self.yes_bids.keys() {
            all_prices.insert(p);
        }
        for &p in self.no_bids.keys() {
            all_prices.insert(p);
        }
        let mut queue_by_level = Vec::with_capacity(all_prices.len());
        let mut yes_cum = 0u64;
        let mut no_cum = 0u64;
        for p in all_prices.into_iter().rev() {
            let yq = self.yes_bids.get(&p).copied().unwrap_or(0);
            let nq = self.no_bids.get(&p).copied().unwrap_or(0);
            yes_cum = yes_cum.saturating_add(yq);
            no_cum = no_cum.saturating_add(nq);
            queue_by_level.push(QueueLevelSummary {
                price_cents: p,
                yes_bid_qty: yq,
                no_bid_qty: nq,
                yes_queue_ahead: yes_cum,
                no_queue_ahead: no_cum,
            });
        }

        OrderbookSnapshotPayload {
            ok: true,
            ticker: ticker.to_string(),
            yes_bids,
            no_bids,
            yes_asks,
            no_asks,
            best_yes_bid,
            best_yes_ask,
            spread_cents,
            mid_cents,
            microprice_cents,
            vwap_10,
            imbalance_top3,
            last_seq: self.last_seq,
            updated_ms: self.updated_ms,
            source: self.source.clone(),
            stale: self.is_stale(now, stale_after_ms),
            needs_resync: self.needs_resync,
            queue_by_level,
        }
    }
}

struct OrderbookInner {
    books: HashMap<String, TickerBook>,
    subscribed_tickers: Vec<String>,
    ws_connected: bool,
    mode: String,
    last_error: Option<String>,
    delta_count: u64,
    resync_count: u64,
    stale_after_ms: i64,
    sub_version: u64,
}

pub struct OrderbookStore {
    data_dir: PathBuf,
    inner: Mutex<OrderbookInner>,
    cv: Condvar,
    walk_fallback_count: AtomicU64,
    revision: AtomicU64,
}

impl OrderbookStore {
    pub fn new(data_dir: PathBuf) -> Self {
        Self::with_stale_threshold_ms(data_dir, DEFAULT_STALE_AFTER_MS)
    }

    pub fn with_stale_threshold_ms(data_dir: PathBuf, stale_after_ms: i64) -> Self {
        Self {
            data_dir,
            inner: Mutex::new(OrderbookInner {
                books: HashMap::new(),
                subscribed_tickers: Vec::new(),
                ws_connected: false,
                mode: "offline".to_string(),
                last_error: None,
                delta_count: 0,
                resync_count: 0,
                stale_after_ms: stale_after_ms.max(1),
                sub_version: 0,
            }),
            cv: Condvar::new(),
            walk_fallback_count: AtomicU64::new(0),
            revision: AtomicU64::new(0),
        }
    }

    pub fn set_stale_after_ms(&self, stale_after_ms: i64) {
        let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        st.stale_after_ms = stale_after_ms.max(1);
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    pub fn record_walk_fallback(&self) {
        self.walk_fallback_count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn walk_fallback_count(&self) -> u64 {
        self.walk_fallback_count.load(Ordering::Relaxed)
    }

    pub fn subscribed_tickers(&self) -> Vec<String> {
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        st.subscribed_tickers.clone()
    }

    /// Subscribe to up to `MAX_SUBSCRIBED_TICKERS` (16) validated market tickers.
    /// When `replace` is true (e.g. `{ tickers: [...] }`), replaces the list (rejecting > 16).
    /// When `replace` is false (single `{ ticker: "..." }`), appends/moves to most-recent and evicts oldest if at cap.
    pub fn subscribe_tickers(&self, raw_tickers: &[String], replace: bool) -> Result<Vec<String>, String> {
        if raw_tickers.is_empty() {
            return Err("at least one ticker is required".to_string());
        }
        if raw_tickers.len() > MAX_SUBSCRIBED_TICKERS {
            return Err(format!(
                "cannot subscribe to more than {MAX_SUBSCRIBED_TICKERS} tickers"
            ));
        }
        let mut clean = Vec::with_capacity(raw_tickers.len());
        for raw in raw_tickers {
            let valid = kalshi_live::validate_ticker(raw)?.to_string();
            if !clean.contains(&valid) {
                clean.push(valid);
            }
        }
        let out = {
            let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            if replace {
                st.subscribed_tickers = clean;
            } else {
                for t in clean {
                    st.subscribed_tickers.retain(|x| x != &t);
                    while st.subscribed_tickers.len() >= MAX_SUBSCRIBED_TICKERS {
                        st.subscribed_tickers.remove(0);
                    }
                    st.subscribed_tickers.push(t);
                }
            }
            st.sub_version = st.sub_version.saturating_add(1);
            st.subscribed_tickers.clone()
        };
        self.cv.notify_all();
        Ok(out)
    }

    /// Inject or apply a full L2 orderbook snapshot (`yes_bids`, `no_bids` as `[price_cents, qty]`).
    pub fn ingest_snapshot(
        &self,
        ticker: &str,
        yes_bids: &[[u64; 2]],
        no_bids: &[[u64; 2]],
        seq: u64,
        source: &str,
    ) {
        let Ok(clean_ticker) = kalshi_live::validate_ticker(ticker) else {
            return;
        };
        let now = now_ms();
        let src = match source {
            "ws" | "rest" | "empty" => source.to_string(),
            _ => "rest".to_string(),
        };

        let mut next_yes = BTreeMap::new();
        for &[p, q] in yes_bids {
            if (1..=99).contains(&p) && q > 0 {
                next_yes.insert(p as u8, q.min(MAX_LEVEL_QTY));
            }
        }
        let mut next_no = BTreeMap::new();
        for &[p, q] in no_bids {
            if (1..=99).contains(&p) && q > 0 {
                next_no.insert(p as u8, q.min(MAX_LEVEL_QTY));
            }
        }

        let need_disk = !self.data_dir.as_os_str().is_empty() && src != "empty";
        let (changed, disk_vecs, sse_payload) = {
            let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let stale_after = st.stale_after_ms;
            let is_subscribed = st.subscribed_tickers.iter().any(|t| t == clean_ticker);
            if !st.books.contains_key(clean_ticker) && st.books.len() >= MAX_CACHED_BOOKS {
                let evict_key = st
                    .books
                    .iter()
                    .filter(|(t, _)| !st.subscribed_tickers.iter().any(|s| s == *t))
                    .min_by_key(|(_, b)| b.updated_ms)
                    .map(|(t, _)| t.clone());
                if let Some(k) = evict_key {
                    st.books.remove(&k);
                }
            }
            let book = st.books.entry(clean_ticker.to_string()).or_default();
            let changed = book.yes_bids != next_yes || book.no_bids != next_no || !book.has_snapshot;
            let disk_vecs = if changed && need_disk {
                let yv: Vec<[u64; 2]> = next_yes.iter().rev().map(|(&p, &q)| [p as u64, q]).collect();
                let nv: Vec<[u64; 2]> = next_no.iter().rev().map(|(&p, &q)| [p as u64, q]).collect();
                Some((yv, nv))
            } else {
                None
            };
            book.yes_bids = next_yes;
            book.no_bids = next_no;
            book.last_seq = seq;
            book.updated_ms = now;
            book.source = src.clone();
            let crossed = book.is_crossed();
            book.needs_resync = crossed;
            book.has_snapshot = true;

            let mut sse = None;
            if is_subscribed && now.saturating_sub(book.last_sse_emit_ms) >= SSE_THROTTLE_MS {
                book.last_sse_emit_ms = now;
                sse = Some(book.to_payload(clean_ticker, now, stale_after));
            }
            if crossed {
                st.resync_count = st.resync_count.saturating_add(1);
            }
            (changed, disk_vecs, sse)
        };

        self.revision.fetch_add(1, Ordering::Relaxed);
        if changed {
            if let Some((yv, nv)) = disk_vecs {
                self.append_orderbook_record(clean_ticker, seq, &src, &yv, &nv, now);
            }
        }
        if let Some(payload) = sse_payload {
            if let Ok(json) = serde_json::to_string(&payload) {
                tape_hub().publish_event("orderbook", json);
            }
        }
    }

    /// Apply an incremental L2 delta (`side` = `"yes"` | `"no"`, signed `delta`, sequence `seq`).
    /// Returns `true` when applied cleanly, or `false` on sequence gap / missing snapshot (setting `needs_resync`).
    pub fn ingest_delta(
        &self,
        ticker: &str,
        side: &str,
        price_cents: u8,
        delta: i64,
        seq: u64,
    ) -> bool {
        let Ok(clean_ticker) = kalshi_live::validate_ticker(ticker) else {
            return false;
        };
        if !(1..=99).contains(&price_cents) {
            return false;
        }
        let side_lc = side.trim().to_ascii_lowercase();
        if side_lc != "yes" && side_lc != "no" {
            return false;
        }
        let now = now_ms();

        let (applied, sse_payload) = {
            let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let stale_after = st.stale_after_ms;
            let is_subscribed = st.subscribed_tickers.iter().any(|t| t == clean_ticker);
            let needs_init = !st.books.contains_key(clean_ticker);
            if needs_init {
                let b = TickerBook {
                    needs_resync: true,
                    ..Default::default()
                };
                st.books.insert(clean_ticker.to_string(), b);
                st.resync_count = st.resync_count.saturating_add(1);
                return false;
            }
            let (applied, sse) = {
                let book = st.books.get_mut(clean_ticker).unwrap();
                if !book.has_snapshot || book.needs_resync {
                    book.needs_resync = true;
                    (false, None)
                } else if seq <= book.last_seq {
                    // Duplicate or already superseded delta: ignore cleanly without flagging gap.
                    return false;
                } else if seq != book.last_seq + 1 {
                    book.needs_resync = true;
                    (false, None)
                } else {
                    let map = if side_lc == "yes" {
                        &mut book.yes_bids
                    } else {
                        &mut book.no_bids
                    };
                    let cur = map.get(&price_cents).copied().unwrap_or(0);
                    let next_qty = if delta >= 0 {
                        cur.saturating_add(delta as u64).min(MAX_LEVEL_QTY)
                    } else {
                        cur.saturating_sub(delta.unsigned_abs())
                    };
                    if next_qty == 0 {
                        map.remove(&price_cents);
                    } else {
                        map.insert(price_cents, next_qty);
                    }
                    book.last_seq = seq;
                    book.updated_ms = now;
                    book.source = "ws".to_string();
                    if book.is_crossed() {
                        book.needs_resync = true;
                        (false, None)
                    } else {
                        let sse = if is_subscribed
                            && now.saturating_sub(book.last_sse_emit_ms) >= SSE_THROTTLE_MS
                        {
                            book.last_sse_emit_ms = now;
                            Some(book.to_payload(clean_ticker, now, stale_after))
                        } else {
                            None
                        };
                        (true, sse)
                    }
                }
            };
            if applied {
                st.delta_count = st.delta_count.saturating_add(1);
            } else {
                st.resync_count = st.resync_count.saturating_add(1);
            }
            (applied, sse)
        };

        if applied {
            self.revision.fetch_add(1, Ordering::Relaxed);
            if let Some(payload) = sse_payload {
                if let Ok(json) = serde_json::to_string(&payload) {
                    tape_hub().publish_event("orderbook", json);
                }
            }
        }
        applied
    }

    pub fn record_trade(&self, ticker: &str, price_cents: u8, size: u64) {
        let Ok(clean_ticker) = kalshi_live::validate_ticker(ticker) else {
            return;
        };
        let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(book) = st.books.get_mut(clean_ticker) {
            book.last_trade_cents = Some(price_cents);
            book.last_trade_size = Some(size);
        }
    }

    /// Force a ticker's `updated_ms` back by `age_ms` (deterministic testing helper).
    pub fn mark_stale_for_test(&self, ticker: &str, age_ms: i64) {
        let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(book) = st.books.get_mut(ticker) {
            book.updated_ms = now_ms().saturating_sub(age_ms.max(0));
        }
    }

    /// Parse and apply one incoming Kalshi v2 WebSocket JSON message.
    pub fn ingest_ws_message(&self, raw: &str) -> Result<(), String> {
        if raw.len() > MAX_WS_FRAME_BYTES {
            return Err("WS JSON message exceeds size limit".to_string());
        }
        let val: serde_json::Value =
            serde_json::from_str(raw).map_err(|e| format!("invalid WS JSON: {e}"))?;
        let msg_type = val.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let seq = val.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
        let msg = val.get("msg").unwrap_or(&val);

        match msg_type {
            "orderbook_snapshot" => {
                let raw_ticker = msg
                    .get("market_ticker")
                    .or_else(|| msg.get("ticker"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "orderbook_snapshot missing market_ticker".to_string())?;
                let ticker = kalshi_live::validate_ticker(raw_ticker)?;
                let parsed = kalshi_live::parse_kalshi_orderbook_json(msg)?;
                self.ingest_snapshot(ticker, &parsed.yes, &parsed.no, seq.max(1), "ws");
                Ok(())
            }
            "orderbook_delta" => {
                if seq == 0 {
                    return Err("orderbook_delta missing positive seq".to_string());
                }
                let raw_ticker = msg
                    .get("market_ticker")
                    .or_else(|| msg.get("ticker"))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "orderbook_delta missing market_ticker".to_string())?;
                let ticker = kalshi_live::validate_ticker(raw_ticker)?;
                let side = msg
                    .get("side")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| "orderbook_delta missing side".to_string())?;
                let side_lc = side.trim().to_ascii_lowercase();
                if side_lc != "yes" && side_lc != "no" {
                    return Err(format!("orderbook_delta invalid side: {side}"));
                }
                let price_c = msg
                    .get("price")
                    .and_then(|v| v.as_u64())
                    .or_else(|| {
                        msg.get("price_dollars")
                            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                            .filter(|d| d.is_finite() && *d >= 0.01 && *d <= 0.99)
                            .map(|d| (d * 100.0).round() as u64)
                    })
                    .filter(|p| (1..=99).contains(p))
                    .ok_or_else(|| "orderbook_delta invalid price".to_string())?
                    as u8;
                let delta = msg
                    .get("delta")
                    .and_then(|v| v.as_i64())
                    .or_else(|| {
                        msg.get("delta_fp")
                            .and_then(|v| v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())))
                            .filter(|d| d.is_finite() && d.abs() <= MAX_LEVEL_QTY as f64)
                            .map(|d| d.round() as i64)
                    })
                    .ok_or_else(|| "orderbook_delta missing delta".to_string())?;
                self.ingest_delta(ticker, &side_lc, price_c, delta, seq);
                Ok(())
            }
            "trade" => {
                if let (Some(ticker), Some(price), Some(count)) = (
                    msg.get("market_ticker").and_then(|v| v.as_str()),
                    msg.get("yes_price").and_then(|v| v.as_u64()),
                    msg.get("count").and_then(|v| v.as_u64()),
                ) {
                    if (1..=99).contains(&price) {
                        self.record_trade(ticker, price as u8, count);
                    }
                }
                Ok(())
            }
            "error" => {
                let err_msg = msg
                    .get("msg")
                    .or_else(|| msg.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Kalshi WS error");
                Err(err_msg.to_string())
            }
            _ => Ok(()),
        }
    }

    pub fn snapshot(&self, ticker: &str) -> OrderbookSnapshotPayload {
        let now = now_ms();
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let stale_after = st.stale_after_ms;
        match st.books.get(ticker) {
            Some(book) => book.to_payload(ticker, now, stale_after),
            None => TickerBook::default().to_payload(ticker, now, stale_after),
        }
    }

    pub fn stream_status(&self) -> OrderbookStreamStatus {
        let now = now_ms();
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let active_books = st.books.values().filter(|b| b.has_snapshot).count();
        let stale_books = st
            .books
            .values()
            .filter(|b| b.has_snapshot && b.is_stale(now, st.stale_after_ms))
            .count();
        OrderbookStreamStatus {
            ws_connected: st.ws_connected,
            mode: st.mode.clone(),
            subscribed_tickers: st.subscribed_tickers.clone(),
            active_books,
            stale_books,
            last_error: st.last_error.clone(),
            delta_count: st.delta_count,
            resync_count: st.resync_count,
            walk_fallback_count: self.walk_fallback_count(),
        }
    }

    /// Walk the opposite-side bids (normalized asks) on `side` (`"yes"` | `"no"`) for `qty` contracts.
    pub fn walk_book(&self, ticker: &str, side: &str, qty: u64) -> Option<BookWalkResult> {
        let side_lc = side.trim().to_ascii_lowercase();
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let book = st.books.get(ticker)?;
        book.walk_asks(ticker, &side_lc, qty)
    }

    /// Same as `walk_book`, but returns `None` when the book is missing, stale, or awaiting resync.
    pub fn walk_book_fresh(&self, ticker: &str, side: &str, qty: u64) -> Option<BookWalkResult> {
        let now = now_ms();
        let side_lc = side.trim().to_ascii_lowercase();
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let book = st.books.get(ticker)?;
        if book.is_stale(now, st.stale_after_ms) {
            return None;
        }
        book.walk_asks(ticker, &side_lc, qty)
    }

    /// Queue contracts ahead of a resting bid at `price_cents` on `side` (`"yes"` | `"no"`).
    pub fn queue_ahead_at(&self, ticker: &str, side: &str, price_cents: u8) -> u64 {
        let side_lc = side.trim().to_ascii_lowercase();
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let Some(book) = st.books.get(ticker) else {
            return 0;
        };
        book.queue_ahead_at(&side_lc, price_cents)
    }

    /// Same as `queue_ahead_at`, returning `None` when the book is missing or stale.
    pub fn queue_ahead_at_fresh(&self, ticker: &str, side: &str, price_cents: u8) -> Option<u64> {
        let now = now_ms();
        let side_lc = side.trim().to_ascii_lowercase();
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let book = st.books.get(ticker)?;
        if book.is_stale(now, st.stale_after_ms) {
            return None;
        }
        Some(book.queue_ahead_at(&side_lc, price_cents))
    }

    /// Return the freshest active L2 ticker matching `series` (e.g., `"KXBTC15M-..."`).
    pub fn fresh_ticker_for_series(&self, series: &str) -> Option<String> {
        let now = now_ms();
        let prefix = format!("{series}-");
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        st.books
            .iter()
            .filter(|(t, b)| {
                !b.is_stale(now, st.stale_after_ms)
                    && (t.starts_with(&prefix) || t.eq_ignore_ascii_case(series))
            })
            .max_by_key(|(_, b)| b.updated_ms)
            .map(|(t, _)| t.clone())
    }

    fn append_orderbook_record(
        &self,
        ticker: &str,
        seq: u64,
        source: &str,
        yes_vec: &[[u64; 2]],
        no_vec: &[[u64; 2]],
        ts_ms: i64,
    ) {
        if self.data_dir.as_os_str().is_empty() {
            return;
        }
        let path = self.data_dir.join("orderbooks.jsonl");
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                return;
            }
            if meta.len() >= MAX_ORDERBOOKS_JSONL_BYTES {
                let backup = self.data_dir.join("orderbooks.jsonl.1");
                let backup_is_symlink = std::fs::symlink_metadata(&backup)
                    .is_ok_and(|m| m.file_type().is_symlink());
                if !backup_is_symlink {
                    let _ = std::fs::rename(&path, &backup);
                }
            }
        }
        let line = serde_json::json!({
            "ts_ms": ts_ms,
            "ticker": ticker,
            "seq": seq,
            "source": source,
            "yes_bids": yes_vec,
            "no_bids": no_vec,
        });
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    fn set_transport_state(&self, ws_connected: bool, mode: &str, last_error: Option<String>) {
        let mut st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        st.ws_connected = ws_connected;
        st.mode = mode.to_string();
        st.last_error = last_error;
    }

    fn resync_needed_tickers(&self) -> Vec<String> {
        let st = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        st.subscribed_tickers
            .iter()
            .filter(|t| st.books.get(*t).is_some_and(|b| b.needs_resync))
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Pure-Rust SHA-1 (RFC 3174) for RFC 6455 Sec-WebSocket-Accept Verification
// ---------------------------------------------------------------------------

pub fn sha1_digest(data: &[u8]) -> [u8; 20] {
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

pub fn compute_ws_accept_key(sec_ws_key: &str) -> String {
    let mut combined = String::with_capacity(sec_ws_key.len() + WS_GUID.len());
    combined.push_str(sec_ws_key.trim());
    combined.push_str(WS_GUID);
    let digest = sha1_digest(combined.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(digest)
}

// ---------------------------------------------------------------------------
// Pure-Rust RFC 6455 WebSocket Client + Exponential Backoff REST Fallback
// ---------------------------------------------------------------------------

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
            .map_err(|e| format!("ws write failed: {e}"))
    }

    fn send_text(&mut self, text: &str) -> Result<(), String> {
        self.send_masked_frame(0x1, text.as_bytes())
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
            if opcode == 0 && first_opcode == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "ws unexpected continuation frame",
                ));
            }
            if first_opcode == 0 && opcode != 0 {
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
                // Ping -> reply Pong immediately and continue reading next frame.
                let _ = self.send_masked_frame(0xA, &full_payload[start..]);
                full_payload.truncate(start);
                continue;
            }
            if opcode == 0xA {
                // Unsolicited Pong -> ignore.
                full_payload.truncate(start);
                continue;
            }
            if fin {
                return Ok((first_opcode, full_payload));
            }
        }
    }
}

fn parse_ws_url(url: &str) -> Result<(bool, String, u16, String), String> {
    let trimmed = url.trim();
    if trimmed.contains('\r') || trimmed.contains('\n') || trimmed.contains('\0') {
        return Err("WS URL contains invalid control characters".to_string());
    }
    let (tls, rest) = if let Some(r) = trimmed.strip_prefix("wss://") {
        (true, r)
    } else if let Some(r) = trimmed.strip_prefix("ws://") {
        (false, r)
    } else {
        return Err("WS URL must start with ws:// or wss://".to_string());
    };
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, "/".to_string()),
    };
    let (host, port) = if let Some((h, p_str)) = authority.rsplit_once(':') {
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
    Ok((tls, host, port, path))
}

fn connect_ws(
    url: &str,
    auth_headers: &[(String, String)],
) -> Result<WsConnection, String> {
    let (tls, host, port, path) = parse_ws_url(url)?;
    let mut io = if !tls {
        let addr = format!("{host}:{port}");
        let stream = TcpStream::connect(&addr).map_err(|e| format!("WS TCP connect failed: {e}"))?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
        WsTransport::Plain(stream)
    } else {
        let target = format!("{host}:{port}");
        let mut child = Command::new("openssl")
            .args(["s_client", "-quiet", "-servername", &host, "-connect", &target])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("WS TLS transport unavailable: {e}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "WS TLS stdin unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "WS TLS stdout unavailable".to_string())?;
        WsTransport::TlsProc {
            child,
            stdin,
            stdout,
        }
    };

    let mut key_raw = [0u8; 16];
    let _ = getrandom::getrandom(&mut key_raw);
    let ws_key = base64::engine::general_purpose::STANDARD.encode(key_raw);
    let expected_accept = compute_ws_accept_key(&ws_key);

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
        .map_err(|e| format!("WS handshake write failed: {e}"))?;

    let mut resp_buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    let hdr_end = loop {
        if resp_buf.len() > 16_384 {
            return Err("WS handshake response too large".to_string());
        }
        let n = io
            .read(&mut tmp)
            .map_err(|e| format!("WS handshake read failed: {e}"))?;
        if n == 0 {
            return Err("WS handshake closed before 101".to_string());
        }
        resp_buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = resp_buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
    };

    let hdr_str = std::str::from_utf8(&resp_buf[..hdr_end])
        .map_err(|_| "WS handshake response not UTF-8".to_string())?;
    let mut lines = hdr_str.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    if !status_line.contains(" 101 ") {
        return Err(format!("WS upgrade rejected: {status_line}"));
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
        return Err("WS Sec-WebSocket-Accept mismatch".to_string());
    }

    let leftover = resp_buf[hdr_end + 4..].to_vec();
    Ok(WsConnection {
        io,
        leftover,
        offset: 0,
    })
}

struct WsAndRestTargets {
    ws_url: Option<String>,
    auth_headers: Option<Vec<(String, String)>>,
    rest_base: String,
}

fn resolve_ws_and_rest_targets(app: &App) -> WsAndRestTargets {
    let ws_override = std::env::var("OPENBELL_WS_URL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let rest_override = std::env::var("OPENBELL_API_BASE")
        .ok()
        .and_then(|s| kalshi_live::validate_api_base_url(&s).ok());

    let (cfg_opt, env_val, effective_base) = {
        let ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
        let configured = ld.config.is_configured();
        (
            if configured {
                Some(ld.config.clone())
            } else {
                None
            },
            ld.config.environment,
            ld.config.effective_base_url(),
        )
    };

    let rest_base = rest_override.unwrap_or(effective_base);

    let ws_url = if let Some(u) = ws_override {
        Some(u)
    } else if cfg_opt.is_some() && rest_base.starts_with("https://") {
        Some(match env_val {
            kalshi_live::KalshiEnv::Prod => KALSHI_PROD_WS.to_string(),
            kalshi_live::KalshiEnv::Demo => KALSHI_DEMO_WS.to_string(),
        })
    } else {
        None
    };

    let auth_headers = if let (Some(ref ws), Some(ref cfg)) = (&ws_url, &cfg_opt) {
        let path = parse_ws_url(ws).ok().map(|(_, _, _, p)| p);
        kalshi_live::KalshiLiveClient::from_config(cfg)
            .ok()
            .and_then(|c| c.sign_ws_handshake_headers(path.as_deref()).ok())
    } else {
        None
    };

    WsAndRestTargets {
        ws_url,
        auth_headers,
        rest_base,
    }
}

fn poll_rest_once(
    app: &App,
    rest_base: &str,
    tickers: &[String],
    seq_counter: &mut u64,
) -> (usize, Option<String>) {
    let mut ok_count = 0usize;
    let mut last_err = None;
    for t in tickers {
        match kalshi_live::fetch_public_market_orderbook(rest_base, t) {
            Ok(ob) => {
                *seq_counter = seq_counter.saturating_add(1);
                app.orderbook
                    .ingest_snapshot(t, &ob.yes, &ob.no, *seq_counter, "rest");
                ok_count += 1;
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
    }
    (ok_count, last_err)
}

/// Start the background Kalshi L2 WebSocket + REST fallback worker thread.
/// Parks idle at 0% CPU / 0 network when `subscribed_tickers` is empty.
pub fn start_background_orderbook(app: Arc<App>) {
    std::thread::spawn(move || {
        let mut backoff_ms = 1_000u64;
        let mut next_ws_attempt_ms = 0i64;
        let mut rest_seq = 0u64;

        loop {
            let (tickers, sub_ver) = {
                let mut st = app.orderbook.inner.lock().unwrap_or_else(|p| p.into_inner());
                while st.subscribed_tickers.is_empty() {
                    st.ws_connected = false;
                    st.mode = "offline".to_string();
                    st = app.orderbook.cv.wait(st).unwrap_or_else(|p| p.into_inner());
                }
                (st.subscribed_tickers.clone(), st.sub_version)
            };

            if std::env::var("OPENBELL_ORDERBOOK_OFFLINE").as_deref() == Ok("1") {
                app.orderbook.set_transport_state(false, "offline", None);
                let st = app.orderbook.inner.lock().unwrap_or_else(|p| p.into_inner());
                let _ = app.orderbook.cv.wait_timeout(st, Duration::from_millis(1_000));
                continue;
            }

            let WsAndRestTargets {
                ws_url: ws_url_opt,
                auth_headers: auth_headers_opt,
                rest_base,
            } = resolve_ws_and_rest_targets(&app);
            let now = now_ms();

            // Attempt authenticated Kalshi WebSocket stream when configured/overridden and backoff elapsed.
            if let Some(ws_url) = ws_url_opt {
                if now >= next_ws_attempt_ms {
                    let hdrs = auth_headers_opt.unwrap_or_default();
                    match connect_ws(&ws_url, &hdrs) {
                        Ok(mut conn) => {
                            backoff_ms = 1_000;
                            app.orderbook.set_transport_state(true, "ws", None);
                            let sub_cmd = serde_json::json!({
                                "id": 1,
                                "cmd": "subscribe",
                                "params": {
                                    "channels": ["orderbook_delta"],
                                    "market_tickers": tickers,
                                }
                            });
                            if let Err(e) = conn.send_text(&sub_cmd.to_string()) {
                                app.orderbook.set_transport_state(false, "rest", Some(e));
                            } else {
                                let mut active_ver = sub_ver;
                                loop {
                                    // Check if subscriptions changed (need update_subscription/reconnect) or resync requested.
                                    let (cur_tickers, cur_ver) = {
                                        let st = app
                                            .orderbook
                                            .inner
                                            .lock()
                                            .unwrap_or_else(|p| p.into_inner());
                                        (st.subscribed_tickers.clone(), st.sub_version)
                                    };
                                    if cur_tickers.is_empty() {
                                        break;
                                    }
                                    if cur_ver != active_ver {
                                        active_ver = cur_ver;
                                        let upd = serde_json::json!({
                                            "id": 2,
                                            "cmd": "subscribe",
                                            "params": {
                                                "channels": ["orderbook_delta"],
                                                "market_tickers": cur_tickers,
                                            }
                                        });
                                        if let Err(e) = conn.send_text(&upd.to_string()) {
                                            app.orderbook.set_transport_state(false, "rest", Some(e));
                                            break;
                                        }
                                    }
                                    let gaps = app.orderbook.resync_needed_tickers();
                                    if !gaps.is_empty() {
                                        let _ = poll_rest_once(&app, &rest_base, &gaps, &mut rest_seq);
                                    }

                                    match conn.read_frame() {
                                        Ok((0x1, payload)) => {
                                            match std::str::from_utf8(&payload) {
                                                Ok(txt) => {
                                                    if let Err(e) = app.orderbook.ingest_ws_message(txt) {
                                                        let mut st = app
                                                            .orderbook
                                                            .inner
                                                            .lock()
                                                            .unwrap_or_else(|p| p.into_inner());
                                                        st.last_error = Some(e);
                                                    }
                                                }
                                                Err(_) => {
                                                    let mut st = app
                                                        .orderbook
                                                        .inner
                                                        .lock()
                                                        .unwrap_or_else(|p| p.into_inner());
                                                    st.last_error =
                                                        Some("invalid UTF-8 in WS text frame".to_string());
                                                }
                                            }
                                        }
                                        Ok((0x8, _)) => {
                                            app.orderbook.set_transport_state(
                                                false,
                                                "rest",
                                                Some("WS closed by remote".to_string()),
                                            );
                                            break;
                                        }
                                        Ok(_) => {}
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
                                            app.orderbook.set_transport_state(
                                                false,
                                                "rest",
                                                Some(format!("WS stream error: {e}")),
                                            );
                                            break;
                                        }
                                    }
                                }
                            }
                            next_ws_attempt_ms = now_ms() + backoff_ms as i64;
                            backoff_ms = (backoff_ms * 2).min(WS_RECONNECT_MAX_MS);
                        }
                        Err(e) => {
                            app.orderbook.set_transport_state(false, "rest", Some(e));
                            next_ws_attempt_ms = now_ms() + backoff_ms as i64;
                            backoff_ms = (backoff_ms * 2).min(WS_RECONNECT_MAX_MS);
                        }
                    }
                }
            }

            // REST L2 fallback poll for active subscriptions.
            let (ok_count, err_opt) = poll_rest_once(&app, &rest_base, &tickers, &mut rest_seq);
            if ok_count > 0 {
                app.orderbook.set_transport_state(false, "rest", err_opt);
            } else if let Some(err) = err_opt {
                app.orderbook
                    .set_transport_state(false, "offline", Some(err));
            } else {
                app.orderbook.set_transport_state(false, "offline", None);
            }

            let st = app.orderbook.inner.lock().unwrap_or_else(|p| p.into_inner());
            if !st.subscribed_tickers.is_empty() && st.sub_version == sub_ver {
                let _ = app
                    .orderbook
                    .cv
                    .wait_timeout(st, Duration::from_millis(REST_POLL_INTERVAL_MS));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_rfc3174_and_ws_accept_key() {
        // RFC 6455 Section 1.3 canonical test vector
        assert_eq!(
            compute_ws_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn snapshot_delta_sequence_gap_and_book_walk_math() {
        let store = OrderbookStore::new(PathBuf::new());
        store.ingest_snapshot(
            "KXBTC15M-TEST",
            &[[55, 20], [53, 40]],
            &[[42, 5], [40, 10]],
            1,
            "ws",
        );

        let snap = store.snapshot("KXBTC15M-TEST");
        assert!(!snap.stale);
        assert_eq!(snap.best_yes_bid, Some(55));
        // NO bid 42 -> YES ask 58 (qty 5); NO bid 40 -> YES ask 60 (qty 10)
        assert_eq!(snap.best_yes_ask, Some(58));
        assert_eq!(snap.spread_cents, Some(3));
        assert_eq!(snap.yes_asks.len(), 2);
        assert_eq!(snap.yes_asks[0].price_cents, 58);
        assert_eq!(snap.yes_asks[1].price_cents, 60);

        // Walk 10 contracts on YES: 5 @ 58c + 5 @ 60c -> VWAP 59.0c, slippage 1.0c
        let w = store.walk_book("KXBTC15M-TEST", "yes", 10).unwrap();
        assert_eq!(w.fillable_qty, 10);
        assert!(w.fully_fillable);
        assert_eq!(w.best_price_cents, 58);
        assert_eq!(w.worst_level_cents, 60);
        assert!((w.vwap_cents - 59.0).abs() < 1e-9);
        assert!((w.slippage_cents - 1.0).abs() < 1e-9);
        assert_eq!(w.levels_touched, 2);

        // Queue ahead at 55c is 20; at 53c is 20 + 40 = 60; at 56c is 0.
        assert_eq!(store.queue_ahead_at("KXBTC15M-TEST", "yes", 55), 20);
        assert_eq!(store.queue_ahead_at("KXBTC15M-TEST", "yes", 53), 60);
        assert_eq!(store.queue_ahead_at("KXBTC15M-TEST", "yes", 56), 0);

        // Sequence 2 delta succeeds.
        assert!(store.ingest_delta("KXBTC15M-TEST", "yes", 55, -5, 2));
        assert_eq!(store.queue_ahead_at("KXBTC15M-TEST", "yes", 55), 15);

        // Sequence 4 skips 3 -> sets needs_resync and rejects delta.
        assert!(!store.ingest_delta("KXBTC15M-TEST", "yes", 55, -5, 4));
        let gap_snap = store.snapshot("KXBTC15M-TEST");
        assert!(gap_snap.needs_resync);
        assert!(gap_snap.stale);
        assert_eq!(store.stream_status().resync_count, 1);

        // Partial fill when requested_qty exceeds total ask depth (15 available vs 25 requested).
        let partial = store.walk_book("KXBTC15M-TEST", "yes", 25).unwrap();
        assert_eq!(partial.fillable_qty, 15);
        assert!(!partial.fully_fillable);
        assert_eq!(partial.worst_level_cents, 60);
        assert!((partial.vwap_cents - (5.0 * 58.0 + 10.0 * 60.0) / 15.0).abs() < 1e-9);

        // NO-side walk: YES bids [55, 15], [53, 40] -> NO asks [45, 15], [47, 40].
        // Walk 25 NO contracts -> 15@45 + 10@47 => VWAP = (675 + 470)/25 = 45.8c
        let no_walk = store.walk_book("KXBTC15M-TEST", "no", 25).unwrap();
        assert_eq!(no_walk.fillable_qty, 25);
        assert!(no_walk.fully_fillable);
        assert_eq!(no_walk.best_price_cents, 45);
        assert_eq!(no_walk.worst_level_cents, 47);
        assert!((no_walk.vwap_cents - 45.8).abs() < 1e-9);

        // Empty book returns None for walk_book and 0 for queue_ahead_at.
        assert!(store.walk_book("KXBTC15M-EMPTY", "yes", 10).is_none());
        assert_eq!(store.queue_ahead_at("KXBTC15M-EMPTY", "yes", 50), 0);
    }

    #[test]
    fn subscription_bounds_and_ws_message_envelope_parsing() {
        let store = OrderbookStore::new(PathBuf::new());
        let too_many: Vec<String> = (1..=17).map(|i| format!("KXBTC15M-T{i}")).collect();
        assert!(store.subscribe_tickers(&too_many, true).is_err());
        assert!(store
            .subscribe_tickers(&["bad/ticker".to_string()], false)
            .is_err());

        for i in 1..=18 {
            let res = store.subscribe_tickers(&[format!("KXBTC15M-T{i}")], false);
            assert!(res.is_ok());
            assert!(res.unwrap().len() <= MAX_SUBSCRIBED_TICKERS);
        }
        assert_eq!(store.subscribed_tickers().len(), 16);

        let ws_snap = serde_json::json!({
            "type": "orderbook_snapshot",
            "seq": 10,
            "msg": {
                "market_ticker": "KXBTC15M-WS1",
                "yes": [[60, 12]],
                "no": [[38, 8], [35, 12]]
            }
        })
        .to_string();
        store.ingest_ws_message(&ws_snap).unwrap();
        let ws_delta = serde_json::json!({
            "type": "orderbook_delta",
            "seq": 11,
            "msg": {
                "market_ticker": "KXBTC15M-WS1",
                "side": "no",
                "price": 38,
                "delta": 2
            }
        })
        .to_string();
        store.ingest_ws_message(&ws_delta).unwrap();
        let snap = store.snapshot("KXBTC15M-WS1");
        assert_eq!(snap.last_seq, 11);
        assert_eq!(snap.source, "ws");
        assert_eq!(snap.yes_asks[0].price_cents, 62);
        assert_eq!(snap.yes_asks[0].qty, 10);
        assert_eq!(store.stream_status().delta_count, 1);
    }

    #[test]
    fn hardening_malformed_crossed_clamping_zero_walk_and_bounded_disk_memory() {
        let dir = std::env::temp_dir().join(format!("ob-l2-hard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = OrderbookStore::new(dir.clone());

        // 1. Malformed WS JSON frames must error cleanly without panic.
        for bad in [
            "{not json",
            r#"{"type":"orderbook_snapshot","msg":{}}"#,
            r#"{"type":"orderbook_snapshot","msg":{"market_ticker":"../bad","yes":[[50,10]]}}"#,
            r#"{"type":"orderbook_delta","seq":0,"msg":{"market_ticker":"KXBTC15M-H1","side":"yes","price":50,"delta":1}}"#,
            r#"{"type":"orderbook_delta","seq":2,"msg":{"market_ticker":"KXBTC15M-H1","side":"maybe","price":50,"delta":1}}"#,
            r#"{"type":"orderbook_delta","seq":2,"msg":{"market_ticker":"KXBTC15M-H1","side":"yes","price":0,"delta":1}}"#,
            r#"{"type":"orderbook_delta","seq":2,"msg":{"market_ticker":"KXBTC15M-H1","side":"yes","price":100,"delta":1}}"#,
            r#"{"type":"orderbook_delta","seq":2,"msg":{"market_ticker":"KXBTC15M-H1","side":"yes","price_dollars":"NaN","delta":1}}"#,
            r#"{"type":"orderbook_delta","seq":2,"msg":{"market_ticker":"KXBTC15M-H1","side":"yes","price":50,"delta_fp":"inf"}}"#,
        ] {
            assert!(store.ingest_ws_message(bad).is_err(), "expected err for {bad}");
        }

        // 2. Price level clamping (only 1..=99c accepted; 0, >99, negative/NaN dropped).
        let mixed = serde_json::json!({
            "type": "orderbook_snapshot",
            "seq": 1,
            "msg": {
                "market_ticker": "KXBTC15M-CLAMP",
                "yes": [[0, 10], [55, 10], [100, 10], [150, 5]],
                "no": [[0, 5], [40, 8], [100, 5]]
            }
        })
        .to_string();
        store.ingest_ws_message(&mixed).unwrap();
        let clamp_snap = store.snapshot("KXBTC15M-CLAMP");
        assert!(!clamp_snap.stale);
        assert_eq!(clamp_snap.yes_bids.len(), 1);
        assert_eq!(clamp_snap.yes_bids[0].price_cents, 55);
        assert_eq!(clamp_snap.no_bids.len(), 1);
        assert_eq!(clamp_snap.no_bids[0].price_cents, 40);

        // 3. Book-walk edge cases: 0 qty -> None; insufficient depth -> partial fill.
        assert!(store.walk_book("KXBTC15M-CLAMP", "yes", 0).is_none());
        assert!(store.walk_book_fresh("KXBTC15M-CLAMP", "yes", 0).is_none());
        let insuff = store.walk_book_fresh("KXBTC15M-CLAMP", "yes", 50).unwrap();
        assert_eq!(insuff.fillable_qty, 8);
        assert!(!insuff.fully_fillable);

        // 4. Crossed book detection fails closed (needs_resync=true, stale=true, walk_book_fresh=None).
        store.ingest_snapshot("KXBTC15M-CROSS", &[[65, 10]], &[[45, 10]], 1, "ws");
        let cross_snap = store.snapshot("KXBTC15M-CROSS");
        assert!(cross_snap.needs_resync);
        assert!(cross_snap.stale);
        assert!(store.walk_book_fresh("KXBTC15M-CROSS", "yes", 5).is_none());

        // Crossed via delta also triggers needs_resync.
        store.ingest_snapshot("KXBTC15M-CROSS2", &[[50, 10]], &[[45, 10]], 1, "ws");
        assert!(!store.snapshot("KXBTC15M-CROSS2").needs_resync);
        assert!(!store.ingest_delta("KXBTC15M-CROSS2", "yes", 60, 5, 2));
        assert!(store.snapshot("KXBTC15M-CROSS2").needs_resync);

        // 5. Out-of-order seq duplicate ignored cleanly vs forward gap flags resync.
        store.ingest_snapshot("KXBTC15M-SEQ", &[[50, 10]], &[[45, 10]], 5, "ws");
        assert!(!store.ingest_delta("KXBTC15M-SEQ", "yes", 50, -2, 5)); // duplicate seq
        assert!(!store.snapshot("KXBTC15M-SEQ").needs_resync);
        assert!(!store.ingest_delta("KXBTC15M-SEQ", "yes", 50, -2, 7)); // gap 5 -> 7
        assert!(store.snapshot("KXBTC15M-SEQ").needs_resync);

        // 6. Bounded memory cache (MAX_CACHED_BOOKS = 128).
        for i in 0..160 {
            store.ingest_snapshot(&format!("KXBTC15M-M{i}"), &[[50, 1]], &[[48, 1]], 1, "rest");
        }
        assert!(store.inner.lock().unwrap().books.len() <= MAX_CACHED_BOOKS);

        // 7. Bounded disk rotation when orderbooks.jsonl >= MAX_ORDERBOOKS_JSONL_BYTES.
        let ob_path = dir.join("orderbooks.jsonl");
        let big_dummy = vec![b' '; MAX_ORDERBOOKS_JSONL_BYTES as usize + 64];
        std::fs::write(&ob_path, &big_dummy).unwrap();
        store.ingest_snapshot("KXBTC15M-ROT", &[[51, 5]], &[[47, 5]], 1, "ws");
        assert!(dir.join("orderbooks.jsonl.1").exists(), "expected rotation backup");
        assert!(
            std::fs::metadata(&ob_path).unwrap().len() < 4096,
            "fresh orderbooks.jsonl should be small after rotation"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
