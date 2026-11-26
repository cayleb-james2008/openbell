//! Stateful Feature-Rich Strategy Runtime v2 & Atomic Confined Library CRUD.
//!
//! Strictly paper-only. Provides:
//! - Strictly causal `TickContext` construction from `ticks <= current_tick` and
//!   IS-only PAV isotonic recalibration blocks (zero lookahead).
//! - Dual-version Rune script runtime: backward-compatible v1 (`allow_entry` /
//!   `entry_quote`) AND v2 (`pub fn evaluate(ctx)` returning `Option` or
//!   `#{ enter, side, limit_cents, post_only }`).
//! - 1-bar causal lookahead canary check verifying `evaluate(ctx)` at tick `t`
//!   produces bit-identical `TickContext` and identical decision whether future
//!   ticks `> t` are mutated or stripped.
//! - Atomic CWE-22 / CWE-59 confined CRUD inside `strategies_dir` with
//!   `check_paper_only_source` + compile + boundary probes + causal canary.

#![allow(dead_code)]

use crate::analytics::{check_paper_only_source, is_valid_whole_cent};
use crate::recalibration::{evaluate_pav, PavBlock};
use crate::{
    fee, is_budget_halt, is_confined_regular_file, parse_iso_z, Snapshot,
    STRATEGY_BUDGET, STRATEGY_MEMORY_BYTES,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static TEMP_FILE_SEQ: AtomicU64 = AtomicU64::new(1);

/// Strategy script ABI version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StrategyVersion {
    V1,
    V2,
}

impl StrategyVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            StrategyVersion::V1 => "v1",
            StrategyVersion::V2 => "v2",
        }
    }
}

/// Strictly causal market + microstructure + IS-PAV context passed to v2
/// `pub fn evaluate(ctx)` scripts. Every field at tick `t` depends exclusively
/// on `ticks[..=t]` and IS-only PAV blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TickContext {
    pub yes_bid: f64,
    pub yes_ask: f64,
    pub no_bid: f64,
    pub no_ask: f64,
    pub secs_to_close: i64,
    pub spread_cents: f64,
    pub yes_bid_size: f64,
    pub no_bid_size: f64,
    pub book_imbalance: f64,
    pub tick_index: i64,
    pub open_yes_ask: f64,
    pub prev_yes_ask: f64,
    pub high_yes_ask: f64,
    pub low_yes_ask: f64,
    pub delta_60s_cents: f64,
    pub window_vol_cents: f64,
    pub pav_yes_prob: f64,
    pub pav_ev_yes_cents: f64,
    pub pav_ev_no_cents: f64,
    pub series: String,
}

impl TickContext {
    /// Convert this `TickContext` into a Rune dynamic `Object` so scripts can
    /// read `ctx.yes_ask`, `ctx.pav_ev_yes_cents`, `ctx.book_imbalance`, etc.
    pub fn to_rune_object(&self) -> Result<rune::runtime::Object, String> {
        let mut obj = rune::runtime::Object::with_capacity(20)
            .map_err(|e| format!("rune object alloc failed: {e}"))?;
        fn insert_val<T: rune::runtime::ToValue>(
            obj: &mut rune::runtime::Object,
            k: &str,
            v: T,
        ) -> Result<(), String> {
            let rk = rune::alloc::String::try_from(k)
                .map_err(|e| format!("rune key alloc failed: {e}"))?;
            obj.insert_value(rk, v)
                .into_result()
                .map_err(|e| format!("rune insert {k} failed: {e}"))
        }
        insert_val(&mut obj, "yes_bid", self.yes_bid)?;
        insert_val(&mut obj, "yes_ask", self.yes_ask)?;
        insert_val(&mut obj, "no_bid", self.no_bid)?;
        insert_val(&mut obj, "no_ask", self.no_ask)?;
        insert_val(&mut obj, "secs_to_close", self.secs_to_close)?;
        insert_val(&mut obj, "spread_cents", self.spread_cents)?;
        insert_val(&mut obj, "yes_bid_size", self.yes_bid_size)?;
        insert_val(&mut obj, "no_bid_size", self.no_bid_size)?;
        insert_val(&mut obj, "book_imbalance", self.book_imbalance)?;
        insert_val(&mut obj, "tick_index", self.tick_index)?;
        insert_val(&mut obj, "open_yes_ask", self.open_yes_ask)?;
        insert_val(&mut obj, "prev_yes_ask", self.prev_yes_ask)?;
        insert_val(&mut obj, "high_yes_ask", self.high_yes_ask)?;
        insert_val(&mut obj, "low_yes_ask", self.low_yes_ask)?;
        insert_val(&mut obj, "delta_60s_cents", self.delta_60s_cents)?;
        insert_val(&mut obj, "window_vol_cents", self.window_vol_cents)?;
        insert_val(&mut obj, "pav_yes_prob", self.pav_yes_prob)?;
        insert_val(&mut obj, "pav_ev_yes_cents", self.pav_ev_yes_cents)?;
        insert_val(&mut obj, "pav_ev_no_cents", self.pav_ev_no_cents)?;
        let rv = rune::alloc::String::try_from(self.series.as_str())
            .map_err(|e| format!("rune string alloc failed: {e}"))?;
        insert_val(&mut obj, "series", rv)?;
        Ok(obj)
    }
}

/// Online strictly-causal state accumulator for a single market window.
/// Updates incrementally as each tick arrives; never looks ahead.
#[derive(Debug, Clone, Default)]
pub struct CausalFeatureTracker {
    tick_count: i64,
    open_yes_ask: Option<f64>,
    prev_yes_ask: Option<f64>,
    high_yes_ask: f64,
    low_yes_ask: f64,
    history_60s: VecDeque<(i64, f64)>,
    vol_mean_cents: f64,
    vol_m2: f64,
    cached_close_str: Option<String>,
    cached_close_epoch: Option<i64>,
}

