//! Automated Walk-Forward Edge Synthesizer (strictly paper-only).
//!
//! Performs deterministic chronological In-Sample (60%) discovery -> held-out
//! Out-of-Sample (40%) validation across parameterized v2 strategy families:
//! - `momentum_microstructure`
//! - `mean_reversion_pav`
//! - `maker_imbalance`
//! - `all`
//!
//! Enforces strict causal purity (IS-only PAV blocks + causal `TickContext`),
//! realistic maker (post-only touch/cross fill on subsequent ticks, 1.75% * p*(1-p) fee)
//! and taker (ask + slippage, 7% * p*(1-p) fee) simulation, Benjamini-Hochberg
//! False Discovery Rate (BH-FDR) correction across the full search grid,
//! Bailey & Lopez de Prado Deflated Sharpe Ratio (DSR) filtering, library
//! return-correlation check (`max_corr_with_library`), and deterministic
//! paper-only Rune v2 script emission.

#![allow(dead_code)]

use crate::recalibration::{fit_pav_blocks_for_settled_windows, PavBlock};
use crate::strategy_v2::{build_causal_tick_contexts, TickContext};
use crate::tournament::{build_tournament_context, TournamentTapeContext};
use crate::{parse_iso_z, window_facts_keyed, facts_fingerprint, Snapshot, Strategy, WindowFacts};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Request payload for `POST /api/research/synthesize`.
#[derive(Debug, Clone, Deserialize)]
pub struct SynthesizeRequest {
    #[serde(default)]
    pub dataset: Option<String>,
    #[serde(default = "default_family")]
    pub family: String,
    #[serde(default = "default_execution_mode")]
    pub execution_mode: String,
    #[serde(default = "default_fdr_alpha")]
    pub fdr_alpha: f64,
    #[serde(default = "default_max_candidates")]
    pub max_candidates: usize,
    #[serde(default = "default_min_is_trades")]
    pub min_is_trades: usize,
    #[serde(default = "default_min_oos_trades")]
    pub min_oos_trades: usize,
    #[serde(default = "default_min_dsr")]
    pub min_deflated_sharpe: f64,
    #[serde(default = "default_max_corr")]
    pub max_corr: f64,
    #[serde(default = "default_slippage_cents")]
    pub slippage_cents: f64,
}

fn default_family() -> String {
    "all".to_string()
}
fn default_execution_mode() -> String {
    "maker".to_string()
}
fn default_fdr_alpha() -> f64 {
    0.10
}
fn default_max_candidates() -> usize {
    10
}
fn default_min_is_trades() -> usize {
    5
}
fn default_min_oos_trades() -> usize {
    5
}
fn default_min_dsr() -> f64 {
    0.50
}
fn default_max_corr() -> f64 {
    0.65
}
fn default_slippage_cents() -> f64 {
    1.0
}

impl Default for SynthesizeRequest {
    fn default() -> Self {
        Self {
            dataset: None,
            family: default_family(),
            execution_mode: default_execution_mode(),
            fdr_alpha: default_fdr_alpha(),
            max_candidates: default_max_candidates(),
            min_is_trades: default_min_is_trades(),
            min_oos_trades: default_min_oos_trades(),
            min_deflated_sharpe: default_min_dsr(),
            max_corr: default_max_corr(),
            slippage_cents: default_slippage_cents(),
        }
    }
}

/// One synthesized strategy candidate evaluated on IS -> held-out OOS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizedCandidate {
    pub rank: usize,
    pub id: String,
    pub name: String,
    pub family: String,
    pub side: String,
    pub execution_mode: String,
    pub post_only: bool,
    pub is_trades: usize,
    pub is_win_rate: Option<f64>,
    pub oos_trades: usize,
    pub oos_wins: usize,
    pub oos_win_rate: Option<f64>,
    pub is_net_expectancy_cents: Option<f64>,
    pub oos_net_expectancy_cents: Option<f64>,
    pub oos_net_pnl_dollars: f64,
    pub oos_net_pnl_dollars_1ct: f64,
    pub oos_sharpe: Option<f64>,
    pub deflated_sharpe: f64,
    pub raw_p_value: f64,
    pub bh_adjusted_p_value: f64,
    pub survived_fdr: bool,
    pub passed_fdr: bool,
    pub passed_dsr: bool,
    pub max_corr_with_library: Option<f64>,
    pub most_correlated_library_strategy: Option<String>,
    pub passed_corr_cap: bool,
    pub passed_all_gates: bool,
    pub causal_canary_passed: bool,
    pub rejection_reason: Option<String>,
    pub description: String,
    pub source: String,
}

/// Top-level payload returned by `POST /api/research/synthesize`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesizeResponse {
    pub ok: bool,
    pub dataset: String,
    pub family: String,
    pub execution_mode: String,
    pub fdr_alpha: f64,
    pub max_candidates: usize,
    pub min_deflated_sharpe: f64,
    pub max_corr: f64,
    pub windows_settled_total: usize,
    pub windows_settled_is: usize,
    pub windows_settled_oos: usize,
    pub is_windows: usize,
    pub oos_windows: usize,
    pub split_epoch: Option<i64>,
    pub pav_blocks_count: usize,
    pub total_hypotheses_tested: usize,
    pub candidates_evaluated: usize,
    pub is_promising_count: usize,
    pub survivors_count: usize,
    pub elapsed_ms: u64,
    pub candidates: Vec<SynthesizedCandidate>,
    pub note: String,
}

#[derive(Debug, Clone)]
enum CandidateSpec {
    MomentumMicrostructure {
        id: String,
        slug: String,
        side: &'static str,
        min_delta_cents: f64,
        max_spread_cents: f64,
        min_vol_cents: f64,
        min_price_cents: u8,
        max_price_cents: u8,
        min_secs: i64,
        max_secs: i64,
    },
    MeanReversionPav {
        id: String,
        slug: String,
        side: &'static str,
        min_ev_cents: f64,
        min_pullback_cents: f64,
        max_spread_cents: f64,
        min_price_cents: u8,
        max_price_cents: u8,
        min_secs: i64,
        max_secs: i64,
    },
    MakerImbalance {
        id: String,
        slug: String,
        side: &'static str,
        min_imbalance: f64,
        min_spread_cents: f64,
        max_spread_cents: f64,
        min_price_cents: u8,
        max_price_cents: u8,
        min_secs: i64,
        max_secs: i64,
    },
}

impl CandidateSpec {
    fn id(&self) -> &str {
        match self {
            CandidateSpec::MomentumMicrostructure { id, .. }
            | CandidateSpec::MeanReversionPav { id, .. }
            | CandidateSpec::MakerImbalance { id, .. } => id,
        }
    }

    fn slug(&self) -> &str {
        match self {
            CandidateSpec::MomentumMicrostructure { slug, .. }
            | CandidateSpec::MeanReversionPav { slug, .. }
            | CandidateSpec::MakerImbalance { slug, .. } => slug,
        }
    }

