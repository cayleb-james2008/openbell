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
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
const MAX_RUNNER_SLOTS: usize = 64;
const MAX_IN_MEMORY_LEDGER: usize = 10_000;
const MAX_TERMINAL_ORDER_IDS: usize = 20_000;

#[inline]
fn sanitize_non_neg_size(sz: f32) -> f32 {
    if sz.is_finite() && sz >= 0.0 {
        sz.min(1_000_000_000.0)
    } else {
        0.0
    }
}

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
    #[serde(default)]
    pub reprice_cooldown_secs: u64,
    #[serde(default)]
    pub max_replaces_per_window: u32,
    #[serde(default)]
    pub take_profit_cents: Option<u8>,
    #[serde(default)]
    pub stop_loss_cents: Option<u8>,
    #[serde(default)]
    pub exit_before_close_secs: u64,
}

fn default_true() -> bool {
    true
}

fn default_buy_action() -> String {
    "buy".to_string()
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
struct PersistedSlotPause {
    slot_id: usize,
    paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pause_reason: Option<String>,
    #[serde(default)]
    peak_realized_pnl_dollars: f64,
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
    #[serde(default)]
    slot_states: Vec<PersistedSlotPause>,
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
    pub reprice_cooldown_secs: u64,
    pub max_replaces_per_window: u32,
    pub take_profit_cents: Option<u8>,
    pub stop_loss_cents: Option<u8>,
    pub exit_before_close_secs: u64,
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
    #[serde(default = "default_buy_action")]
    pub action: String,
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
    #[serde(default)]
    pub replace_count: u32,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
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
    last_bid_cents: u8,
    last_l2_updated_ms: i64,
    last_l2_trade_ms: Option<i64>,
    last_replaced_at: i64,
    replay_mode: bool,
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
    close_epoch: i64,
    replay_mode: bool,
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
    settled_or_filled_slot_tickers: HashMap<String, HashSet<usize>>,
    terminal_order_ids: HashSet<String>,
    terminal_order_queue: VecDeque<String>,
    applied_live_trade_ids: HashSet<String>,
    applied_live_trade_queue: VecDeque<String>,
    ticker_cursor: HashMap<String, usize>,
    l2_eval_ms: HashMap<String, i64>,
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
    quarantine_state: crate::execution_audit::QuarantineStateFile,
    recent_fill_tickers: HashMap<String, i64>,
    last_markout_sample_ts: HashMap<String, i64>,
    audit_dirty: bool,
}

struct PendingLiveOrderIntent {
    slot_id: usize,
    strategy: String,
    ticker: String,
    series: String,
    execution_mode: String,
    side: String,
    limit_cents: u8,
    contracts: u32,
    posted_at: i64,
    close_epoch: i64,
    client_order_id: String,
    replay_mode: bool,
}

struct PendingLiveAmend {
    order_id: String,
    live_order_id: String,
    ticker: String,
    side: String,
    action: String,
    old_limit_cents: u8,
    new_limit_cents: u8,
    contracts: u32,
    eval_ts: i64,
}

struct PendingLiveExit {
    slot_id: usize,
    strategy: String,
    ticker: String,
    series: String,
    side: String,
    contracts: u32,
    limit_cents: u8,
    client_order_id: String,
    reason: String,
    eval_ts: i64,
    close_epoch: i64,
    replay_mode: bool,
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
    if slot.reprice_cooldown_secs > 3600 {
        return Err(format!(
            "slot {} reprice_cooldown_secs must be <= 3600",
            slot.slot_id
        ));
    }
    if slot.max_replaces_per_window > 100 {
        return Err(format!(
            "slot {} max_replaces_per_window must be <= 100",
            slot.slot_id
        ));
    }
    if let Some(tp) = slot.take_profit_cents {
        if !(1..=99).contains(&tp) {
            return Err(format!(
                "slot {} take_profit_cents must be 1..=99",
                slot.slot_id
            ));
        }
    }
    if let Some(sl) = slot.stop_loss_cents {
        if !(1..=99).contains(&sl) {
            return Err(format!(
                "slot {} stop_loss_cents must be 1..=99",
                slot.slot_id
            ));
        }
    }
    if slot.exit_before_close_secs > 3600 {
        return Err(format!(
            "slot {} exit_before_close_secs must be <= 3600",
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
        reprice_cooldown_secs: slot.reprice_cooldown_secs,
        max_replaces_per_window: slot.max_replaces_per_window,
        take_profit_cents: slot.take_profit_cents,
        stop_loss_cents: slot.stop_loss_cents,
        exit_before_close_secs: slot.exit_before_close_secs,
    })
}

fn load_ledger_events(path: &Path) -> Vec<LedgerEvent> {
    use std::io::{BufRead, BufReader};
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Vec::new();
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Vec::new();
    }
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let mut reader = BufReader::new(file);
    let mut buf = Vec::with_capacity(512);
    let mut out = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => break,
        }
        let Ok(line_str) = std::str::from_utf8(&buf) else {
            continue;
        };
        let trimmed = line_str.trim();
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
            slot_states: self
                .slots
                .iter()
                .map(|s| PersistedSlotPause {
                    slot_id: s.cfg.slot_id,
                    paused: s.paused,
                    pause_reason: s.pause_reason.clone(),
                    peak_realized_pnl_dollars: s.peak_realized_pnl_dollars,
                })
                .collect(),
        };
        write_atomic_json(&state_path(&self.data_dir), &persisted)
    }

    fn next_order_id(&mut self) -> String {
        self.next_order_num = self.next_order_num.saturating_add(1);
        format!("ob-run-{}-{}", now_unix(), self.next_order_num)
    }

    fn append_event(&mut self, mut ev: LedgerEvent) -> &LedgerEvent {
        self.next_seq = self.next_seq.saturating_add(1);
        ev.seq = self.next_seq;
        if let Ok(line) = serde_json::to_string(&ev) {
            let mut row = line;
            row.push('\n');
            let _ = append_tape(&ledger_path(&self.data_dir), &row);
        }
        self.ledger.push(ev);
        if self.ledger.len() > MAX_IN_MEMORY_LEDGER {
            let excess = self.ledger.len() - MAX_IN_MEMORY_LEDGER;
            self.ledger.drain(..excess);
        }
        self.ledger.last().expect("ledger non-empty after push")
    }

    fn append_and_publish(&mut self, ev: LedgerEvent, sse_kind: &str) {
        let stored = self.append_event(ev);
        publish_runner_sse(sse_kind, stored);
    }

    fn has_slot_ticker(&self, slot_id: usize, ticker: &str) -> bool {
        self.settled_or_filled_slot_tickers
            .get(ticker)
            .is_some_and(|s| s.contains(&slot_id))
    }

    fn mark_slot_ticker(&mut self, slot_id: usize, ticker: &str) {
        if let Some(set) = self.settled_or_filled_slot_tickers.get_mut(ticker) {
            set.insert(slot_id);
        } else {
            let mut set = HashSet::new();
            set.insert(slot_id);
            self.settled_or_filled_slot_tickers
                .insert(ticker.to_string(), set);
        }
    }

    /// Returns `true` if `order_id` was not yet terminal and has now been marked terminal.
    fn mark_terminal_order(&mut self, order_id: &str) -> bool {
        if order_id.is_empty() {
            return true;
        }
        if !self.terminal_order_ids.insert(order_id.to_string()) {
            return false;
        }
        self.terminal_order_queue.push_back(order_id.to_string());
        while self.terminal_order_queue.len() > MAX_TERMINAL_ORDER_IDS {
            if let Some(old) = self.terminal_order_queue.pop_front() {
                self.terminal_order_ids.remove(&old);
            }
        }
        true
    }

    fn mark_applied_live_trade(&mut self, trade_id: &str) -> bool {
        let trimmed = trade_id.trim();
        if trimmed.is_empty() {
            return true;
        }
        if !self.applied_live_trade_ids.insert(trimmed.to_string()) {
            return false;
        }
        self.applied_live_trade_queue.push_back(trimmed.to_string());
        while self.applied_live_trade_queue.len() > MAX_TERMINAL_ORDER_IDS {
            if let Some(old) = self.applied_live_trade_queue.pop_front() {
                self.applied_live_trade_ids.remove(&old);
            }
        }
        true
    }

    fn emit_quote_canceled(
        &mut self,
        q: &VirtualOrLiveOrder,
        ts: i64,
        reason: &str,
    ) {
        if !self.mark_terminal_order(&q.order_id) {
            return;
        }
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
            action: Some(q.action.clone()),
        };
        self.append_and_publish(ev, "quote_canceled");
    }

    fn emit_order_amended(
        &mut self,
        q: &VirtualOrLiveOrder,
        ts: i64,
        reason: &str,
    ) {
        let ev = LedgerEvent {
            seq: 0,
            ts,
            event: "OrderAmended".to_string(),
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
            action: Some(q.action.clone()),
        };
        self.append_and_publish(ev, "order_amended");
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_slot_realized_pnl(
        &mut self,
        slot_id: usize,
        net_pnl: f64,
        mode: &str,
        ticker: &str,
        series: &str,
        ts: i64,
    ) {
        let mut pause_trigger: Option<(usize, String, String, f64, f64)> = None;
        if let Some(slot) = self.slots.iter_mut().find(|s| s.cfg.slot_id == slot_id) {
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
                mode: mode.to_string(),
                slot_id,
                strategy,
                ticker: ticker.to_string(),
                series: series.to_string(),
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
                action: None,
            };
            self.append_and_publish(pause_ev, "slot_auto_paused");
            let _ = self.persist_state();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn close_filled_positions_on_exit(
        &mut self,
        slot_id: usize,
        ticker: &str,
        side: &str,
        exit_contracts: u32,
        exit_cents: u8,
        exit_exact_cents: f64,
        exit_fee_dollars: f64,
        exit_order_id: &str,
        live_order_id: Option<String>,
        mode: &str,
        strategy: &str,
        series: &str,
        execution_mode: &str,
        limit_cents: u8,
        reason: &str,
        ts: i64,
    ) {
        let mut remaining_to_close = exit_contracts;
        let mut entry_cost = 0.0f64;
        let mut entry_fee = 0.0f64;
        let mut closed_total = 0u32;
        let mut next_positions = Vec::with_capacity(self.filled_positions.len());
        for mut pos in self.filled_positions.drain(..) {
            if remaining_to_close > 0
                && pos.slot_id == slot_id
                && pos.ticker == ticker
                && pos.side == side
                && pos.contracts > 0
            {
                let take = remaining_to_close.min(pos.contracts);
                let frac = take as f64 / pos.contracts as f64;
                let part_fee = pos.fee_dollars * frac;
                entry_cost += (pos.fill_cents as f64 / 100.0) * (take as f64);
                entry_fee += part_fee;
                closed_total = closed_total.saturating_add(take);
                remaining_to_close -= take;
                pos.contracts -= take;
                pos.fee_dollars = (pos.fee_dollars - part_fee).max(0.0);
                if pos.contracts > 0 {
                    next_positions.push(pos);
                }
            } else {
                next_positions.push(pos);
            }
        }
        self.filled_positions = next_positions;
        if closed_total == 0 {
            return;
        }
        let proceeds = (exit_exact_cents / 100.0) * (closed_total as f64);
        let net_pnl = proceeds - entry_cost - entry_fee - exit_fee_dollars;
        self.total_realized_pnl_dollars += net_pnl;
        self.mark_slot_ticker(slot_id, ticker);
        let _ = self.mark_terminal_order(exit_order_id);

        let ev = LedgerEvent {
            seq: 0,
            ts,
            event: "PositionExited".to_string(),
            mode: mode.to_string(),
            slot_id,
            strategy: strategy.to_string(),
            ticker: ticker.to_string(),
            series: series.to_string(),
            order_id: exit_order_id.to_string(),
            side: side.to_string(),
            execution_mode: execution_mode.to_string(),
            limit_cents,
            fill_cents: Some(exit_cents.clamp(1, 99)),
            contracts: closed_total,
            slippage_cents: Some(((limit_cents as f64) - exit_exact_cents).max(0.0)),
            queue_wait_secs: Some(0.0),
            fee_dollars: Some(exit_fee_dollars),
            realized_pnl_dollars: Some(net_pnl),
            result: Some(format!("exited_{reason}")),
            reason: Some(reason.to_string()),
            live_order_id,
            action: Some("sell".to_string()),
        };
        self.append_and_publish(ev, "position_exited");
        self.apply_slot_realized_pnl(slot_id, net_pnl, mode, ticker, series, ts);
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
        if q.contracts == 0 || !self.mark_terminal_order(&q.order_id) {
            return;
        }
        let fill_cents = fill_cents.clamp(1, 99);
        let safe_slip = if slippage_cents.is_finite() { slippage_cents } else { 0.0 };
        let safe_wait = if queue_wait_secs.is_finite() && queue_wait_secs >= 0.0 {
            queue_wait_secs
        } else {
            0.0
        };
        let safe_fee = if fee_dollars.is_finite() && fee_dollars >= 0.0 {
            fee_dollars
        } else {
            0.0
        };
        self.total_fills = self.total_fills.saturating_add(1);
        self.slippage_sum_cents += safe_slip;
        self.slippage_count = self.slippage_count.saturating_add(1);
        self.queue_wait_sum_secs += safe_wait;
        self.queue_wait_count = self.queue_wait_count.saturating_add(1);
        if let Some(slot) = self.slots.iter_mut().find(|s| s.cfg.slot_id == q.slot_id) {
            slot.fills = slot.fills.saturating_add(1);
        }
        self.mark_slot_ticker(q.slot_id, &q.ticker);
        self.recent_fill_tickers
            .entry(q.ticker.clone())
            .and_modify(|prev| *prev = (*prev).max(filled_at))
            .or_insert(filled_at);
        self.audit_dirty = true;
        let replay_mode = now_unix() >= q.close_epoch && filled_at < q.close_epoch;
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
            fee_dollars: safe_fee,
            live_order_id: q.live_order_id.clone(),
            close_epoch: q.close_epoch,
            replay_mode,
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
            slippage_cents: Some(safe_slip),
            queue_wait_secs: Some(safe_wait),
            fee_dollars: Some(safe_fee),
            realized_pnl_dollars: None,
            result: None,
            reason: None,
            live_order_id: q.live_order_id.clone(),
            action: Some(q.action.clone()),
        };
        self.append_and_publish(ev, "quote_filled");
    }

    /// Record a partial or full live venue fill with real fill price, slippage, and fee.
    fn record_live_fill(
        &mut self,
        q: &mut VirtualOrLiveOrder,
        fill_cents: u8,
        fill_contracts: u32,
        fee_dollars: f64,
        filled_at: i64,
    ) {
        let remaining_cap = q.contracts.saturating_sub(q.filled_contracts);
        let applied = fill_contracts.min(remaining_cap);
        if applied == 0 {
            return;
        }
        let fill_cents = fill_cents.clamp(1, 99);
        let safe_slip = (fill_cents as f64) - (q.limit_cents as f64);
        let wait_s = (filled_at - q.posted_at).max(0) as f64;
        let safe_fee = if fee_dollars.is_finite() && fee_dollars >= 0.0 {
            fee_dollars
        } else {
            0.0
        };

        q.filled_contracts = q.filled_contracts.saturating_add(applied);
        if q.filled_contracts >= q.contracts {
            q.status = "filled".to_string();
            let _ = self.mark_terminal_order(&q.order_id);
        } else {
            q.status = "partial".to_string();
        }

        self.total_fills = self.total_fills.saturating_add(1);
        self.slippage_sum_cents += safe_slip;
        self.slippage_count = self.slippage_count.saturating_add(1);
        self.queue_wait_sum_secs += wait_s;
        self.queue_wait_count = self.queue_wait_count.saturating_add(1);
        if let Some(slot) = self.slots.iter_mut().find(|s| s.cfg.slot_id == q.slot_id) {
            slot.fills = slot.fills.saturating_add(1);
        }
        self.mark_slot_ticker(q.slot_id, &q.ticker);
        self.recent_fill_tickers
            .entry(q.ticker.clone())
            .and_modify(|prev| *prev = (*prev).max(filled_at))
            .or_insert(filled_at);
        self.audit_dirty = true;
        if q.action == "sell" {
            self.close_filled_positions_on_exit(
                q.slot_id,
                &q.ticker,
                &q.side,
                applied,
                fill_cents,
                fill_cents as f64,
                safe_fee,
                &q.order_id,
                q.live_order_id.clone(),
                &q.mode,
                &q.strategy,
                &q.series,
                &q.execution_mode,
                q.limit_cents,
                "live_exit_filled",
                filled_at,
            );
            return;
        }
        let replay_mode = now_unix() >= q.close_epoch && filled_at < q.close_epoch;
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
            contracts: applied,
            fee_dollars: safe_fee,
            live_order_id: q.live_order_id.clone(),
            close_epoch: q.close_epoch,
            replay_mode,
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
            contracts: applied,
            slippage_cents: Some(safe_slip),
            queue_wait_secs: Some(wait_s),
            fee_dollars: Some(safe_fee),
            realized_pnl_dollars: None,
            result: None,
            reason: None,
            live_order_id: q.live_order_id.clone(),
            action: Some(q.action.clone()),
        };
        self.append_and_publish(ev, "quote_filled");
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
                action: None,
            };
            self.append_and_publish(ev, "window_settled");

            self.apply_slot_realized_pnl(
                pos.slot_id,
                net_pnl,
                &pos.mode,
                &pos.ticker,
                &pos.series,
                ts,
            );
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
                .filter(|rq| rq.order.slot_id == sid && rq.order.action != "sell")
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
                reprice_cooldown_secs: slot.cfg.reprice_cooldown_secs,
                max_replaces_per_window: slot.cfg.max_replaces_per_window,
                take_profit_cents: slot.cfg.take_profit_cents,
                stop_loss_cents: slot.cfg.stop_loss_cents,
                exit_before_close_secs: slot.cfg.exit_before_close_secs,
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

        let (mode, tick_interval_ms, cancel_before_close_secs, bankroll_dollars, slot_cfgs, persisted_slot_states) =
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
                        .take(MAX_RUNNER_SLOTS)
                        .filter_map(|sl| normalize_slot(sl).ok())
                        .collect();
                    let bankroll = if s.bankroll_dollars.is_finite()
                        && s.bankroll_dollars > 0.0
                        && s.bankroll_dollars <= 10_000_000.0
                    {
                        s.bankroll_dollars
                    } else if default_bankroll.is_finite()
                        && default_bankroll > 0.0
                        && default_bankroll <= 10_000_000.0
                    {
                        default_bankroll
                    } else {
                        DEFAULT_BANKROLL_DOLLARS
                    };
                    let clamped_tick = s
                        .tick_interval_ms
                        .clamp(MIN_TICK_INTERVAL_MS, MAX_TICK_INTERVAL_MS);
                    let clamped_cancel = s.cancel_before_close_secs.min(MAX_CANCEL_BEFORE_CLOSE_SECS);
                    let slot_states_map: HashMap<usize, PersistedSlotPause> = s
                        .slot_states
                        .iter()
                        .map(|ps| (ps.slot_id, ps.clone()))
                        .collect();
                    // Sanitize on disk immediately if persisted mode was "live_auto" or invalid.
                    if s.mode != safe_mode.persisted_str() {
                        let sanitized = PersistedRunnerState {
                            mode: safe_mode.persisted_str().to_string(),
                            tick_interval_ms: clamped_tick,
                            cancel_before_close_secs: clamped_cancel,
                            bankroll_dollars: bankroll,
                            slots: valid_slots.clone(),
                            slot_states: s.slot_states.clone(),
                        };
                        let _ = write_atomic_json(&state_path(data_dir), &sanitized);
                    }
                    (
                        safe_mode,
                        clamped_tick,
                        clamped_cancel,
                        bankroll,
                        valid_slots,
                        slot_states_map,
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
                    HashMap::new(),
                ),
            };

        let mut ledger = load_ledger_events(&ledger_path(data_dir));
        let mut next_seq = 0u64;
        let next_order_num = ledger.len() as u64;
        let mut total_quotes_posted = 0u64;
        let mut total_fills = 0u64;
        let mut total_realized_pnl_dollars = 0.0;
        let mut slippage_sum_cents = 0.0;
        let mut slippage_count = 0u64;
        let mut queue_wait_sum_secs = 0.0;
        let mut queue_wait_count = 0u64;
        let mut slot_stats: HashMap<usize, (u64, u64, f64, f64)> = HashMap::new();
        let mut ledger_paused: HashMap<usize, String> = HashMap::new();
        let mut settled_or_filled_slot_tickers: HashMap<String, HashSet<usize>> = HashMap::new();
        let mut terminal_order_ids: HashSet<String> = HashSet::new();
        let mut terminal_order_queue: VecDeque<String> = VecDeque::new();

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
                "QuoteCanceled" => {
                    if !ev.order_id.is_empty() && terminal_order_ids.insert(ev.order_id.clone()) {
                        terminal_order_queue.push_back(ev.order_id.clone());
                        while terminal_order_queue.len() > MAX_TERMINAL_ORDER_IDS {
                            if let Some(old) = terminal_order_queue.pop_front() {
                                terminal_order_ids.remove(&old);
                            }
                        }
                    }
                }
                "OrderFilled" => {
                    total_fills = total_fills.saturating_add(1);
                    entry.1 = entry.1.saturating_add(1);
                    if !ev.ticker.is_empty() {
                        settled_or_filled_slot_tickers
                            .entry(ev.ticker.clone())
                            .or_default()
                            .insert(ev.slot_id);
                    }
                    if !ev.order_id.is_empty() && terminal_order_ids.insert(ev.order_id.clone()) {
                        terminal_order_queue.push_back(ev.order_id.clone());
                        while terminal_order_queue.len() > MAX_TERMINAL_ORDER_IDS {
                            if let Some(old) = terminal_order_queue.pop_front() {
                                terminal_order_ids.remove(&old);
                            }
                        }
                    }
                    if let Some(sl) = ev.slippage_cents.filter(|v| v.is_finite()) {
                        slippage_sum_cents += sl;
                        slippage_count = slippage_count.saturating_add(1);
                    }
                    if let Some(qw) = ev.queue_wait_secs.filter(|v| v.is_finite() && *v >= 0.0) {
                        queue_wait_sum_secs += qw;
                        queue_wait_count = queue_wait_count.saturating_add(1);
                    }
                }
                "WindowSettled" => {
                    if !ev.ticker.is_empty() {
                        settled_or_filled_slot_tickers
                            .entry(ev.ticker.clone())
                            .or_default()
                            .insert(ev.slot_id);
                    }
                    if let Some(pnl) = ev.realized_pnl_dollars.filter(|v| v.is_finite()) {
                        total_realized_pnl_dollars += pnl;
                        entry.2 += pnl;
                        if entry.2 > entry.3 {
                            entry.3 = entry.2;
                        }
                    }
                }
                "PositionExited" => {
                    if !ev.ticker.is_empty() {
                        settled_or_filled_slot_tickers
                            .entry(ev.ticker.clone())
                            .or_default()
                            .insert(ev.slot_id);
                    }
                    if !ev.order_id.is_empty() && terminal_order_ids.insert(ev.order_id.clone()) {
                        terminal_order_queue.push_back(ev.order_id.clone());
                        while terminal_order_queue.len() > MAX_TERMINAL_ORDER_IDS {
                            if let Some(old) = terminal_order_queue.pop_front() {
                                terminal_order_ids.remove(&old);
                            }
                        }
                    }
                    if let Some(pnl) = ev.realized_pnl_dollars.filter(|v| v.is_finite()) {
                        total_realized_pnl_dollars += pnl;
                        entry.2 += pnl;
                        if entry.2 > entry.3 {
                            entry.3 = entry.2;
                        }
                    }
                }
                "SlotPaused" => {
                    if let Some(ref r) = ev.reason {
                        ledger_paused.insert(ev.slot_id, r.clone());
                    } else {
                        ledger_paused.insert(ev.slot_id, "auto-paused".to_string());
                    }
                }
                _ => {}
            }
        }
        if ledger.len() > MAX_IN_MEMORY_LEDGER {
            let excess = ledger.len() - MAX_IN_MEMORY_LEDGER;
            ledger.drain(..excess);
        }

        let slots = slot_cfgs
            .into_iter()
            .map(|cfg| {
                let (qp, fl, pnl, peak) = slot_stats
                    .get(&cfg.slot_id)
                    .copied()
                    .unwrap_or((0, 0, 0.0, 0.0));
                let (paused, pause_reason, persisted_peak) =
                    if let Some(ps) = persisted_slot_states.get(&cfg.slot_id) {
                        (
                            ps.paused,
                            ps.pause_reason.clone(),
                            if ps.peak_realized_pnl_dollars.is_finite() {
                                ps.peak_realized_pnl_dollars.max(peak)
                            } else {
                                peak
                            },
                        )
                    } else if persisted_slot_states.is_empty() {
                        if let Some(reason) = ledger_paused.get(&cfg.slot_id) {
                            (true, Some(reason.clone()), peak)
                        } else {
                            (false, None, peak)
                        }
                    } else {
                        (false, None, peak)
                    };
                SlotRuntime {
                    cfg,
                    paused,
                    pause_reason,
                    quotes_posted: qp,
                    fills: fl,
                    realized_pnl_dollars: pnl,
                    peak_realized_pnl_dollars: persisted_peak,
                }
            })
            .collect();

        let quarantine_state = crate::execution_audit::load_quarantine_state(data_dir);

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
                settled_or_filled_slot_tickers,
                terminal_order_ids,
                terminal_order_queue,
                applied_live_trade_ids: HashSet::new(),
                applied_live_trade_queue: VecDeque::new(),
                ticker_cursor: HashMap::new(),
                l2_eval_ms: HashMap::new(),
                ledger,
                next_seq,
                next_order_num,
                total_quotes_posted,
                total_fills,
                total_realized_pnl_dollars,
                slippage_sum_cents,
                slippage_count,
                queue_wait_sum_secs,
                queue_wait_count,
                live_poll_last: None,
                quarantine_state,
                recent_fill_tickers: HashMap::new(),
                last_markout_sample_ts: HashMap::new(),
                audit_dirty: false,
            }),
            cv: Condvar::new(),
            halted: AtomicBool::new(false),
            tick_notify: AtomicBool::new(false),
        }
    }

    pub fn notify_tick(&self) {
        self.tick_notify.store(true, Ordering::SeqCst);
        let _st = self.state.lock().unwrap_or_else(|p| p.into_inner());
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
        if req.slots.len() > MAX_RUNNER_SLOTS {
            return Err(format!("too many slots (max {MAX_RUNNER_SLOTS})"));
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

    #[allow(dead_code)]
    pub fn quarantine_state(&self) -> crate::execution_audit::QuarantineStateFile {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.quarantine_state.clone()
    }

    pub fn set_quarantine_state(&self, qs: crate::execution_audit::QuarantineStateFile) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        st.quarantine_state = qs;
        let q_strats: HashSet<String> = st
            .quarantine_state
            .strategies
            .iter()
            .filter(|(_, e)| e.status == "quarantined" || e.throttle_mult <= 0.0)
            .map(|(k, _)| k.clone())
            .collect();
        if !q_strats.is_empty() {
            let now = now_unix();
            let existing = std::mem::take(&mut st.open_quotes);
            let mut kept = Vec::with_capacity(existing.len());
            let mut canceled = Vec::new();
            for rq in existing {
                if rq.order.mode == "shadow"
                    && (q_strats.contains(&rq.order.strategy)
                        || q_strats.contains(&rq.order.strategy.replace('-', "_"))
                        || q_strats.contains(&rq.order.strategy.replace('_', "-")))
                {
                    canceled.push(rq.order);
                } else {
                    kept.push(rq);
                }
            }
            st.open_quotes = kept;
            for q in canceled {
                st.emit_quote_canceled(&q, now, "strategy_quarantined");
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn sync_from_allocator(
        &self,
        report: &crate::auto_allocator::AutoAllocatorReport,
        rails: &risk_rails::RiskRailsConfig,
        quarantine: &crate::execution_audit::QuarantineStateFile,
        bankroll_override: Option<f64>,
        execution_mode_override: Option<&str>,
        live_armed: bool,
        killed: bool,
    ) -> Result<(Vec<RunnerSlotConfig>, Vec<String>, RunnerStatusResponse), String> {
        let mut excluded_quarantined: Vec<String> = Vec::new();
        for q in &report.quarantined {
            if !excluded_quarantined.contains(&q.strategy) {
                excluded_quarantined.push(q.strategy.clone());
            }
        }
        let mut series_by_strat: HashMap<&str, &str> = HashMap::new();
        for row in &report.edge_screener {
            if !row.top_series.is_empty() {
                series_by_strat.insert(row.strategy.as_str(), row.top_series.as_str());
            }
        }

        let exec_override = execution_mode_override
            .map(|m| m.trim().to_ascii_lowercase())
            .filter(|m| m == "maker" || m == "taker");

        let raw_bankroll = bankroll_override.unwrap_or(report.starting_bankroll);
        let bankroll_valid = raw_bankroll.is_finite() && raw_bankroll > 0.0;
        let bankroll = if bankroll_valid {
            raw_bankroll.clamp(0.01, 10_000_000.0)
        } else {
            0.0
        };

        let mut slots_out: Vec<RunnerSlotConfig> = Vec::new();
        let mut next_slot_id = 1usize;
        if bankroll_valid {
            for w in &report.active_winners {
                if slots_out.len() >= MAX_RUNNER_SLOTS {
                    break;
                }
                if quarantine.is_quarantined(&w.strategy) || w.execution_status == "quarantined" {
                    if !excluded_quarantined.contains(&w.strategy) {
                        excluded_quarantined.push(w.strategy.clone());
                    }
                    continue;
                }
                let mult = quarantine.throttle_mult(&w.strategy);
                if mult <= 0.0 {
                    if !excluded_quarantined.contains(&w.strategy) {
                        excluded_quarantined.push(w.strategy.clone());
                    }
                    continue;
                }
                let rail_cap = rails.max_order_contracts.clamp(1, 10_000);
                let raw_contracts = w.rail_clamped_contracts.min(rail_cap);
                if raw_contracts == 0 {
                    continue;
                }
                let max_contracts = if mult < 1.0 {
                    ((raw_contracts as f64) * mult).floor().max(1.0) as u32
                } else {
                    raw_contracts
                }
                .min(rail_cap)
                .clamp(1, 10_000);

                let series = match series_by_strat.get(w.strategy.as_str()) {
                    Some(s) if !s.is_empty() => vec![(*s).to_string()],
                    _ => Vec::new(),
                };
                let exec_mode = exec_override
                    .clone()
                    .unwrap_or_else(|| {
                        if w.execution_mode == "taker" {
                            "taker".to_string()
                        } else {
                            "maker".to_string()
                        }
                    });
                let auto_pause_drawdown = (rails.max_daily_loss_dollars
                    * (w.allocated_pct / 100.0).max(0.1))
                .clamp(5.0, rails.max_daily_loss_dollars.max(5.0));

                let slot = normalize_slot(&RunnerSlotConfig {
                    slot_id: next_slot_id,
                    strategy: w.strategy.clone(),
                    series,
                    max_contracts,
                    execution_mode: exec_mode,
                    auto_pause_drawdown_dollars: auto_pause_drawdown,
                    enabled: true,
                    reprice_cooldown_secs: 0,
                    max_replaces_per_window: 0,
                    take_profit_cents: None,
                    stop_loss_cents: None,
                    exit_before_close_secs: 0,
                })?;
                slots_out.push(slot);
                next_slot_id += 1;
            }
        }

        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        // Safety invariant: sync_from_allocator NEVER sets RunnerMode::LiveAuto.
        if st.mode == RunnerMode::LiveAuto {
            st.mode = RunnerMode::Stopped;
        }
        st.bankroll_dollars = bankroll;
        st.quarantine_state = quarantine.clone();
        // Cancel any resting shadow quotes for quarantined strategies (or all when synced slots are empty).
        let now = now_unix();
        let existing_quotes = std::mem::take(&mut st.open_quotes);
        let mut kept_quotes = Vec::with_capacity(existing_quotes.len());
        let mut canceled_quotes = Vec::new();
        for rq in existing_quotes {
            if rq.order.mode == "shadow"
                && (slots_out.is_empty() || quarantine.is_quarantined(&rq.order.strategy))
            {
                canceled_quotes.push(rq.order);
            } else {
                kept_quotes.push(rq);
            }
        }
        st.open_quotes = kept_quotes;
        for q in canceled_quotes {
            st.emit_quote_canceled(&q, now, "allocator_sync_cleared");
        }
        st.slots = slots_out
            .iter()
            .cloned()
            .map(|cfg| SlotRuntime {
                cfg,
                paused: false,
                pause_reason: None,
                quotes_posted: 0,
                fills: 0,
                realized_pnl_dollars: 0.0,
                peak_realized_pnl_dollars: 0.0,
            })
            .collect();
        st.persist_state()?;
        let snap = st.status_snapshot(live_armed, killed);
        drop(st);
        self.cv.notify_all();
        Ok((slots_out, excluded_quarantined, snap))
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
                "PositionExited" => {
                    if let Some(fee) = ev.fee_dollars {
                        a.total_fees_dollars += fee;
                    }
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

    /// Reconcile open live orders and filled live positions against fresh venue
    /// `orders`, `fills`, and `settlements`, recording partial/full fills with real
    /// fill prices, slippage, and maker/taker fees. Returns remaining open `(live_order_id, ticker)`.
    pub fn reconcile_live_data(
        &self,
        orders: &[kalshi_live::KalshiOrder],
        fills: &[kalshi_live::KalshiFill],
        settlements: &[kalshi_live::KalshiSettlement],
        now: i64,
    ) -> Vec<(String, String)> {
        let mut fills_by_order: HashMap<&str, Vec<&kalshi_live::KalshiFill>> = HashMap::new();
        for f in fills {
            if !f.order_id.is_empty() {
                fills_by_order.entry(f.order_id.as_str()).or_default().push(f);
            }
        }
        let orders_by_id: HashMap<&str, &kalshi_live::KalshiOrder> = orders
            .iter()
            .map(|o| (o.order_id.as_str(), o))
            .collect();

        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let existing = std::mem::take(&mut st.open_quotes);
        let mut next_open = Vec::with_capacity(existing.len());

        for mut rq in existing {
            if rq.order.mode != "live" {
                next_open.push(rq);
                continue;
            }
            let Some(ref lid) = rq.order.live_order_id.clone() else {
                next_open.push(rq);
                continue;
            };

            // 1. Apply any unapplied real venue fills (supports partial fills & real price/fee).
            if let Some(order_fills) = fills_by_order.get(lid.as_str()) {
                for f in order_fills {
                    if !f.trade_id.trim().is_empty() && !st.mark_applied_live_trade(&f.trade_id) {
                        continue;
                    }
                    let count_u32 = u32::try_from(f.count.max(0)).unwrap_or(0);
                    if count_u32 == 0 {
                        continue;
                    }
                    let fill_c_i64 = if rq.order.side == "yes" {
                        f.yes_price.or_else(|| f.no_price.map(|p| 100 - p))
                    } else {
                        f.no_price.or_else(|| f.yes_price.map(|p| 100 - p))
                    }
                    .unwrap_or(rq.order.limit_cents as i64);
                    let fill_c = fill_c_i64.clamp(1, 99) as u8;
                    let is_taker = f.is_taker.unwrap_or(rq.order.execution_mode == "taker");
                    let fee_d = f
                        .fee_cost_dollars
                        .filter(|v| v.is_finite() && *v >= 0.0)
                        .unwrap_or_else(|| {
                            let fee_fn = if is_taker {
                                taker_fee_dollars
                            } else {
                                maker_fee_dollars
                            };
                            fee_fn(fill_c as f64 / 100.0) * (count_u32 as f64)
                        });
                    let fill_ts = f
                        .ts
                        .or_else(|| f.created_time.as_deref().and_then(parse_iso_z))
                        .unwrap_or(now);
                    st.record_live_fill(&mut rq.order, fill_c, count_u32, fee_d, fill_ts);
                }
            }

            if rq.order.filled_contracts >= rq.order.contracts {
                continue;
            }

            match orders_by_id.get(lid.as_str()) {
                Some(vo)
                    if vo.status.eq_ignore_ascii_case("canceled")
                        || vo.status.eq_ignore_ascii_case("cancelled") =>
                {
                    st.emit_quote_canceled(&rq.order, now, "venue_canceled");
                }
                Some(vo)
                    if vo.status.eq_ignore_ascii_case("executed")
                        || (vo.remaining_count == 0 && vo.fill_count > 0) =>
                {
                    // Fallback when a venue reports executed without granular fill rows.
                    let rem = rq.order.contracts.saturating_sub(rq.order.filled_contracts);
                    if rem > 0 {
                        let fill_c_i64 = if rq.order.side == "yes" {
                            vo.yes_price.or_else(|| vo.no_price.map(|p| 100 - p))
                        } else {
                            vo.no_price.or_else(|| vo.yes_price.map(|p| 100 - p))
                        }
                        .unwrap_or(rq.order.limit_cents as i64);
                        let fill_c = fill_c_i64.clamp(1, 99) as u8;
                        let fee_fn = if rq.order.execution_mode == "maker" {
                            maker_fee_dollars
                        } else {
                            taker_fee_dollars
                        };
                        let fee_d = fee_fn(fill_c as f64 / 100.0) * (rem as f64);
                        st.record_live_fill(&mut rq.order, fill_c, rem, fee_d, now);
                    }
                }
                Some(vo) if vo.fill_count > 0 && vo.remaining_count > 0 => {
                    rq.order.status = "partial".to_string();
                    next_open.push(rq);
                }
                _ => next_open.push(rq),
            }
        }
        st.open_quotes = next_open;

        // 2. Grade any live filled positions / open quotes on settled tickers.
        for s in settlements {
            let res = s.market_result.trim().to_ascii_lowercase();
            if res != "yes" && res != "no" {
                continue;
            }
            let has_live = st
                .open_quotes
                .iter()
                .any(|rq| rq.order.mode == "live" && rq.order.ticker == s.ticker)
                || st
                    .filled_positions
                    .iter()
                    .any(|p| p.mode == "live" && p.ticker == s.ticker);
            if has_live {
                let settled_ts = s
                    .settled_time
                    .as_deref()
                    .and_then(parse_iso_z)
                    .unwrap_or(now);
                st.settle_ticker(&s.ticker, &res, settled_ts);
            }
        }

        st.open_quotes
            .iter()
            .filter_map(|rq| {
                if rq.order.mode == "live" {
                    rq.order
                        .live_order_id
                        .as_ref()
                        .map(|lid| (lid.clone(), rq.order.ticker.clone()))
                } else {
                    None
                }
            })
            .collect()
    }

    pub fn mark_live_order_canceled(&self, live_order_id: &str, now: i64, reason: &str) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let existing = std::mem::take(&mut st.open_quotes);
        let mut kept = Vec::with_capacity(existing.len());
        let mut canceled = Vec::new();
        for rq in existing {
            if rq.order.live_order_id.as_deref() == Some(live_order_id) {
                canceled.push(rq.order);
            } else {
                kept.push(rq);
            }
        }
        st.open_quotes = kept;
        for q in canceled {
            st.emit_quote_canceled(&q, now, reason);
        }
    }

    pub fn mark_live_order_amended(
        &self,
        live_order_id: &str,
        new_limit_cents: u8,
        new_contracts: u32,
        now: i64,
    ) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut amended_order: Option<(VirtualOrLiveOrder, u8)> = None;
        for rq in &mut st.open_quotes {
            if rq.order.live_order_id.as_deref() == Some(live_order_id) {
                let old_c = rq.order.limit_cents;
                rq.order.limit_cents = new_limit_cents.clamp(1, 99);
                if new_contracts > 0 {
                    rq.order.contracts = new_contracts.max(rq.order.filled_contracts);
                }
                rq.order.replace_count = rq.order.replace_count.saturating_add(1);
                rq.last_replaced_at = now;
                amended_order = Some((rq.order.clone(), old_c));
                break;
            }
        }
        if let Some((ord, old_c)) = amended_order {
            st.emit_order_amended(
                &ord,
                now,
                &format!("operator_amended:{old_c}->{}", ord.limit_cents),
            );
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
                && (st
                    .open_quotes
                    .iter()
                    .any(|rq| rq.order.mode == "live" && rq.order.live_order_id.is_some())
                    || st.filled_positions.iter().any(|p| p.mode == "live"))
                && st
                    .live_poll_last
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(2));
            if poll {
                st.live_poll_last = Some(Instant::now());
            }
            (st.mode, poll)
        };

        // Reconcile open live orders, partial/full fills, and settlements against venue outside state locks.
        if need_live_poll {
            let _ = crate::reconcile_live_venue(app);
        }

        // Fail-closed gate: `live_auto` refuses new orders if live credentials are configured
        // AND (`last_venue_sync_epoch` is `None` or `> 30s` old). Paper `Shadow` mode is unaffected.
        let allow_live_entries = if mode == RunnerMode::LiveAuto && live_cfg_opt.is_some() {
            let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
            let stale = ld.rails.is_venue_sync_stale(now_unix());
            if stale {
                ld.rails.last_error = Some(
                    "refusing live_auto new orders: venue sync is stale (>30s) or pending"
                        .to_string(),
                );
            }
            !stale
        } else {
            true
        };

        let mut pending_live_intents: Vec<PendingLiveOrderIntent> = Vec::new();
        let mut pending_live_amends: Vec<PendingLiveAmend> = Vec::new();
        let mut pending_live_exits: Vec<PendingLiveExit> = Vec::new();
        let mut live_cancels: Vec<(String, VirtualOrLiveOrder, i64, String)> = Vec::new();

        // Snapshot current L2 orderbooks for all resting quotes + filled positions OUTSIDE state lock.
        let open_tickers: Vec<String> = {
            let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if st.mode == RunnerMode::Stopped {
                return;
            }
            let mut set = HashSet::new();
            for rq in &st.open_quotes {
                set.insert(rq.order.ticker.clone());
            }
            for pos in &st.filled_positions {
                set.insert(pos.ticker.clone());
            }
            set.into_iter().collect()
        };
        let mut ob_snapshots: HashMap<String, crate::orderbook::OrderbookSnapshotPayload> =
            HashMap::with_capacity(open_tickers.len());
        for t in &open_tickers {
            ob_snapshots.insert(t.clone(), app.orderbook.snapshot(t));
        }

        // Phase 1: Decoupled resting-quote lifecycle & direct L2 matching on EVERY tick
        // (runs even when snapshots.jsonl has zero new REST rows or is empty).
        {
            let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if st.mode == RunnerMode::Stopped {
                return;
            }
            let cancel_before = st.cancel_before_close_secs as i64;
            let wall_now = now_unix();
            let existing_quotes = std::mem::take(&mut st.open_quotes);
            let mut kept_quotes = Vec::with_capacity(existing_quotes.len());
            for mut rq in existing_quotes {
                if !rq.replay_mode && (rq.order.close_epoch - wall_now) <= cancel_before {
                    if rq.order.mode == "live" {
                        if let Some(ref lid) = rq.order.live_order_id {
                            live_cancels.push((
                                lid.clone(),
                                rq.order,
                                wall_now,
                                "cancel_before_close".to_string(),
                            ));
                            continue;
                        }
                    }
                    st.emit_quote_canceled(&rq.order, wall_now, "cancel_before_close");
                    continue;
                }
                if rq.order.mode == "live" {
                    kept_quotes.push(rq);
                    continue;
                }
                if let Some(ob) = ob_snapshots.get(&rq.order.ticker) {
                    if !ob.stale
                        && !ob.needs_resync
                        && ob.source != "empty"
                        && (ob.updated_ms != rq.last_l2_updated_ms
                            || ob.last_trade_ms != rq.last_l2_trade_ms)
                    {
                        let filled = match_resting_quote_against_l2(&mut rq, ob);
                        if filled {
                            let fill_ts = if rq.replay_mode {
                                rq.order.posted_at.max(ob.updated_ms / 1000)
                            } else {
                                wall_now
                            };
                            rq.order.filled_contracts = rq.order.contracts;
                            rq.order.status = "filled".to_string();
                            let limit_c = rq.order.limit_cents;
                            let fee_d = maker_fee_dollars(limit_c as f64 / 100.0)
                                * (rq.order.contracts as f64);
                            let wait_s = (fill_ts - rq.order.posted_at).max(0) as f64;
                            st.record_fill(&rq.order, limit_c, 0.0, wait_s, fee_d, fill_ts);
                            continue;
                        }
                    }
                }
                kept_quotes.push(rq);
            }
            st.open_quotes = kept_quotes;

            // Evaluate L2-driven TP/SL/exit-before-close for filled positions in Phase 1.
            let pos_tickers: Vec<String> = st
                .filled_positions
                .iter()
                .map(|p| p.ticker.clone())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            for t in pos_tickers {
                if let Some(ob) = ob_snapshots.get(&t) {
                    if !ob.stale && !ob.needs_resync && ob.source != "empty" {
                        if let (Some(yb_c), Some(ya_c)) = (ob.best_yes_bid, ob.best_yes_ask) {
                            let nb_c = ob
                                .no_bids
                                .first()
                                .map(|l| l.price_cents)
                                .unwrap_or_else(|| 100u8.saturating_sub(ya_c).max(1));
                            evaluate_exits_for_market(
                                &mut st,
                                &t,
                                wall_now,
                                None,
                                yb_c,
                                nb_c,
                                Some(ob),
                                true,
                                &mut live_cancels,
                                &mut pending_live_exits,
                            );
                        }
                    }
                }
            }
        }

        let tape = app.tape(&app.live_snapshots_path);
        if !tape.grouped.is_empty() {
            // Collect fresh L2 snapshots for open tickers that have no new snapshots.jsonl rows
            // so new strategy entries can also react to live L2 updates outside state lock.
            let stale_cursor_tickers: Vec<String> = {
                let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
                tape.grouped
                    .iter()
                    .filter_map(|(t, snaps)| {
                        let prev_len = st.ticker_cursor.get(t.as_str()).copied()?;
                        if prev_len >= snaps.len() {
                            let last = snaps.last()?;
                            if last.result.is_empty()
                                && matches!(last.status.as_str(), "open" | "active")
                                && !ob_snapshots.contains_key(t.as_str())
                            {
                                return Some(t.clone());
                            }
                        }
                        None
                    })
                    .collect()
            };
            for t in stale_cursor_tickers {
                ob_snapshots.insert(t.clone(), app.orderbook.snapshot(&t));
            }

            // Refresh compiled strategy cache only when strategies_dir changes.
            cached_strats.refresh_if_needed(&app.strategies_dir, app.paper_pot);
            let has_v2 = cached_strats.strategies.iter().any(|s| s.is_v2());
            let mut pav_blocks_cache: Option<Vec<crate::recalibration::PavBlock>> = None;

            let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if st.mode == RunnerMode::Stopped {
                return;
            }
            let cancel_before = st.cancel_before_close_secs as i64;
            let bankroll = st.bankroll_dollars;
            let wall_now = now_unix();

            for (ticker_sym, snaps) in tape.grouped.iter() {
                let ticker = ticker_sym.as_str();
                if snaps.is_empty() {
                    continue;
                }
                let prev_len = st.ticker_cursor.get(ticker).copied();
                let start_idx = match prev_len {
                    Some(n) if n >= snaps.len() => {
                        // Zero new REST rows: check if fresh L2 + last snapshot can evaluate new entries.
                        let last = &snaps[snaps.len() - 1];
                        if last.result.is_empty()
                            && matches!(last.status.as_str(), "open" | "active")
                        {
                            if let (Some(close_epoch), Some(ob)) =
                                (parse_iso_z(last.close_time.as_str()), ob_snapshots.get(ticker))
                            {
                                let secs_to_close = close_epoch - wall_now;
                                let last_eval = st.l2_eval_ms.get(ticker).copied().unwrap_or(0);
                                if wall_now < close_epoch
                                    && secs_to_close > cancel_before
                                    && !ob.stale
                                    && !ob.needs_resync
                                    && ob.source != "empty"
                                    && ob.updated_ms > last_eval
                                {
                                    if let (Some(yb_c), Some(ya_c)) =
                                        (ob.best_yes_bid, ob.best_yes_ask)
                                    {
                                        let nb_c = ob
                                            .no_bids
                                            .first()
                                            .map(|l| l.price_cents)
                                            .unwrap_or_else(|| 100u8.saturating_sub(ya_c).max(1));
                                        let na_c = ob
                                            .no_asks
                                            .first()
                                            .map(|l| l.price_cents)
                                            .unwrap_or_else(|| 100u8.saturating_sub(yb_c).max(1));
                                        let yb_sz = ob
                                            .yes_bids
                                            .first()
                                            .map(|l| l.qty as f32)
                                            .unwrap_or(last.yes_bid_size);
                                        let nb_sz = ob
                                            .no_bids
                                            .first()
                                            .map(|l| l.qty as f32)
                                            .unwrap_or(last.no_bid_size);
                                        st.l2_eval_ms.insert(ticker.to_string(), ob.updated_ms);
                                        let live_spot =
                                            app.spot_feed.latest_fresh_for_series(last.series.as_str());
                                        let strike_opt = last.floor_strike.or(last.cap_strike);
                                        let cur_ctx = if has_v2 {
                                            let pav_blocks = pav_blocks_cache.get_or_insert_with(|| {
                                                crate::recalibration::fit_is_market_pav_blocks(
                                                    &tape.grouped,
                                                )
                                                .0
                                            });
                                            let mut tr =
                                                crate::strategy_v2::CausalFeatureTracker::new();
                                            let mut ctx_out = None;
                                            for pre in snaps {
                                                ctx_out = tr.push_tick(pre, pav_blocks);
                                            }
                                            if let Some(ref mut c) = ctx_out {
                                                inject_live_spot_strike(
                                                    c,
                                                    live_spot.as_ref(),
                                                    strike_opt,
                                                );
                                            }
                                            ctx_out
                                        } else {
                                            None
                                        };
                                        evaluate_reprices_for_market(
                                            &mut st,
                                            app,
                                            cached_strats,
                                            ticker,
                                            last.series.as_str(),
                                            wall_now,
                                            secs_to_close,
                                            cancel_before,
                                            (ya_c as f64) / 100.0,
                                            (na_c as f64) / 100.0,
                                            ya_c,
                                            na_c,
                                            yb_c,
                                            nb_c,
                                            yb_sz,
                                            nb_sz,
                                            cur_ctx.as_ref(),
                                            live_spot.as_ref(),
                                            strike_opt,
                                            &mut pending_live_amends,
                                        );
                                        if allow_live_entries {
                                            evaluate_slots_for_market(
                                                &mut st,
                                                app,
                                                cached_strats,
                                                mode,
                                                bankroll,
                                                ticker,
                                                last.series.as_str(),
                                                wall_now,
                                                close_epoch,
                                                secs_to_close,
                                                (ya_c as f64) / 100.0,
                                                (na_c as f64) / 100.0,
                                                ya_c,
                                                na_c,
                                                yb_c,
                                                nb_c,
                                                yb_sz,
                                                nb_sz,
                                                cur_ctx.as_ref(),
                                                live_spot.as_ref(),
                                                strike_opt,
                                                false,
                                                ob.updated_ms,
                                                ob.last_trade_ms,
                                                &mut pending_live_intents,
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        continue;
                    }
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

                let mut v2_tracker = if has_v2 {
                    let pav_blocks = pav_blocks_cache.get_or_insert_with(|| {
                        crate::recalibration::fit_is_market_pav_blocks(&tape.grouped).0
                    });
                    let mut tr = crate::strategy_v2::CausalFeatureTracker::new();
                    for pre in &snaps[..start_idx] {
                        let _ = tr.push_tick(pre, pav_blocks);
                    }
                    Some(tr)
                } else {
                    None
                };

                for (rel_idx, snap) in snaps[start_idx..].iter().enumerate() {
                    let is_latest_snap = start_idx + rel_idx + 1 == snaps.len();
                    let mut cur_ctx = v2_tracker.as_mut().and_then(|tr| {
                        let pav_blocks = pav_blocks_cache.as_deref().unwrap_or(&[]);
                        tr.push_tick(snap, pav_blocks)
                    });
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

                    // 1. Update resting quotes on this ticker (snapshot fallback + replay clock).
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
                        let (side_ask_c, side_bid_c, raw_bid_sz) = if rq.order.side == "yes" {
                            (ya_c, yb_c, snap.yes_bid_size)
                        } else {
                            (na_c, nb_c, snap.no_bid_size)
                        };
                        let side_bid_sz = sanitize_non_neg_size(raw_bid_sz);
                        let prev_bid_sz = sanitize_non_neg_size(rq.last_bid_size);
                        let limit_c = rq.order.limit_cents;
                        let side_last_hit = last_c.is_some_and(|p| {
                            let lp = if rq.order.side == "yes" {
                                p
                            } else {
                                100u8.saturating_sub(p)
                            };
                            lp <= limit_c
                        });
                        let bid_fell_through = side_bid_c < limit_c && rq.last_bid_cents >= limit_c;
                        let strict_cross = side_ask_c < limit_c || bid_fell_through;
                        let touch = side_ask_c == limit_c || side_last_hit;

                        if strict_cross {
                            rq.order.queue_ahead_remaining = 0;
                        } else if touch {
                            let bid_drop = if side_bid_c == limit_c && prev_bid_sz > side_bid_sz {
                                (prev_bid_sz - side_bid_sz).round().max(0.0) as u32
                            } else {
                                0
                            };
                            let step_deplete = bid_drop.max(rq.order.contracts).max(10);
                            rq.order.queue_ahead_remaining =
                                rq.order.queue_ahead_remaining.saturating_sub(step_deplete);
                        } else if side_bid_c == limit_c
                            && prev_bid_sz > side_bid_sz
                            && rq.order.queue_ahead_remaining > 1
                        {
                            let drop = (prev_bid_sz - side_bid_sz).round().max(0.0) as u32;
                            let new_rem = rq.order.queue_ahead_remaining.saturating_sub(drop).max(1);
                            rq.order.queue_ahead_remaining = new_rem;
                        }
                        rq.order.queue_ahead_remaining = rq
                            .order
                            .queue_ahead_remaining
                            .min(rq.order.queue_ahead_initial);
                        rq.last_bid_size = side_bid_sz;
                        rq.last_bid_cents = side_bid_c;

                        if (strict_cross || touch) && rq.order.queue_ahead_remaining == 0 {
                            rq.order.filled_contracts = rq.order.contracts;
                            rq.order.status = "filled".to_string();
                            let fee_d = maker_fee_dollars(limit_c as f64 / 100.0)
                                * (rq.order.contracts as f64);
                            let wait_s = (snap.fetched_at - rq.order.posted_at).max(0) as f64;
                            st.record_fill(&rq.order, limit_c, 0.0, wait_s, fee_d, snap.fetched_at);
                        } else {
                            kept_quotes.push(rq);
                        }
                    }
                    st.open_quotes = kept_quotes;

                    // 1b. Evaluate intra-window TP / SL / exit-before-close on filled positions.
                    let fresh_ob = ob_snapshots
                        .get(ticker)
                        .filter(|ob| !ob.stale && !ob.needs_resync && ob.source != "empty");
                    evaluate_exits_for_market(
                        &mut st,
                        ticker,
                        snap.fetched_at,
                        Some(secs_to_close),
                        yb_c,
                        nb_c,
                        fresh_ob,
                        false,
                        &mut live_cancels,
                        &mut pending_live_exits,
                    );

                    // 1c. Dynamic maker repricing on resting quotes (cooldown & max_replaces_per_window).
                    let replay_mode = wall_now >= close_epoch && snap.fetched_at < close_epoch;
                    let live_spot = if !replay_mode && is_latest_snap {
                        app.spot_feed.latest_fresh_for_series(snap.series.as_str())
                    } else {
                        None
                    };
                    let strike_opt = snap.floor_strike.or(snap.cap_strike);
                    if let Some(ref mut c) = cur_ctx {
                        inject_live_spot_strike(c, live_spot.as_ref(), strike_opt);
                    }
                    evaluate_reprices_for_market(
                        &mut st,
                        app,
                        cached_strats,
                        ticker,
                        snap.series.as_str(),
                        snap.fetched_at,
                        secs_to_close,
                        cancel_before,
                        ya,
                        na,
                        ya_c,
                        na_c,
                        yb_c,
                        nb_c,
                        snap.yes_bid_size,
                        snap.no_bid_size,
                        cur_ctx.as_ref(),
                        live_spot.as_ref(),
                        strike_opt,
                        &mut pending_live_amends,
                    );

                    // 2. Evaluate active slots without a quote or filled position on this ticker.
                    if secs_to_close <= cancel_before || !allow_live_entries {
                        continue;
                    }
                    evaluate_slots_for_market(
                        &mut st,
                        app,
                        cached_strats,
                        mode,
                        bankroll,
                        ticker,
                        snap.series.as_str(),
                        snap.fetched_at,
                        close_epoch,
                        secs_to_close,
                        ya,
                        na,
                        ya_c,
                        na_c,
                        yb_c,
                        nb_c,
                        snap.yes_bid_size,
                        snap.no_bid_size,
                        cur_ctx.as_ref(),
                        live_spot.as_ref(),
                        strike_opt,
                        replay_mode,
                        0,
                        None,
                        &mut pending_live_intents,
                    );
                }
                if allow_live_entries || !snaps.last().is_some_and(|s| s.result.is_empty()) {
                    st.ticker_cursor.insert(ticker.to_string(), snaps.len());
                }
            }
        }

        // Execute any pending live cancellations outside runner lock.
        if !live_cancels.is_empty() {
            if let Some(ref cfg) = live_cfg_opt {
                if let Ok(client) = kalshi_live::KalshiLiveClient::from_config(cfg) {
                    for (lid, q, ts, reason) in live_cancels {
                        if client.cancel_order(&lid).is_ok() {
                            let rem = q.contracts.saturating_sub(q.filled_contracts);
                            let notional = (rem as f64) * (q.limit_cents as f64 / 100.0);
                            let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                            let reserved = ld.reserved_order_exposure.remove(&lid).unwrap_or(notional);
                            ld.rails.release_exposure(reserved);
                            ld.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                                kind: "order_canceled".to_string(),
                                venue_id: lid.clone(),
                                ts,
                                order_id: lid.clone(),
                                client_order_id: Some(q.order_id.clone()),
                                trade_id: None,
                                ticker: q.ticker.clone(),
                                side: q.side.clone(),
                                action: "buy".to_string(),
                                count: rem as i64,
                                price_cents: q.limit_cents,
                                is_taker: None,
                                fee_dollars: 0.0,
                                revenue_cents: None,
                                realized_pnl_dollars: None,
                            });
                        }
                        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
                        st.open_quotes.retain(|rq| rq.order.order_id != q.order_id);
                        st.emit_quote_canceled(&q, ts, &reason);
                    }
                }
            }
        }

        // Execute pending guarded live order amendments (dynamic maker repricing).
        for amend_info in pending_live_amends {
            if self.halted.load(Ordering::SeqCst) {
                break;
            }
            let auth_res = {
                let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                if !ld.rails.live_armed || ld.rails.killed {
                    Err("disarmed or killed".to_string())
                } else if ld.rails.is_venue_sync_stale(now_unix()) {
                    let msg = "refusing live_auto amend: venue sync is stale (>30s) or pending".to_string();
                    ld.rails.last_error = Some(msg.clone());
                    Err(msg)
                } else {
                    let old_reserved = ld
                        .reserved_order_exposure
                        .get(&amend_info.live_order_id)
                        .copied()
                        .unwrap_or_else(|| {
                            (amend_info.contracts as f64) * (amend_info.old_limit_cents as f64 / 100.0)
                        });
                    if amend_info.new_limit_cents > amend_info.old_limit_cents {
                        let intent = risk_rails::LiveOrderIntent {
                            ticker: amend_info.ticker.clone(),
                            action: amend_info.action.clone(),
                            side: amend_info.side.clone(),
                            count: amend_info.contracts,
                            price_cents: amend_info.new_limit_cents,
                            client_order_id: Some(amend_info.order_id.clone()),
                            dry_run: false,
                        };
                        let check = ld.rails.check_amend_order(&intent, old_reserved);
                        if !check.allowed {
                            let msg = check.rail_violations.join("; ");
                            ld.rails.last_error = Some(msg.clone());
                            Err(msg)
                        } else {
                            Ok((ld.config.clone(), old_reserved))
                        }
                    } else {
                        Ok((ld.config.clone(), old_reserved))
                    }
                }
            };
            let (cfg, old_reserved) = match auth_res {
                Ok(v) => v,
                Err(_) => continue,
            };
            let Ok(client) = kalshi_live::KalshiLiveClient::from_config(&cfg) else {
                continue;
            };
            match client.amend_order_with_side(
                &amend_info.live_order_id,
                Some(&amend_info.side),
                amend_info.new_limit_cents,
                None,
            ) {
                Ok(_) => {
                    {
                        let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                        ld.connected = true;
                        ld.rails.last_error = None;
                        if amend_info.action.eq_ignore_ascii_case("buy") {
                            let new_reserved = (amend_info.contracts as f64)
                                * (amend_info.new_limit_cents as f64 / 100.0);
                            ld.rails.apply_amended_exposure(old_reserved, new_reserved);
                            ld.reserved_order_exposure
                                .insert(amend_info.live_order_id.clone(), new_reserved);
                        }
                        ld.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                            kind: "order_amended".to_string(),
                            venue_id: format!(
                                "{}:{}:{}:{}",
                                amend_info.live_order_id,
                                kalshi_live::now_millis_string(),
                                amend_info.new_limit_cents,
                                amend_info.contracts
                            ),
                            ts: amend_info.eval_ts,
                            order_id: amend_info.live_order_id.clone(),
                            client_order_id: Some(amend_info.order_id.clone()),
                            trade_id: None,
                            ticker: amend_info.ticker.clone(),
                            side: amend_info.side.clone(),
                            action: amend_info.action.clone(),
                            count: amend_info.contracts as i64,
                            price_cents: amend_info.new_limit_cents,
                            is_taker: None,
                            fee_dollars: 0.0,
                            revenue_cents: None,
                            realized_pnl_dollars: None,
                        });
                    }
                    let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    let mut amended_ord = None;
                    for rq in &mut st.open_quotes {
                        if rq.order.order_id == amend_info.order_id {
                            rq.order.limit_cents = amend_info.new_limit_cents;
                            rq.order.replace_count = rq.order.replace_count.saturating_add(1);
                            rq.last_replaced_at = amend_info.eval_ts;
                            amended_ord = Some(rq.order.clone());
                            break;
                        }
                    }
                    if let Some(ord) = amended_ord {
                        st.emit_order_amended(
                            &ord,
                            amend_info.eval_ts,
                            &format!(
                                "reprice:{}->{}",
                                amend_info.old_limit_cents, amend_info.new_limit_cents
                            ),
                        );
                    }
                }
                Err(e) => {
                    let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                    ld.rails.last_error = Some(e);
                }
            }
        }

        // Execute pending guarded live position exits (TP / SL / exit-before-close).
        for exit_info in pending_live_exits {
            if self.halted.load(Ordering::SeqCst) {
                break;
            }
            let sell_intent = risk_rails::LiveOrderIntent {
                ticker: exit_info.ticker.clone(),
                action: "sell".to_string(),
                side: exit_info.side.clone(),
                count: exit_info.contracts,
                price_cents: exit_info.limit_cents,
                client_order_id: Some(exit_info.client_order_id.clone()),
                dry_run: false,
            };
            let auth_res = {
                let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                if !ld.rails.live_armed || ld.rails.killed {
                    Err("disarmed or killed".to_string())
                } else if ld.rails.is_venue_sync_stale(now_unix()) {
                    let msg = "refusing live_auto exit order: venue sync is stale (>30s) or pending".to_string();
                    ld.rails.last_error = Some(msg.clone());
                    Err(msg)
                } else {
                    match ld.rails.authorize_order(&sell_intent) {
                        Ok((req, check)) => Ok((req, check, ld.config.clone())),
                        Err(check) => Err(check.rail_violations.join("; ")),
                    }
                }
            };
            let (kalshi_req, check, cfg) = match auth_res {
                Ok(v) => v,
                Err(_) => continue,
            };
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
                            .record_submitted_order("sell", check.order_notional_dollars);
                        ld.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                            kind: "order_submitted".to_string(),
                            venue_id: placed.order_id.clone(),
                            ts: exit_info.eval_ts,
                            order_id: placed.order_id.clone(),
                            client_order_id: Some(exit_info.client_order_id.clone()),
                            trade_id: None,
                            ticker: exit_info.ticker.clone(),
                            side: exit_info.side.clone(),
                            action: "sell".to_string(),
                            count: exit_info.contracts as i64,
                            price_cents: exit_info.limit_cents,
                            is_taker: Some(true),
                            fee_dollars: 0.0,
                            revenue_cents: None,
                            realized_pnl_dollars: None,
                        });
                    }
                    let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    let immediate_full = placed.status == "executed"
                        || (placed.remaining_count == 0 && placed.fill_count > 0);
                    let exit_order = VirtualOrLiveOrder {
                        order_id: exit_info.client_order_id.clone(),
                        slot_id: exit_info.slot_id,
                        strategy: exit_info.strategy.clone(),
                        ticker: exit_info.ticker.clone(),
                        series: exit_info.series.clone(),
                        mode: "live".to_string(),
                        execution_mode: "taker".to_string(),
                        action: "sell".to_string(),
                        side: exit_info.side.clone(),
                        limit_cents: exit_info.limit_cents,
                        contracts: exit_info.contracts,
                        filled_contracts: if immediate_full { exit_info.contracts } else { 0 },
                        queue_ahead_initial: 0,
                        queue_ahead_remaining: 0,
                        posted_at: exit_info.eval_ts,
                        close_epoch: exit_info.close_epoch,
                        live_order_id: Some(placed.order_id.clone()),
                        status: placed.status.clone(),
                        replace_count: 0,
                    };
                    if immediate_full {
                        let actual_c = if exit_info.side == "yes" {
                            placed.yes_price.or_else(|| placed.no_price.map(|p| 100 - p))
                        } else {
                            placed.no_price.or_else(|| placed.yes_price.map(|p| 100 - p))
                        }
                        .unwrap_or(exit_info.limit_cents as i64)
                        .clamp(1, 99) as u8;
                        let exit_fee = taker_fee_dollars(actual_c as f64 / 100.0)
                            * (exit_info.contracts as f64);
                        st.close_filled_positions_on_exit(
                            exit_info.slot_id,
                            &exit_info.ticker,
                            &exit_info.side,
                            exit_info.contracts,
                            actual_c,
                            actual_c as f64,
                            exit_fee,
                            &exit_info.client_order_id,
                            Some(placed.order_id),
                            "live",
                            &exit_info.strategy,
                            &exit_info.series,
                            "taker",
                            exit_info.limit_cents,
                            &exit_info.reason,
                            exit_info.eval_ts,
                        );
                    } else {
                        let post_ev = LedgerEvent {
                            seq: 0,
                            ts: exit_info.eval_ts,
                            event: "QuotePosted".to_string(),
                            mode: "live".to_string(),
                            slot_id: exit_info.slot_id,
                            strategy: exit_info.strategy.clone(),
                            ticker: exit_info.ticker.clone(),
                            series: exit_info.series.clone(),
                            order_id: exit_info.client_order_id.clone(),
                            side: exit_info.side.clone(),
                            execution_mode: "taker".to_string(),
                            limit_cents: exit_info.limit_cents,
                            fill_cents: None,
                            contracts: exit_info.contracts,
                            slippage_cents: None,
                            queue_wait_secs: None,
                            fee_dollars: None,
                            realized_pnl_dollars: None,
                            result: None,
                            reason: Some(exit_info.reason.clone()),
                            live_order_id: Some(placed.order_id.clone()),
                            action: Some("sell".to_string()),
                        };
                        st.append_and_publish(post_ev, "quote_posted");
                        st.open_quotes.push(RestingQuote {
                            order: exit_order,
                            last_bid_size: 0.0,
                            last_bid_cents: 0,
                            last_l2_updated_ms: 0,
                            last_l2_trade_ms: None,
                            last_replaced_at: exit_info.eval_ts,
                            replay_mode: exit_info.replay_mode,
                        });
                    }
                }
                Err(e) => {
                    let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                    ld.rails.last_error = Some(e);
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
                } else if ld.rails.is_venue_sync_stale(now_unix()) {
                    let msg = "refusing live_auto new order: venue sync is stale (>30s) or pending".to_string();
                    ld.rails.last_error = Some(msg.clone());
                    Err(msg)
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
                        ld.reserved_order_exposure
                            .insert(placed.order_id.clone(), check.order_notional_dollars);
                        ld.executions.append_idempotent(risk_rails::LiveExecutionRecord {
                            kind: "order_submitted".to_string(),
                            venue_id: placed.order_id.clone(),
                            ts: intent_info.posted_at,
                            order_id: placed.order_id.clone(),
                            client_order_id: Some(intent_info.client_order_id.clone()),
                            trade_id: None,
                            ticker: intent_info.ticker.clone(),
                            side: intent_info.side.clone(),
                            action: "buy".to_string(),
                            count: intent_info.contracts as i64,
                            price_cents: intent_info.limit_cents,
                            is_taker: Some(intent_info.execution_mode != "maker"),
                            fee_dollars: 0.0,
                            revenue_cents: None,
                            realized_pnl_dollars: None,
                        });
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
                    let immediate_full = placed.status == "executed"
                        || (placed.remaining_count == 0 && placed.fill_count > 0);
                    let mut order = VirtualOrLiveOrder {
                        order_id: intent_info.client_order_id.clone(),
                        slot_id: intent_info.slot_id,
                        strategy: intent_info.strategy.clone(),
                        ticker: intent_info.ticker.clone(),
                        series: intent_info.series.clone(),
                        mode: "live".to_string(),
                        execution_mode: intent_info.execution_mode.clone(),
                        action: "buy".to_string(),
                        side: intent_info.side.clone(),
                        limit_cents: intent_info.limit_cents,
                        contracts: intent_info.contracts,
                        filled_contracts: 0,
                        queue_ahead_initial: 0,
                        queue_ahead_remaining: 0,
                        posted_at: intent_info.posted_at,
                        close_epoch: intent_info.close_epoch,
                        live_order_id: Some(placed.order_id.clone()),
                        status: placed.status.clone(),
                        replace_count: 0,
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
                        action: Some("buy".to_string()),
                    };
                    st.append_and_publish(post_ev, "quote_posted");

                    if immediate_full {
                        let actual_c = if order.side == "yes" {
                            placed.yes_price.or_else(|| placed.no_price.map(|p| 100 - p))
                        } else {
                            placed.no_price.or_else(|| placed.yes_price.map(|p| 100 - p))
                        }
                        .unwrap_or(intent_info.limit_cents as i64)
                        .clamp(1, 99) as u8;
                        let fee_fn = if intent_info.execution_mode == "maker" {
                            maker_fee_dollars
                        } else {
                            taker_fee_dollars
                        };
                        let fee_d = fee_fn(actual_c as f64 / 100.0)
                            * (intent_info.contracts as f64);
                        st.record_live_fill(
                            &mut order,
                            actual_c,
                            intent_info.contracts,
                            fee_d,
                            intent_info.posted_at,
                        );
                    } else {
                        st.open_quotes.push(RestingQuote {
                            order,
                            last_bid_size: 0.0,
                            last_bid_cents: 0,
                            last_l2_updated_ms: 0,
                            last_l2_trade_ms: None,
                            last_replaced_at: intent_info.posted_at,
                            replay_mode: intent_info.replay_mode,
                        });
                    }
                }
                Err(e) => {
                    let mut ld = app.live_desk.lock().unwrap_or_else(|p| p.into_inner());
                    ld.rails.last_error = Some(e);
                }
            }
        }

        // Sample markout midpoints for recent fills and refresh execution quarantine state.
        let (data_dir, need_audit) = {
            let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let wall_now = now_unix();
            let mut to_sample: Vec<(String, f64)> = Vec::new();
            let mut expired: Vec<String> = Vec::new();
            for (t, fts) in &st.recent_fill_tickers {
                if wall_now - *fts > 180 {
                    expired.push(t.clone());
                    continue;
                }
                if st.last_markout_sample_ts.get(t).copied().unwrap_or(0) >= wall_now {
                    continue;
                }
                if let Some(ob) = ob_snapshots.get(t) {
                    if !ob.stale && !ob.needs_resync && ob.source != "empty" {
                        if let Some(mid) = ob.mid_cents.filter(|m| m.is_finite()) {
                            to_sample.push((t.clone(), mid));
                            continue;
                        }
                    }
                }
                if let Some(snaps) = tape.grouped.get(t.as_str()) {
                    if let Some(last) = snaps.last() {
                        if last.result.is_empty() {
                            if let (Some(yb), Some(ya)) = (last.yes_bid.get(), last.yes_ask.get()) {
                                to_sample.push((t.clone(), (yb + ya) * 50.0));
                            }
                        }
                    }
                }
            }
            for t in expired {
                st.recent_fill_tickers.remove(&t);
            }
            let sampled = !to_sample.is_empty();
            for (t, mid) in to_sample {
                st.last_markout_sample_ts.insert(t.clone(), wall_now);
                crate::execution_audit::append_markout_sample(
                    &st.data_dir,
                    &crate::execution_audit::MarkoutSampleRecord {
                        ts_ms: wall_now * 1000,
                        ticker: t,
                        yes_mid_cents: mid,
                    },
                );
            }
            let dirty = st.audit_dirty || sampled;
            st.audit_dirty = false;
            (st.data_dir.clone(), dirty)
        };

        if need_audit {
            let mut tape_mids: HashMap<String, Vec<crate::execution_audit::MidpointObs>> =
                HashMap::new();
            let mut tape_settle: HashMap<String, String> = HashMap::new();
            for (t, snaps) in tape.grouped.iter() {
                for s in snaps {
                    let res = s.result.as_str().trim().to_ascii_lowercase();
                    if res == "yes" || res == "no" {
                        tape_settle.insert(t.clone(), res);
                    } else if let (Some(yb), Some(ya)) = (s.yes_bid.get(), s.yes_ask.get()) {
                        tape_mids
                            .entry(t.clone())
                            .or_default()
                            .push(crate::execution_audit::MidpointObs {
                                ts_secs: s.fetched_at as f64,
                                yes_mid_cents: (yb + ya) * 50.0,
                            });
                    }
                }
            }
            let health = crate::execution_audit::evaluate_execution_health(
                &data_dir,
                Some(&app.orderbook),
                Some(&tape_mids),
                Some(&tape_settle),
                true,
            );
            self.set_quarantine_state(health.quarantine_state);
        }
    }
}