impl CausalFeatureTracker {
    pub fn new() -> Self {
        Self {
            tick_count: 0,
            open_yes_ask: None,
            prev_yes_ask: None,
            high_yes_ask: f64::NEG_INFINITY,
            low_yes_ask: f64::INFINITY,
            history_60s: VecDeque::new(),
            vol_mean_cents: 0.0,
            vol_m2: 0.0,
            cached_close_str: None,
            cached_close_epoch: None,
        }
    }

    /// Advance the causal window state by one snapshot and emit its `TickContext`
    /// if the snapshot has valid whole-cent asks and a parseable `close_time`.
    pub fn push_tick(&mut self, snap: &Snapshot, pav_blocks: &[PavBlock]) -> Option<TickContext> {
        let (Some(ya), Some(na)) = (snap.yes_ask.get(), snap.no_ask.get()) else {
            return None;
        };
        if !is_valid_whole_cent(ya) || !is_valid_whole_cent(na) {
            return None;
        }
        let close_epoch = if self.cached_close_str.as_deref() == Some(snap.close_time.as_str()) {
            self.cached_close_epoch
        } else {
            let parsed = parse_iso_z(&snap.close_time);
            self.cached_close_str = Some(snap.close_time.to_string());
            self.cached_close_epoch = parsed;
            parsed
        }?;
        let secs_to_close = close_epoch - snap.fetched_at;

        let yes_bid = snap
            .yes_bid
            .get()
            .filter(|v| v.is_finite() && *v >= 0.01 && *v <= 0.99)
            .unwrap_or_else(|| (1.0 - na).clamp(0.01, 0.99));
        let no_bid = snap
            .no_bid
            .get()
            .filter(|v| v.is_finite() && *v >= 0.01 && *v <= 0.99)
            .unwrap_or_else(|| (1.0 - ya).clamp(0.01, 0.99));
        let spread_cents = ((ya - yes_bid) * 100.0).max(0.0);

        let yes_bid_size = if snap.yes_bid_size.is_finite() && snap.yes_bid_size >= 0.0 {
            snap.yes_bid_size as f64
        } else {
            0.0
        };
        let no_bid_size = if snap.no_bid_size.is_finite() && snap.no_bid_size >= 0.0 {
            snap.no_bid_size as f64
        } else {
            0.0
        };
        let total_size = yes_bid_size + no_bid_size;
        let book_imbalance = if total_size > 0.0 {
            ((yes_bid_size - no_bid_size) / total_size).clamp(-1.0, 1.0)
        } else {
            0.0
        };

        let tick_index = self.tick_count;
        let open_yes_ask = match self.open_yes_ask {
            Some(o) => o,
            None => {
                self.open_yes_ask = Some(ya);
                ya
            }
        };
        let prev_yes_ask = self.prev_yes_ask.unwrap_or(ya);
        self.prev_yes_ask = Some(ya);

        if ya > self.high_yes_ask {
            self.high_yes_ask = ya;
        }
        if ya < self.low_yes_ask {
            self.low_yes_ask = ya;
        }

        // Causal 60s delta: latest prior tick <= current.fetched_at - 60,
        // else earliest observed tick in this window so far.
        self.history_60s.push_back((snap.fetched_at, ya));
        let target_ts = snap.fetched_at - 60;
        let mut ref_ya = self.history_60s.front().map(|x| x.1).unwrap_or(ya);
        let mut latest_old_idx: Option<usize> = None;
        for (idx, &(ts, val)) in self.history_60s.iter().enumerate() {
            if ts <= target_ts {
                ref_ya = val;
                latest_old_idx = Some(idx);
            } else {
                break;
            }
        }
        // Keep only the latest tick <= target_ts (and everything newer) so memory stays bounded.
        if let Some(keep_idx) = latest_old_idx {
            for _ in 0..keep_idx {
                self.history_60s.pop_front();
            }
        }
        let delta_60s_cents = (ya - ref_ya) * 100.0;

        // Welford online population standard deviation of yes_ask in cents.
        self.tick_count += 1;
        let n_f = self.tick_count as f64;
        let ya_cents = ya * 100.0;
        let delta = ya_cents - self.vol_mean_cents;
        self.vol_mean_cents += delta / n_f;
        let delta2 = ya_cents - self.vol_mean_cents;
        self.vol_m2 += delta * delta2;
        let window_vol_cents = if self.tick_count >= 2 {
            (self.vol_m2 / n_f).max(0.0).sqrt()
        } else {
            0.0
        };

        // IS-only PAV recalibrated YES probability & net EV after Kalshi taker fee.
        let pav_yes_prob = if pav_blocks.is_empty() {
            ya.clamp(0.01, 0.99)
        } else {
            evaluate_pav(pav_blocks, ya).clamp(0.01, 0.99)
        };
        let pav_ev_yes_cents = (pav_yes_prob - ya - fee(ya)) * 100.0;
        let pav_ev_no_cents = ((1.0 - pav_yes_prob) - na - fee(na)) * 100.0;

        Some(TickContext {
            yes_bid,
            yes_ask: ya,
            no_bid,
            no_ask: na,
            secs_to_close,
            spread_cents,
            yes_bid_size,
            no_bid_size,
            book_imbalance,
            tick_index,
            open_yes_ask,
            prev_yes_ask,
            high_yes_ask: self.high_yes_ask,
            low_yes_ask: self.low_yes_ask,
            delta_60s_cents,
            window_vol_cents,
            pav_yes_prob,
            pav_ev_yes_cents,
            pav_ev_no_cents,
            series: snap.series.to_string(),
        })
    }
}

/// Build the full sequence of strictly causal `TickContext` values for a window.
pub fn build_causal_tick_contexts(
    snaps: &[Snapshot],
    pav_blocks: &[PavBlock],
) -> Vec<TickContext> {
    let mut tracker = CausalFeatureTracker::new();
    let mut out = Vec::with_capacity(snaps.len());
    for snap in snaps {
        if let Some(ctx) = tracker.push_tick(snap, pav_blocks) {
            out.push(ctx);
        }
    }
    out
}

