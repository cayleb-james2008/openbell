//! Tick-driven Paper/Shadow Orderbook Matching Engine & Fail-Closed Automated Runner (GAP #1).
//!
//! Runs active slot strategies against live open Kalshi markets from `Snapshot` ticks
//! using `Strategy::entry_quote()`, rests whole-cent maker quotes (and executes taker intents),
//! simulates queue-ahead depletion and touch/cross fills on subsequent ticks, cancels unfilled
//! quotes `cancel_before_close_secs` before close, grades settlements into persistent
//! `<OPENBELL_DATA>/shadow_ledger.jsonl` and `<OPENBELL_DATA>/runner_state.json`
//! (NEVER persisting `"live_auto"`), enforces per-slot auto-pause on realized drawdown,
//! and routes `"live_auto"` orders ONLY when `live_armed && !killed && confirm_auto_live`.

use crate::{
    append_tape, iso_z, kalshi_live, load_strategies_with_errors, now_unix, parse_iso_z,
    risk_rails, strategies_dir_key, tape_hub, App, Strategy, SERIES,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const DEFAULT_TICK_INTERVAL_MS: u64 = 500;
const MIN_TICK_INTERVAL_MS: u64 = 50;
const MAX_TICK_INTERVAL_MS: u64 = 60_000;
const DEFAULT_CANCEL_BEFORE_CLOSE_SECS: u64 = 30;
const MAX_CANCEL_BEFORE_CLOSE_SECS: u64 = 600;
const DEFAULT_BANKROLL_DOLLARS: f64 = 500.0;

#[inline]
fn maker_fee_dollars(price: f64) -> f64 {
    0.0175 * price * (1.0 - price)
}

#[inline]
fn taker_fee_dollars(price: f64) -> f64 {
    0.07 * price * (1.0 - price)
}

#[inline]
fn to_whole_cents(price: f64) -> Option<u8> {
    if !price.is_finite() || !(0.01..=0.99).contains(&price) {
        return None;
    }
    let c = (price * 100.0).round();
    if !(1.0..=99.0).contains(&c) {
        return None;
    }
    Some(c as u8)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerMode {
    Stopped,
    Shadow,
    LiveAuto,
}

impl RunnerMode {
    pub fn as_str(self) -> &'static str {
        match self {
            RunnerMode::Stopped => "stopped",
            RunnerMode::Shadow => "shadow",
            RunnerMode::LiveAuto => "live_auto",
        }
    }

    /// Safety invariant: persisted mode is ONLY `"stopped"` or `"shadow"`, NEVER `"live_auto"`.
    fn persisted_str(self) -> &'static str {
        match self {
            RunnerMode::Shadow => "shadow",
            RunnerMode::Stopped | RunnerMode::LiveAuto => "stopped",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerSlotConfig {
    pub slot_id: usize,
    pub strategy: String,
    #[serde(default)]
    pub series: Vec<String>,
    pub max_contracts: u32,
    pub execution_mode: String,
    #[serde(default)]
    pub auto_pause_drawdown_dollars: f64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunnerConfigRequest {
    pub slots: Vec<RunnerSlotConfig>,
    #[serde(default = "default_cancel_before_close")]
    pub cancel_before_close_secs: u64,
    #[serde(default = "default_bankroll")]
    pub bankroll_dollars: f64,
    #[serde(default)]
    pub tick_interval_ms: Option<u64>,
}

fn default_cancel_before_close() -> u64 {
    DEFAULT_CANCEL_BEFORE_CLOSE_SECS
}

fn default_bankroll() -> f64 {
    DEFAULT_BANKROLL_DOLLARS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedRunnerState {
    #[serde(default)]
    mode: String,
    #[serde(default = "default_tick_interval")]
    tick_interval_ms: u64,
    #[serde(default = "default_cancel_before_close")]
    cancel_before_close_secs: u64,
    #[serde(default = "default_bankroll")]
    bankroll_dollars: f64,
    #[serde(default)]
    slots: Vec<RunnerSlotConfig>,
}

fn default_tick_interval() -> u64 {
    DEFAULT_TICK_INTERVAL_MS
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerSlotState {
    pub slot_id: usize,
    pub strategy: String,
    pub series: Vec<String>,
    pub max_contracts: u32,
    pub execution_mode: String,
    pub auto_pause_drawdown_dollars: f64,
    pub enabled: bool,
    pub paused: bool,
    pub pause_reason: Option<String>,
    pub open_quotes_count: usize,
    pub filled_positions_count: usize,
    pub quotes_posted: u64,
    pub fills: u64,
    pub realized_pnl_dollars: f64,
    pub unrealized_exposure_dollars: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualOrLiveOrder {
    pub order_id: String,
    pub slot_id: usize,
    pub strategy: String,
    pub ticker: String,
    pub series: String,
    pub mode: String,
    pub execution_mode: String,
    pub side: String,
    pub limit_cents: u8,
    pub contracts: u32,
    pub filled_contracts: u32,
    pub queue_ahead_initial: u32,
    pub queue_ahead_remaining: u32,
    pub posted_at: i64,
    pub close_epoch: i64,
    pub live_order_id: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerStats {
    pub quotes_posted: u64,
    pub fills: u64,
    pub fill_rate_pct: f64,
    pub realized_pnl_dollars: f64,
    pub unrealized_exposure_dollars: f64,
    pub avg_fill_slippage_cents: f64,
    pub avg_queue_wait_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerStatusResponse {
    pub mode: String,
    pub live_armed: bool,
    pub killed: bool,
    pub tick_interval_ms: u64,
    pub cancel_before_close_secs: u64,
    pub bankroll_dollars: f64,
    pub active_slots: Vec<RunnerSlotState>,
    pub open_quotes: Vec<VirtualOrLiveOrder>,
    pub stats: RunnerStats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEvent {
    pub seq: u64,
    pub ts: i64,
    pub event: String,
    pub mode: String,
    pub slot_id: usize,
    pub strategy: String,
    #[serde(default)]
    pub ticker: String,
    #[serde(default)]
    pub series: String,
    #[serde(default)]
    pub order_id: String,
    #[serde(default)]
    pub side: String,
    #[serde(default)]
    pub execution_mode: String,
    #[serde(default)]
    pub limit_cents: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_cents: Option<u8>,
    #[serde(default)]
    pub contracts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slippage_cents: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_wait_secs: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_dollars: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realized_pnl_dollars: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_order_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyExecutionStats {
    pub strategy: String,
    pub quotes_posted: u64,
    pub quotes_canceled: u64,
    pub fills: u64,
    pub windows_settled: u64,
    pub fill_rate_pct: f64,
    pub realized_pnl_dollars: f64,
    pub total_fees_dollars: f64,
    pub avg_fill_slippage_cents: f64,
    pub avg_queue_wait_secs: f64,
    pub paused_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerLedgerResponse {
    pub entries: Vec<LedgerEvent>,
    pub summary_by_strategy: Vec<StrategyExecutionStats>,
}

#[derive(Debug, Clone)]
struct SlotRuntime {
    cfg: RunnerSlotConfig,
    paused: bool,
    pause_reason: Option<String>,
    quotes_posted: u64,
    fills: u64,
    realized_pnl_dollars: f64,
    peak_realized_pnl_dollars: f64,
}

#[derive(Debug, Clone)]
struct RestingQuote {
    order: VirtualOrLiveOrder,
    last_bid_size: f32,
}

#[derive(Debug, Clone)]
struct FilledPosition {
    order_id: String,
    slot_id: usize,
    strategy: String,
    ticker: String,
    series: String,
    mode: String,
    execution_mode: String,
    side: String,
    fill_cents: u8,
    contracts: u32,
    fee_dollars: f64,
    live_order_id: Option<String>,
}

struct RunnerState {
    data_dir: PathBuf,
    mode: RunnerMode,
    tick_interval_ms: u64,
    cancel_before_close_secs: u64,
    bankroll_dollars: f64,
    slots: Vec<SlotRuntime>,
    open_quotes: Vec<RestingQuote>,
    filled_positions: Vec<FilledPosition>,
    settled_or_filled_slot_tickers: HashSet<(usize, String)>,
    ticker_cursor: HashMap<String, usize>,
    ledger: Vec<LedgerEvent>,
    next_seq: u64,
    next_order_num: u64,
    total_quotes_posted: u64,
    total_fills: u64,
    total_realized_pnl_dollars: f64,
    slippage_sum_cents: f64,
    slippage_count: u64,
    queue_wait_sum_secs: f64,
    queue_wait_count: u64,
    live_poll_last: Option<Instant>,
}

struct PendingLiveOrderIntent {
    slot_id: usize,
    strategy: String,
    ticker: String,
    series: String,
    execution_mode: String,
    side: String,
    limit_cents: u8,
    quote_cents: u8,
    contracts: u32,
    posted_at: i64,
    close_epoch: i64,
    client_order_id: String,
}

pub struct ShadowRunner {
    state: Mutex<RunnerState>,
    cv: Condvar,
    halted: AtomicBool,
    tick_notify: AtomicBool,
}

fn ledger_path(data_dir: &Path) -> PathBuf {
    data_dir.join("shadow_ledger.jsonl")
}

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("runner_state.json")
}

fn write_atomic_json<T: Serialize>(path: &Path, val: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create data dir: {e}"))?;
    }
    let body = serde_json::to_vec_pretty(val).map_err(|e| format!("serialize state: {e}"))?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    std::fs::write(&tmp, &body).map_err(|e| format!("write temp state: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename state file: {e}")
    })
}

fn normalize_slot(slot: &RunnerSlotConfig) -> Result<RunnerSlotConfig, String> {
    let strategy = slot.strategy.trim().to_string();
    if strategy.is_empty() {
        return Err(format!("slot {} strategy name cannot be empty", slot.slot_id));
    }
    if slot.max_contracts == 0 || slot.max_contracts > 10_000 {
        return Err(format!(
            "slot {} max_contracts must be between 1 and 10,000",
            slot.slot_id
        ));
    }
    let exec_mode = slot.execution_mode.trim().to_ascii_lowercase();
    if exec_mode != "maker" && exec_mode != "taker" {
        return Err(format!(
            "slot {} execution_mode must be \"maker\" or \"taker\", got \"{}\"",
            slot.slot_id, slot.execution_mode
        ));
    }
    if !slot.auto_pause_drawdown_dollars.is_finite() || slot.auto_pause_drawdown_dollars < 0.0 {
        return Err(format!(
            "slot {} auto_pause_drawdown_dollars must be finite >= 0.0",
            slot.slot_id
        ));
    }
    let series: Vec<String> = slot
        .series
        .iter()
        .map(|s| s.trim().to_ascii_uppercase())
        .filter(|s| !s.is_empty())
        .collect();
    Ok(RunnerSlotConfig {
        slot_id: slot.slot_id,
        strategy,
        series,
        max_contracts: slot.max_contracts,
        execution_mode: exec_mode,
        auto_pause_drawdown_dollars: slot.auto_pause_drawdown_dollars,
        enabled: slot.enabled,
    })
}

fn load_ledger_events(path: &Path) -> Vec<LedgerEvent> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<LedgerEvent>(trimmed) {
            out.push(ev);
        }
    }
    out
}

fn publish_runner_sse(kind: &str, ev: &LedgerEvent) {
    let payload = serde_json::json!({
        "type": kind,
        "seq": ev.seq,
        "ts": ev.ts,
        "mode": ev.mode,
        "slot_id": ev.slot_id,
        "strategy": ev.strategy,
        "ticker": ev.ticker,
        "series": ev.series,
        "order_id": ev.order_id,
        "side": ev.side,
        "execution_mode": ev.execution_mode,
        "limit_cents": ev.limit_cents,
        "fill_cents": ev.fill_cents,
        "contracts": ev.contracts,
        "realized_pnl_dollars": ev.realized_pnl_dollars,
        "result": ev.result,
        "reason": ev.reason,
        "live_order_id": ev.live_order_id,
    });
    tape_hub().publish_runner(payload.to_string());
}

impl RunnerState {
    fn persist_state(&self) -> Result<(), String> {
        let persisted = PersistedRunnerState {
            // Safety invariant: NEVER persist "live_auto".
            mode: self.mode.persisted_str().to_string(),
            tick_interval_ms: self.tick_interval_ms,
            cancel_before_close_secs: self.cancel_before_close_secs,
            bankroll_dollars: self.bankroll_dollars,
            slots: self.slots.iter().map(|s| s.cfg.clone()).collect(),
        };
        write_atomic_json(&state_path(&self.data_dir), &persisted)
    }

    fn next_order_id(&mut self) -> String {
        self.next_order_num = self.next_order_num.saturating_add(1);
        format!("ob-run-{}-{}", now_unix(), self.next_order_num)
    }

    fn append_event(&mut self, mut ev: LedgerEvent) {
        self.next_seq = self.next_seq.saturating_add(1);
        ev.seq = self.next_seq;
        if let Ok(line) = serde_json::to_string(&ev) {
            let mut row = line;
            row.push('\n');
            let _ = append_tape(&ledger_path(&self.data_dir), &row);
        }
        self.ledger.push(ev);
    }

    fn emit_quote_canceled(
        &mut self,
        q: &VirtualOrLiveOrder,
        ts: i64,
        reason: &str,
    ) {
        let ev = LedgerEvent {
            seq: 0,
            ts,
            event: "QuoteCanceled".to_string(),
            mode: q.mode.clone(),
            slot_id: q.slot_id,
            strategy: q.strategy.clone(),
            ticker: q.ticker.clone(),
            series: q.series.clone(),
            order_id: q.order_id.clone(),
            side: q.side.clone(),
            execution_mode: q.execution_mode.clone(),
            limit_cents: q.limit_cents,
            fill_cents: None,
            contracts: q.contracts.saturating_sub(q.filled_contracts),
            slippage_cents: None,
            queue_wait_secs: None,
            fee_dollars: None,
            realized_pnl_dollars: None,
            result: None,
            reason: Some(reason.to_string()),
            live_order_id: q.live_order_id.clone(),
        };
        self.append_event(ev.clone());
        publish_runner_sse("quote_canceled", &ev);
    }

    fn record_fill(
        &mut self,
        q: &VirtualOrLiveOrder,
        fill_cents: u8,
        slippage_cents: f64,
        queue_wait_secs: f64,
        fee_dollars: f64,
        filled_at: i64,
    ) {
        self.total_fills = self.total_fills.saturating_add(1);
        self.slippage_sum_cents += slippage_cents;
        self.slippage_count = self.slippage_count.saturating_add(1);
        self.queue_wait_sum_secs += queue_wait_secs;
        self.queue_wait_count = self.queue_wait_count.saturating_add(1);
        if let Some(slot) = self.slots.iter_mut().find(|s| s.cfg.slot_id == q.slot_id) {
            slot.fills = slot.fills.saturating_add(1);
        }
        self.settled_or_filled_slot_tickers
            .insert((q.slot_id, q.ticker.clone()));
        self.filled_positions.push(FilledPosition {
            order_id: q.order_id.clone(),
            slot_id: q.slot_id,
            strategy: q.strategy.clone(),
            ticker: q.ticker.clone(),
            series: q.series.clone(),
            mode: q.mode.clone(),
            execution_mode: q.execution_mode.clone(),
            side: q.side.clone(),
            fill_cents,
            contracts: q.contracts,
            fee_dollars,
            live_order_id: q.live_order_id.clone(),
        });
        let ev = LedgerEvent {
            seq: 0,
            ts: filled_at,
            event: "OrderFilled".to_string(),
            mode: q.mode.clone(),
            slot_id: q.slot_id,
            strategy: q.strategy.clone(),
            ticker: q.ticker.clone(),
            series: q.series.clone(),
            order_id: q.order_id.clone(),
            side: q.side.clone(),
            execution_mode: q.execution_mode.clone(),
            limit_cents: q.limit_cents,
            fill_cents: Some(fill_cents),
            contracts: q.contracts,
            slippage_cents: Some(slippage_cents),
            queue_wait_secs: Some(queue_wait_secs),
            fee_dollars: Some(fee_dollars),
            realized_pnl_dollars: None,
            result: None,
            reason: None,
            live_order_id: q.live_order_id.clone(),
        };
        self.append_event(ev.clone());
        publish_runner_sse("quote_filled", &ev);
    }

    fn settle_ticker(&mut self, ticker: &str, result: &str, ts: i64) {
        // Cancel any unfilled resting quotes on the settled ticker first.
        let mut remaining_quotes = Vec::with_capacity(self.open_quotes.len());
        let mut to_cancel = Vec::new();
        for rq in self.open_quotes.drain(..) {
            if rq.order.ticker == ticker {
                to_cancel.push(rq.order);
            } else {
                remaining_quotes.push(rq);
            }
        }
        self.open_quotes = remaining_quotes;
        for q in to_cancel {
            self.emit_quote_canceled(&q, ts, "market_settled");
        }

        // Grade any filled positions on this ticker against venue settlement result.
        let mut remaining_positions = Vec::with_capacity(self.filled_positions.len());
        let mut settling = Vec::new();
        for pos in self.filled_positions.drain(..) {
            if pos.ticker == ticker {
                settling.push(pos);
            } else {
                remaining_positions.push(pos);
            }
        }
        self.filled_positions = remaining_positions;

        for pos in settling {
            let won = pos.side == result;
            let gross_payout = if won { pos.contracts as f64 } else { 0.0 };
            let cost = (pos.fill_cents as f64 / 100.0) * (pos.contracts as f64);
            let net_pnl = gross_payout - cost - pos.fee_dollars;
            self.total_realized_pnl_dollars += net_pnl;

            let ev = LedgerEvent {
                seq: 0,
                ts,
                event: "WindowSettled".to_string(),
                mode: pos.mode.clone(),
                slot_id: pos.slot_id,
                strategy: pos.strategy.clone(),
                ticker: pos.ticker.clone(),
                series: pos.series.clone(),
                order_id: pos.order_id.clone(),
                side: pos.side.clone(),
                execution_mode: pos.execution_mode.clone(),
                limit_cents: pos.fill_cents,
                fill_cents: Some(pos.fill_cents),
                contracts: pos.contracts,
                slippage_cents: None,
                queue_wait_secs: None,
                fee_dollars: Some(pos.fee_dollars),
                realized_pnl_dollars: Some(net_pnl),
                result: Some(result.to_string()),
                reason: None,
                live_order_id: pos.live_order_id.clone(),
            };
            self.append_event(ev.clone());
            publish_runner_sse("window_settled", &ev);

            // Check per-slot realized drawdown circuit breaker.
            let mut pause_trigger: Option<(usize, String, String, f64, f64)> = None;
            if let Some(slot) = self.slots.iter_mut().find(|s| s.cfg.slot_id == pos.slot_id) {
                slot.realized_pnl_dollars += net_pnl;
                if slot.realized_pnl_dollars > slot.peak_realized_pnl_dollars {
                    slot.peak_realized_pnl_dollars = slot.realized_pnl_dollars;
                }
                let drawdown = slot.peak_realized_pnl_dollars - slot.realized_pnl_dollars;
                if slot.cfg.auto_pause_drawdown_dollars > 0.0
                    && drawdown >= slot.cfg.auto_pause_drawdown_dollars - 1e-9
                    && !slot.paused
                {
                    let reason = format!(
                        "auto-paused: realized drawdown ${drawdown:.2} >= limit ${:.2}",
                        slot.cfg.auto_pause_drawdown_dollars
                    );
                    slot.paused = true;
                    slot.pause_reason = Some(reason.clone());
                    pause_trigger = Some((
                        slot.cfg.slot_id,
                        slot.cfg.strategy.clone(),
                        reason,
                        drawdown,
                        slot.realized_pnl_dollars,
                    ));
                }
            }
            if let Some((slot_id, strategy, reason, _dd, slot_pnl)) = pause_trigger {
                // Cancel any resting shadow quotes for the paused slot.
                let mut kept = Vec::with_capacity(self.open_quotes.len());
                let mut paused_quotes = Vec::new();
                for rq in self.open_quotes.drain(..) {
                    if rq.order.slot_id == slot_id {
                        paused_quotes.push(rq.order);
                    } else {
                        kept.push(rq);
                    }
                }
                self.open_quotes = kept;
                for q in paused_quotes {
                    self.emit_quote_canceled(&q, ts, "slot_auto_paused");
                }
                let pause_ev = LedgerEvent {
                    seq: 0,
                    ts,
                    event: "SlotPaused".to_string(),
                    mode: pos.mode,
                    slot_id,
                    strategy,
                    ticker: pos.ticker,
                    series: pos.series,
                    order_id: String::new(),
                    side: String::new(),
                    execution_mode: String::new(),
                    limit_cents: 0,
                    fill_cents: None,
                    contracts: 0,
                    slippage_cents: None,
                    queue_wait_secs: None,
                    fee_dollars: None,
                    realized_pnl_dollars: Some(slot_pnl),
                    result: None,
                    reason: Some(reason),
                    live_order_id: None,
                };
                self.append_event(pause_ev.clone());
                publish_runner_sse("slot_auto_paused", &pause_ev);
            }
        }
    }

    fn status_snapshot(&self, live_armed: bool, killed: bool) -> RunnerStatusResponse {
        let mut active_slots = Vec::with_capacity(self.slots.len());
        let mut total_unrealized = 0.0;
        for slot in &self.slots {
            let sid = slot.cfg.slot_id;
            let open_quotes_count = self
                .open_quotes
                .iter()
                .filter(|rq| rq.order.slot_id == sid)
                .count();
            let filled_positions_count = self
                .filled_positions
                .iter()
                .filter(|p| p.slot_id == sid)
                .count();
            let quote_exp: f64 = self
                .open_quotes
                .iter()
                .filter(|rq| rq.order.slot_id == sid)
                .map(|rq| {
                    let rem = rq.order.contracts.saturating_sub(rq.order.filled_contracts);
                    (rq.order.limit_cents as f64 / 100.0) * (rem as f64)
                })
                .sum();
            let pos_exp: f64 = self
                .filled_positions
                .iter()
                .filter(|p| p.slot_id == sid)
                .map(|p| (p.fill_cents as f64 / 100.0) * (p.contracts as f64))
                .sum();
            let slot_unrealized = quote_exp + pos_exp;
            total_unrealized += slot_unrealized;
            active_slots.push(RunnerSlotState {
                slot_id: sid,
                strategy: slot.cfg.strategy.clone(),
                series: slot.cfg.series.clone(),
                max_contracts: slot.cfg.max_contracts,
                execution_mode: slot.cfg.execution_mode.clone(),
                auto_pause_drawdown_dollars: slot.cfg.auto_pause_drawdown_dollars,
                enabled: slot.cfg.enabled,
                paused: slot.paused,
                pause_reason: slot.pause_reason.clone(),
                open_quotes_count,
                filled_positions_count,
                quotes_posted: slot.quotes_posted,
                fills: slot.fills,
                realized_pnl_dollars: slot.realized_pnl_dollars,
                unrealized_exposure_dollars: slot_unrealized,
            });
        }

        let open_quotes: Vec<VirtualOrLiveOrder> = self
            .open_quotes
            .iter()
            .map(|rq| rq.order.clone())
            .collect();
        let fill_rate_pct = if self.total_quotes_posted > 0 {
            (self.total_fills as f64 / self.total_quotes_posted as f64) * 100.0
        } else {
            0.0
        };
        let avg_fill_slippage_cents = if self.slippage_count > 0 {
            self.slippage_sum_cents / self.slippage_count as f64
        } else {
            0.0
        };
        let avg_queue_wait_secs = if self.queue_wait_count > 0 {
            self.queue_wait_sum_secs / self.queue_wait_count as f64
        } else {
            0.0
        };

        RunnerStatusResponse {
            mode: self.mode.as_str().to_string(),
            live_armed,
            killed,
            tick_interval_ms: self.tick_interval_ms,
            cancel_before_close_secs: self.cancel_before_close_secs,
            bankroll_dollars: self.bankroll_dollars,
            active_slots,
            open_quotes,
            stats: RunnerStats {
                quotes_posted: self.total_quotes_posted,
                fills: self.total_fills,
                fill_rate_pct,
                realized_pnl_dollars: self.total_realized_pnl_dollars,
                unrealized_exposure_dollars: total_unrealized,
                avg_fill_slippage_cents,
                avg_queue_wait_secs,
            },
        }
    }
}

impl ShadowRunner {
    pub fn load(data_dir: &Path, default_bankroll: f64) -> Self {
        let disk_state = std::fs::read_to_string(state_path(data_dir))
            .ok()
            .and_then(|raw| serde_json::from_str::<PersistedRunnerState>(&raw).ok());

        let (mode, tick_interval_ms, cancel_before_close_secs, bankroll_dollars, slot_cfgs) =
            match disk_state {
                Some(s) => {
                    // Safety invariant: NEVER boot in "live_auto", even if runner_state.json was edited.
                    let safe_mode = if s.mode == "shadow" {
                        RunnerMode::Shadow
                    } else {
                        RunnerMode::Stopped
                    };
                    let valid_slots: Vec<RunnerSlotConfig> = s
                        .slots
                        .iter()
                        .filter_map(|sl| normalize_slot(sl).ok())
                        .collect();
                    let bankroll = if s.bankroll_dollars.is_finite() && s.bankroll_dollars > 0.0 {
                        s.bankroll_dollars
                    } else if default_bankroll.is_finite() && default_bankroll > 0.0 {
                        default_bankroll
                    } else {
                        DEFAULT_BANKROLL_DOLLARS
                    };
                    (
                        safe_mode,
                        s.tick_interval_ms
                            .clamp(MIN_TICK_INTERVAL_MS, MAX_TICK_INTERVAL_MS),
                        s.cancel_before_close_secs
                            .min(MAX_CANCEL_BEFORE_CLOSE_SECS),
                        bankroll,
                        valid_slots,
                    )
                }
                None => (
                    RunnerMode::Stopped,
                    DEFAULT_TICK_INTERVAL_MS,
                    DEFAULT_CANCEL_BEFORE_CLOSE_SECS,
                    if default_bankroll.is_finite() && default_bankroll > 0.0 {
                        default_bankroll
                    } else {
                        DEFAULT_BANKROLL_DOLLARS
                    },
                    Vec::new(),
                ),
            };

        let ledger = load_ledger_events(&ledger_path(data_dir));
        let mut next_seq = 0u64;
        let mut total_quotes_posted = 0u64;
        let mut total_fills = 0u64;
        let mut total_realized_pnl_dollars = 0.0;
        let mut slippage_sum_cents = 0.0;
        let mut slippage_count = 0u64;
        let mut queue_wait_sum_secs = 0.0;
        let mut queue_wait_count = 0u64;
        let mut slot_stats: HashMap<usize, (u64, u64, f64, f64)> = HashMap::new();

        for ev in &ledger {
            if ev.seq > next_seq {
                next_seq = ev.seq;
            }
            let entry = slot_stats.entry(ev.slot_id).or_insert((0, 0, 0.0, 0.0));
            match ev.event.as_str() {
                "QuotePosted" => {
                    total_quotes_posted = total_quotes_posted.saturating_add(1);
                    entry.0 = entry.0.saturating_add(1);
                }
                "OrderFilled" => {
                    total_fills = total_fills.saturating_add(1);
                    entry.1 = entry.1.saturating_add(1);
                    if let Some(sl) = ev.slippage_cents {
                        slippage_sum_cents += sl;
                        slippage_count = slippage_count.saturating_add(1);
                    }
                    if let Some(qw) = ev.queue_wait_secs {
                        queue_wait_sum_secs += qw;
                        queue_wait_count = queue_wait_count.saturating_add(1);
                    }
                }
                "WindowSettled" => {
                    if let Some(pnl) = ev.realized_pnl_dollars {
                        total_realized_pnl_dollars += pnl;
                        entry.2 += pnl;
                        if entry.2 > entry.3 {
                            entry.3 = entry.2;
                        }
                    }
                }
                _ => {}
            }
        }

        let slots = slot_cfgs
            .into_iter()
            .map(|cfg| {
                let (qp, fl, pnl, peak) = slot_stats
                    .get(&cfg.slot_id)
                    .copied()
                    .unwrap_or((0, 0, 0.0, 0.0));
                SlotRuntime {
                    cfg,
                    paused: false,
                    pause_reason: None,
                    quotes_posted: qp,
                    fills: fl,
                    realized_pnl_dollars: pnl,
                    peak_realized_pnl_dollars: peak,
                }
            })
            .collect();

        Self {
            state: Mutex::new(RunnerState {
                data_dir: data_dir.to_path_buf(),
                mode,
                tick_interval_ms,
                cancel_before_close_secs,
                bankroll_dollars,
                slots,
                open_quotes: Vec::new(),
                filled_positions: Vec::new(),
                settled_or_filled_slot_tickers: HashSet::new(),
                ticker_cursor: HashMap::new(),
                ledger,
                next_seq,
                next_order_num: 0,
                total_quotes_posted,
                total_fills,
                total_realized_pnl_dollars,
                slippage_sum_cents,
                slippage_count,
                queue_wait_sum_secs,
                queue_wait_count,
                live_poll_last: None,
            }),
            cv: Condvar::new(),
            halted: AtomicBool::new(false),
            tick_notify: AtomicBool::new(false),
        }
    }

    pub fn notify_tick(&self) {
        self.tick_notify.store(true, Ordering::Release);
        self.cv.notify_all();
    }

    pub fn status(&self, live_armed: bool, killed: bool) -> RunnerStatusResponse {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.status_snapshot(live_armed, killed)
    }

    pub fn configure(
        &self,
        req: RunnerConfigRequest,
        live_armed: bool,
        killed: bool,
    ) -> Result<RunnerStatusResponse, String> {
        if !req.bankroll_dollars.is_finite() || req.bankroll_dollars <= 0.0 || req.bankroll_dollars > 10_000_000.0 {
            return Err("bankroll_dollars must be finite between $0.01 and $10,000,000".to_string());
        }
        if req.cancel_before_close_secs > MAX_CANCEL_BEFORE_CLOSE_SECS {
            return Err(format!(
                "cancel_before_close_secs must be <= {MAX_CANCEL_BEFORE_CLOSE_SECS}"
            ));
        }
        let mut normalized = Vec::with_capacity(req.slots.len());
        let mut seen_ids = HashSet::new();
        for s in &req.slots {
            let norm = normalize_slot(s)?;
            if !seen_ids.insert(norm.slot_id) {
                return Err(format!("duplicate slot_id {}", norm.slot_id));
            }
            normalized.push(norm);
        }

        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(iv) = req.tick_interval_ms {
            st.tick_interval_ms = iv.clamp(MIN_TICK_INTERVAL_MS, MAX_TICK_INTERVAL_MS);
        }
        st.cancel_before_close_secs = req.cancel_before_close_secs;
        st.bankroll_dollars = req.bankroll_dollars;

        let prev_map: HashMap<usize, SlotRuntime> = st
            .slots
            .drain(..)
            .map(|sr| (sr.cfg.slot_id, sr))
            .collect();
        let mut new_slots = Vec::with_capacity(normalized.len());
        for cfg in normalized {
            if let Some(prev) = prev_map.get(&cfg.slot_id) {
                let reset_pause = cfg.enabled && prev.cfg.strategy == cfg.strategy && prev.paused;
                new_slots.push(SlotRuntime {
                    cfg,
                    paused: if reset_pause || !prev.cfg.enabled { false } else { prev.paused },
                    pause_reason: if reset_pause { None } else { prev.pause_reason.clone() },
                    quotes_posted: prev.quotes_posted,
                    fills: prev.fills,
                    realized_pnl_dollars: prev.realized_pnl_dollars,
                    // Reset high-water peak when operator reconfigures/unpauses so a cleared pause doesn't re-trip immediately.
                    peak_realized_pnl_dollars: if reset_pause {
                        prev.realized_pnl_dollars
                    } else {
                        prev.peak_realized_pnl_dollars
                    },
                });
            } else {
                new_slots.push(SlotRuntime {
                    cfg,
                    paused: false,
                    pause_reason: None,
                    quotes_posted: 0,
                    fills: 0,
                    realized_pnl_dollars: 0.0,
                    peak_realized_pnl_dollars: 0.0,
                });
            }
        }
        st.slots = new_slots;
        st.persist_state()?;
        let snap = st.status_snapshot(live_armed, killed);
        drop(st);
        self.cv.notify_all();
        Ok(snap)
    }

    pub fn start(
        &self,
        requested_mode: &str,
        confirm_auto_live: bool,
        live_armed: bool,
        killed: bool,
    ) -> Result<RunnerStatusResponse, (u16, String)> {
        let mode = match requested_mode.trim() {
            "shadow" => RunnerMode::Shadow,
            "live_auto" => RunnerMode::LiveAuto,
            other => {
                return Err((
                    400,
                    format!("mode must be \"shadow\" or \"live_auto\", got \"{other}\""),
                ));
            }
        };

        if mode == RunnerMode::LiveAuto {
            if killed {
                return Err((
                    403,
                    "refusing live_auto: emergency kill switch is engaged".to_string(),
                ));
            }
            if !live_armed {
                return Err((
                    403,
                    "refusing live_auto: live trading is disarmed (live_armed=false)".to_string(),
                ));
            }
            if !confirm_auto_live {
                return Err((
                    403,
                    "refusing live_auto: explicit confirm_auto_live=true is required".to_string(),
                ));
            }
        }

        self.halted.store(false, Ordering::SeqCst);
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.mode = mode;
        if let Err(e) = st.persist_state() {
            return Err((500, e));
        }
        let snap = st.status_snapshot(live_armed, killed);
        drop(st);
        self.tick_notify.store(true, Ordering::Release);
        self.cv.notify_all();
        Ok(snap)
    }

    pub fn stop(
        &self,
        cancel_resting: bool,
        live_cfg: Option<&kalshi_live::PersistedLiveConfig>,
        live_armed: bool,
        killed: bool,
    ) -> Result<(RunnerStatusResponse, usize, Vec<String>), String> {
        let now = now_unix();
        let (was_live_auto, live_ids, shadow_cancelled_count, snap) = {
            let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let was_live = st.mode == RunnerMode::LiveAuto;
            st.mode = RunnerMode::Stopped;
            let mut live_ids = Vec::new();
            let mut cancelled_count = 0usize;
            if cancel_resting {
                let quotes: Vec<VirtualOrLiveOrder> =
                    st.open_quotes.drain(..).map(|rq| rq.order).collect();
                cancelled_count = quotes.len();
                for q in quotes {
                    if let Some(ref lid) = q.live_order_id {
                        live_ids.push(lid.clone());
                    }
                    st.emit_quote_canceled(&q, now, "runner_stopped");
                }
            }
            st.persist_state()?;
            let snap = st.status_snapshot(live_armed, killed);
            (was_live, live_ids, cancelled_count, snap)
        };
        self.cv.notify_all();

        let mut live_errors = Vec::new();
        let mut live_cancelled = 0usize;
        if cancel_resting && (was_live_auto || !live_ids.is_empty()) {
            if let Some(cfg) = live_cfg {
                if cfg.is_configured() {
                    match kalshi_live::KalshiLiveClient::from_config(cfg) {
                        Ok(client) => {
                            if was_live_auto && live_ids.is_empty() {
                                let rep = client.cancel_all_orders();
                                live_cancelled = rep.cancelled_count;
                                live_errors.extend(rep.errors);
                            } else {
                                for oid in &live_ids {
                                    match client.cancel_order(oid) {
                                        Ok(_) => live_cancelled += 1,
                                        Err(e) => live_errors.push(format!("{oid}: {e}")),
                                    }
                                }
                            }
                        }
                        Err(e) => live_errors.push(e),
                    }
                }
            }
        }

        let total_cancelled = shadow_cancelled_count.max(live_cancelled);
        Ok((snap, total_cancelled, live_errors))
    }

    /// Fail-closed atomic halt invoked by `POST /api/live/kill-switch` BEFORE `cancel_all_orders()`.
    pub fn emergency_halt(&self, reason: &str) {
        self.halted.store(true, Ordering::SeqCst);
        let now = now_unix();
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let prev_mode = st.mode;
        st.mode = RunnerMode::Stopped;
        let quotes: Vec<VirtualOrLiveOrder> =
            st.open_quotes.drain(..).map(|rq| rq.order).collect();
        for q in &quotes {
            st.emit_quote_canceled(q, now, "emergency_kill_switch");
        }
        let _ = st.persist_state();
        let seq = st.next_seq;
        drop(st);
        self.cv.notify_all();

        let ev_payload = serde_json::json!({
            "type": "runner_halted",
            "seq": seq,
            "ts": now,
            "previous_mode": prev_mode.as_str(),
            "cancelled_shadow_quotes": quotes.len(),
            "reason": reason,
        });
        tape_hub().publish_runner(ev_payload.to_string());
    }

    pub fn ledger_query(
        &self,
        limit: usize,
        strategy_filter: Option<&str>,
        mode_filter: Option<&str>,
    ) -> RunnerLedgerResponse {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let limit = limit.clamp(1, 2000);
        let strat_f = strategy_filter.map(str::trim).filter(|s| !s.is_empty());
        let mode_f = mode_filter.map(str::trim).filter(|s| !s.is_empty());

        let mut agg: BTreeMap<String, StrategyAgg> = BTreeMap::new();
        let mut filtered: Vec<LedgerEvent> = Vec::new();

        for ev in &st.ledger {
            if let Some(sf) = strat_f {
                if ev.strategy != sf {
                    continue;
                }
            }
            if let Some(mf) = mode_f {
                if ev.mode != mf {
                    continue;
                }
            }
            let a = agg.entry(ev.strategy.clone()).or_default();
            match ev.event.as_str() {
                "QuotePosted" => a.quotes_posted += 1,
                "QuoteCanceled" => a.quotes_canceled += 1,
                "OrderFilled" => {
                    a.fills += 1;
                    if let Some(sl) = ev.slippage_cents {
                        a.slippage_sum += sl;
                        a.slippage_cnt += 1;
                    }
                    if let Some(qw) = ev.queue_wait_secs {
                        a.queue_wait_sum += qw;
                        a.queue_wait_cnt += 1;
                    }
                    if let Some(fee) = ev.fee_dollars {
                        a.total_fees_dollars += fee;
                    }
                }
                "WindowSettled" => {
                    a.windows_settled += 1;
                    if let Some(pnl) = ev.realized_pnl_dollars {
                        a.realized_pnl_dollars += pnl;
                    }
                }
                "SlotPaused" => {
                    a.paused_count += 1;
                }
                _ => {}
            }
            filtered.push(ev.clone());
        }

        let entries: Vec<LedgerEvent> = filtered.into_iter().rev().take(limit).collect();
        let summary_by_strategy = agg
            .into_iter()
            .map(|(strategy, a)| StrategyExecutionStats {
                strategy,
                quotes_posted: a.quotes_posted,
                quotes_canceled: a.quotes_canceled,
                fills: a.fills,
                windows_settled: a.windows_settled,
                fill_rate_pct: if a.quotes_posted > 0 {
                    (a.fills as f64 / a.quotes_posted as f64) * 100.0
                } else {
                    0.0
                },
                realized_pnl_dollars: a.realized_pnl_dollars,
                total_fees_dollars: a.total_fees_dollars,
                avg_fill_slippage_cents: if a.slippage_cnt > 0 {
                    a.slippage_sum / a.slippage_cnt as f64
                } else {
                    0.0
                },
                avg_queue_wait_secs: if a.queue_wait_cnt > 0 {
                    a.queue_wait_sum / a.queue_wait_cnt as f64
                } else {
                    0.0
                },
                paused_count: a.paused_count,
            })
            .collect();

        RunnerLedgerResponse {
            entries,
            summary_by_strategy,
        }
    }

    /// Execute one tick of the matching & strategy evaluation engine.
    pub fn step_once(&self, app: &App, cached_strats: &mut CachedStrategies) {
        let (live_armed, killed, live_cfg_opt) = {
            let ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
            let cfg_opt = if ld.config.is_configured() {
                Some(ld.config.clone())
            } else {
                None
            };
            (ld.rails.live_armed, ld.rails.killed, cfg_opt)
        };

        let (mode, need_live_poll) = {
            let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if st.mode == RunnerMode::Stopped {
                return;
            }
            if st.mode == RunnerMode::LiveAuto
                && (self.halted.load(Ordering::SeqCst) || !live_armed || killed)
            {
                let now = now_unix();
                st.mode = RunnerMode::Stopped;
                let quotes: Vec<VirtualOrLiveOrder> =
                    st.open_quotes.drain(..).map(|rq| rq.order).collect();
                for q in quotes {
                    st.emit_quote_canceled(&q, now, "live_disarmed_or_killed");
                }
                let _ = st.persist_state();
                return;
            }
            let poll = st.mode == RunnerMode::LiveAuto
                && st
                    .open_quotes
                    .iter()
                    .any(|rq| rq.order.mode == "live" && rq.order.live_order_id.is_some())
                && st
                    .live_poll_last
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(2));
            if poll {
                st.live_poll_last = Some(Instant::now());
            }
            (st.mode, poll)
        };

        // Reconcile open live orders against venue outside any state locks.
        if need_live_poll {
            if let Some(ref cfg) = live_cfg_opt {
                if let Ok(client) = kalshi_live::KalshiLiveClient::from_config(cfg) {
                    if let Ok(venue_orders) = client.get_orders(None) {
                        let by_id: HashMap<&str, &kalshi_live::KalshiOrder> = venue_orders
                            .iter()
                            .map(|o| (o.order_id.as_str(), o))
                            .collect();
                        let now = now_unix();
                        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
                        let mut next_open = Vec::with_capacity(st.open_quotes.len());
                        let existing = std::mem::take(&mut st.open_quotes);
                        for rq in existing {
                            if rq.order.mode != "live" {
                                next_open.push(rq);
                                continue;
                            }
                            let Some(ref lid) = rq.order.live_order_id else {
                                next_open.push(rq);
                                continue;
                            };
                            match by_id.get(lid.as_str()) {
                                Some(vo) if vo.status == "executed" || (vo.remaining_count == 0 && vo.fill_count > 0) => {
                                    let fill_cents = rq.order.limit_cents;
                                    let fee_fn = if rq.order.execution_mode == "maker" {
                                        maker_fee_dollars
                                    } else {
                                        taker_fee_dollars
                                    };
                                    let fee_d = fee_fn(fill_cents as f64 / 100.0)
                                        * (rq.order.contracts as f64);
                                    let wait_s = (now - rq.order.posted_at).max(0) as f64;
                                    st.record_fill(&rq.order, fill_cents, 0.0, wait_s, fee_d, now);
                                }
                                Some(vo) if vo.status == "canceled" || vo.status == "cancelled" => {
                                    st.emit_quote_canceled(&rq.order, now, "venue_canceled");
                                }
                                _ => next_open.push(rq),
                            }
                        }
                        st.open_quotes = next_open;
                    }
                }
            }
        }

        let tape = app.tape(&app.live_snapshots_path);
        if tape.grouped.is_empty() {
            return;
        }

        // Refresh compiled strategy cache only when strategies_dir changes.
        cached_strats.refresh_if_needed(&app.strategies_dir, app.paper_pot);

        let mut pending_live_intents: Vec<PendingLiveOrderIntent> = Vec::new();
        let mut live_cancels: Vec<(String, VirtualOrLiveOrder, i64, String)> = Vec::new();

        {
            let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if st.mode == RunnerMode::Stopped {
                return;
            }
            let cancel_before = st.cancel_before_close_secs as i64;
            let bankroll = st.bankroll_dollars;

            for (ticker_sym, snaps) in tape.grouped.iter() {
                let ticker = ticker_sym.as_str();
                if snaps.is_empty() {
                    continue;
                }
                let prev_len = st.ticker_cursor.get(ticker).copied();
                let start_idx = match prev_len {
                    Some(n) if n >= snaps.len() => continue,
                    Some(n) => n,
                    None => {
                        let last = &snaps[snaps.len() - 1];
                        let has_open_or_pos = st
                            .open_quotes
                            .iter()
                            .any(|rq| rq.order.ticker == ticker)
                            || st.filled_positions.iter().any(|p| p.ticker == ticker);
                        if !last.result.is_empty() && !has_open_or_pos {
                            st.ticker_cursor.insert(ticker.to_string(), snaps.len());
                            continue;
                        }
                        snaps.len() - 1
                    }
                };

                for snap in &snaps[start_idx..] {
                    if !snap.result.is_empty() {
                        // Cancel any live orders on venue if needed, and settle shadow/filled positions.
                        let live_to_cancel: Vec<VirtualOrLiveOrder> = st
                            .open_quotes
                            .iter()
                            .filter(|rq| rq.order.ticker == ticker && rq.order.mode == "live")
                            .map(|rq| rq.order.clone())
                            .collect();
                        for q in live_to_cancel {
                            if let Some(ref lid) = q.live_order_id {
                                live_cancels.push((
                                    lid.clone(),
                                    q.clone(),
                                    snap.fetched_at,
                                    "market_settled".to_string(),
                                ));
                            }
                        }
                        st.settle_ticker(ticker, snap.result.as_str(), snap.fetched_at);
                        continue;
                    }

                    if !matches!(snap.status.as_str(), "open" | "active") {
                        continue;
                    }
                    let Some(close_epoch) = parse_iso_z(snap.close_time.as_str()) else {
                        continue;
                    };
                    let secs_to_close = close_epoch - snap.fetched_at;
                    let (Some(ya), Some(na)) = (snap.yes_ask.get(), snap.no_ask.get()) else {
                        continue;
                    };
                    let (Some(ya_c), Some(na_c)) = (to_whole_cents(ya), to_whole_cents(na)) else {
                        continue;
                    };
                    let yb_c = snap
                        .yes_bid
                        .get()
                        .and_then(to_whole_cents)
                        .unwrap_or_else(|| 100u8.saturating_sub(na_c).max(1));
                    let nb_c = snap
                        .no_bid
                        .get()
                        .and_then(to_whole_cents)
                        .unwrap_or_else(|| 100u8.saturating_sub(ya_c).max(1));
                    let last_c = snap.last_price.get().and_then(to_whole_cents);

                    // 1. Update resting quotes on this ticker (pre-close cancel + queue-ahead matching).
                    let existing_quotes = std::mem::take(&mut st.open_quotes);
                    let mut kept_quotes = Vec::with_capacity(existing_quotes.len());
                    for mut rq in existing_quotes {
                        if rq.order.ticker != ticker {
                            kept_quotes.push(rq);
                            continue;
                        }
                        if secs_to_close <= cancel_before {
                            if rq.order.mode == "live" {
                                if let Some(ref lid) = rq.order.live_order_id {
                                    live_cancels.push((
                                        lid.clone(),
                                        rq.order,
                                        snap.fetched_at,
                                        "cancel_before_close".to_string(),
                                    ));
                                    continue;
                                }
                            }
                            st.emit_quote_canceled(&rq.order, snap.fetched_at, "cancel_before_close");
                            continue;
                        }
                        if rq.order.mode == "live" {
                            kept_quotes.push(rq);
                            continue;
                        }

                        // Shadow maker matching engine: cross or touch + queue-ahead depletion.
                        let (side_ask_c, side_bid_c, side_bid_sz) = if rq.order.side == "yes" {
                            (ya_c, yb_c, snap.yes_bid_size)
                        } else {
                            (na_c, nb_c, snap.no_bid_size)
                        };
                        let limit_c = rq.order.limit_cents;
                        let side_last_hit = last_c.is_some_and(|p| {
                            let lp = if rq.order.side == "yes" {
                                p
                            } else {
                                100u8.saturating_sub(p)
                            };
                            lp <= limit_c
                        });
                        let strict_cross = side_ask_c < limit_c || side_bid_c < limit_c;
                        let touch = side_ask_c == limit_c || side_last_hit;

                        if strict_cross {
                            rq.order.queue_ahead_remaining = 0;
                        } else if touch {
                            let bid_drop = if side_bid_c == limit_c && rq.last_bid_size > side_bid_sz {
                                (rq.last_bid_size - side_bid_sz).round().max(0.0) as u32
                            } else {
                                0
                            };
                            let step_deplete = bid_drop.max(rq.order.contracts).max(10);
                            rq.order.queue_ahead_remaining =
                                rq.order.queue_ahead_remaining.saturating_sub(step_deplete);
                        } else if side_bid_c == limit_c
                            && rq.last_bid_size > side_bid_sz
                            && rq.order.queue_ahead_remaining > 1
                        {
                            // Bid depth ahead of us canceled/eroded while resting at best bid.
                            let drop = (rq.last_bid_size - side_bid_sz).round().max(0.0) as u32;
                            let new_rem = rq.order.queue_ahead_remaining.saturating_sub(drop).max(1);
                            rq.order.queue_ahead_remaining = new_rem;
                        }
                        rq.last_bid_size = side_bid_sz;

                        if rq.order.queue_ahead_remaining == 0 {
                            let fee_d = maker_fee_dollars(limit_c as f64 / 100.0)
                                * (rq.order.contracts as f64);
                            let wait_s = (snap.fetched_at - rq.order.posted_at).max(0) as f64;
                            st.record_fill(&rq.order, limit_c, 0.0, wait_s, fee_d, snap.fetched_at);
                        } else {
                            kept_quotes.push(rq);
                        }
                    }
                    st.open_quotes = kept_quotes;

                    // 2. Evaluate active slots without a quote or filled position on this ticker.
                    if secs_to_close <= cancel_before {
                        continue;
                    }

                    let enabled_count = st
                        .slots
                        .iter()
                        .filter(|s| s.cfg.enabled && !s.paused)
                        .count()
                        .max(1);
                    let slot_budget = bankroll / (enabled_count as f64);
                    let slot_count = st.slots.len();

                    for idx in 0..slot_count {
                        let (slot_id, strat_name, series_filter, max_contracts, exec_mode, enabled, paused) = {
                            let s = &st.slots[idx];
                            (
                                s.cfg.slot_id,
                                s.cfg.strategy.clone(),
                                s.cfg.series.clone(),
                                s.cfg.max_contracts,
                                s.cfg.execution_mode.clone(),
                                s.cfg.enabled,
                                s.paused,
                            )
                        };
                        if !enabled || paused {
                            continue;
                        }
                        let series_match = if series_filter.is_empty() {
                            SERIES.iter().any(|&s| s == snap.series.as_str())
                        } else {
                            series_filter
                                .iter()
                                .any(|s| s.eq_ignore_ascii_case(snap.series.as_str()))
                        };
                        if !series_match {
                            continue;
                        }
                        if st
                            .open_quotes
                            .iter()
                            .any(|rq| rq.order.slot_id == slot_id && rq.order.ticker == ticker)
                            || st
                                .settled_or_filled_slot_tickers
                                .contains(&(slot_id, ticker.to_string()))
                        {
                            continue;
                        }

                        let Some(strat) = cached_strats.find(&strat_name) else {
                            continue;
                        };
                        let Some(quote_dollars) = strat.entry_quote(ya, na, secs_to_close) else {
                            continue;
                        };
                        let Some(quote_cents) = to_whole_cents(quote_dollars) else {
                            continue;
                        };

                        let (side_ask_c, side_bid_c, side_bid_sz) = if strat.side == "yes" {
                            (ya_c, yb_c, snap.yes_bid_size)
                        } else {
                            (na_c, nb_c, snap.no_bid_size)
                        };

                        let (limit_cents, eff_contracts, taker_exact_cents, taker_slip_cents) =
                            if exec_mode == "taker" {
                                let (fc, qty, exact_c, slip, _used_l2) = resolve_taker_execution(
                                    &app.orderbook,
                                    ticker,
                                    &strat.side,
                                    max_contracts,
                                    side_bid_c,
                                    side_ask_c,
                                    quote_cents,
                                );
                                (fc, qty, exact_c, slip)
                            } else {
                                let lc = if quote_cents >= side_ask_c {
                                    if side_ask_c <= 1 {
                                        continue;
                                    }
                                    side_ask_c - 1
                                } else {
                                    quote_cents
                                };
                                (lc, max_contracts, lc as f64, 0.0)
                            };
                        if !(1..=99).contains(&limit_cents) || eff_contracts == 0 {
                            continue;
                        }

                        let affordable = (slot_budget / (limit_cents as f64 / 100.0)).floor() as u32;
                        let contracts = eff_contracts.min(affordable);
                        if contracts == 0 {
                            continue;
                        }

                        let order_id = st.next_order_id();
                        if mode == RunnerMode::LiveAuto {
                            if pending_live_intents.iter().any(|p| p.slot_id == slot_id && p.ticker == ticker) {
                                continue;
                            }
                            pending_live_intents.push(PendingLiveOrderIntent {
                                slot_id,
                                strategy: strat.name.clone(),
                                ticker: ticker.to_string(),
                                series: snap.series.as_str().to_string(),
                                execution_mode: exec_mode,
                                side: strat.side.clone(),
                                limit_cents,
                                quote_cents,
                                contracts,
                                posted_at: snap.fetched_at,
                                close_epoch,
                                client_order_id: order_id,
                            });
                            continue;
                        }

                        // Shadow execution: record QuotePosted, then either immediate taker fill or resting maker quote.
                        let queue_initial = if exec_mode == "taker" || limit_cents > side_bid_c {
                            0
                        } else if let Some(l2_q) = app
                            .orderbook
                            .queue_ahead_at_fresh(ticker, &strat.side, limit_cents)
                        {
                            l2_q.min(u32::MAX as u64) as u32
                        } else if limit_cents == side_bid_c {
                            side_bid_sz.round().max(0.0) as u32
                        } else {
                            (side_bid_sz.round().max(0.0) as u32).max(10)
                        };

                        let order = VirtualOrLiveOrder {
                            order_id: order_id.clone(),
                            slot_id,
                            strategy: strat.name.clone(),
                            ticker: ticker.to_string(),
                            series: snap.series.as_str().to_string(),
                            mode: "shadow".to_string(),
                            execution_mode: exec_mode.clone(),
                            side: strat.side.clone(),
                            limit_cents,
                            contracts,
                            filled_contracts: if exec_mode == "taker" { contracts } else { 0 },
                            queue_ahead_initial: queue_initial,
                            queue_ahead_remaining: queue_initial,
                            posted_at: snap.fetched_at,
                            close_epoch,
                            live_order_id: None,
                            status: if exec_mode == "taker" {
                                "filled".to_string()
                            } else {
                                "resting".to_string()
                            },
                        };

                        st.total_quotes_posted = st.total_quotes_posted.saturating_add(1);
                        if let Some(sl) = st.slots.get_mut(idx) {
                            sl.quotes_posted = sl.quotes_posted.saturating_add(1);
                        }
                        let post_ev = LedgerEvent {
                            seq: 0,
                            ts: snap.fetched_at,
                            event: "QuotePosted".to_string(),
                            mode: "shadow".to_string(),
                            slot_id,
                            strategy: strat.name.clone(),
                            ticker: ticker.to_string(),
                            series: snap.series.as_str().to_string(),
                            order_id: order_id.clone(),
                            side: strat.side.clone(),
                            execution_mode: exec_mode.clone(),
                            limit_cents,
                            fill_cents: None,
                            contracts,
                            slippage_cents: None,
                            queue_wait_secs: None,
                            fee_dollars: None,
                            realized_pnl_dollars: None,
                            result: None,
                            reason: None,
                            live_order_id: None,
                        };
                        st.append_event(post_ev.clone());
                        publish_runner_sse("quote_posted", &post_ev);

                        if exec_mode == "taker" {
                            let fee_d = taker_fee_dollars(taker_exact_cents / 100.0)
                                * (contracts as f64);
                            st.record_fill(
                                &order,
                                limit_cents,
                                taker_slip_cents,
                                0.0,
                                fee_d,
                                snap.fetched_at,
                            );
                        } else {
                            st.open_quotes.push(RestingQuote {
                                order,
                                last_bid_size: side_bid_sz,
                            });
                        }
                    }
                }
                st.ticker_cursor.insert(ticker.to_string(), snaps.len());
            }
        }

        // Execute any pending live cancellations outside runner lock.
        if !live_cancels.is_empty() {
            if let Some(ref cfg) = live_cfg_opt {
                if let Ok(client) = kalshi_live::KalshiLiveClient::from_config(cfg) {
                    for (lid, q, ts, reason) in live_cancels {
                        let _ = client.cancel_order(&lid);
                        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
                        st.open_quotes.retain(|rq| rq.order.order_id != q.order_id);
                        st.emit_quote_canceled(&q, ts, &reason);
                    }
                }
            }
        }

        // Route pending live_auto intents strictly through RiskRailsState + KalshiLiveClient.
        for intent_info in pending_live_intents {
            if self.halted.load(Ordering::SeqCst) {
                break;
            }
            let live_intent = risk_rails::LiveOrderIntent {
                ticker: intent_info.ticker.clone(),
                action: "buy".to_string(),
                side: intent_info.side.clone(),
                count: intent_info.contracts,
                price_cents: intent_info.limit_cents,
                client_order_id: Some(intent_info.client_order_id.clone()),
                dry_run: false,
            };
            let auth_res = {
                let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                if !ld.rails.live_armed || ld.rails.killed {
                    Err("disarmed or killed".to_string())
                } else {
                    match ld.rails.authorize_order(&live_intent) {
                        Ok((base_req, check)) => {
                            let req_res = if intent_info.execution_mode == "maker" {
                                kalshi_live::KalshiOrderRequest::maker_limit(
                                    &live_intent.ticker,
                                    &live_intent.action,
                                    &live_intent.side,
                                    live_intent.count,
                                    live_intent.price_cents,
                                    Some(base_req.client_order_id),
                                )
                            } else {
                                Ok(base_req)
                            };
                            match req_res {
                                Ok(req) => Ok((req, check, ld.config.clone())),
                                Err(e) => Err(e),
                            }
                        }
                        Err(check) => Err(check.rail_violations.join("; ")),
                    }
                }
            };
            let (kalshi_req, check, cfg) = match auth_res {
                Ok(v) => v,
                Err(_) => continue,
            };
            if self.halted.load(Ordering::SeqCst) {
                break;
            }
            let Ok(client) = kalshi_live::KalshiLiveClient::from_config(&cfg) else {
                continue;
            };
            match client.place_order(&kalshi_req) {
                Ok(placed) => {
                    {
                        let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        ld.connected = true;
                        ld.rails.last_error = None;
                        ld.rails
                            .record_submitted_order("buy", check.order_notional_dollars);
                    }
                    let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    if st.mode != RunnerMode::LiveAuto || self.halted.load(Ordering::SeqCst) {
                        // Kill-switch fired mid-flight: cancel immediately!
                        drop(st);
                        let _ = client.cancel_order(&placed.order_id);
                        break;
                    }
                    st.total_quotes_posted = st.total_quotes_posted.saturating_add(1);
                    if let Some(sl) = st
                        .slots
                        .iter_mut()
                        .find(|s| s.cfg.slot_id == intent_info.slot_id)
                    {
                        sl.quotes_posted = sl.quotes_posted.saturating_add(1);
                    }
                    let order = VirtualOrLiveOrder {
                        order_id: intent_info.client_order_id.clone(),
                        slot_id: intent_info.slot_id,
                        strategy: intent_info.strategy.clone(),
                        ticker: intent_info.ticker.clone(),
                        series: intent_info.series.clone(),
                        mode: "live".to_string(),
                        execution_mode: intent_info.execution_mode.clone(),
                        side: intent_info.side.clone(),
                        limit_cents: intent_info.limit_cents,
                        contracts: intent_info.contracts,
                        filled_contracts: placed.fill_count.max(0) as u32,
                        queue_ahead_initial: 0,
                        queue_ahead_remaining: 0,
                        posted_at: intent_info.posted_at,
                        close_epoch: intent_info.close_epoch,
                        live_order_id: Some(placed.order_id.clone()),
                        status: placed.status.clone(),
                    };
                    let post_ev = LedgerEvent {
                        seq: 0,
                        ts: intent_info.posted_at,
                        event: "QuotePosted".to_string(),
                        mode: "live".to_string(),
                        slot_id: intent_info.slot_id,
                        strategy: intent_info.strategy,
                        ticker: intent_info.ticker,
                        series: intent_info.series,
                        order_id: intent_info.client_order_id,
                        side: intent_info.side,
                        execution_mode: intent_info.execution_mode.clone(),
                        limit_cents: intent_info.limit_cents,
                        fill_cents: None,
                        contracts: intent_info.contracts,
                        slippage_cents: None,
                        queue_wait_secs: None,
                        fee_dollars: None,
                        realized_pnl_dollars: None,
                        result: None,
                        reason: None,
                        live_order_id: Some(placed.order_id),
                    };
                    st.append_event(post_ev.clone());
                    publish_runner_sse("quote_posted", &post_ev);

                    if placed.status == "executed"
                        || (placed.remaining_count == 0 && placed.fill_count > 0)
                    {
                        let fee_fn = if intent_info.execution_mode == "maker" {
                            maker_fee_dollars
                        } else {
                            taker_fee_dollars
                        };
                        let fee_d = fee_fn(intent_info.limit_cents as f64 / 100.0)
                            * (intent_info.contracts as f64);
                        let slip = (intent_info.limit_cents as f64
                            - intent_info.quote_cents as f64)
                            .max(0.0);
                        st.record_fill(
                            &order,
                            intent_info.limit_cents,
                            slip,
                            0.0,
                            fee_d,
                            intent_info.posted_at,
                        );
                    } else {
                        st.open_quotes.push(RestingQuote {
                            order,
                            last_bid_size: 0.0,
                        });
                    }
                }
                Err(e) => {
                    let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                    ld.rails.last_error = Some(e);
                }
            }
        }
    }
}

#[derive(Default)]
struct StrategyAgg {
    quotes_posted: u64,
    quotes_canceled: u64,
    fills: u64,
    windows_settled: u64,
    realized_pnl_dollars: f64,
    total_fees_dollars: f64,
    slippage_sum: f64,
    slippage_cnt: u64,
    queue_wait_sum: f64,
    queue_wait_cnt: u64,
    paused_count: u64,
}

pub struct CachedStrategies {
    key: u64,
    strategies: Vec<Strategy>,
    stem_map: HashMap<String, usize>,
}

impl CachedStrategies {
    pub fn new() -> Self {
        Self {
            key: 0,
            strategies: Vec::new(),
            stem_map: HashMap::new(),
        }
    }

    fn refresh_if_needed(&mut self, dir: &Path, paper_pot: f64) {
        let cur_key = strategies_dir_key(dir, paper_pot);
        if cur_key != self.key || self.strategies.is_empty() {
            let (strats, _) = load_strategies_with_errors(dir);
            let mut stem_map = HashMap::new();
            for (i, s) in strats.iter().enumerate() {
                stem_map.insert(s.name.clone(), i);
                stem_map.insert(s.name.replace('-', "_"), i);
            }
            self.strategies = strats;
            self.stem_map = stem_map;
            self.key = cur_key;
        }
    }

    fn find(&self, name: &str) -> Option<&Strategy> {
        self.stem_map
            .get(name)
            .or_else(|| self.stem_map.get(&name.replace('-', "_")))
            .or_else(|| self.stem_map.get(&name.replace('_', "-")))
            .and_then(|&i| self.strategies.get(i))
    }
}

/// Start the continuous tick-driven background runner thread.
/// Parks idle at 0% CPU on `Condvar` when `mode == RunnerMode::Stopped`,
/// wakes immediately on `start()`, `config()`, `stop()`, `emergency_halt()`,
/// or `notify_tick()`.
pub fn start_background_runner(app: Arc<App>) {
    std::thread::spawn(move || {
        let mut cached_strats = CachedStrategies::new();
        loop {
            let tick_ms = {
                let mut st = app.runner.state.lock().unwrap_or_else(|p| p.into_inner());
                while st.mode == RunnerMode::Stopped {
                    st = app.runner.cv.wait(st).unwrap_or_else(|p| p.into_inner());
                }
                st.tick_interval_ms
            };

            app.runner.tick_notify.store(false, Ordering::Release);
            app.runner.step_once(&app, &mut cached_strats);

            // Wait up to tick_interval_ms unless immediately woken by stop/halt/notify_tick.
            if !app.runner.tick_notify.load(Ordering::Acquire) {
                let st = app.runner.state.lock().unwrap_or_else(|p| p.into_inner());
                if st.mode != RunnerMode::Stopped && !app.runner.tick_notify.load(Ordering::Acquire)
                {
                    let _ = app
                        .runner
                        .cv
                        .wait_timeout(st, Duration::from_millis(tick_ms));
                }
            }
        }
    });
}

/// Resolve taker execution price (`fill_cents`, `fillable_contracts`, `exact_vwap_cents`,
/// `slippage_cents`, `used_l2`) using `OrderbookStore::walk_book_fresh` when a fresh L2 book
/// is available, or falling back cleanly to quote midpoint when missing/stale.
pub fn resolve_taker_execution(
    orderbook: &crate::orderbook::OrderbookStore,
    ticker: &str,
    side: &str,
    contracts: u32,
    side_bid_c: u8,
    side_ask_c: u8,
    quote_cents: u8,
) -> (u8, u32, f64, f64, bool) {
    if let Some(walk) = orderbook.walk_book_fresh(ticker, side, contracts as u64) {
        let fill_c = walk.vwap_cents.round().clamp(1.0, 99.0) as u8;
        let filled_qty = (walk.fillable_qty as u32).min(contracts).max(1);
        let slip = (walk.vwap_cents - (quote_cents as f64)).max(0.0);
        return (fill_c, filled_qty, walk.vwap_cents, slip, true);
    }
    orderbook.record_walk_fallback();
    let mid = ((side_bid_c as f64) + (side_ask_c as f64)) * 0.5;
    let fill_c = mid.round().clamp(1.0, 99.0) as u8;
    let slip = (mid - (quote_cents as f64)).max(0.0);
    (fill_c, contracts, mid, slip, false)
}

#[allow(dead_code)]
fn _iso_z_keepalive(e: i64) -> String {
    iso_z(e)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orderbook::OrderbookStore;
    use std::io::Write;

    #[test]
    fn taker_execution_uses_l2_vwap_when_fresh_and_midpoint_fallback_when_stale() {
        let ob = OrderbookStore::with_stale_threshold_ms(PathBuf::new(), 5_000);
        // Empty book -> midpoint (54 + 60) / 2 = 57c, fallback incremented.
        let (fc0, q0, exact0, _, used0) =
            resolve_taker_execution(&ob, "KXBTC15M-T1", "yes", 10, 54, 60, 56);
        assert!(!used0);
        assert_eq!(fc0, 57);
        assert!((exact0 - 57.0).abs() < 1e-9);
        assert_eq!(q0, 10);
        assert_eq!(ob.walk_fallback_count(), 1);

        // Fresh L2 book: NO bids [40, 5] -> YES ask 60x5; [36, 5] -> YES ask 64x5.
        // Walking 10 contracts -> 5@60 + 5@64 => VWAP = 62.0c.
        ob.ingest_snapshot("KXBTC15M-T1", &[[54, 20]], &[[40, 5], [36, 5]], 1, "ws");
        let (fc1, q1, exact1, slip1, used1) =
            resolve_taker_execution(&ob, "KXBTC15M-T1", "yes", 10, 54, 60, 58);
        assert!(used1);
        assert_eq!(fc1, 62);
        assert!((exact1 - 62.0).abs() < 1e-9);
        assert!((slip1 - 4.0).abs() < 1e-9);
        assert_eq!(q1, 10);
        assert_eq!(ob.walk_fallback_count(), 1);

        // Mark stale -> falls back to midpoint and increments fallback count.
        ob.mark_stale_for_test("KXBTC15M-T1", 10_000);
        let (fc2, _, _, _, used2) =
            resolve_taker_execution(&ob, "KXBTC15M-T1", "yes", 10, 54, 60, 56);
        assert!(!used2);
        assert_eq!(fc2, 57);
        assert_eq!(ob.walk_fallback_count(), 2);
    }

    #[test]
    fn shadow_runner_step_once_uses_vwap_vs_midpoint_and_l2_queue() {
        let dir = std::env::temp_dir().join(format!("ob-sr-l2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data_dir = dir.join("data");
        let strat_dir = dir.join("strategies");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&strat_dir).unwrap();

        std::fs::write(
            strat_dir.join("taker_test.rn"),
            r#"
pub fn name() { "taker-test" }
pub fn side() { "yes" }
pub fn allow_entry(yes_ask, no_ask, secs_to_close) { secs_to_close >= 60 }
pub fn entry_quote(yes_ask, no_ask, secs_to_close) { Some(0.55) }
"#,
        )
        .unwrap();

        let snap_line = serde_json::json!({
            "fetched_at": 1_800_000_000i64,
            "ticker": "KXBTC15M-VWAP",
            "event_ticker": "KXBTC15M-EV",
            "series": "KXBTC15M",
            "close_time": "2027-01-20T00:15:00Z",
            "status": "open",
            "result": "",
            "yes_bid": 0.50,
            "yes_ask": 0.60,
            "no_bid": 0.40,
            "no_ask": 0.50,
            "last_price": 0.55,
            "yes_bid_size": 10.0,
            "no_bid_size": 10.0
        });
        std::fs::write(data_dir.join("snapshots.jsonl"), format!("{snap_line}\n")).unwrap();

        let app = App::new(data_dir.clone(), strat_dir, 500.0, 60);
        // Fresh L2 book: NO bids 40c x 4 (YES ask 60c x 4) and 34c x 6 (YES ask 66c x 6).
        // Walking 10 contracts -> (4*60 + 6*66) / 10 = 63.6c -> rounds to 64c!
        app.orderbook.ingest_snapshot(
            "KXBTC15M-VWAP",
            &[[50, 20]],
            &[[40, 4], [34, 6]],
            1,
            "ws",
        );

        app.runner
            .configure(
                RunnerConfigRequest {
                    slots: vec![RunnerSlotConfig {
                        slot_id: 1,
                        strategy: "taker-test".to_string(),
                        series: vec!["KXBTC15M".to_string()],
                        max_contracts: 10,
                        execution_mode: "taker".to_string(),
                        auto_pause_drawdown_dollars: 50.0,
                        enabled: true,
                    }],
                    cancel_before_close_secs: 30,
                    bankroll_dollars: 500.0,
                    tick_interval_ms: Some(100),
                },
                false,
                false,
            )
            .unwrap();
        app.runner.start("shadow", false, false, false).unwrap();

        let mut cached = CachedStrategies::new();
        app.runner.step_once(&app, &mut cached);

        let ledger = app.runner.ledger_query(20, None, None);
        let fill_ev = ledger
            .entries
            .iter()
            .find(|e| e.event == "OrderFilled" && e.ticker == "KXBTC15M-VWAP")
            .expect("expected OrderFilled on fresh L2 book");
        assert_eq!(fill_ev.fill_cents, Some(64));
        assert_eq!(app.orderbook.walk_fallback_count(), 0);

        // Now add a second ticker WITHOUT L2 book -> should fall back to midpoint (50+60)/2 = 55c and increment fallback!
        let snap2 = serde_json::json!({
            "fetched_at": 1_800_000_010i64,
            "ticker": "KXBTC15M-MID",
            "event_ticker": "KXBTC15M-EV2",
            "series": "KXBTC15M",
            "close_time": "2027-01-20T00:15:00Z",
            "status": "open",
            "result": "",
            "yes_bid": 0.50,
            "yes_ask": 0.60,
            "no_bid": 0.40,
            "no_ask": 0.50,
            "last_price": 0.55,
            "yes_bid_size": 10.0,
            "no_bid_size": 10.0
        });
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(data_dir.join("snapshots.jsonl"))
            .unwrap();
        writeln!(f, "{snap2}").unwrap();
        drop(f);

        app.runner.step_once(&app, &mut cached);
        let ledger2 = app.runner.ledger_query(20, None, None);
        let mid_ev = ledger2
            .entries
            .iter()
            .find(|e| e.event == "OrderFilled" && e.ticker == "KXBTC15M-MID")
            .expect("expected OrderFilled via midpoint fallback");
        assert_eq!(mid_ev.fill_cents, Some(55));
        assert_eq!(app.orderbook.walk_fallback_count(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