fn match_resting_quote_against_l2(
    rq: &mut RestingQuote,
    ob: &crate::orderbook::OrderbookSnapshotPayload,
) -> bool {
    let limit_c = rq.order.limit_cents;
    let (has_bid, side_bid_c, has_ask, side_ask_c, side_bid_sz) = if rq.order.side == "yes" {
        let sz = ob.yes_bids.first().map(|l| l.qty as f32).unwrap_or(0.0);
        (
            ob.best_yes_bid.is_some(),
            ob.best_yes_bid.unwrap_or(0),
            ob.best_yes_ask.is_some(),
            ob.best_yes_ask.unwrap_or(100),
            sz,
        )
    } else {
        let nb = ob.no_bids.first();
        let na = ob.no_asks.first();
        let sz = nb.map(|l| l.qty as f32).unwrap_or(0.0);
        (
            nb.is_some(),
            nb.map(|l| l.price_cents).unwrap_or(0),
            na.is_some(),
            na.map(|l| l.price_cents).unwrap_or(100),
            sz,
        )
    };
    let side_bid_sz = sanitize_non_neg_size(side_bid_sz);
    let prev_bid_sz = sanitize_non_neg_size(rq.last_bid_size);

    let trade_new = ob.last_trade_ms.is_some() && ob.last_trade_ms != rq.last_l2_trade_ms;
    let trade_side_p = if trade_new {
        ob.last_trade_cents.map(|p| {
            if rq.order.side == "yes" {
                p
            } else {
                100u8.saturating_sub(p)
            }
        })
    } else {
        None
    };
    let trade_sz = if trade_side_p.is_some_and(|lp| lp <= limit_c) {
        ob.last_trade_size.unwrap_or(0).min(u32::MAX as u64) as u32
    } else {
        0
    };

    let bid_fell_through = has_bid && side_bid_c < limit_c && rq.last_bid_cents >= limit_c;
    let strict_cross = (has_ask && side_ask_c < limit_c)
        || bid_fell_through
        || trade_side_p.is_some_and(|lp| lp < limit_c);
    let touch = (has_ask && side_ask_c == limit_c) || trade_side_p.is_some_and(|lp| lp == limit_c);

    if strict_cross {
        rq.order.queue_ahead_remaining = 0;
    } else if touch {
        let bid_drop = if has_bid && side_bid_c == limit_c && prev_bid_sz > side_bid_sz {
            (prev_bid_sz - side_bid_sz).round().max(0.0) as u32
        } else {
            0
        };
        let step_deplete = bid_drop.max(trade_sz).max(rq.order.contracts).max(10);
        rq.order.queue_ahead_remaining = rq
            .order
            .queue_ahead_remaining
            .saturating_sub(step_deplete);
    } else if has_bid
        && side_bid_c == limit_c
        && prev_bid_sz > side_bid_sz
        && rq.order.queue_ahead_remaining > 1
    {
        let drop = (prev_bid_sz - side_bid_sz).round().max(0.0) as u32;
        rq.order.queue_ahead_remaining = rq
            .order
            .queue_ahead_remaining
            .saturating_sub(drop)
            .max(1);
    }

    rq.order.queue_ahead_remaining = rq
        .order
        .queue_ahead_remaining
        .min(rq.order.queue_ahead_initial);
    rq.last_bid_size = side_bid_sz;
    if has_bid {
        rq.last_bid_cents = side_bid_c;
    }
    rq.last_l2_updated_ms = ob.updated_ms;
    rq.last_l2_trade_ms = ob.last_trade_ms;
    (strict_cross || touch) && rq.order.queue_ahead_remaining == 0
}