/// Parsed v2 entry decision emitted by `pub fn evaluate(ctx)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct V2Decision {
    pub enter: bool,
    pub side: String,
    pub limit_cents: u8,
    pub post_only: bool,
}

fn parse_limit_cents_value(v: &rune::Value) -> Result<u8, String> {
    if let Ok(i) = rune::from_value::<i64>(v.clone()) {
        if (1..=99).contains(&i) {
            return Ok(i as u8);
        }
        return Err(format!("limit_cents must be in 1..=99, got {i}"));
    }
    if let Ok(f) = rune::from_value::<f64>(v.clone()) {
        if f.is_finite() && (1.0..=99.0).contains(&f) {
            let rounded = f.round();
            if (f - rounded).abs() < 1e-6 {
                return Ok(rounded as u8);
            }
        }
        return Err(format!("limit_cents must be whole cents in 1..=99, got {f}"));
    }
    Err("limit_cents must be an integer in 1..=99".to_string())
}

fn parse_v2_object(
    obj: &rune::runtime::Object,
    ctx: &TickContext,
    default_side: &str,
    require_enter_field: bool,
) -> Result<Option<V2Decision>, String> {
    let enter = match obj.get("enter") {
        Some(v) => rune::from_value::<bool>(v.clone())
            .map_err(|e| format!("`enter` must be a boolean: {e}"))?,
        None => {
            if require_enter_field {
                return Err("evaluate(ctx) map must contain boolean `enter`".to_string());
            }
            true
        }
    };
    if !enter {
        return Ok(None);
    }

    let side = match obj.get("side") {
        Some(v) => {
            let s = rune::from_value::<String>(v.clone())
                .map_err(|e| format!("`side` must be a string: {e}"))?;
            if s != "yes" && s != "no" {
                return Err(format!("`side` must be \"yes\" or \"no\", got \"{s}\""));
            }
            s
        }
        None => {
            if default_side == "yes" || default_side == "no" {
                default_side.to_string()
            } else {
                return Err("`side` must be \"yes\" or \"no\"".to_string());
            }
        }
    };

    let limit_cents = match obj.get("limit_cents") {
        Some(v) => parse_limit_cents_value(v)?,
        None => {
            let ask = if side == "yes" { ctx.yes_ask } else { ctx.no_ask };
            let c = (ask * 100.0).round();
            if !(1.0..=99.0).contains(&c) {
                return Err("default limit_cents out of 1..=99".to_string());
            }
            c as u8
        }
    };

    let post_only = match obj.get("post_only") {
        Some(v) => rune::from_value::<bool>(v.clone())
            .map_err(|e| format!("`post_only` must be a boolean: {e}"))?,
        None => false,
    };

    Ok(Some(V2Decision {
        enter: true,
        side,
        limit_cents,
        post_only,
    }))
}

/// Parse the Rune return value from `evaluate(ctx)` supporting both:
/// - `Option<#{ enter, side, limit_cents, post_only }>` (`None` or `Some(map)`)
/// - Direct `#{ enter, side, limit_cents, post_only }` map
pub fn parse_v2_return_value(
    val: rune::Value,
    ctx: &TickContext,
    default_side: &str,
) -> Result<Option<V2Decision>, String> {
    if let Ok(opt) = rune::from_value::<Option<rune::runtime::Object>>(val.clone()) {
        return match opt {
            None => Ok(None),
            Some(obj) => parse_v2_object(&obj, ctx, default_side, false),
        };
    }
    if let Ok(obj) = rune::from_value::<rune::runtime::Object>(val) {
        return parse_v2_object(&obj, ctx, default_side, true);
    }
    Err("evaluate(ctx) must return Option or #{ enter, side, limit_cents, post_only } map".to_string())
}

/// Probe a Rune VM to determine whether it exports `evaluate(ctx)` (v2) vs v1.
pub fn detect_vm_has_evaluate(vm: &mut rune::Vm) -> Result<bool, String> {
    Ok(vm.lookup_function(["evaluate"]).is_ok())
}

/// Evaluate `evaluate(ctx)` on a borrowed Rune VM with instruction budget & memory limit.
pub fn call_evaluate_v2_vm(
    vm: &mut rune::Vm,
    evaluate_hash: rune::Hash,
    ctx: &TickContext,
    default_side: &str,
) -> Result<Option<V2Decision>, (String, bool)> {
    let obj = ctx
        .to_rune_object()
        .map_err(|e| (e, false))?;
    let out = rune::runtime::budget::with(
        STRATEGY_BUDGET,
        rune::alloc::limit::with(STRATEGY_MEMORY_BYTES, || vm.call(evaluate_hash, (obj,))),
    )
    .call();
    match out {
        Ok(val) => parse_v2_return_value(val, ctx, default_side).map_err(|e| (e, false)),
        Err(e) => {
            vm.clear();
            Err((e.to_string(), is_budget_halt(&e)))
        }
    }
}