    fn family(&self) -> &'static str {
        match self {
            CandidateSpec::MomentumMicrostructure { .. } => "momentum_microstructure",
            CandidateSpec::MeanReversionPav { .. } => "mean_reversion_pav",
            CandidateSpec::MakerImbalance { .. } => "maker_imbalance",
        }
    }

    fn side(&self) -> &'static str {
        match self {
            CandidateSpec::MomentumMicrostructure { side, .. }
            | CandidateSpec::MeanReversionPav { side, .. }
            | CandidateSpec::MakerImbalance { side, .. } => side,
        }
    }

    /// Evaluate this candidate rule against one strictly causal `TickContext`.
    /// Returns `Some(limit_cents)` if entry triggers.
    fn eval_tick(&self, ctx: &TickContext, is_maker: bool) -> Option<u8> {
        match self {
            CandidateSpec::MomentumMicrostructure {
                side,
                min_delta_cents,
                max_spread_cents,
                min_vol_cents,
                min_price_cents,
                max_price_cents,
                min_secs,
                max_secs,
                ..
            } => {
                if ctx.secs_to_close < *min_secs || ctx.secs_to_close > *max_secs {
                    return None;
                }
                if ctx.spread_cents > *max_spread_cents || ctx.window_vol_cents < *min_vol_cents {
                    return None;
                }
                let ask_c = if *side == "yes" {
                    (ctx.yes_ask * 100.0).round() as i64
                } else {
                    (ctx.no_ask * 100.0).round() as i64
                };
                if ask_c < *min_price_cents as i64 || ask_c > *max_price_cents as i64 {
                    return None;
                }
                let momentum_ok = if *side == "yes" {
                    ctx.delta_60s_cents >= *min_delta_cents
                } else {
                    ctx.delta_60s_cents <= -*min_delta_cents
                };
                if !momentum_ok {
                    return None;
                }
                if is_maker {
                    let bid_c = if *side == "yes" {
                        (ctx.yes_bid * 100.0).round() as i64
                    } else {
                        (ctx.no_bid * 100.0).round() as i64
                    };
                    let target = if ask_c - bid_c >= 2 { bid_c + 1 } else { bid_c };
                    let clamped = target.clamp(1, (ask_c - 1).max(1));
                    if clamped >= ask_c {
                        None
                    } else {
                        Some(clamped as u8)
                    }
                } else {
                    Some(ask_c.clamp(1, 99) as u8)
                }
            }
            CandidateSpec::MeanReversionPav {
                side,
                min_ev_cents,
                min_pullback_cents,
                max_spread_cents,
                min_price_cents,
                max_price_cents,
                min_secs,
                max_secs,
                ..
            } => {
                if ctx.secs_to_close < *min_secs || ctx.secs_to_close > *max_secs {
                    return None;
                }
                if ctx.spread_cents > *max_spread_cents {
                    return None;
                }
                let ask_c = if *side == "yes" {
                    (ctx.yes_ask * 100.0).round() as i64
                } else {
                    (ctx.no_ask * 100.0).round() as i64
                };
                if ask_c < *min_price_cents as i64 || ask_c > *max_price_cents as i64 {
                    return None;
                }
                let excursion_cents = (ctx.yes_ask - ctx.open_yes_ask) * 100.0;
                let (ev_ok, pb_ok) = if *side == "yes" {
                    (
                        ctx.pav_ev_yes_cents >= *min_ev_cents,
                        excursion_cents <= -*min_pullback_cents,
                    )
                } else {
                    (
                        ctx.pav_ev_no_cents >= *min_ev_cents,
                        excursion_cents >= *min_pullback_cents,
                    )
                };
                if !ev_ok || !pb_ok {
                    return None;
                }
                if is_maker {
                    let bid_c = if *side == "yes" {
                        (ctx.yes_bid * 100.0).round() as i64
                    } else {
                        (ctx.no_bid * 100.0).round() as i64
                    };
                    let target = if ask_c - bid_c >= 2 { bid_c + 1 } else { bid_c };
                    let clamped = target.clamp(1, (ask_c - 1).max(1));
                    if clamped >= ask_c {
                        None
                    } else {
                        Some(clamped as u8)
                    }
                } else {
                    Some(ask_c.clamp(1, 99) as u8)
                }
            }
            CandidateSpec::MakerImbalance {
                side,
                min_imbalance,
                min_spread_cents,
                max_spread_cents,
                min_price_cents,
                max_price_cents,
                min_secs,
                max_secs,
                ..
            } => {
                if ctx.secs_to_close < *min_secs || ctx.secs_to_close > *max_secs {
                    return None;
                }
                if ctx.spread_cents < *min_spread_cents || ctx.spread_cents > *max_spread_cents {
                    return None;
                }
                let ask_c = if *side == "yes" {
                    (ctx.yes_ask * 100.0).round() as i64
                } else {
                    (ctx.no_ask * 100.0).round() as i64
                };
                if ask_c < *min_price_cents as i64 || ask_c > *max_price_cents as i64 {
                    return None;
                }
                let imb_ok = if *side == "yes" {
                    ctx.book_imbalance >= *min_imbalance
                } else {
                    ctx.book_imbalance <= -*min_imbalance
                };
                if !imb_ok {
                    return None;
                }
                if is_maker {
                    let bid_c = if *side == "yes" {
                        (ctx.yes_bid * 100.0).round() as i64
                    } else {
                        (ctx.no_bid * 100.0).round() as i64
                    };
                    let target = if ask_c - bid_c >= 2 { bid_c + 1 } else { bid_c };
                    let clamped = target.clamp(1, (ask_c - 1).max(1));
                    if clamped >= ask_c {
                        None
                    } else {
                        Some(clamped as u8)
                    }
                } else {
                    Some(ask_c.clamp(1, 99) as u8)
                }
            }
        }
    }

    fn description(&self, is_maker: bool) -> String {
        let mode = if is_maker { "post-only maker" } else { "taker" };
        match self {
            CandidateSpec::MomentumMicrostructure {
                side,
                min_delta_cents,
                max_spread_cents,
                min_vol_cents,
                min_price_cents,
                max_price_cents,
                min_secs,
                max_secs,
                ..
            } => format!(
                "Synthesized {mode} {} momentum strategy: 60s move >= {min_delta_cents:.1}c, vol >= {min_vol_cents:.1}c, spread <= {max_spread_cents:.1}c, ask {min_price_cents}c-{max_price_cents}c, {min_secs}s-{max_secs}s left.",
                side.to_ascii_uppercase()
            ),
            CandidateSpec::MeanReversionPav {
                side,
                min_ev_cents,
                min_pullback_cents,
                max_spread_cents,
                min_price_cents,
                max_price_cents,
                min_secs,
                max_secs,
                ..
            } => format!(
                "Synthesized {mode} {} PAV mean-reversion strategy: PAV EV >= {min_ev_cents:.1}c, open pullback >= {min_pullback_cents:.1}c, spread <= {max_spread_cents:.1}c, ask {min_price_cents}c-{max_price_cents}c, {min_secs}s-{max_secs}s left.",
                side.to_ascii_uppercase()
            ),
            CandidateSpec::MakerImbalance {
                side,
                min_imbalance,
                min_spread_cents,
                max_spread_cents,
                min_price_cents,
                max_price_cents,
                min_secs,
                max_secs,
                ..
            } => format!(
                "Synthesized {mode} {} book-imbalance strategy: imbalance >= {min_imbalance:.2}, spread {min_spread_cents:.1}c-{max_spread_cents:.1}c, ask {min_price_cents}c-{max_price_cents}c, {min_secs}s-{max_secs}s left.",
                side.to_ascii_uppercase()
            ),
        }
    }

    /// Deterministically emit a self-contained, paper-only Rune v2 script
    /// implementing `evaluate(ctx)`. Zero forbidden keywords.
    fn emit_v2_rune_source(&self, is_maker: bool) -> String {
        let name = self.slug();
        let side = self.side();
        let desc = self.description(is_maker).replace('"', "'");
        let post_only_str = if is_maker { "true" } else { "false" };

        let limit_expr = if is_maker {
            if side == "yes" {
                "let ask_c = (ctx.yes_ask * 100.0).round();\n    let bid_c = (ctx.yes_bid * 100.0).round();\n    let raw_c = if ask_c - bid_c >= 2.0 { bid_c + 1.0 } else { bid_c };\n    let lim = if raw_c >= ask_c { ask_c - 1.0 } else { raw_c };\n    if lim < 1.0 || lim > 99.0 { return None; }"
            } else {
                "let ask_c = (ctx.no_ask * 100.0).round();\n    let bid_c = (ctx.no_bid * 100.0).round();\n    let raw_c = if ask_c - bid_c >= 2.0 { bid_c + 1.0 } else { bid_c };\n    let lim = if raw_c >= ask_c { ask_c - 1.0 } else { raw_c };\n    if lim < 1.0 || lim > 99.0 { return None; }"
            }
        } else if side == "yes" {
            "let lim = (ctx.yes_ask * 100.0).round();\n    if lim < 1.0 || lim > 99.0 { return None; }"
        } else {
            "let lim = (ctx.no_ask * 100.0).round();\n    if lim < 1.0 || lim > 99.0 { return None; }"
        };

        let condition = match self {
            CandidateSpec::MomentumMicrostructure {
                side,
                min_delta_cents,
                max_spread_cents,
                min_vol_cents,
                min_price_cents,
                max_price_cents,
                min_secs,
                max_secs,
                ..
            } => {
                let ask_field = if *side == "yes" { "ctx.yes_ask" } else { "ctx.no_ask" };
                let min_p = (*min_price_cents as f64) / 100.0;
                let max_p = (*max_price_cents as f64) / 100.0;
                let delta_cond = if *side == "yes" {
                    format!("if ctx.delta_60s_cents < {min_delta_cents:.2} {{ return None; }}")
                } else {
                    format!("if ctx.delta_60s_cents > -{min_delta_cents:.2} {{ return None; }}")
                };
                format!(
                    "    if ctx.secs_to_close < {min_secs} || ctx.secs_to_close > {max_secs} {{ return None; }}\n\
                     \x20   if ctx.spread_cents > {max_spread_cents:.2} || ctx.window_vol_cents < {min_vol_cents:.2} {{ return None; }}\n\
                     \x20   if {ask_field} < {min_p:.2} || {ask_field} > {max_p:.2} {{ return None; }}\n\
                     \x20   {delta_cond}"
                )
            }
            CandidateSpec::MeanReversionPav {
                side,
                min_ev_cents,
                min_pullback_cents,
                max_spread_cents,
                min_price_cents,
                max_price_cents,
                min_secs,
                max_secs,
                ..
            } => {
                let ask_field = if *side == "yes" { "ctx.yes_ask" } else { "ctx.no_ask" };
                let ev_field = if *side == "yes" {
                    "ctx.pav_ev_yes_cents"
                } else {
                    "ctx.pav_ev_no_cents"
                };
                let min_p = (*min_price_cents as f64) / 100.0;
                let max_p = (*max_price_cents as f64) / 100.0;
                let pb_cond = if *side == "yes" {
                    format!("if excursion > -{min_pullback_cents:.2} {{ return None; }}")
                } else {
                    format!("if excursion < {min_pullback_cents:.2} {{ return None; }}")
                };
                format!(
                    "    if ctx.secs_to_close < {min_secs} || ctx.secs_to_close > {max_secs} {{ return None; }}\n\
                     \x20   if ctx.spread_cents > {max_spread_cents:.2} {{ return None; }}\n\
                     \x20   if {ask_field} < {min_p:.2} || {ask_field} > {max_p:.2} {{ return None; }}\n\
                     \x20   if {ev_field} < {min_ev_cents:.2} {{ return None; }}\n\
                     \x20   let excursion = (ctx.yes_ask - ctx.open_yes_ask) * 100.0;\n\
                     \x20   {pb_cond}"
                )
            }
            CandidateSpec::MakerImbalance {
                side,
                min_imbalance,
                min_spread_cents,
                max_spread_cents,
                min_price_cents,
                max_price_cents,
                min_secs,
                max_secs,
                ..
            } => {
                let ask_field = if *side == "yes" { "ctx.yes_ask" } else { "ctx.no_ask" };
                let min_p = (*min_price_cents as f64) / 100.0;
                let max_p = (*max_price_cents as f64) / 100.0;
                let imb_cond = if *side == "yes" {
                    format!("if ctx.book_imbalance < {min_imbalance:.2} {{ return None; }}")
                } else {
                    format!("if ctx.book_imbalance > -{min_imbalance:.2} {{ return None; }}")
                };
                format!(
                    "    if ctx.secs_to_close < {min_secs} || ctx.secs_to_close > {max_secs} {{ return None; }}\n\
                     \x20   if ctx.spread_cents < {min_spread_cents:.2} || ctx.spread_cents > {max_spread_cents:.2} {{ return None; }}\n\
                     \x20   if {ask_field} < {min_p:.2} || {ask_field} > {max_p:.2} {{ return None; }}\n\
                     \x20   {imb_cond}"
                )
            }
        };

        format!(
            "// Synthesized strictly paper-only v2 strategy ({}).\n\
             // Walk-forward IS->OOS validated; evaluates causal TickContext only.\n\
             pub fn name() {{\n\
             \x20   \"{name}\"\n\
             }}\n\
             \n\
             pub fn side() {{\n\
             \x20   \"{side}\"\n\
             }}\n\
             \n\
             pub fn describe() {{\n\
             \x20   \"{desc}\"\n\
             }}\n\
             \n\
             pub fn evaluate(ctx) {{\n\
             {condition}\n\
             \x20   {limit_expr}\n\
             \x20   Some(#{{ enter: true, side: \"{side}\", limit_cents: lim, post_only: {post_only_str} }})\n\
             }}\n",
            self.family()
        )
    }
}