fn resolve_bid_exit_execution(
    ob: Option<&crate::orderbook::OrderbookSnapshotPayload>,
    side: &str,
    contracts: u32,
    fallback_bid_c: u8,
) -> (u8, f64) {
    let fb = fallback_bid_c.clamp(1, 99);
    if contracts == 0 {
        return (fb, fb as f64);
    }
    if let Some(book) = ob {
        if !book.stale && !book.needs_resync && book.source != "empty" {
            let levels = if side == "yes" {
                &book.yes_bids
            } else {
                &book.no_bids
            };
            let mut rem = contracts as u64;
            let mut filled = 0u64;
            let mut notional_c = 0.0f64;
            for lvl in levels {
                if rem == 0 {
                    break;
                }
                let take = rem.min(lvl.qty);
                if take > 0 {
                    notional_c += (lvl.price_cents as f64) * (take as f64);
                    filled += take;
                    rem -= take;
                }
            }
            if filled > 0 {
                let vwap = notional_c / (filled as f64);
                let c = vwap.round().clamp(1.0, 99.0) as u8;
                return (c, vwap);
            }
        }
    }
    (fb, fb as f64)
}

fn inject_live_spot_strike(
    ctx: &mut crate::strategy_v2::TickContext,
    spot_opt: Option<&crate::spot_feed::SpotAssetStatus>,
    strike_opt: Option<f64>,
) {
    if ctx.strike_price <= 0.0 {
        if let Some(strike) = strike_opt.filter(|v| v.is_finite() && *v > 0.0) {
            ctx.strike_price = strike;
        }
    }
    if let Some(spot) = spot_opt {
        if let Some(px) = spot.price.filter(|v| v.is_finite() && *v > 0.0) {
            ctx.spot_price = px;
            if ctx.strike_price > 0.0 {
                let d = ((px - ctx.strike_price) / ctx.strike_price) * 10_000.0;
                if d.is_finite() {
                    ctx.spot_distance_bps = d;
                }
            }
        }
        if let Some(d30) = spot.delta_30s_bps.filter(|v| v.is_finite()) {
            ctx.spot_delta_30s_bps = d30;
        }
        if let Some(vb) = spot.vol_bps.filter(|v| v.is_finite()) {
            ctx.spot_vol_bps = vb;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn build_slot_tick_context(
    st: &RunnerState,
    slot_id: usize,
    ticker: &str,
    series: &str,
    secs_to_close: i64,
    ya: f64,
    na: f64,
    yb_c: u8,
    nb_c: u8,
    base_ctx: Option<&crate::strategy_v2::TickContext>,
    live_spot: Option<&crate::spot_feed::SpotAssetStatus>,
    strike_opt: Option<f64>,
    need_v2: bool,
) -> Option<crate::strategy_v2::TickContext> {
    if base_ctx.is_none() && !need_v2 {
        return None;
    }
    let mut pos_contracts = 0i64;
    let mut cost_sum = 0.0f64;
    let mut unreal_sum = 0.0f64;
    for pos in &st.filled_positions {
        if pos.slot_id == slot_id && pos.ticker == ticker && pos.contracts > 0 {
            let qty = pos.contracts as i64;
            let cur_bid = if pos.side == "yes" { yb_c } else { nb_c };
            pos_contracts += qty;
            cost_sum += (pos.fill_cents as f64) * (qty as f64);
            unreal_sum += ((cur_bid as f64) - (pos.fill_cents as f64)) * (qty as f64);
        }
    }
    let avg_entry = if pos_contracts > 0 {
        cost_sum / (pos_contracts as f64)
    } else {
        0.0
    };
    let resting_cents = st
        .open_quotes
        .iter()
        .find(|rq| rq.order.slot_id == slot_id && rq.order.ticker == ticker && rq.order.action != "sell")
        .map(|rq| rq.order.limit_cents as i64)
        .unwrap_or(0);

    let mut ctx = match base_ctx {
        Some(c) => c.clone(),
        None => {
            let yb = (yb_c as f64 / 100.0).clamp(0.01, 0.99);
            let nb = (nb_c as f64 / 100.0).clamp(0.01, 0.99);
            crate::strategy_v2::TickContext {
                yes_bid: yb,
                yes_ask: ya,
                no_bid: nb,
                no_ask: na,
                secs_to_close,
                spread_cents: ((ya - yb) * 100.0).max(0.0),
                yes_bid_size: 0.0,
                no_bid_size: 0.0,
                book_imbalance: 0.0,
                tick_index: 0,
                open_yes_ask: ya,
                prev_yes_ask: ya,
                high_yes_ask: ya,
                low_yes_ask: ya,
                delta_60s_cents: 0.0,
                window_vol_cents: 0.0,
                pav_yes_prob: ya.clamp(0.01, 0.99),
                pav_ev_yes_cents: 0.0,
                pav_ev_no_cents: 0.0,
                series: series.to_string(),
                slot_position_contracts: 0,
                slot_avg_entry_cents: 0.0,
                slot_unrealized_pnl_cents: 0.0,
                slot_resting_cents: 0,
                spot_price: 0.0,
                strike_price: 0.0,
                spot_distance_bps: 0.0,
                spot_delta_30s_bps: 0.0,
                spot_vol_bps: 0.0,
            }
        }
    };
    ctx.slot_position_contracts = pos_contracts;
    ctx.slot_avg_entry_cents = avg_entry;
    ctx.slot_unrealized_pnl_cents = unreal_sum;
    ctx.slot_resting_cents = resting_cents;
    inject_live_spot_strike(&mut ctx, live_spot, strike_opt);
    Some(ctx)
}

#[allow(clippy::too_many_arguments)]
fn evaluate_exits_for_market(
    st: &mut RunnerState,
    ticker: &str,
    eval_ts: i64,
    secs_to_close_opt: Option<i64>,
    yb_c: u8,
    nb_c: u8,
    ob: Option<&crate::orderbook::OrderbookSnapshotPayload>,
    respect_replay_guard: bool,
    live_cancels: &mut Vec<(String, VirtualOrLiveOrder, i64, String)>,
    pending_live_exits: &mut Vec<PendingLiveExit>,
) {
    let candidates: Vec<FilledPosition> = st
        .filled_positions
        .iter()
        .filter(|p| p.ticker == ticker && p.contracts > 0)
        .cloned()
        .collect();
    if candidates.is_empty() {
        return;
    }

    for pos in candidates {
        if st.open_quotes.iter().any(|rq| {
            rq.order.slot_id == pos.slot_id
                && rq.order.ticker == ticker
                && rq.order.action == "sell"
        }) || pending_live_exits
            .iter()
            .any(|e| e.slot_id == pos.slot_id && e.ticker == ticker)
        {
            continue;
        }
        let Some((tp_opt, sl_opt, exit_before)) = st
            .slots
            .iter()
            .find(|s| s.cfg.slot_id == pos.slot_id)
            .map(|s| {
                (
                    s.cfg.take_profit_cents,
                    s.cfg.stop_loss_cents,
                    s.cfg.exit_before_close_secs,
                )
            })
        else {
            continue;
        };
        let bid_c = if pos.side == "yes" { yb_c } else { nb_c };
        let pnl_cents = (bid_c as i64) - (pos.fill_cents as i64);
        let secs_to_close = secs_to_close_opt.unwrap_or_else(|| pos.close_epoch - eval_ts);
        let check_ebc = exit_before > 0 && (!respect_replay_guard || !pos.replay_mode);

        let reason = if check_ebc && secs_to_close <= exit_before as i64 {
            Some("exit_before_close")
        } else if tp_opt.is_some_and(|tp| pnl_cents >= tp as i64) {
            Some("take_profit")
        } else if sl_opt.is_some_and(|sl| -pnl_cents >= sl as i64) {
            Some("stop_loss")
        } else {
            None
        };
        let Some(reason) = reason else {
            continue;
        };

        // Cancel any resting buy quote for this slot/ticker before exiting.
        let mut kept_quotes = Vec::with_capacity(st.open_quotes.len());
        let mut to_cancel_shadow = Vec::new();
        for rq in st.open_quotes.drain(..) {
            if rq.order.slot_id == pos.slot_id
                && rq.order.ticker == ticker
                && rq.order.action != "sell"
            {
                if rq.order.mode == "live" {
                    if let Some(ref lid) = rq.order.live_order_id {
                        live_cancels.push((
                            lid.clone(),
                            rq.order,
                            eval_ts,
                            reason.to_string(),
                        ));
                        continue;
                    }
                }
                to_cancel_shadow.push(rq.order);
            } else {
                kept_quotes.push(rq);
            }
        }
        st.open_quotes = kept_quotes;
        for q in to_cancel_shadow {
            st.emit_quote_canceled(&q, eval_ts, reason);
        }

        let exit_order_id = st.next_order_id();
        if pos.mode == "live" {
            pending_live_exits.push(PendingLiveExit {
                slot_id: pos.slot_id,
                strategy: pos.strategy.clone(),
                ticker: pos.ticker.clone(),
                series: pos.series.clone(),
                side: pos.side.clone(),
                contracts: pos.contracts,
                limit_cents: bid_c.clamp(1, 99),
                client_order_id: exit_order_id,
                reason: reason.to_string(),
                eval_ts,
                close_epoch: pos.close_epoch,
                replay_mode: pos.replay_mode,
            });
            continue;
        }

        let (exit_c, exit_exact_c) = resolve_bid_exit_execution(ob, &pos.side, pos.contracts, bid_c);
        let exit_fee = taker_fee_dollars(exit_exact_c / 100.0) * (pos.contracts as f64);
        st.close_filled_positions_on_exit(
            pos.slot_id,
            &pos.ticker,
            &pos.side,
            pos.contracts,
            exit_c,
            exit_exact_c,
            exit_fee,
            &exit_order_id,
            None,
            &pos.mode,
            &pos.strategy,
            &pos.series,
            "taker",
            bid_c.clamp(1, 99),
            reason,
            eval_ts,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_reprices_for_market(
    st: &mut RunnerState,
    app: &App,
    cached_strats: &CachedStrategies,
    ticker: &str,
    series: &str,
    eval_ts: i64,
    secs_to_close: i64,
    cancel_before: i64,
    ya: f64,
    na: f64,
    ya_c: u8,
    na_c: u8,
    yb_c: u8,
    nb_c: u8,
    yes_bid_size: f32,
    no_bid_size: f32,
    cur_ctx: Option<&crate::strategy_v2::TickContext>,
    live_spot: Option<&crate::spot_feed::SpotAssetStatus>,
    strike_opt: Option<f64>,
    pending_live_amends: &mut Vec<PendingLiveAmend>,
) {
    if secs_to_close <= cancel_before {
        return;
    }
    let quote_len = st.open_quotes.len();
    for q_idx in 0..quote_len {
        let (
            slot_id,
            q_order_id,
            q_mode,
            q_exec_mode,
            q_action,
            q_side,
            q_limit_cents,
            q_contracts,
            q_replace_count,
            q_live_order_id,
            q_status,
            last_replaced_at,
        ) = {
            let rq = &st.open_quotes[q_idx];
            if rq.order.ticker != ticker {
                continue;
            }
            (
                rq.order.slot_id,
                rq.order.order_id.clone(),
                rq.order.mode.clone(),
                rq.order.execution_mode.clone(),
                rq.order.action.clone(),
                rq.order.side.clone(),
                rq.order.limit_cents,
                rq.order.contracts,
                rq.order.replace_count,
                rq.order.live_order_id.clone(),
                rq.order.status.clone(),
                rq.last_replaced_at,
            )
        };
        if q_action == "sell" || (q_status != "resting" && q_status != "partial") {
            continue;
        }
        let Some((strat_name, cooldown, max_replaces, exit_before)) = st
            .slots
            .iter()
            .find(|s| s.cfg.slot_id == slot_id && s.cfg.enabled && !s.paused)
            .map(|s| {
                (
                    s.cfg.strategy.clone(),
                    s.cfg.reprice_cooldown_secs,
                    s.cfg.max_replaces_per_window,
                    s.cfg.exit_before_close_secs,
                )
            })
        else {
            continue;
        };
        if max_replaces == 0 || q_replace_count >= max_replaces {
            continue;
        }
        if exit_before > 0 && secs_to_close <= exit_before as i64 {
            continue;
        }
        if eval_ts.saturating_sub(last_replaced_at) < cooldown as i64 {
            continue;
        }
        let Some(strat) = cached_strats.find(&strat_name) else {
            continue;
        };
        let slot_ctx = build_slot_tick_context(
            st,
            slot_id,
            ticker,
            series,
            secs_to_close,
            ya,
            na,
            yb_c,
            nb_c,
            cur_ctx,
            live_spot,
            strike_opt,
            strat.is_v2(),
        );
        let Some((quote_dollars, eval_side, post_only)) = strat.entry_quote_with_ctx(
            ya,
            na,
            secs_to_close,
            slot_ctx.as_ref(),
            q_exec_mode != "taker",
        ) else {
            continue;
        };
        if eval_side != q_side || (!post_only && q_exec_mode != "maker") {
            continue;
        }
        let Some(quote_cents) = to_whole_cents(quote_dollars) else {
            continue;
        };
        let (side_ask_c, side_bid_c, raw_bid_sz) = if q_side == "yes" {
            (ya_c, yb_c, yes_bid_size)
        } else {
            (na_c, nb_c, no_bid_size)
        };
        let new_limit_cents = if quote_cents >= side_ask_c {
            if side_ask_c <= 1 {
                continue;
            }
            side_ask_c - 1
        } else {
            quote_cents
        };
        if !(1..=99).contains(&new_limit_cents) || new_limit_cents == q_limit_cents {
            continue;
        }
        if q_mode == "live" {
            if let Some(lid) = q_live_order_id {
                if !pending_live_amends.iter().any(|a| a.order_id == q_order_id) {
                    pending_live_amends.push(PendingLiveAmend {
                        order_id: q_order_id,
                        live_order_id: lid,
                        ticker: ticker.to_string(),
                        side: q_side,
                        action: q_action,
                        old_limit_cents: q_limit_cents,
                        new_limit_cents,
                        contracts: q_contracts,
                        eval_ts,
                    });
                }
            }
            continue;
        }

        let side_bid_sz = sanitize_non_neg_size(raw_bid_sz);
        let new_queue = if new_limit_cents > side_bid_c {
            0
        } else if let Some(l2_q) = app
            .orderbook
            .queue_ahead_at_fresh(ticker, &q_side, new_limit_cents)
        {
            l2_q.min(u32::MAX as u64) as u32
        } else if new_limit_cents == side_bid_c {
            side_bid_sz.round().max(0.0) as u32
        } else {
            (side_bid_sz.round().max(0.0) as u32).max(10)
        };
        let amended_order = {
            let rq = &mut st.open_quotes[q_idx];
            rq.order.limit_cents = new_limit_cents;
            rq.order.replace_count = rq.order.replace_count.saturating_add(1);
            rq.order.queue_ahead_initial = new_queue;
            rq.order.queue_ahead_remaining = new_queue;
            rq.last_replaced_at = eval_ts;
            rq.order.clone()
        };
        st.emit_order_amended(
            &amended_order,
            eval_ts,
            &format!("reprice:{q_limit_cents}->{new_limit_cents}"),
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn evaluate_slots_for_market(
    st: &mut RunnerState,
    app: &App,
    cached_strats: &CachedStrategies,
    mode: RunnerMode,
    bankroll: f64,
    ticker: &str,
    series: &str,
    eval_ts: i64,
    close_epoch: i64,
    secs_to_close: i64,
    ya: f64,
    na: f64,
    ya_c: u8,
    na_c: u8,
    yb_c: u8,
    nb_c: u8,
    yes_bid_size: f32,
    no_bid_size: f32,
    cur_ctx: Option<&crate::strategy_v2::TickContext>,
    live_spot: Option<&crate::spot_feed::SpotAssetStatus>,
    strike_opt: Option<f64>,
    replay_mode: bool,
    l2_updated_ms: i64,
    l2_trade_ms: Option<i64>,
    pending_live_intents: &mut Vec<PendingLiveOrderIntent>,
) {
    let enabled_count = st
        .slots
        .iter()
        .filter(|s| s.cfg.enabled && !s.paused)
        .count()
        .max(1);
    let slot_budget = bankroll / (enabled_count as f64);
    let slot_count = st.slots.len();

    for idx in 0..slot_count {
        let (slot_id, max_contracts) = {
            let s = &st.slots[idx];
            if !s.cfg.enabled || s.paused {
                continue;
            }
            let series_match = if s.cfg.series.is_empty() {
                SERIES.contains(&series)
            } else {
                s.cfg
                    .series
                    .iter()
                    .any(|sr| sr.eq_ignore_ascii_case(series))
            };
            if !series_match {
                continue;
            }
            (s.cfg.slot_id, s.cfg.max_contracts)
        };
        if st
            .open_quotes
            .iter()
            .any(|rq| rq.order.slot_id == slot_id && rq.order.ticker == ticker)
            || st.has_slot_ticker(slot_id, ticker)
        {
            continue;
        }
        let (strat_name, exec_mode, exit_before) = {
            let s = &st.slots[idx];
            (
                s.cfg.strategy.clone(),
                s.cfg.execution_mode.clone(),
                s.cfg.exit_before_close_secs,
            )
        };
        if exit_before > 0 && secs_to_close <= exit_before as i64 {
            continue;
        }
        let q_mult = st.quarantine_state.throttle_mult(&strat_name);
        if q_mult <= 0.0 {
            continue;
        }
        let eff_max_contracts = if q_mult < 1.0 {
            ((max_contracts as f64) * q_mult).floor().max(1.0) as u32
        } else {
            max_contracts
        };
        let eff_slot_budget = slot_budget * q_mult;

        let Some(strat) = cached_strats.find(&strat_name) else {
            continue;
        };
        let slot_ctx = build_slot_tick_context(
            st,
            slot_id,
            ticker,
            series,
            secs_to_close,
            ya,
            na,
            yb_c,
            nb_c,
            cur_ctx,
            live_spot,
            strike_opt,
            strat.is_v2(),
        );
        let Some((quote_dollars, entry_side, post_only)) = strat.entry_quote_with_ctx(
            ya,
            na,
            secs_to_close,
            slot_ctx.as_ref(),
            exec_mode != "taker",
        ) else {
            continue;
        };
        let Some(quote_cents) = to_whole_cents(quote_dollars) else {
            continue;
        };
        let eff_exec_mode = if post_only && exec_mode == "maker" {
            "maker".to_string()
        } else if !post_only && strat.is_v2() {
            "taker".to_string()
        } else {
            exec_mode.clone()
        };

        let (side_ask_c, side_bid_c, raw_bid_sz) = if entry_side == "yes" {
            (ya_c, yb_c, yes_bid_size)
        } else {
            (na_c, nb_c, no_bid_size)
        };
        let side_bid_sz = sanitize_non_neg_size(raw_bid_sz);

        let (limit_cents, eff_contracts, taker_exact_cents, taker_slip_cents) =
            if eff_exec_mode == "taker" {
                let (fc, qty, exact_c, slip, _used_l2) = resolve_taker_execution(
                    &app.orderbook,
                    ticker,
                    &entry_side,
                    eff_max_contracts,
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
                (lc, eff_max_contracts, lc as f64, 0.0)
            };
        if !(1..=99).contains(&limit_cents) || eff_contracts == 0 {
            continue;
        }

        let affordable = (eff_slot_budget / (limit_cents as f64 / 100.0)).floor() as u32;
        let contracts = eff_contracts.min(affordable);
        if contracts == 0 {
            continue;
        }

        let order_id = st.next_order_id();
        if mode == RunnerMode::LiveAuto {
            if pending_live_intents
                .iter()
                .any(|p| p.slot_id == slot_id && p.ticker == ticker)
            {
                continue;
            }
            pending_live_intents.push(PendingLiveOrderIntent {
                slot_id,
                strategy: strat.name.clone(),
                ticker: ticker.to_string(),
                series: series.to_string(),
                execution_mode: eff_exec_mode,
                side: entry_side,
                limit_cents,
                contracts,
                posted_at: eval_ts,
                close_epoch,
                client_order_id: order_id,
                replay_mode,
            });
            continue;
        }

        let queue_initial = if eff_exec_mode == "taker" || limit_cents > side_bid_c {
            0
        } else if let Some(l2_q) = app
            .orderbook
            .queue_ahead_at_fresh(ticker, &entry_side, limit_cents)
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
            series: series.to_string(),
            mode: "shadow".to_string(),
            execution_mode: eff_exec_mode.clone(),
            action: "buy".to_string(),
            side: entry_side.clone(),
            limit_cents,
            contracts,
            filled_contracts: if eff_exec_mode == "taker" {
                contracts
            } else {
                0
            },
            queue_ahead_initial: queue_initial,
            queue_ahead_remaining: queue_initial,
            posted_at: eval_ts,
            close_epoch,
            live_order_id: None,
            status: if eff_exec_mode == "taker" {
                "filled".to_string()
            } else {
                "resting".to_string()
            },
            replace_count: 0,
        };

        st.total_quotes_posted = st.total_quotes_posted.saturating_add(1);
        if let Some(sl) = st.slots.get_mut(idx) {
            sl.quotes_posted = sl.quotes_posted.saturating_add(1);
        }
        let post_ev = LedgerEvent {
            seq: 0,
            ts: eval_ts,
            event: "QuotePosted".to_string(),
            mode: "shadow".to_string(),
            slot_id,
            strategy: strat.name.clone(),
            ticker: ticker.to_string(),
            series: series.to_string(),
            order_id: order_id.clone(),
            side: entry_side,
            execution_mode: eff_exec_mode.clone(),
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
            action: Some("buy".to_string()),
        };
        st.append_and_publish(post_ev, "quote_posted");

        if eff_exec_mode == "taker" {
            let fee_d = taker_fee_dollars(taker_exact_cents / 100.0) * (contracts as f64);
            st.record_fill(&order, limit_cents, taker_slip_cents, 0.0, fee_d, eval_ts);
        } else {
            st.open_quotes.push(RestingQuote {
                order,
                last_bid_size: side_bid_sz,
                last_bid_cents: side_bid_c,
                last_l2_updated_ms: l2_updated_ms,
                last_l2_trade_ms: l2_trade_ms,
                last_replaced_at: eval_ts,
                replay_mode,
            });
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
                let underscored = s.name.replace('-', "_");
                if underscored != s.name {
                    stem_map.insert(underscored, i);
                }
                let hyphenated = s.name.replace('_', "-");
                if hyphenated != s.name {
                    stem_map.insert(hyphenated, i);
                }
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

            app.runner.tick_notify.store(false, Ordering::SeqCst);
            app.runner.step_once(&app, &mut cached_strats);

            // Wait up to tick_interval_ms unless immediately woken by stop/halt/notify_tick.
            if !app.runner.tick_notify.load(Ordering::SeqCst) {
                let st = app.runner.state.lock().unwrap_or_else(|p| p.into_inner());
                if st.mode != RunnerMode::Stopped && !app.runner.tick_notify.load(Ordering::SeqCst)
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
    if contracts == 0 {
        let fc = side_ask_c.clamp(1, 99);
        return (fc, 0, fc as f64, 0.0, false);
    }
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
                        reprice_cooldown_secs: 0,
                        max_replaces_per_window: 0,
                        take_profit_cents: None,
                        stop_loss_cents: None,
                        exit_before_close_secs: 0,
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

    #[test]
    fn restart_clamps_live_auto_and_duplicate_fill_cancel_are_idempotent() {
        let dir = std::env::temp_dir().join(format!("ob-sr-hard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data_dir = dir.join("data");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Write tampered runner_state.json with mode = "live_auto"
        std::fs::write(
            state_path(&data_dir),
            serde_json::json!({
                "mode": "live_auto",
                "tick_interval_ms": 100,
                "cancel_before_close_secs": 30,
                "bankroll_dollars": 500.0,
                "slots": [{
                    "slot_id": 1,
                    "strategy": "demo-strat",
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

        let runner = ShadowRunner::load(&data_dir, 500.0);
        let snap = runner.status(false, false);
        assert_eq!(snap.mode, "stopped", "live_auto MUST clamp to stopped on boot");
        assert!(!snap.live_armed);
        // Persisted file on disk must also be sanitized.
        let disk_raw = std::fs::read_to_string(state_path(&data_dir)).unwrap();
        let disk_v: serde_json::Value = serde_json::from_str(&disk_raw).unwrap();
        assert_eq!(disk_v["mode"], "stopped");

        // Duplicate fill & duplicate cancel idempotency on same order_id.
        let order = VirtualOrLiveOrder {
            order_id: "ob-run-idem-1".to_string(),
            slot_id: 1,
            strategy: "demo-strat".to_string(),
            ticker: "KXBTC15M-IDEM1".to_string(),
            series: "KXBTC15M".to_string(),
            mode: "shadow".to_string(),
            execution_mode: "maker".to_string(),
            action: "buy".to_string(),
            side: "yes".to_string(),
            limit_cents: 55,
            contracts: 4,
            filled_contracts: 0,
            queue_ahead_initial: 10,
            queue_ahead_remaining: 10,
            posted_at: 1_800_000_000,
            close_epoch: 1_800_000_900,
            live_order_id: None,
            status: "resting".to_string(),
            replace_count: 0,
        };
        {
            let mut st = runner.state.lock().unwrap();
            st.record_fill(&order, 55, 0.0, 1.0, 0.02, 1_800_000_005);
            st.record_fill(&order, 55, 0.0, 1.0, 0.02, 1_800_000_006);
            // Cancel on an already-filled order_id must be ignored.
            st.emit_quote_canceled(&order, 1_800_000_007, "late_cancel");
            assert_eq!(st.total_fills, 1);
            assert_eq!(st.filled_positions.len(), 1);
        }

        let cancel_order = VirtualOrLiveOrder {
            order_id: "ob-run-idem-2".to_string(),
            ticker: "KXBTC15M-IDEM2".to_string(),
            ..order
        };
        {
            let mut st = runner.state.lock().unwrap();
            st.emit_quote_canceled(&cancel_order, 1_800_000_010, "first_cancel");
            st.emit_quote_canceled(&cancel_order, 1_800_000_011, "second_cancel");
            st.record_fill(&cancel_order, 55, 0.0, 1.0, 0.02, 1_800_000_012);
            assert_eq!(st.total_fills, 1);
        }
        let ledger = runner.ledger_query(50, None, None);
        assert_eq!(
            ledger.entries.iter().filter(|e| e.order_id == "ob-run-idem-1").count(),
            1
        );
        assert_eq!(
            ledger.entries.iter().filter(|e| e.order_id == "ob-run-idem-2").count(),
            1
        );

        // Zero-contract guard in resolve_taker_execution.
        let ob = OrderbookStore::new(PathBuf::new());
        let (_, q0, _, _, used0) = resolve_taker_execution(&ob, "KXBTC15M-IDEM1", "yes", 0, 50, 55, 55);
        assert_eq!(q0, 0);
        assert!(!used0);

        // Queue depletion bounds with NaN/Inf/negative bid sizes.
        assert_eq!(sanitize_non_neg_size(f32::NAN), 0.0);
        assert_eq!(sanitize_non_neg_size(f32::INFINITY), 0.0);
        assert_eq!(sanitize_non_neg_size(-12.5), 0.0);
        assert_eq!(sanitize_non_neg_size(42.0), 42.0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn live_reconcile_partial_fills_real_prices_fees_and_stale_gate() {
        let dir = std::env::temp_dir().join(format!("ob-sr-reconcile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data_dir = dir.join("data");
        let strat_dir = dir.join("strategies");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&strat_dir).unwrap();

        let runner = ShadowRunner::load(&data_dir, 500.0);
        runner
            .configure(
                RunnerConfigRequest {
                    slots: vec![RunnerSlotConfig {
                        slot_id: 1,
                        strategy: "demo-strat".to_string(),
                        series: vec!["KXBTC15M".to_string()],
                        max_contracts: 10,
                        execution_mode: "maker".to_string(),
                        auto_pause_drawdown_dollars: 50.0,
                        enabled: true,
                        reprice_cooldown_secs: 0,
                        max_replaces_per_window: 0,
                        take_profit_cents: None,
                        stop_loss_cents: None,
                        exit_before_close_secs: 0,
                    }],
                    cancel_before_close_secs: 30,
                    bankroll_dollars: 500.0,
                    tick_interval_ms: Some(100),
                },
                true,
                false,
            )
            .unwrap();
        runner.start("live_auto", true, true, false).unwrap();

        {
            let mut st = runner.state.lock().unwrap();
            st.open_quotes.push(RestingQuote {
                order: VirtualOrLiveOrder {
                    order_id: "cid-live-1".to_string(),
                    slot_id: 1,
                    strategy: "demo-strat".to_string(),
                    ticker: "KXBTC15M-REC1".to_string(),
                    series: "KXBTC15M".to_string(),
                    mode: "live".to_string(),
                    execution_mode: "maker".to_string(),
                    action: "buy".to_string(),
                    side: "yes".to_string(),
                    limit_cents: 60,
                    contracts: 10,
                    filled_contracts: 0,
                    queue_ahead_initial: 0,
                    queue_ahead_remaining: 0,
                    posted_at: 1_800_000_000,
                    close_epoch: 1_800_000_900,
                    live_order_id: Some("kalshi-ord-1".to_string()),
                    status: "resting".to_string(),
                    replace_count: 0,
                },
                last_bid_size: 0.0,
                last_bid_cents: 0,
                last_l2_updated_ms: 0,
                last_l2_trade_ms: None,
                last_replaced_at: 1_800_000_000,
                replay_mode: false,
            });
        }

        // Partial fill #1: 4 contracts filled at 57c (price improvement -3.0c), maker fee 0.0175*0.57*0.43*4.
        let partial_ord = kalshi_live::KalshiOrder {
            order_id: "kalshi-ord-1".to_string(),
            client_order_id: Some("cid-live-1".to_string()),
            ticker: "KXBTC15M-REC1".to_string(),
            status: "resting".to_string(),
            action: "buy".to_string(),
            side: "yes".to_string(),
            order_type: "limit".to_string(),
            yes_price: Some(60),
            no_price: None,
            initial_count: 10,
            remaining_count: 6,
            fill_count: 4,
            created_time: None,
        };
        let fill_1 = kalshi_live::KalshiFill {
            trade_id: "tr-part-1".to_string(),
            order_id: "kalshi-ord-1".to_string(),
            ticker: "KXBTC15M-REC1".to_string(),
            action: "buy".to_string(),
            side: "yes".to_string(),
            count: 4,
            yes_price: Some(57),
            no_price: None,
            is_taker: Some(false),
            fee_cost_dollars: None,
            created_time: None,
            ts: Some(1_800_000_010),
        };
        let rem_open = runner.reconcile_live_data(&[partial_ord], &[fill_1.clone()], &[], 1_800_000_011);
        assert_eq!(rem_open.len(), 1);
        assert_eq!(rem_open[0].0, "kalshi-ord-1");
        {
            let st = runner.state.lock().unwrap();
            assert_eq!(st.open_quotes.len(), 1);
            assert_eq!(st.open_quotes[0].order.filled_contracts, 4);
            assert_eq!(st.open_quotes[0].order.status, "partial");
        }

        // Re-running with same trade_id is idempotent; adding partial fill #2 (6 @ 59c taker) completes it,
        // and settlement grades all 10 contracts!
        let exec_ord = kalshi_live::KalshiOrder {
            order_id: "kalshi-ord-1".to_string(),
            client_order_id: Some("cid-live-1".to_string()),
            ticker: "KXBTC15M-REC1".to_string(),
            status: "executed".to_string(),
            action: "buy".to_string(),
            side: "yes".to_string(),
            order_type: "limit".to_string(),
            yes_price: Some(60),
            no_price: None,
            initial_count: 10,
            remaining_count: 0,
            fill_count: 10,
            created_time: None,
        };
        let fill_2 = kalshi_live::KalshiFill {
            trade_id: "tr-part-2".to_string(),
            order_id: "kalshi-ord-1".to_string(),
            ticker: "KXBTC15M-REC1".to_string(),
            action: "buy".to_string(),
            side: "yes".to_string(),
            count: 6,
            yes_price: Some(59),
            no_price: None,
            is_taker: Some(true),
            fee_cost_dollars: Some(0.11),
            created_time: None,
            ts: Some(1_800_000_020),
        };
        let settle = kalshi_live::KalshiSettlement {
            ticker: "KXBTC15M-REC1".to_string(),
            event_ticker: None,
            market_result: "yes".to_string(),
            yes_count: 10,
            yes_total_cost: 582,
            no_count: 0,
            no_total_cost: 0,
            revenue: 1000,
            value: None,
            fee_cost_dollars: 0.0,
            settled_time: None,
        };
        let rem_open2 = runner.reconcile_live_data(
            &[exec_ord],
            &[fill_1, fill_2],
            &[settle],
            1_800_000_030,
        );
        assert!(rem_open2.is_empty());

        let ledger = runner.ledger_query(20, None, None);
        let fills: Vec<&LedgerEvent> = ledger
            .entries
            .iter()
            .filter(|e| e.event == "OrderFilled")
            .collect();
        assert_eq!(fills.len(), 2);
        assert!(fills.iter().any(|e| e.fill_cents == Some(57) && e.contracts == 4 && e.slippage_cents == Some(-3.0)));
        assert!(fills.iter().any(|e| e.fill_cents == Some(59) && e.contracts == 6 && e.fee_dollars == Some(0.11)));
        let settles: Vec<&LedgerEvent> = ledger
            .entries
            .iter()
            .filter(|e| e.event == "WindowSettled")
            .collect();
        assert_eq!(settles.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cycle3_hardening_sync_from_allocator_zero_bankroll_and_all_quarantined() {
        let dir = std::env::temp_dir().join(format!("ob-runner-c3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 1. Corrupt runner_state.json boots fail-closed (Stopped, never LiveAuto)
        std::fs::write(state_path(&dir), r#"{"mode":"live_auto","slots":[{"#).unwrap();
        let runner = ShadowRunner::load(&dir, 500.0);
        let boot_snap = runner.status(false, false);
        assert_eq!(boot_snap.mode, "stopped");
        assert!(!boot_snap.live_armed);

        // 2. Tampered mode="live_auto" in valid JSON still boots Stopped and sanitizes disk
        std::fs::write(
            state_path(&dir),
            r#"{"mode":"live_auto","tick_interval_ms":250,"cancel_before_close_secs":30,"bankroll_dollars":500.0,"slots":[]}"#,
        )
        .unwrap();
        let runner2 = ShadowRunner::load(&dir, 500.0);
        let boot_snap2 = runner2.status(false, false);
        assert_eq!(boot_snap2.mode, "stopped");
        assert!(!boot_snap2.live_armed);

        // Build a baseline AutoAllocatorReport with one active winner
        let ctx = crate::tournament::TournamentTapeContext {
            strategies: Vec::new(),
            windows_settled_total: 0,
            windows_settled_is: 0,
            windows_settled_oos: 0,
            windows_conflicted: 0,
            split_epoch: None,
            observed_series: vec!["KXBTC15M".to_string()],
            entries: Vec::new(),
            window_vol_scores: HashMap::new(),
            window_spread_cents: HashMap::new(),
            window_depth_contracts: HashMap::new(),
        };
        let rails = risk_rails::RiskRailsConfig::default();
        let mut rep = crate::auto_allocator::compute_auto_allocator_report_with_audit(
            &ctx,
            &crate::auto_allocator::AutoAllocatorRequest::default(),
            &rails,
            false,
            false,
            None,
            None,
        );
        let dummy_stress = crate::auto_allocator::StressProfile {
            extra_slippage_0c_cents: 2.0,
            extra_slippage_1c_cents: 1.0,
            extra_slippage_2c_cents: 0.0,
            folds_positive: 3,
            folds_total: 3,
            worst_fold_expectancy_cents: Some(1.0),
            stress_resilient: true,
        };
        rep.active_winners.push(crate::auto_allocator::AllocatedWinner {
            strategy: "winner-a".to_string(),
            side: "yes".to_string(),
            execution_mode: "maker".to_string(),
            is_net_expectancy_cents: Some(2.0),
            oos_trades: 20,
            oos_win_rate: 0.6,
            oos_avg_entry_price: 0.50,
            taker_fee_cents: 1.75,
            maker_fee_cents: 0.44,
            gross_expectancy_cents: 3.0,
            fee_drag_cents: 0.44,
            oos_net_expectancy_cents: 2.56,
            full_kelly_pct: 10.0,
            breakeven_fill_rate_pct: 50.0,
            regime_throttle_mult: 1.0,
            execution_throttle_mult: 1.0,
            execution_status: "active".to_string(),
            allocated_pct: 10.0,
            allocated_dollars: 50.0,
            rail_clamped_contracts: 5,
            max_abs_corr: None,
            status: "ACTIVE_WINNER".to_string(),
            stress: dummy_stress,
        });

        // Put runner into LiveAuto transiently to verify sync_from_allocator NEVER leaves it LiveAuto
        runner2.start("live_auto", true, true, false).unwrap();
        let q_empty = crate::execution_audit::QuarantineStateFile::default();

        // 3. Bankroll == 0.0 -> leaves slots empty, bankroll 0.0, and forces mode != live_auto
        let (slots0, _, snap0) = runner2
            .sync_from_allocator(&rep, &rails, &q_empty, Some(0.0), None, false, false)
            .unwrap();
        assert!(slots0.is_empty(), "bankroll=0 must leave synced slots empty");
        assert!(snap0.active_slots.is_empty());
        assert_eq!(snap0.bankroll_dollars, 0.0);
        assert_ne!(snap0.mode, "live_auto");
        assert!(!snap0.live_armed);

        // Report starting_bankroll == 0.0 with no override -> also leaves slots empty
        let mut rep_zero = rep.clone();
        rep_zero.starting_bankroll = 0.0;
        let (slots0b, _, snap0b) = runner2
            .sync_from_allocator(&rep_zero, &rails, &q_empty, None, None, false, false)
            .unwrap();
        assert!(slots0b.is_empty());
        assert!(snap0b.active_slots.is_empty());
        assert_ne!(snap0b.mode, "live_auto");

        // 4. All strategies quarantined -> leaves slots empty, excludes winner-a, never touches LiveAuto
        let mut q_all = crate::execution_audit::QuarantineStateFile::default();
        q_all.strategies.insert(
            "winner-a".to_string(),
            crate::execution_audit::StrategyQuarantineEntry {
                status: "quarantined".to_string(),
                throttle_mult: 0.0,
                reason: "adverse markout".to_string(),
                ..Default::default()
            },
        );
        runner2.start("live_auto", true, true, false).unwrap();
        let (slots_q, excluded_q, snap_q) = runner2
            .sync_from_allocator(&rep, &rails, &q_all, Some(500.0), None, false, false)
            .unwrap();
        assert!(slots_q.is_empty(), "all-quarantined must leave slots empty");
        assert!(snap_q.active_slots.is_empty());
        assert!(excluded_q.contains(&"winner-a".to_string()));
        assert_ne!(snap_q.mode, "live_auto");
        assert!(!snap_q.live_armed);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn v033_runner_reprice_cooldown_inventory_and_tp_sl_exit_lifecycle() {
        let dir = std::env::temp_dir().join(format!("ob-sr-e1-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data_dir = dir.join("data");
        let strat_dir = dir.join("strategies");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&strat_dir).unwrap();

        // v2 strategy that quotes yes_bid and tracks inventory via ctx.slot_*
        std::fs::write(
            strat_dir.join("e1_maker_v2.rn"),
            r#"
pub fn name() { "e1-maker-v2" }
pub fn side() { "yes" }
pub fn describe() { "Active maker quoting yes_bid when flat." }
pub fn evaluate(ctx) {
    if ctx.slot_position_contracts > 0 {
        return None;
    }
    let lc = (ctx.yes_bid * 100.0).round() as i64;
    if lc >= 1 && lc <= 98 {
        return Some(#{ enter: true, side: "yes", limit_cents: lc, post_only: true });
    }
    None
}
"#,
        )
        .unwrap();

        let close_ts = 1_800_000_900i64;
        let close_iso = iso_z(close_ts);
        let snap1 = serde_json::json!({
            "fetched_at": 1_800_000_000i64,
            "ticker": "KXBTC15M-E1",
            "event_ticker": "KXBTC15M-E1EV",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.45,
            "yes_ask": 0.55,
            "no_bid": 0.45,
            "no_ask": 0.55,
            "last_price": 0.50,
            "yes_bid_size": 10.0,
            "no_bid_size": 10.0
        });
        std::fs::write(data_dir.join("snapshots.jsonl"), format!("{snap1}\n")).unwrap();

        let app = App::new(data_dir.clone(), strat_dir, 500.0, 60);
        app.runner
            .configure(
                RunnerConfigRequest {
                    slots: vec![RunnerSlotConfig {
                        slot_id: 1,
                        strategy: "e1-maker-v2".to_string(),
                        series: vec!["KXBTC15M".to_string()],
                        max_contracts: 5,
                        execution_mode: "maker".to_string(),
                        auto_pause_drawdown_dollars: 50.0,
                        enabled: true,
                        reprice_cooldown_secs: 10,
                        max_replaces_per_window: 2,
                        take_profit_cents: Some(15),
                        stop_loss_cents: Some(10),
                        exit_before_close_secs: 60,
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
        let st1 = app.runner.status(false, false);
        assert_eq!(st1.open_quotes.len(), 1);
        assert_eq!(st1.open_quotes[0].limit_cents, 45);
        assert_eq!(st1.open_quotes[0].action, "buy");
        assert_eq!(st1.open_quotes[0].replace_count, 0);

        // Tick 2 at +5s (inside 10s cooldown) with yes_bid=0.47 -> MUST NOT reprice yet!
        let snap2 = serde_json::json!({
            "fetched_at": 1_800_000_005i64,
            "ticker": "KXBTC15M-E1",
            "event_ticker": "KXBTC15M-E1EV",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.47,
            "yes_ask": 0.55,
            "no_bid": 0.45,
            "no_ask": 0.53,
            "last_price": 0.50,
            "yes_bid_size": 10.0,
            "no_bid_size": 10.0
        });
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(data_dir.join("snapshots.jsonl"))
            .unwrap();
        writeln!(f, "{snap2}").unwrap();
        app.runner.step_once(&app, &mut cached);
        let st2 = app.runner.status(false, false);
        assert_eq!(st2.open_quotes[0].limit_cents, 45);
        assert_eq!(st2.open_quotes[0].replace_count, 0);

        // Tick 3 at +12s (past 10s cooldown) with yes_bid=0.48 -> reprices to 48c, replace_count=1!
        let snap3 = serde_json::json!({
            "fetched_at": 1_800_000_012i64,
            "ticker": "KXBTC15M-E1",
            "event_ticker": "KXBTC15M-E1EV",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.48,
            "yes_ask": 0.55,
            "no_bid": 0.45,
            "no_ask": 0.52,
            "last_price": 0.50,
            "yes_bid_size": 10.0,
            "no_bid_size": 10.0
        });
        writeln!(f, "{snap3}").unwrap();
        app.runner.step_once(&app, &mut cached);
        let st3 = app.runner.status(false, false);
        assert_eq!(st3.open_quotes[0].limit_cents, 48);
        assert_eq!(st3.open_quotes[0].replace_count, 1);

        // Tick 4 at +25s -> strict cross yes_ask=0.47 fills the 48c resting quote!
        let snap4 = serde_json::json!({
            "fetched_at": 1_800_000_025i64,
            "ticker": "KXBTC15M-E1",
            "event_ticker": "KXBTC15M-E1EV",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.46,
            "yes_ask": 0.47,
            "no_bid": 0.53,
            "no_ask": 0.54,
            "last_price": 0.47,
            "yes_bid_size": 10.0,
            "no_bid_size": 10.0
        });
        writeln!(f, "{snap4}").unwrap();
        app.runner.step_once(&app, &mut cached);
        let st4 = app.runner.status(false, false);
        assert!(st4.open_quotes.is_empty());
        assert_eq!(st4.active_slots[0].filled_positions_count, 1);

        // Verify TickContext inventory injection on filled position
        {
            let st_lock = app.runner.state.lock().unwrap();
            let ctx = build_slot_tick_context(
                &st_lock,
                1,
                "KXBTC15M-E1",
                "KXBTC15M",
                800,
                0.65,
                0.35,
                63,
                34,
                None,
                None,
                None,
                true,
            )
            .unwrap();
            assert_eq!(ctx.slot_position_contracts, 5);
            assert!((ctx.slot_avg_entry_cents - 48.0).abs() < 1e-9);
            assert!((ctx.slot_unrealized_pnl_cents - 75.0).abs() < 1e-9);
            assert_eq!(ctx.slot_resting_cents, 0);
        }

        // Tick 5 at +40s -> yes_bid=0.63 (+15c >= take_profit_cents=15) -> triggers PositionExited!
        let snap5 = serde_json::json!({
            "fetched_at": 1_800_000_040i64,
            "ticker": "KXBTC15M-E1",
            "event_ticker": "KXBTC15M-E1EV",
            "series": "KXBTC15M",
            "close_time": close_iso,
            "status": "open",
            "result": "",
            "yes_bid": 0.63,
            "yes_ask": 0.65,
            "no_bid": 0.35,
            "no_ask": 0.37,
            "last_price": 0.63,
            "yes_bid_size": 10.0,
            "no_bid_size": 10.0
        });
        writeln!(f, "{snap5}").unwrap();
        app.runner.step_once(&app, &mut cached);

        let st5 = app.runner.status(false, false);
        assert_eq!(st5.active_slots[0].filled_positions_count, 0);
        assert!(st5.stats.realized_pnl_dollars > 0.6);

        let ledger = app.runner.ledger_query(50, None, None);
        assert!(
            ledger.entries.iter().any(|e| e.event == "OrderAmended" && e.limit_cents == 48),
            "expected OrderAmended in ledger: {:?}",
            ledger.entries
        );
        assert!(
            ledger.entries.iter().any(|e| e.event == "PositionExited" && e.reason.as_deref() == Some("take_profit")),
            "expected PositionExited(take_profit) in ledger: {:?}",
            ledger.entries
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