fn sample_boundary_contexts() -> Vec<TickContext> {
    vec![
        TickContext {
            yes_bid: 0.68,
            yes_ask: 0.70,
            no_bid: 0.30,
            no_ask: 0.32,
            secs_to_close: 240,
            spread_cents: 2.0,
            yes_bid_size: 120.0,
            no_bid_size: 40.0,
            book_imbalance: 0.5,
            tick_index: 5,
            open_yes_ask: 0.64,
            prev_yes_ask: 0.69,
            high_yes_ask: 0.71,
            low_yes_ask: 0.63,
            delta_60s_cents: 4.0,
            window_vol_cents: 2.5,
            pav_yes_prob: 0.74,
            pav_ev_yes_cents: 2.53,
            pav_ev_no_cents: -7.52,
            series: "KXBTC15M".to_string(),
        },
        TickContext {
            yes_bid: 0.49,
            yes_ask: 0.50,
            no_bid: 0.50,
            no_ask: 0.52,
            secs_to_close: 450,
            spread_cents: 1.0,
            yes_bid_size: 50.0,
            no_bid_size: 50.0,
            book_imbalance: 0.0,
            tick_index: 0,
            open_yes_ask: 0.50,
            prev_yes_ask: 0.50,
            high_yes_ask: 0.50,
            low_yes_ask: 0.50,
            delta_60s_cents: 0.0,
            window_vol_cents: 0.0,
            pav_yes_prob: 0.50,
            pav_ev_yes_cents: -1.75,
            pav_ev_no_cents: -3.75,
            series: "KXETH15M".to_string(),
        },
        TickContext {
            yes_bid: 0.01,
            yes_ask: 0.01,
            no_bid: 0.98,
            no_ask: 0.99,
            secs_to_close: 61,
            spread_cents: 0.0,
            yes_bid_size: 10.0,
            no_bid_size: 100.0,
            book_imbalance: -0.8181818181818182,
            tick_index: 10,
            open_yes_ask: 0.15,
            prev_yes_ask: 0.02,
            high_yes_ask: 0.15,
            low_yes_ask: 0.01,
            delta_60s_cents: -8.0,
            window_vol_cents: 4.2,
            pav_yes_prob: 0.02,
            pav_ev_yes_cents: 0.93,
            pav_ev_no_cents: -1.07,
            series: "KXSOL15M".to_string(),
        },
        TickContext {
            yes_bid: 0.97,
            yes_ask: 0.99,
            no_bid: 0.01,
            no_ask: 0.02,
            secs_to_close: 899,
            spread_cents: 2.0,
            yes_bid_size: 200.0,
            no_bid_size: 0.0,
            book_imbalance: 1.0,
            tick_index: 1,
            open_yes_ask: 0.98,
            prev_yes_ask: 0.98,
            high_yes_ask: 0.99,
            low_yes_ask: 0.98,
            delta_60s_cents: 1.0,
            window_vol_cents: 0.5,
            pav_yes_prob: 0.98,
            pav_ev_yes_cents: -1.07,
            pav_ev_no_cents: -0.14,
            series: "KXXRP15M".to_string(),
        },
    ]
}

#[allow(clippy::too_many_arguments)]
fn canary_snapshot(
    ticker: &str,
    series: &str,
    fetched_at: i64,
    close_time: &str,
    ya: f64,
    na: f64,
    yb: f64,
    nb: f64,
    y_sz: f32,
    n_sz: f32,
    result: &str,
) -> Snapshot {
    use crate::{OptPrice, Sym};
    Snapshot {
        fetched_at,
        ticker: Sym::new(ticker),
        event_ticker: Sym::new("KXBTC15M-CANARY"),
        series: Sym::new(series),
        close_time: Sym::new(close_time),
        status: Sym::new(if result.is_empty() { "open" } else { "settled" }),
        result: Sym::new(result),
        yes_bid: OptPrice::from_opt(Some(yb)),
        yes_ask: OptPrice::from_opt(Some(ya)),
        no_bid: OptPrice::from_opt(Some(nb)),
        no_ask: OptPrice::from_opt(Some(na)),
        last_price: OptPrice::from_opt(Some(ya)),
        yes_bid_size: y_sz,
        no_bid_size: n_sz,
    }
}