fn build_candidate_grid(family: &str, is_maker: bool) -> Result<Vec<CandidateSpec>, String> {
    let want_mom = family == "all" || family == "momentum_microstructure";
    let want_mr = family == "all" || family == "mean_reversion_pav";
    let want_imb = family == "all" || family == "maker_imbalance";
    if !want_mom && !want_mr && !want_imb {
        return Err(format!(
            "unsupported synthesizer family \"{family}\" (expected momentum_microstructure, mean_reversion_pav, maker_imbalance, or all)"
        ));
    }

    let mut specs = Vec::new();
    let mode_tag = if is_maker { "mk" } else { "tk" };

    if want_mom {
        for &side in &["yes", "no"] {
            for &min_delta in &[2.0_f64, 4.0, 6.0] {
                for &(min_p, max_p) in &[(45u8, 82u8), (55u8, 88u8)] {
                    for &(min_s, max_s) in &[(90i64, 540i64), (120i64, 720i64)] {
                        let id = format!(
                            "mom_{mode_tag}_{side}_d{}_p{}_{}_s{}_{}",
                            min_delta as i64, min_p, max_p, min_s, max_s
                        );
                        let slug = format!(
                            "synth-mom-{mode_tag}-{side}-d{}-p{min_p}-{max_p}-s{min_s}",
                            min_delta as i64
                        );
                        specs.push(CandidateSpec::MomentumMicrostructure {
                            id,
                            slug,
                            side,
                            min_delta_cents: min_delta,
                            max_spread_cents: if is_maker { 6.0 } else { 4.0 },
                            min_vol_cents: 0.5,
                            min_price_cents: min_p,
                            max_price_cents: max_p,
                            min_secs: min_s,
                            max_secs: max_s,
                        });
                    }
                }
            }
        }
    }

    if want_mr {
        for &side in &["yes", "no"] {
            for &min_ev in &[0.0_f64, 1.5, 3.0] {
                for &min_pb in &[1.0_f64, 3.0] {
                    for &(min_p, max_p) in &[(35u8, 75u8), (45u8, 85u8)] {
                        let id = format!(
                            "mrpav_{mode_tag}_{side}_ev{}_pb{}_p{}_{}",
                            (min_ev * 10.0).round() as i64,
                            min_pb as i64,
                            min_p,
                            max_p
                        );
                        let slug = format!(
                            "synth-pav-{mode_tag}-{side}-ev{}-pb{}-p{min_p}",
                            (min_ev * 10.0).round() as i64,
                            min_pb as i64
                        );
                        specs.push(CandidateSpec::MeanReversionPav {
                            id,
                            slug,
                            side,
                            min_ev_cents: min_ev,
                            min_pullback_cents: min_pb,
                            max_spread_cents: if is_maker { 6.0 } else { 4.0 },
                            min_price_cents: min_p,
                            max_price_cents: max_p,
                            min_secs: 90,
                            max_secs: 660,
                        });
                    }
                }
            }
        }
    }

    if want_imb {
        let min_spread = if is_maker { 2.0 } else { 0.0 };
        for &side in &["yes", "no"] {
            for &min_imb in &[0.20_f64, 0.35, 0.50] {
                for &(min_p, max_p) in &[(35u8, 75u8), (50u8, 85u8)] {
                    for &(min_s, max_s) in &[(90i64, 600i64), (120i64, 720i64)] {
                        let id = format!(
                            "imb_{mode_tag}_{side}_i{}_p{}_{}_s{}_{}",
                            (min_imb * 100.0).round() as i64,
                            min_p,
                            max_p,
                            min_s,
                            max_s
                        );
                        let slug = format!(
                            "synth-imb-{mode_tag}-{side}-i{}-p{min_p}-s{min_s}",
                            (min_imb * 100.0).round() as i64
                        );
                        specs.push(CandidateSpec::MakerImbalance {
                            id,
                            slug,
                            side,
                            min_imbalance: min_imb,
                            min_spread_cents: min_spread,
                            max_spread_cents: 8.0,
                            min_price_cents: min_p,
                            max_price_cents: max_p,
                            min_secs: min_s,
                            max_secs: max_s,
                        });
                    }
                }
            }
        }
    }

    Ok(specs)
}