/// 1-bar causal lookahead canary check:
/// Verifies both that `TickContext` at tick `t` and `evaluate(ctx)` at tick `t`
/// produce identical results whether future ticks `> t` are stripped or mutated.
pub fn verify_causal_lookahead_canary_vm(
    vm: &mut rune::Vm,
    evaluate_hash: rune::Hash,
    default_side: &str,
) -> Result<(), String> {
    let close_time = "2026-11-26T15:15:00Z";
    let base_ts = parse_iso_z(close_time).unwrap_or(1_795_706_100) - 900;
    let base_ticks = vec![
        canary_snapshot("KXBTC15M-CANARY-T", "KXBTC15M", base_ts + 60, close_time, 0.52, 0.50, 0.50, 0.48, 40.0, 35.0, ""),
        canary_snapshot("KXBTC15M-CANARY-T", "KXBTC15M", base_ts + 120, close_time, 0.55, 0.47, 0.53, 0.45, 55.0, 30.0, ""),
        canary_snapshot("KXBTC15M-CANARY-T", "KXBTC15M", base_ts + 180, close_time, 0.58, 0.44, 0.56, 0.42, 70.0, 25.0, ""),
        canary_snapshot("KXBTC15M-CANARY-T", "KXBTC15M", base_ts + 240, close_time, 0.62, 0.40, 0.60, 0.38, 90.0, 20.0, ""),
        canary_snapshot("KXBTC15M-CANARY-T", "KXBTC15M", base_ts + 300, close_time, 0.66, 0.36, 0.64, 0.34, 110.0, 15.0, ""),
        canary_snapshot("KXBTC15M-CANARY-T", "KXBTC15M", base_ts + 360, close_time, 0.63, 0.39, 0.61, 0.37, 80.0, 40.0, ""),
        canary_snapshot("KXBTC15M-CANARY-T", "KXBTC15M", base_ts + 420, close_time, 0.71, 0.31, 0.69, 0.29, 130.0, 18.0, ""),
        canary_snapshot("KXBTC15M-CANARY-T", "KXBTC15M", base_ts + 480, close_time, 0.74, 0.28, 0.72, 0.26, 150.0, 12.0, "yes"),
    ];
    let pav_blocks = vec![
        PavBlock {
            min_p: 0.25,
            max_p: 0.35,
            mean_pred_p: 0.30,
            calibrated_p: 0.28,
            weight: 10.0,
            wins: 2.8,
        },
        PavBlock {
            min_p: 0.65,
            max_p: 0.75,
            mean_pred_p: 0.70,
            calibrated_p: 0.75,
            weight: 12.0,
            wins: 9.0,
        },
    ];

    let orig_ctxs = build_causal_tick_contexts(&base_ticks, &pav_blocks);
    if orig_ctxs.len() != base_ticks.len() {
        return Err("causal canary failed to build baseline contexts".to_string());
    }

    for &t in &[2usize, 4usize, 6usize] {
        // 1. Future ticks > t stripped.
        let stripped_ctxs = build_causal_tick_contexts(&base_ticks[..=t], &pav_blocks);
        // 2. Future ticks > t drastically mutated.
        let mut mutated = base_ticks.clone();
        for (idx, snap) in mutated.iter_mut().enumerate().skip(t + 1) {
            let flipped_ya = if idx % 2 == 0 { 0.99 } else { 0.01 };
            let flipped_na = if idx % 2 == 0 { 0.01 } else { 0.99 };
            *snap = canary_snapshot(
                "KXBTC15M-CANARY-T",
                "KXBTC15M",
                base_ts + 60 * ((idx as i64) + 1),
                close_time,
                flipped_ya,
                flipped_na,
                (flipped_ya - 0.01).max(0.01),
                (flipped_na - 0.01).max(0.01),
                if idx % 2 == 0 { 999.0 } else { 1.0 },
                if idx % 2 == 0 { 1.0 } else { 999.0 },
                "no",
            );
        }
        let mutated_ctxs = build_causal_tick_contexts(&mutated, &pav_blocks);

        if orig_ctxs[t] != stripped_ctxs[t] {
            return Err(format!(
                "causal lookahead canary failed at tick {t}: context changed when future ticks were stripped"
            ));
        }
        if orig_ctxs[t] != mutated_ctxs[t] {
            return Err(format!(
                "causal lookahead canary failed at tick {t}: context changed when future ticks were mutated"
            ));
        }

        let d_orig = call_evaluate_v2_vm(vm, evaluate_hash, &orig_ctxs[t], default_side)
            .map_err(|(e, _)| format!("causal canary evaluate failed at tick {t}: {e}"))?;
        let d_stripped = call_evaluate_v2_vm(vm, evaluate_hash, &stripped_ctxs[t], default_side)
            .map_err(|(e, _)| format!("causal canary stripped evaluate failed at tick {t}: {e}"))?;
        let d_mutated = call_evaluate_v2_vm(vm, evaluate_hash, &mutated_ctxs[t], default_side)
            .map_err(|(e, _)| format!("causal canary mutated evaluate failed at tick {t}: {e}"))?;

        if d_orig != d_stripped || d_orig != d_mutated {
            return Err(format!(
                "causal lookahead canary failed at tick {t}: evaluate(ctx) decision diverged across stripped/mutated future ticks"
            ));
        }
    }
    Ok(())
}

/// Compiled & validated strategy metadata + version detection (v1 or v2).
pub struct ValidatedStrategySource {
    pub name: String,
    pub side: String,
    pub description: Option<String>,
    pub version: StrategyVersion,
    pub warnings: Vec<String>,
    pub diagnostics: String,
    pub causal_canary_passed: bool,
    pub unit: Arc<rune::Unit>,
    pub runtime: Arc<rune::runtime::RuntimeContext>,
}

/// Compile, paper-only check, boundary-probe, and run 1-bar causal canary on a
/// Rune v1 or v2 strategy source string purely in memory.
pub fn validate_strategy_source_full(source: &str) -> Result<ValidatedStrategySource, (Vec<String>, String)> {
    let violations = check_paper_only_source(source);
    if !violations.is_empty() {
        return Err((violations, String::new()));
    }

    let context = rune::Context::with_default_modules()
        .map_err(|e| (vec![format!("rune context error: {e}")], String::new()))?;
    let runtime = Arc::new(
        context
            .runtime()
            .map_err(|e| (vec![format!("rune runtime error: {e}")], String::new()))?,
    );
    let mut sources = rune::Sources::new();
    let src = rune::Source::new("strategy.rn", source)
        .map_err(|e| (vec![format!("invalid source: {e}")], String::new()))?;
    sources
        .insert(src)
        .map_err(|e| (vec![format!("source insert error: {e}")], String::new()))?;

    let mut diagnostics = rune::Diagnostics::new();
    let build_res = rune::prepare(&mut sources)
        .with_context(&context)
        .with_diagnostics(&mut diagnostics)
        .build();

    let diag_text = if !diagnostics.is_empty() {
        diagnostics
            .diagnostics()
            .iter()
            .map(|d| match d {
                rune::diagnostics::Diagnostic::Fatal(f) => f.to_string(),
                rune::diagnostics::Diagnostic::Warning(w) => format!("warning: {w}"),
                other => format!("{other:?}"),
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        String::new()
    };

    let unit = match build_res {
        Ok(u) => Arc::new(u),
        Err(e) => {
            let msg = e.to_string().replace(" (see diagnostics for details)", "");
            return Err((vec![format!("compile error: {msg}")], diag_text));
        }
    };

    let mut vm = rune::Vm::new(runtime.clone(), unit.clone());
    let call_str = |vm: &mut rune::Vm, fn_name: &str| -> Result<String, String> {
        let val = rune::runtime::budget::with(
            STRATEGY_BUDGET,
            rune::alloc::limit::with(STRATEGY_MEMORY_BYTES, || vm.call([fn_name], ())),
        )
        .call()
        .map_err(|e| format!("call `{fn_name}()` failed: {e}"))?;
        rune::from_value::<String>(val)
            .map_err(|e| format!("`{fn_name}()` must return String: {e}"))
    };

    let name = call_str(&mut vm, "name").map_err(|e| (vec![e], diag_text.clone()))?;
    if name.trim().is_empty() || name.len() > 64 {
        return Err((
            vec!["`name()` must return a non-empty string (<= 64 chars)".to_string()],
            diag_text,
        ));
    }

    let side = call_str(&mut vm, "side").map_err(|e| (vec![e], diag_text.clone()))?;
    if side != "yes" && side != "no" {
        return Err((
            vec![format!("`side()` must return \"yes\" or \"no\", got \"{side}\"")],
            diag_text,
        ));
    }

    let description = call_str(&mut vm, "describe").ok();
    let has_v2 = detect_vm_has_evaluate(&mut vm).map_err(|e| (vec![e], diag_text.clone()))?;

    let version = if has_v2 {
        let eval_hash = rune::Hash::type_hash(["evaluate"]);
        for ctx in sample_boundary_contexts() {
            call_evaluate_v2_vm(&mut vm, eval_hash, &ctx, &side)
                .map_err(|(e, _)| (vec![format!("`evaluate(ctx)` failed boundary probe: {e}")], diag_text.clone()))?;
        }
        verify_causal_lookahead_canary_vm(&mut vm, eval_hash, &side)
            .map_err(|e| (vec![e], diag_text.clone()))?;
        StrategyVersion::V2
    } else {
        let allow_hash = rune::Hash::type_hash(["allow_entry"]);
        let probes = [
            (0.70, 0.32, 240i64),
            (0.50, 0.52, 450i64),
            (0.01, 0.99, 61i64),
            (0.99, 0.01, 899i64),
        ];
        for (ya, na, secs) in probes {
            let out = rune::runtime::budget::with(
                STRATEGY_BUDGET,
                rune::alloc::limit::with(STRATEGY_MEMORY_BYTES, || {
                    vm.call(allow_hash, (ya, na, secs))
                }),
            )
            .call()
            .map_err(|e| {
                (
                    vec![format!("`allow_entry({ya}, {na}, {secs})` failed validation probe: {e}")],
                    diag_text.clone(),
                )
            })?;
            rune::from_value::<bool>(out).map_err(|e| {
                (
                    vec![format!("`allow_entry` must return bool: {e}")],
                    diag_text.clone(),
                )
            })?;
        }
        StrategyVersion::V1
    };

    let mut warnings = Vec::new();
    if !diag_text.is_empty() {
        warnings.push(diag_text.clone());
    }

    Ok(ValidatedStrategySource {
        name,
        side,
        description,
        version,
        warnings,
        diagnostics: diag_text,
        causal_canary_passed: true,
        unit,
        runtime,
    })
}

/// Result of a confined atomic save operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveStrategyResult {
    pub ok: bool,
    pub strategy: String,
    pub file: String,
    pub side: String,
    pub version: StrategyVersion,
    pub causal_canary_passed: bool,
    pub warnings: Vec<String>,
}

/// Result of a confined delete operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteStrategyResult {
    pub ok: bool,
    pub deleted: String,
    pub name: String,
    pub file: String,
}

#[derive(Debug)]
pub enum CrudError {
    BadRequest(Vec<String>),
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    Internal(String),
}

impl CrudError {
    pub fn status(&self) -> u16 {
        match self {
            CrudError::BadRequest(_) => 400,
            CrudError::Forbidden(_) => 403,
            CrudError::NotFound(_) => 404,
            CrudError::Conflict(_) => 409,
            CrudError::Internal(_) => 500,
        }
    }

    pub fn errors(&self) -> Vec<String> {
        match self {
            CrudError::BadRequest(v) => v.clone(),
            CrudError::Forbidden(s)
            | CrudError::NotFound(s)
            | CrudError::Conflict(s)
            | CrudError::Internal(s) => vec![s.clone()],
        }
    }

    pub fn message(&self) -> String {
        self.errors().join("; ")
    }
}

/// Validate a strategy slug or filename against CWE-22 path traversal.
/// Rejects slashes, backslashes, `..`, `%`, null bytes, whitespace, and any
/// non-`[a-zA-Z0-9_-]` character (after stripping at most one trailing `.rn`).
pub fn validate_strategy_slug(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("strategy name/slug cannot be empty".to_string());
    }
    if trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains("..")
        || trimmed.contains('%')
        || trimmed.contains('\0')
        || trimmed.contains(':')
    {
        return Err(format!("invalid strategy identifier (path traversal rejected): {trimmed}"));
    }
    let stem = trimmed.strip_suffix(".rn").unwrap_or(trimmed);
    if stem.is_empty() || stem.len() > 64 {
        return Err("strategy slug must be 1..=64 characters".to_string());
    }
    let mut chars = stem.chars();
    let Some(first) = chars.next() else {
        return Err("strategy slug cannot be empty".to_string());
    };
    if !first.is_ascii_alphanumeric() {
        return Err(format!("strategy slug must start with an ASCII letter or digit: {stem}"));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        return Err(format!(
            "strategy slug may only contain [a-zA-Z0-9_-]: {stem}"
        ));
    }
    Ok(stem.to_string())
}

fn ensure_confined_strategies_dir(dir: &Path) -> Result<PathBuf, CrudError> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
                return Err(CrudError::Forbidden(
                    "strategies_dir is a symlink or not a directory".to_string(),
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir)
                .map_err(|err| CrudError::Internal(format!("failed to create strategies_dir: {err}")))?;
        }
        Err(e) => {
            return Err(CrudError::Internal(format!("failed to inspect strategies_dir: {e}")));
        }
    }
    dir.canonicalize()
        .map_err(|e| CrudError::Internal(format!("failed to canonicalize strategies_dir: {e}")))
}

fn find_existing_strategy_file_by_name(
    dir: &Path,
    strategy_name: &str,
) -> Option<(PathBuf, String)> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "rn"))
        .filter(|p| is_confined_regular_file(dir, p))
        .collect();
    paths.sort();
    for path in paths {
        if let Ok(s) = crate::Strategy::load(&path) {
            if s.name == strategy_name {
                let file = path
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                return Some((path, file));
            }
        }
    }
    None
}