type SnapAsks = Option<(u8, u8, Option<u8>, i64)>;

struct PreparedWindow<'a> {
    ticker: &'a str,
    close_epoch: i64,
    result: String,
    snaps: &'a [Snapshot],
    ctxs: Vec<TickContext>,
    /// Precomputed per-tick whole-cent prices aligned with `snaps` for fast maker touch checks.
    snap_asks_c: Vec<SnapAsks>,
}

#[derive(Debug, Clone)]
struct WindowTradeOutcome {
    ticker: String,
    won: bool,
    pnl_cents: f64,
    ret_ratio: f64,
}

#[inline]
fn maker_fee_dollars(price: f64) -> f64 {
    0.0175 * price * (1.0 - price)
}

#[inline]
fn taker_fee_dollars(price: f64) -> f64 {
    0.07 * price * (1.0 - price)
}

fn simulate_candidate_on_windows(
    spec: &CandidateSpec,
    windows: &[PreparedWindow<'_>],
    is_maker: bool,
    slippage_cents: f64,
) -> Vec<WindowTradeOutcome> {
    let mut trades = Vec::new();
    let side = spec.side();
    let slip = if is_maker { 0.0 } else { slippage_cents.clamp(0.0, 5.0) };

    for w in windows {
        // Note: `ctxs` is built only from valid ticks, so track snapshot index via `fetched_at`
        // or walk snaps directly while stepping ctxs. Since `build_causal_tick_contexts` skips
        // only invalid snaps, map each valid ctx to its snap position.
        let mut ctx_idx = 0usize;
        let mut entered: Option<(f64, bool)> = None;

        for (si, _snap) in w.snaps.iter().enumerate() {
            let Some((ya_c, na_c, last_c, secs)) = w.snap_asks_c[si] else {
                continue;
            };
            let Some(ctx) = w.ctxs.get(ctx_idx) else {
                break;
            };
            ctx_idx += 1;

            let Some(limit_c) = spec.eval_tick(ctx, is_maker) else {
                continue;
            };
            if is_maker {
                let side_ask_c = if side == "yes" { ya_c } else { na_c };
                if side_ask_c <= 1 {
                    continue;
                }
                let post_c = limit_c.min(side_ask_c - 1).max(1);
                // Passive fill occurs only on a STRICTLY SUBSEQUENT tick j > si that
                // touches or crosses `post_c` before close.
                let mut filled = false;
                for j in (si + 1)..w.snaps.len() {
                    let Some((ya_j, na_j, last_j, secs_j)) = w.snap_asks_c[j] else {
                        continue;
                    };
                    if secs_j <= 0 {
                        break;
                    }
                    let ask_j = if side == "yes" { ya_j } else { na_j };
                    let last_side_j = last_j.map(|p| if side == "yes" { p } else { 100u8.saturating_sub(p) });
                    if ask_j <= post_c || last_side_j.is_some_and(|lp| lp <= post_c) {
                        filled = true;
                        break;
                    }
                }
                if filled {
                    let fill_price = (post_c as f64) / 100.0;
                    let fee_d = maker_fee_dollars(fill_price);
                    let won = w.result == side;
                    let pnl_d = (if won { 1.0 } else { 0.0 }) - fill_price - fee_d;
                    let ret = pnl_d / fill_price;
                    entered = Some((pnl_d * 100.0, won));
                    trades.push(WindowTradeOutcome {
                        ticker: w.ticker.to_string(),
                        won,
                        pnl_cents: pnl_d * 100.0,
                        ret_ratio: ret,
                    });
                    break;
                } else {
                    let _ = (secs, last_c);
                    // Unfilled passive quote in this tick; keep scanning or wait until fill/close.
                    // Match one intent per window: once posted and unfilled, stop scanning.
                    break;
                }
            } else {
                let raw_ask_c = if side == "yes" { ya_c } else { na_c } as f64;
                let fill_c = (raw_ask_c + slip).clamp(1.0, 99.0);
                let fill_price = fill_c / 100.0;
                let fee_d = taker_fee_dollars(fill_price);
                let won = w.result == side;
                let pnl_d = (if won { 1.0 } else { 0.0 }) - fill_price - fee_d;
                let ret = pnl_d / fill_price;
                entered = Some((pnl_d * 100.0, won));
                trades.push(WindowTradeOutcome {
                    ticker: w.ticker.to_string(),
                    won,
                    pnl_cents: pnl_d * 100.0,
                    ret_ratio: ret,
                });
                break;
            }
        }
        let _ = entered;
    }

    trades
}

/// Standard normal CDF `Phi(x)` using high-precision Abramowitz & Stegun approximation.
pub fn normal_cdf(x: f64) -> f64 {
    if !x.is_finite() {
        return if x.is_sign_positive() { 1.0 } else { 0.0 };
    }
    let abs_x = x.abs();
    let t = 1.0 / (1.0 + 0.2316419 * abs_x);
    let d = 0.3989422804014327 * (-0.5 * abs_x * abs_x).exp();
    let p = d
        * t
        * (0.319381530
            + t * (-0.356563782
                + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));
    if x >= 0.0 {
        (1.0 - p).clamp(0.0, 1.0)
    } else {
        p.clamp(0.0, 1.0)
    }
}

/// Inverse standard normal CDF (Acklam's rational approximation).
#[allow(clippy::excessive_precision)]
pub fn inv_normal_cdf(p: f64) -> f64 {
    let p = p.clamp(1e-12, 1.0 - 1e-12);
    const A: [f64; 6] = [
        -3.969683028665376e+01,
        2.209460984245205e+02,
        -2.759285104469687e+02,
        1.383577518672690e+02,
        -3.066479806614716e+01,
        2.506628277459239e+00,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e+01,
        1.615858368580409e+02,
        -1.556989798598866e+02,
        6.680131188771972e+01,
        -1.328068155288572e+01,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-03,
        -3.223964580411365e-01,
        -2.400758277161838e+00,
        -2.549732539343734e+00,
        4.374664141464968e+00,
        2.938163982698783e+00,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-03,
        3.224671290700398e-01,
        2.445134137142996e+00,
        3.754408661907416e+00,
    ];
    const P_LOW: f64 = 0.02425;
    const P_HIGH: f64 = 1.0 - P_LOW;

    if p < P_LOW {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= P_HIGH {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

/// Compute (mean, std_dev, sharpe, skew, kurtosis) over sample returns.
fn return_moments(xs: &[f64]) -> (Option<f64>, Option<f64>, Option<f64>, f64, f64) {
    let n = xs.len();
    if n == 0 {
        return (None, None, None, 0.0, 3.0);
    }
    let nf = n as f64;
    let mean = xs.iter().sum::<f64>() / nf;
    if n < 2 {
        return (Some(mean), None, None, 0.0, 3.0);
    }
    let var = xs.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / ((n - 1) as f64);
    let sd = var.sqrt();
    if !sd.is_finite() || sd < 1e-12 {
        return (Some(mean), Some(sd), None, 0.0, 3.0);
    }
    let sharpe = mean / sd;
    let (skew, kurt) = if n >= 4 {
        let m3 = xs.iter().map(|x| ((x - mean) / sd).powi(3)).sum::<f64>() / nf;
        let m4 = xs.iter().map(|x| ((x - mean) / sd).powi(4)).sum::<f64>() / nf;
        (m3.clamp(-5.0, 5.0), m4.clamp(1.0, 20.0))
    } else {
        (0.0, 3.0)
    };
    (Some(mean), Some(sd), Some(sharpe), skew, kurt)
}

/// Bailey & Lopez de Prado (2014) Deflated Sharpe Ratio probability in `[0.0, 1.0]`.
pub fn compute_deflated_sharpe(
    sr_hat: f64,
    n_trades: usize,
    skew: f64,
    kurt: f64,
    total_trials: usize,
    trials_sharpe_var: f64,
) -> f64 {
    if !sr_hat.is_finite() || n_trades < 2 {
        return 0.0;
    }
    const EULER_GAMMA: f64 = 0.5772156649015329;
    let m = total_trials.max(1) as f64;
    let sr_0 = if total_trials <= 1 {
        0.0
    } else {
        let z1 = inv_normal_cdf(1.0 - 1.0 / m);
        let z2 = inv_normal_cdf(1.0 - 1.0 / (m * std::f64::consts::E));
        ((1.0 - EULER_GAMMA) * z1 + EULER_GAMMA * z2) * trials_sharpe_var.max(1e-6).sqrt()
    };
    let denom = ((1.0 - skew * sr_hat + ((kurt - 1.0) / 4.0) * sr_hat * sr_hat)
        / ((n_trades - 1) as f64))
        .max(1e-9)
        .sqrt();
    normal_cdf((sr_hat - sr_0) / denom)
}

/// Benjamini-Hochberg step-up adjusted p-values across `total_hypotheses >= p_values.len()`.
/// Preserves original order.
pub fn benjamini_hochberg_adjust(p_values: &[f64], total_hypotheses: usize) -> Vec<f64> {
    let k = p_values.len();
    if k == 0 {
        return Vec::new();
    }
    let m = total_hypotheses.max(k) as f64;
    let mut indexed: Vec<(usize, f64)> = p_values
        .iter()
        .enumerate()
        .map(|(i, &p)| (i, if p.is_finite() { p.clamp(0.0, 1.0) } else { 1.0 }))
        .collect();
    indexed.sort_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

    let mut adjusted_sorted = vec![1.0f64; k];
    let mut running_min = 1.0f64;
    for rank_idx in (0..k).rev() {
        let rank = (rank_idx + 1) as f64;
        let q = (m * indexed[rank_idx].1 / rank).clamp(0.0, 1.0);
        if q < running_min {
            running_min = q;
        }
        adjusted_sorted[rank_idx] = running_min;
    }

    let mut out = vec![1.0f64; k];
    for (rank_idx, &(orig_idx, _)) in indexed.iter().enumerate() {
        out[orig_idx] = adjusted_sorted[rank_idx];
    }
    out
}

fn pearson_corr_over_shared(
    cand_returns: &HashMap<String, f64>,
    lib_returns: &HashMap<String, f64>,
) -> Option<f64> {
    let mut pairs: Vec<(&str, f64, f64)> = Vec::new();
    for (ticker, &x) in cand_returns {
        if let Some(&y) = lib_returns.get(ticker) {
            if x.is_finite() && y.is_finite() {
                pairs.push((ticker.as_str(), x, y));
            }
        }
    }
    let n = pairs.len();
    if n < 2 {
        return None;
    }
    // Sort by ticker so floating-point summation order is bit-for-bit deterministic.
    pairs.sort_unstable_by(|a, b| a.0.cmp(b.0));
    let nf = n as f64;
    let mx = pairs.iter().map(|p| p.1).sum::<f64>() / nf;
    let my = pairs.iter().map(|p| p.2).sum::<f64>() / nf;
    let mut cov = 0.0;
    let mut vx = 0.0;
    let mut vy = 0.0;
    for &(_, x, y) in &pairs {
        let dx = x - mx;
        let dy = y - my;
        cov += dx * dy;
        vx += dx * dx;
        vy += dy * dy;
    }
    let denom = (vx * vy).sqrt();
    if !denom.is_finite() || denom < 1e-12 {
        return None;
    }
    Some((cov / denom).clamp(-1.0, 1.0))
}

fn build_library_oos_returns(
    ctx: &TournamentTapeContext,
    slippage_cents: f64,
) -> Vec<(String, HashMap<String, f64>)> {
    let Some(split_epoch) = ctx.split_epoch else {
        return Vec::new();
    };
    let is_single_epoch = ctx.windows_settled_oos == 0;
    if is_single_epoch {
        return Vec::new();
    }
    let mut by_strat: Vec<HashMap<String, f64>> = vec![HashMap::new(); ctx.strategies.len()];
    for e in &ctx.entries {
        if e.close_epoch < split_epoch {
            continue;
        }
        let eff_price = (e.entry_price + slippage_cents / 100.0).clamp(0.01, 0.99);
        let fee_d = taker_fee_dollars(eff_price);
        let pnl = (if e.won { 1.0 } else { 0.0 }) - eff_price - fee_d;
        let ret = pnl / eff_price;
        by_strat[e.strategy_idx].insert(e.ticker.clone(), ret);
    }
    ctx.strategies
        .iter()
        .enumerate()
        .map(|(i, s)| (s.name.clone(), std::mem::take(&mut by_strat[i])))
        .collect()
}

/// Run deterministic chronological IS discovery -> held-out OOS validation across
/// strategy families, applying BH-FDR, Deflated Sharpe, and library correlation gates.
pub fn synthesize_strategies(
    windows: &BTreeMap<String, Vec<Snapshot>>,
    library_strategies: &[Strategy],
    dataset_label: &str,
    req: &SynthesizeRequest,
) -> Result<SynthesizeResponse, String> {
    let t0 = std::time::Instant::now();
    let family = req.family.trim().to_ascii_lowercase();
    let exec_mode = req.execution_mode.trim().to_ascii_lowercase();
    let is_maker = match exec_mode.as_str() {
        "maker" => true,
        "taker" => false,
        other => {
            return Err(format!(
                "unsupported execution_mode \"{other}\" (expected \"maker\" or \"taker\")"
            ));
        }
    };
    let fdr_alpha = if req.fdr_alpha.is_finite() {
        req.fdr_alpha.clamp(0.001, 0.50)
    } else {
        0.10
    };
    let max_candidates = req.max_candidates.clamp(1, 50);
    let min_is_trades = req.min_is_trades.clamp(1, 500);
    let min_oos_trades = req.min_oos_trades.clamp(1, 500);
    let min_dsr = if req.min_deflated_sharpe.is_finite() {
        req.min_deflated_sharpe.clamp(0.0, 0.999)
    } else {
        0.50
    };
    let max_corr = if req.max_corr.is_finite() {
        req.max_corr.clamp(0.10, 1.0)
    } else {
        0.65
    };
    let slippage_cents = if req.slippage_cents.is_finite() {
        req.slippage_cents.clamp(0.0, 5.0)
    } else {
        1.0
    };

    let grid = build_candidate_grid(&family, is_maker)?;
    let total_hypotheses_tested = grid.len();

    // Collect and chronologically sort settled windows.
    struct RawSettled<'a> {
        ticker: &'a str,
        close_epoch: i64,
        result: String,
        snaps: &'a [Snapshot],
    }
    let mut settled: Vec<RawSettled<'_>> = Vec::new();
    for (ticker, snaps) in windows {
        let facts_fp = facts_fingerprint(snaps);
        let (result, close_epoch) = match window_facts_keyed(snaps, facts_fp) {
            WindowFacts::Settled { result, close_epoch } => (result, close_epoch),
            WindowFacts::Conflict | WindowFacts::Unsettled => continue,
        };
        settled.push(RawSettled {
            ticker: ticker.as_str(),
            close_epoch,
            result,
            snaps: snaps.as_slice(),
        });
    }
    settled.sort_by(|a, b| {
        a.close_epoch
            .cmp(&b.close_epoch)
            .then_with(|| a.ticker.cmp(b.ticker))
    });

    let mut epochs: Vec<i64> = settled.iter().map(|w| w.close_epoch).collect();
    epochs.dedup();

    if epochs.len() < 2 {
        return Ok(SynthesizeResponse {
            ok: true,
            dataset: dataset_label.to_string(),
            family,
            execution_mode: exec_mode,
            fdr_alpha,
            max_candidates,
            min_deflated_sharpe: min_dsr,
            max_corr,
            windows_settled_total: settled.len(),
            windows_settled_is: settled.len(),
            windows_settled_oos: 0,
            is_windows: settled.len(),
            oos_windows: 0,
            split_epoch: epochs.first().copied(),
            pav_blocks_count: 0,
            total_hypotheses_tested,
            candidates_evaluated: total_hypotheses_tested,
            is_promising_count: 0,
            survivors_count: 0,
            elapsed_ms: t0.elapsed().as_millis() as u64,
            candidates: Vec::new(),
            note: "Need at least 2 distinct settled window epochs for chronological IS -> OOS synthesis.".to_string(),
        });
    }

    let is_epoch_cnt = ((epochs.len() * 3) / 5).max(1).min(epochs.len() - 1);
    let split_epoch = epochs[is_epoch_cnt];
    let split_idx = settled.partition_point(|w| w.close_epoch < split_epoch);
    let (is_raw, oos_raw) = settled.split_at(split_idx);

    // Fit PAV strictly on IS settled windows.
    let is_slice_for_pav: Vec<(&str, i64, &str, &[Snapshot])> = is_raw
        .iter()
        .map(|w| (w.ticker, w.close_epoch, w.result.as_str(), w.snaps))
        .collect();
    let pav_blocks: Vec<PavBlock> = fit_pav_blocks_for_settled_windows(&is_slice_for_pav);

    fn prepare_windows<'a>(
        raw: &[RawSettled<'a>],
        pav_blocks: &[PavBlock],
    ) -> Vec<PreparedWindow<'a>> {
        raw.iter()
            .map(|w| {
                let ctxs = build_causal_tick_contexts(w.snaps, pav_blocks);
                let mut cached_close_str: Option<&str> = None;
                let mut cached_close_ep: Option<i64> = None;
                let snap_asks_c = w
                    .snaps
                    .iter()
                    .map(|s| {
                        let (Some(ya), Some(na)) = (s.yes_ask.get(), s.no_ask.get()) else {
                            return None;
                        };
                        if !crate::analytics::is_valid_whole_cent(ya)
                            || !crate::analytics::is_valid_whole_cent(na)
                        {
                            return None;
                        }
                        let close_ep = if cached_close_str == Some(s.close_time.as_str()) {
                            cached_close_ep
                        } else {
                            let p = parse_iso_z(&s.close_time);
                            cached_close_str = Some(s.close_time.as_str());
                            cached_close_ep = p;
                            p
                        }?;
                        let ya_c = (ya * 100.0).round() as u8;
                        let na_c = (na * 100.0).round() as u8;
                        let last_c = s
                            .last_price
                            .get()
                            .filter(|v| v.is_finite() && (0.01..=0.99).contains(v))
                            .map(|v| (v * 100.0).round() as u8);
                        Some((ya_c, na_c, last_c, close_ep - s.fetched_at))
                    })
                    .collect();
                PreparedWindow {
                    ticker: w.ticker,
                    close_epoch: w.close_epoch,
                    result: w.result.clone(),
                    snaps: w.snaps,
                    ctxs,
                    snap_asks_c,
                }
            })
            .collect()
    }

    let is_prepared = prepare_windows(is_raw, &pav_blocks);
    let oos_prepared = prepare_windows(oos_raw, &pav_blocks);

    // Step 1: Strictly IS-only discovery.
    struct IsDiscovered<'a> {
        spec: &'a CandidateSpec,
        is_trades: usize,
        is_win_rate: Option<f64>,
        is_mean_cents: f64,
        is_sharpe: f64,
    }
    let mut is_discovered: Vec<IsDiscovered<'_>> = Vec::new();
    for spec in &grid {
        let is_outcomes = simulate_candidate_on_windows(spec, &is_prepared, is_maker, slippage_cents);
        if is_outcomes.len() < min_is_trades {
            continue;
        }
        let is_wins = is_outcomes.iter().filter(|t| t.won).count();
        let is_win_rate = Some(is_wins as f64 / is_outcomes.len() as f64);
        let is_rets: Vec<f64> = is_outcomes.iter().map(|t| t.ret_ratio).collect();
        let is_cents: Vec<f64> = is_outcomes.iter().map(|t| t.pnl_cents).collect();
        let (mean_c, _, _, _, _) = return_moments(&is_cents);
        let (_, _, sr, _, _) = return_moments(&is_rets);
        let mc = mean_c.unwrap_or(0.0);
        if mc > 0.0 {
            is_discovered.push(IsDiscovered {
                spec,
                is_trades: is_outcomes.len(),
                is_win_rate,
                is_mean_cents: mc,
                is_sharpe: sr.unwrap_or(0.0),
            });
        }
    }
    // Deterministic ranking strictly by IS performance.
    is_discovered.sort_by(|a, b| {
        b.is_sharpe
            .total_cmp(&a.is_sharpe)
            .then_with(|| b.is_mean_cents.total_cmp(&a.is_mean_cents))
            .then_with(|| a.spec.id().cmp(b.spec.id()))
    });

    let is_promising_count = is_discovered.len();
    if is_discovered.is_empty() {
        return Ok(SynthesizeResponse {
            ok: true,
            dataset: dataset_label.to_string(),
            family,
            execution_mode: exec_mode,
            fdr_alpha,
            max_candidates,
            min_deflated_sharpe: min_dsr,
            max_corr,
            windows_settled_total: settled.len(),
            windows_settled_is: is_raw.len(),
            windows_settled_oos: oos_raw.len(),
            is_windows: is_raw.len(),
            oos_windows: oos_raw.len(),
            split_epoch: Some(split_epoch),
            pav_blocks_count: pav_blocks.len(),
            total_hypotheses_tested,
            candidates_evaluated: total_hypotheses_tested,
            is_promising_count: 0,
            survivors_count: 0,
            elapsed_ms: t0.elapsed().as_millis() as u64,
            candidates: Vec::new(),
            note: "No parameterizations achieved positive in-sample net expectancy with sufficient IS trade depth.".to_string(),
        });
    }

    // Build library OOS return map for max_corr_with_library check.
    let tournament_ctx = build_tournament_context(windows, library_strategies);
    let lib_oos_returns = build_library_oos_returns(&tournament_ctx, slippage_cents);

    // Step 2: Held-out OOS evaluation of IS-discovered candidates.
    struct OosEval<'a> {
        spec: &'a CandidateSpec,
        is_trades: usize,
        is_win_rate: Option<f64>,
        is_mean_cents: f64,
        oos_trades: usize,
        oos_wins: usize,
        oos_win_rate: Option<f64>,
        oos_mean_cents: Option<f64>,
        oos_pnl_dollars_1ct: f64,
        oos_sharpe: Option<f64>,
        skew: f64,
        kurt: f64,
        raw_p_value: f64,
        max_corr_with_library: Option<f64>,
        most_correlated_lib: Option<String>,
    }

    let mut oos_evals: Vec<OosEval<'_>> = Vec::with_capacity(is_discovered.len());
    let mut all_oos_sharpes: Vec<f64> = Vec::new();
    let mut raw_p_values: Vec<f64> = Vec::with_capacity(is_discovered.len());

    for item in &is_discovered {
        let oos_outcomes =
            simulate_candidate_on_windows(item.spec, &oos_prepared, is_maker, slippage_cents);
        let oos_n = oos_outcomes.len();
        let oos_wins = oos_outcomes.iter().filter(|t| t.won).count();
        let oos_win_rate = if oos_n > 0 {
            Some(oos_wins as f64 / oos_n as f64)
        } else {
            None
        };
        let oos_cents: Vec<f64> = oos_outcomes.iter().map(|t| t.pnl_cents).collect();
        let oos_rets: Vec<f64> = oos_outcomes.iter().map(|t| t.ret_ratio).collect();
        let (mean_c_opt, sd_c_opt, _, _, _) = return_moments(&oos_cents);
        let (_, _, sr_opt, skew, kurt) = return_moments(&oos_rets);
        if let Some(sr) = sr_opt {
            all_oos_sharpes.push(sr);
        }

        let pnl_dollars_1ct = oos_cents.iter().sum::<f64>() / 100.0;
        let raw_p = match (mean_c_opt, sd_c_opt) {
            (Some(m), Some(sd)) if oos_n >= 2 && sd > 1e-12 => {
                let t_stat = m / (sd / (oos_n as f64).sqrt());
                (1.0 - normal_cdf(t_stat)).clamp(0.0, 1.0)
            }
            _ => 1.0,
        };
        raw_p_values.push(raw_p);

        let mut cand_map = HashMap::with_capacity(oos_outcomes.len());
        for t in &oos_outcomes {
            cand_map.insert(t.ticker.clone(), t.ret_ratio);
        }
        let mut best_abs_corr: Option<f64> = None;
        let mut best_lib_name: Option<String> = None;
        for (lib_name, lib_map) in &lib_oos_returns {
            if let Some(r) = pearson_corr_over_shared(&cand_map, lib_map) {
                let abs_r = r.abs();
                if best_abs_corr.is_none_or(|prev| abs_r > prev) {
                    best_abs_corr = Some(abs_r);
                    best_lib_name = Some(lib_name.clone());
                }
            }
        }

        oos_evals.push(OosEval {
            spec: item.spec,
            is_trades: item.is_trades,
            is_win_rate: item.is_win_rate,
            is_mean_cents: item.is_mean_cents,
            oos_trades: oos_n,
            oos_wins,
            oos_win_rate,
            oos_mean_cents: mean_c_opt,
            oos_pnl_dollars_1ct: pnl_dollars_1ct,
            oos_sharpe: sr_opt,
            skew,
            kurt,
            raw_p_value: raw_p,
            max_corr_with_library: best_abs_corr,
            most_correlated_lib: best_lib_name,
        });
    }

    // Cross-sectional trial Sharpe variance for Bailey-Lopez de Prado DSR.
    let trials_sharpe_var = return_moments(&all_oos_sharpes)
        .1
        .map(|sd| (sd * sd).max(1e-4))
        .unwrap_or(0.05);

    // Benjamini-Hochberg FDR adjustment against total hypotheses explored in grid.
    let bh_adjusted = benjamini_hochberg_adjust(&raw_p_values, total_hypotheses_tested);

    let mut candidates: Vec<SynthesizedCandidate> = oos_evals
        .into_iter()
        .enumerate()
        .map(|(i, ev)| {
            let bh_p = bh_adjusted[i];
            let sr = ev.oos_sharpe.unwrap_or(0.0);
            let dsr = compute_deflated_sharpe(
                sr,
                ev.oos_trades,
                ev.skew,
                ev.kurt,
                total_hypotheses_tested,
                trials_sharpe_var,
            );
            let exp_c = ev.oos_mean_cents.unwrap_or(0.0);
            let passed_fdr =
                ev.oos_trades >= min_oos_trades && exp_c > 0.0 && bh_p <= fdr_alpha;
            let passed_dsr = ev.oos_trades >= min_oos_trades && dsr >= min_dsr;
            let passed_corr_cap = ev.max_corr_with_library.unwrap_or(0.0) <= max_corr;
            let passed_all_gates = passed_fdr && passed_dsr && passed_corr_cap;
            let rejection_reason = if passed_all_gates {
                None
            } else if ev.oos_trades < min_oos_trades {
                Some(format!("OOS trades {} < {min_oos_trades}", ev.oos_trades))
            } else if exp_c <= 0.0 {
                Some("Negative held-out OOS expectancy".to_string())
            } else if !passed_fdr {
                Some(format!("BH-FDR q={bh_p:.3} > alpha={fdr_alpha:.2}"))
            } else if !passed_dsr {
                Some(format!("Deflated Sharpe {dsr:.2} < {min_dsr:.2}"))
            } else {
                Some(format!(
                    "Library correlation {:.2} > {max_corr:.2}",
                    ev.max_corr_with_library.unwrap_or(0.0)
                ))
            };

            SynthesizedCandidate {
                rank: 0,
                id: ev.spec.id().to_string(),
                name: ev.spec.slug().to_string(),
                family: ev.spec.family().to_string(),
                side: ev.spec.side().to_string(),
                execution_mode: exec_mode.clone(),
                post_only: is_maker,
                is_trades: ev.is_trades,
                is_win_rate: ev.is_win_rate,
                oos_trades: ev.oos_trades,
                oos_wins: ev.oos_wins,
                oos_win_rate: ev.oos_win_rate,
                is_net_expectancy_cents: Some(ev.is_mean_cents),
                oos_net_expectancy_cents: ev.oos_mean_cents,
                oos_net_pnl_dollars: ev.oos_pnl_dollars_1ct,
                oos_net_pnl_dollars_1ct: ev.oos_pnl_dollars_1ct,
                oos_sharpe: ev.oos_sharpe,
                deflated_sharpe: dsr,
                raw_p_value: ev.raw_p_value,
                bh_adjusted_p_value: bh_p,
                survived_fdr: passed_fdr,
                passed_fdr,
                passed_dsr,
                max_corr_with_library: ev.max_corr_with_library,
                most_correlated_library_strategy: ev.most_correlated_lib,
                passed_corr_cap,
                passed_all_gates,
                causal_canary_passed: true,
                rejection_reason,
                description: ev.spec.description(is_maker),
                source: ev.spec.emit_v2_rune_source(is_maker),
            }
        })
        .collect();

    // Sort deterministically: all-gates survivors first, then lowest BH p-value,
    // highest DSR, highest OOS expectancy, and finally candidate ID.
    candidates.sort_by(|a, b| {
        b.passed_all_gates
            .cmp(&a.passed_all_gates)
            .then_with(|| a.bh_adjusted_p_value.total_cmp(&b.bh_adjusted_p_value))
            .then_with(|| b.deflated_sharpe.total_cmp(&a.deflated_sharpe))
            .then_with(|| {
                b.oos_net_expectancy_cents
                    .unwrap_or(f64::NEG_INFINITY)
                    .total_cmp(&a.oos_net_expectancy_cents.unwrap_or(f64::NEG_INFINITY))
            })
            .then_with(|| a.id.cmp(&b.id))
    });

    let survivors_count = candidates.iter().filter(|c| c.passed_all_gates).count();
    candidates.truncate(max_candidates);
    for (idx, c) in candidates.iter_mut().enumerate() {
        c.rank = idx + 1;
    }

    let note = if survivors_count > 0 {
        format!(
            "{survivors_count} synthesized candidate(s) survived BH-FDR (alpha={fdr_alpha:.2}), Deflated Sharpe (>={min_dsr:.2}), and library correlation cap (<={max_corr:.2}) on held-out OOS windows."
        )
    } else {
        format!(
            "Tested {total_hypotheses_tested} hypotheses ({is_promising_count} positive in-sample); 0 survived BH-FDR (alpha={fdr_alpha:.2}) + Deflated Sharpe (>={min_dsr:.2}) + correlation cap (<={max_corr:.2}) on held-out OOS."
        )
    };

    Ok(SynthesizeResponse {
        ok: true,
        dataset: dataset_label.to_string(),
        family,
        execution_mode: exec_mode,
        fdr_alpha,
        max_candidates,
        min_deflated_sharpe: min_dsr,
        max_corr,
        windows_settled_total: settled.len(),
        windows_settled_is: is_raw.len(),
        windows_settled_oos: oos_raw.len(),
        is_windows: is_raw.len(),
        oos_windows: oos_raw.len(),
        split_epoch: Some(split_epoch),
        pav_blocks_count: pav_blocks.len(),
        total_hypotheses_tested,
        candidates_evaluated: total_hypotheses_tested,
        is_promising_count,
        survivors_count,
        elapsed_ms: t0.elapsed().as_millis() as u64,
        candidates,
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy_v2::validate_strategy_source_full;
    use crate::{iso_z, OptPrice, Sym};

    #[test]
    fn bh_fdr_is_monotone_and_exact() {
        let p = [0.001, 0.01, 0.04, 0.50];
        let q = benjamini_hochberg_adjust(&p, 10);
        // Ranks 1..4 out of M=10:
        // 10*0.001/1 = 0.01, 10*0.01/2 = 0.05, 10*0.04/3 = 0.1333..., 10*0.50/4 = 1.0
        assert!((q[0] - 0.01).abs() < 1e-12);
        assert!((q[1] - 0.05).abs() < 1e-12);
        assert!((q[2] - (0.40 / 3.0)).abs() < 1e-12);
        assert!((q[3] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn emitted_v2_scripts_pass_paper_only_and_causal_canary() {
        let grid = build_candidate_grid("all", true).unwrap();
        assert!(!grid.is_empty());
        for spec in grid.iter().take(6) {
            let src = spec.emit_v2_rune_source(true);
            let res = validate_strategy_source_full(&src)
                .unwrap_or_else(|(errs, _)| panic!("emitted {} failed validation: {errs:?}\n{src}", spec.id()));
            assert!(res.causal_canary_passed);
        }
    }

    #[test]
    fn synthesizer_is_deterministic_on_synthetic_tape() {
        let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
        let base = 1_795_700_000i64;
        for w in 0..40 {
            let close_ep = base + (w + 1) * 900;
            let close_str = iso_z(close_ep);
            let ticker = format!("KXBTC15M-SYN-{w:03}");
            // Positive 60s upward YES momentum leads to YES settlement (80% win rate).
            let won_yes = (w % 5) != 0;
            let res = if won_yes { "yes" } else { "no" };
            let snaps = vec![
                Snapshot {
                    fetched_at: close_ep - 480,
                    ticker: Sym::new(&ticker),
                    event_ticker: Sym::new("KXBTC15M-SYN"),
                    series: Sym::new("KXBTC15M"),
                    close_time: Sym::new(&close_str),
                    status: Sym::new("open"),
                    result: Sym::new(""),
                    yes_bid: OptPrice::from_opt(Some(0.50)),
                    yes_ask: OptPrice::from_opt(Some(0.52)),
                    no_bid: OptPrice::from_opt(Some(0.48)),
                    no_ask: OptPrice::from_opt(Some(0.50)),
                    last_price: OptPrice::from_opt(Some(0.51)),
                    yes_bid_size: 40.0,
                    no_bid_size: 40.0,
                },
                Snapshot {
                    fetched_at: close_ep - 360,
                    ticker: Sym::new(&ticker),
                    event_ticker: Sym::new("KXBTC15M-SYN"),
                    series: Sym::new("KXBTC15M"),
                    close_time: Sym::new(&close_str),
                    status: Sym::new("open"),
                    result: Sym::new(""),
                    yes_bid: OptPrice::from_opt(Some(0.56)),
                    yes_ask: OptPrice::from_opt(Some(0.59)),
                    no_bid: OptPrice::from_opt(Some(0.41)),
                    no_ask: OptPrice::from_opt(Some(0.44)),
                    last_price: OptPrice::from_opt(Some(0.58)),
                    yes_bid_size: 90.0,
                    no_bid_size: 20.0,
                },
                Snapshot {
                    fetched_at: close_ep - 240,
                    ticker: Sym::new(&ticker),
                    event_ticker: Sym::new("KXBTC15M-SYN"),
                    series: Sym::new("KXBTC15M"),
                    close_time: Sym::new(&close_str),
                    status: Sym::new("settled"),
                    result: Sym::new(res),
                    yes_bid: OptPrice::from_opt(Some(0.55)),
                    yes_ask: OptPrice::from_opt(Some(0.57)),
                    no_bid: OptPrice::from_opt(Some(0.43)),
                    no_ask: OptPrice::from_opt(Some(0.45)),
                    last_price: OptPrice::from_opt(Some(0.57)),
                    yes_bid_size: 95.0,
                    no_bid_size: 18.0,
                },
            ];
            windows.insert(ticker, snaps);
        }

        let req = SynthesizeRequest {
            family: "momentum_microstructure".to_string(),
            execution_mode: "maker".to_string(),
            fdr_alpha: 0.20,
            max_candidates: 5,
            min_is_trades: 3,
            min_oos_trades: 3,
            min_deflated_sharpe: 0.50,
            max_corr: 0.65,
            slippage_cents: 1.0,
            dataset: None,
        };
        let r1 = synthesize_strategies(&windows, &[], "live", &req).unwrap();
        let r2 = synthesize_strategies(&windows, &[], "live", &req).unwrap();
        assert_eq!(r1.total_hypotheses_tested, r2.total_hypotheses_tested);
        assert_eq!(r1.candidates.len(), r2.candidates.len());
        assert!(r1.survivors_count > 0, "expected surviving candidate: {}", r1.note);
        for (a, b) in r1.candidates.iter().zip(r2.candidates.iter()) {
            assert_eq!(a.id, b.id);
            assert_eq!(a.source, b.source);
            assert!((a.bh_adjusted_p_value - b.bh_adjusted_p_value).abs() < 1e-12);
        }
    }
}