/// Atomically validate and save a strategy `.rn` file inside `strategies_dir`
/// with CWE-22/59 confinement, paper-only check, compile, boundary probes, and
/// 1-bar causal canary verification.
pub fn save_strategy_atomic(
    strategies_dir: &Path,
    requested_name_or_file: Option<&str>,
    source: &str,
) -> Result<SaveStrategyResult, CrudError> {
    save_strategy_atomic_with_overwrite(strategies_dir, requested_name_or_file, source, true)
}

/// Atomically validate and save a strategy `.rn` file inside `strategies_dir`,
/// honoring `overwrite` and resolving existing hyphen/underscore library stems.
pub fn save_strategy_atomic_with_overwrite(
    strategies_dir: &Path,
    requested_name_or_file: Option<&str>,
    source: &str,
    overwrite: bool,
) -> Result<SaveStrategyResult, CrudError> {
    let validated = validate_strategy_source_full(source)
        .map_err(|(errs, _)| CrudError::BadRequest(errs))?;

    let canon_dir = ensure_confined_strategies_dir(strategies_dir)?;

    let stem_raw = match requested_name_or_file.map(str::trim).filter(|s| !s.is_empty()) {
        Some(raw) => raw.to_string(),
        None => validated.name.replace(' ', "_"),
    };
    let stem = validate_strategy_slug(&stem_raw)
        .map_err(|e| CrudError::BadRequest(vec![e]))?;
    let mut file_name = format!("{stem}.rn");
    let mut dest_path = canon_dir.join(&file_name);
    if std::fs::symlink_metadata(&dest_path).is_err() {
        let alt_name = format!("{}.rn", stem.replace('-', "_"));
        let alt_path = canon_dir.join(&alt_name);
        if std::fs::symlink_metadata(&alt_path).is_ok() {
            file_name = alt_name;
            dest_path = alt_path;
        }
    }

    // Check symlink confinement on target if it already exists.
    if let Ok(meta) = std::fs::symlink_metadata(&dest_path) {
        if meta.file_type().is_symlink() || !meta.file_type().is_file() {
            return Err(CrudError::Forbidden(format!(
                "refusing to overwrite symlink or non-regular file: {file_name}"
            )));
        }
        let canon_dest = dest_path.canonicalize().map_err(|e| {
            CrudError::Forbidden(format!("failed to canonicalize target path: {e}"))
        })?;
        if !canon_dest.starts_with(&canon_dir) || canon_dest.parent() != Some(canon_dir.as_path()) {
            return Err(CrudError::Forbidden(format!(
                "target path escapes strategies_dir: {file_name}"
            )));
        }
        if !overwrite {
            return Err(CrudError::Conflict(format!(
                "strategy file {file_name} already exists; use Save to overwrite"
            )));
        }
    }

    // Prevent another file in strategies_dir from colliding on `pub fn name()`.
    if let Some((existing_path, existing_file)) =
        find_existing_strategy_file_by_name(&canon_dir, &validated.name)
    {
        if existing_path != dest_path {
            return Err(CrudError::Conflict(format!(
                "strategy name \"{}\" is already defined in {existing_file}",
                validated.name
            )));
        }
    }

    // CWE-59 safe atomic write inside canon_dir using O_CREAT | O_EXCL (`create_new(true)`).
    let seq = TEMP_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(".tmp.{stem}.{}.{seq}.rn", std::process::id());
    let tmp_path = canon_dir.join(&tmp_name);
    let write_res = (|| -> std::io::Result<()> {
        let mut f = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        f.write_all(source.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp_path, &dest_path)?;
        Ok(())
    })();

    if let Err(e) = write_res {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(CrudError::Internal(format!(
            "atomic write failed for {file_name}: {e}"
        )));
    }

    Ok(SaveStrategyResult {
        ok: true,
        strategy: validated.name,
        file: file_name,
        side: validated.side,
        version: validated.version,
        causal_canary_passed: validated.causal_canary_passed,
        warnings: validated.warnings,
    })
}

/// Delete a strategy `.rn` file inside `strategies_dir` with strict CWE-22/59
/// traversal & symlink confinement.
pub fn delete_strategy_confined(
    strategies_dir: &Path,
    raw_name: &str,
) -> Result<DeleteStrategyResult, CrudError> {
    let decoded = percent_decode_strict(raw_name)
        .map_err(|e| CrudError::BadRequest(vec![e]))?;
    // Reject traversal attempts before inspecting the filesystem at all.
    let stem = validate_strategy_slug(&decoded)
        .map_err(|e| CrudError::BadRequest(vec![e]))?;

    let canon_dir = match std::fs::symlink_metadata(strategies_dir) {
        Ok(m) => {
            if m.file_type().is_symlink() || !m.file_type().is_dir() {
                return Err(CrudError::Forbidden(
                    "strategies_dir is a symlink or not a directory".to_string(),
                ));
            }
            strategies_dir
                .canonicalize()
                .map_err(|e| CrudError::Internal(e.to_string()))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(CrudError::NotFound(format!("no such strategy: {decoded}")));
        }
        Err(e) => return Err(CrudError::Internal(e.to_string())),
    };

    let candidate_paths = [
        canon_dir.join(format!("{stem}.rn")),
        canon_dir.join(format!("{}.rn", stem.replace('-', "_"))),
    ];

    let mut target: Option<(PathBuf, String, String)> = None;
    for p in &candidate_paths {
        if let Ok(meta) = std::fs::symlink_metadata(p) {
            if meta.file_type().is_symlink() || !meta.file_type().is_file() {
                return Err(CrudError::Forbidden(
                    "refusing to delete symlink or non-regular file".to_string(),
                ));
            }
            let canon_p = p
                .canonicalize()
                .map_err(|e| CrudError::Forbidden(format!("path confinement error: {e}")))?;
            if !canon_p.starts_with(&canon_dir) || canon_p.parent() != Some(canon_dir.as_path()) {
                return Err(CrudError::Forbidden(
                    "strategy path escapes strategies_dir".to_string(),
                ));
            }
            let file_name = canon_p
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            let sname = crate::Strategy::load(&canon_p)
                .map(|s| s.name)
                .unwrap_or_else(|_| stem.clone());
            target = Some((canon_p, sname, file_name));
            break;
        }
    }

    if target.is_none() {
        let lookup_name = decoded.strip_suffix(".rn").unwrap_or(&decoded);
        if let Some((p, file_name)) = find_existing_strategy_file_by_name(&canon_dir, lookup_name) {
            target = Some((p, lookup_name.to_string(), file_name));
        }
    }

    let Some((target_path, deleted_name, file_name)) = target else {
        return Err(CrudError::NotFound(format!("no such strategy: {decoded}")));
    };

    if !is_confined_regular_file(&canon_dir, &target_path) {
        return Err(CrudError::Forbidden(
            "refusing to delete unconfined or symlinked strategy file".to_string(),
        ));
    }

    std::fs::remove_file(&target_path)
        .map_err(|e| CrudError::Internal(format!("failed to delete {file_name}: {e}")))?;

    Ok(DeleteStrategyResult {
        ok: true,
        deleted: deleted_name.clone(),
        name: deleted_name,
        file: file_name,
    })
}

fn percent_decode_strict(raw: &str) -> Result<String, String> {
    if raw.contains('\0') {
        return Err("null byte rejected in strategy name".to_string());
    }
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err("malformed percent-encoding in strategy name".to_string());
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| "invalid UTF-8 in percent-encoding".to_string())?;
            let byte = u8::from_str_radix(hex, 16)
                .map_err(|_| "invalid hex in percent-encoding".to_string())?;
            // Reject encoded path separators or null bytes immediately.
            if byte == b'/' || byte == b'\\' || byte == 0 {
                return Err("encoded path separator rejected (CWE-22)".to_string());
            }
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "invalid UTF-8 in strategy name".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_snap(ts: i64, ya: f64, na: f64, yb: f64, nb: f64, y_sz: f32, n_sz: f32) -> Snapshot {
        canary_snapshot(
            "KXBTC15M-UNIT",
            "KXBTC15M",
            ts,
            "2026-11-26T15:15:00Z",
            ya,
            na,
            yb,
            nb,
            y_sz,
            n_sz,
            "",
        )
    }

    #[test]
    fn causal_tick_context_is_strictly_invariant_to_future_ticks() {
        let close = parse_iso_z("2026-11-26T15:15:00Z").unwrap();
        let start = close - 900;
        let ticks = vec![
            test_snap(start + 30, 0.50, 0.52, 0.48, 0.50, 20.0, 20.0),
            test_snap(start + 90, 0.54, 0.48, 0.52, 0.46, 40.0, 20.0),
            test_snap(start + 150, 0.58, 0.44, 0.56, 0.42, 60.0, 10.0),
            test_snap(start + 210, 0.61, 0.41, 0.59, 0.39, 80.0, 10.0),
            test_snap(start + 270, 0.40, 0.62, 0.38, 0.60, 5.0, 120.0),
        ];
        let pav = vec![
            PavBlock {
                min_p: 0.35,
                max_p: 0.45,
                mean_pred_p: 0.40,
                calibrated_p: 0.42,
                weight: 10.0,
                wins: 4.2,
            },
            PavBlock {
                min_p: 0.65,
                max_p: 0.75,
                mean_pred_p: 0.70,
                calibrated_p: 0.73,
                weight: 10.0,
                wins: 7.3,
            },
        ];
        let full = build_causal_tick_contexts(&ticks, &pav);
        let stripped = build_causal_tick_contexts(&ticks[..=2], &pav);
        assert_eq!(full[2], stripped[2]);
        assert_eq!(full[2].tick_index, 2);
        assert!((full[2].open_yes_ask - 0.50).abs() < 1e-12);
        assert!((full[2].prev_yes_ask - 0.54).abs() < 1e-12);
        assert!((full[2].high_yes_ask - 0.58).abs() < 1e-12);
        assert!((full[2].low_yes_ask - 0.50).abs() < 1e-12);
        assert!((full[2].delta_60s_cents - 4.0).abs() < 1e-9);
        assert!(full[2].window_vol_cents > 0.0);
        assert!((full[2].book_imbalance - (50.0 / 70.0)).abs() < 1e-12);
    }

    #[test]
    fn v2_validate_and_canary_pass_for_valid_script() {
        let src = r#"
pub fn name() { "unit-v2-sample" }
pub fn side() { "yes" }
pub fn describe() { "Sample v2 strategy using causal TickContext." }
pub fn evaluate(ctx) {
    if ctx.secs_to_close <= 600 && ctx.secs_to_close >= 90 && ctx.delta_60s_cents >= 3.0 && ctx.book_imbalance >= 0.2 {
        let lc = (ctx.yes_bid * 100.0).round() as i64;
        if lc >= 1 && lc <= 99 {
            return Some(#{ enter: true, side: "yes", limit_cents: lc, post_only: true });
        }
    }
    None
}
"#;
        let res = validate_strategy_source_full(src).expect("v2 source must validate");
        assert_eq!(res.version, StrategyVersion::V2);
        assert!(res.causal_canary_passed);
        assert_eq!(res.name, "unit-v2-sample");
        assert_eq!(res.side, "yes");
    }

    #[test]
    fn slug_validation_rejects_traversal() {
        for bad in &[
            "",
            "../evil",
            "..\\evil",
            "a/b",
            "foo%2fbar",
            ".hidden",
            "-leading",
            "bad.txt",
            "bad name",
        ] {
            assert!(validate_strategy_slug(bad).is_err(), "expected err for {bad}");
        }
        assert_eq!(validate_strategy_slug("good_strat-1.rn").unwrap(), "good_strat-1");
    }
}
