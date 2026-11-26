//! Adverse-Selection Markout Telemetry, Fill-Rate / Slippage Residuals &
//! Persistent Auto-Quarantine Decay Feedback Loop.
//!
//! Reads `<OPENBELL_DATA>/shadow_ledger.jsonl`, `live_executions.jsonl`,
//! `orderbooks.jsonl`, `markout_samples.jsonl`, `snapshots.jsonl`, and live
//! `OrderbookStore` state to compute per-fill `+10s/+30s/+60s`/settlement
//! markouts, predicted vs realized win rates, assumed vs actual fill rates,
//! and slippage residuals. Maintains `<OPENBELL_DATA>/quarantine_state.json`
//! atomically (`0600`) across restarts.

#![allow(dead_code)]

use crate::orderbook::OrderbookStore;
use crate::risk_rails::LiveExecutionRecord;
use crate::shadow_runner::LedgerEvent;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const ROLLING_WINDOW_FILLS: usize = 20;
pub const MIN_DECAY_FILLS: u64 = 5;
pub const DEFAULT_ASSUMED_MAKER_FILL_RATE_PCT: f64 = 75.0;
pub const DEFAULT_MIN_FILL_RATE_PCT: f64 = 40.0;
pub const DEFAULT_TAKER_SLIPPAGE_CENTS: f64 = 0.5;
pub const QUARANTINE_MARKOUT_30S_CENTS: f64 = -3.0;
pub const THROTTLE_MARKOUT_30S_CENTS: f64 = -1.0;
pub const QUARANTINE_NET_EXPECTANCY_CENTS: f64 = -1.5;
const MAX_JSONL_SCAN_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RECENT_FILLS_PER_STRATEGY: usize = 25;

#[inline]
fn finite_or(v: f64, fallback: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

#[inline]
fn finite_opt(v: f64) -> Option<f64> {
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn quarantine_state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("quarantine_state.json")
}

pub fn markout_samples_path(data_dir: &Path) -> PathBuf {
    data_dir.join("markout_samples.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MidpointObs {
    pub ts_secs: f64,
    pub yes_mid_cents: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkoutSampleRecord {
    pub ts_ms: i64,
    pub ticker: String,
    pub yes_mid_cents: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyQuarantineEntry {
    pub status: String,
    pub throttle_mult: f64,
    pub reason: String,
    pub triggered_at: i64,
    pub rolling_fills: u64,
    pub rolling_markout_30s_cents: f64,
    pub rolling_fill_rate_pct: f64,
    pub rolling_net_pnl_dollars: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_after_ts: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_after_seq: Option<u64>,
    #[serde(default)]
    pub manual_quarantine: bool,
}

impl Default for StrategyQuarantineEntry {
    fn default() -> Self {
        Self {
            status: "active".to_string(),
            throttle_mult: 1.0,
            reason: String::new(),
            triggered_at: 0,
            rolling_fills: 0,
            rolling_markout_30s_cents: 0.0,
            rolling_fill_rate_pct: 100.0,
            rolling_net_pnl_dollars: 0.0,
            reset_after_ts: None,
            reset_after_seq: None,
            manual_quarantine: false,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuarantineStateFile {
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default)]
    pub strategies: BTreeMap<String, StrategyQuarantineEntry>,
}

impl QuarantineStateFile {
    pub fn find_entry(&self, strategy: &str) -> Option<&StrategyQuarantineEntry> {
        self.strategies
            .get(strategy)
            .or_else(|| self.strategies.get(&strategy.replace('-', "_")))
            .or_else(|| self.strategies.get(&strategy.replace('_', "-")))
    }

    pub fn is_quarantined(&self, strategy: &str) -> bool {
        self.find_entry(strategy)
            .is_some_and(|e| e.status == "quarantined" || e.throttle_mult <= 0.0)
    }

    pub fn throttle_mult(&self, strategy: &str) -> f64 {
        match self.find_entry(strategy) {
            Some(e) if e.status == "quarantined" => 0.0,
            Some(e) if e.status == "throttled" => finite_or(e.throttle_mult, 0.5).clamp(0.05, 1.0),
            Some(e) => finite_or(e.throttle_mult, 1.0).clamp(0.0, 1.0),
            None => 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillAuditRecord {
    pub order_id: String,
    pub strategy: String,
    pub ticker: String,
    pub series: String,
    pub mode: String,
    pub execution_mode: String,
    pub side: String,
    pub ts: i64,
    pub fill_cents: u8,
    pub contracts: u32,
    pub fee_dollars: f64,
    pub markout_10s_cents: Option<f64>,
    pub markout_30s_cents: Option<f64>,
    pub markout_60s_cents: Option<f64>,
    pub settlement_markout_cents: Option<f64>,
    pub predicted_prob: Option<f64>,
    pub expected_slippage_cents: f64,
    pub realized_slippage_cents: f64,
    pub realized_net_pnl_dollars: Option<f64>,
    pub won: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyExecutionHealth {
    pub strategy: String,
    pub quotes_posted: u64,
    pub fills: u64,
    pub rolling_fills: u64,
    pub settled_fills: u64,
    pub predicted_prob: Option<f64>,
    pub realized_win_rate: Option<f64>,
    pub predicted_win_rate_pct: Option<f64>,
    pub realized_win_rate_pct: Option<f64>,
    pub win_rate_residual_pct: Option<f64>,
    pub assumed_fill_rate_pct: f64,
    pub actual_fill_rate_pct: f64,
    pub realized_fill_rate_pct: f64,
    pub fill_rate_residual_pct: f64,
    pub expected_slippage_cents: f64,
    pub realized_slippage_cents: f64,
    pub slippage_residual_cents: f64,
    pub markout_10s_cents: Option<f64>,
    pub markout_30s_cents: Option<f64>,
    pub markout_60s_cents: Option<f64>,
    pub settlement_markout_cents: Option<f64>,
    pub realized_net_pnl_dollars: f64,
    pub rolling_net_pnl_dollars: f64,
    pub status: String,
    pub throttle_mult: f64,
    pub reason: String,
    pub quarantine_reason: String,
    pub recent_fills: Vec<FillAuditRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OverallExecutionHealth {
    pub quotes_posted: u64,
    pub total_fills: u64,
    pub settled_fills: u64,
    pub predicted_win_rate_pct: Option<f64>,
    pub realized_win_rate_pct: Option<f64>,
    pub assumed_fill_rate_pct: f64,
    pub actual_fill_rate_pct: f64,
    pub expected_slippage_cents: f64,
    pub realized_slippage_cents: f64,
    pub slippage_residual_cents: f64,
    pub markout_10s_cents: Option<f64>,
    pub markout_30s_cents: Option<f64>,
    pub markout_60s_cents: Option<f64>,
    pub settlement_markout_cents: Option<f64>,
    pub realized_net_pnl_dollars: f64,
    pub active_strategies: usize,
    pub throttled_strategies: usize,
    pub quarantined_strategies: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecutionHealthSummary {
    pub total_fills: u64,
    pub active_strategies: usize,
    pub throttled_strategies: usize,
    pub quarantined_strategies: usize,
    pub avg_markout_10s_cents: Option<f64>,
    pub avg_markout_30s_cents: Option<f64>,
    pub avg_markout_60s_cents: Option<f64>,
    pub avg_settlement_markout_cents: Option<f64>,
    pub actual_fill_rate_pct: Option<f64>,
    pub avg_slippage_residual_cents: Option<f64>,
    pub quarantined_names: Vec<String>,
    pub throttled_names: Vec<String>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionHealthReport {
    pub ok: bool,
    pub strategies: Vec<StrategyExecutionHealth>,
    pub quarantine_state: QuarantineStateFile,
    pub overall: OverallExecutionHealth,
}

impl ExecutionHealthReport {
    pub fn to_summary(&self) -> ExecutionHealthSummary {
        let mut quarantined_names = Vec::new();
        let mut throttled_names = Vec::new();
        for (name, entry) in &self.quarantine_state.strategies {
            if entry.status == "quarantined" {
                quarantined_names.push(name.clone());
            } else if entry.status == "throttled" {
                throttled_names.push(name.clone());
            }
        }
        let actual_fill_rate_pct = if self.overall.quotes_posted > 0 || self.overall.total_fills > 0 {
            Some(self.overall.actual_fill_rate_pct)
        } else {
            None
        };
        let avg_slippage_residual_cents = if self.overall.total_fills > 0 {
            Some(self.overall.slippage_residual_cents)
        } else {
            None
        };
        let summary = if self.overall.total_fills == 0 && quarantined_names.is_empty() && throttled_names.is_empty() {
            "No shadow/live execution fills recorded yet.".to_string()
        } else {
            format!(
                "{} fills: 30s markout {}, fill rate {:.1}%, active {}, throttled {}, quarantined {}",
                self.overall.total_fills,
                self.overall
                    .markout_30s_cents
                    .map(|v| format!("{v:+.2}c"))
                    .unwrap_or_else(|| "n/a".to_string()),
                self.overall.actual_fill_rate_pct,
                self.overall.active_strategies,
                self.overall.throttled_strategies,
                self.overall.quarantined_strategies,
            )
        };
        ExecutionHealthSummary {
            total_fills: self.overall.total_fills,
            active_strategies: self.overall.active_strategies,
            throttled_strategies: self.overall.throttled_strategies,
            quarantined_strategies: self.overall.quarantined_strategies,
            avg_markout_10s_cents: self.overall.markout_10s_cents,
            avg_markout_30s_cents: self.overall.markout_30s_cents,
            avg_markout_60s_cents: self.overall.markout_60s_cents,
            avg_settlement_markout_cents: self.overall.settlement_markout_cents,
            actual_fill_rate_pct,
            avg_slippage_residual_cents,
            quarantined_names,
            throttled_names,
            summary,
        }
    }
}

fn check_regular_non_symlink(data_dir: &Path, path: &Path) -> Result<bool, String> {
    if data_dir.as_os_str().is_empty() {
        return Ok(false);
    }
    match std::fs::symlink_metadata(data_dir) {
        Ok(m) => {
            if m.file_type().is_symlink() || !m.file_type().is_dir() {
                return Err("data_dir must be a regular directory".to_string());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(format!("stat data_dir: {e}")),
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            if m.file_type().is_symlink() || !m.file_type().is_file() {
                return Err(format!("{} must be a regular non-symlink file", path.display()));
            }
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("stat {}: {e}", path.display())),
    }
}

pub fn load_quarantine_state(data_dir: &Path) -> QuarantineStateFile {
    let path = quarantine_state_path(data_dir);
    if !matches!(check_regular_non_symlink(data_dir, &path), Ok(true)) {
        return QuarantineStateFile::default();
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return QuarantineStateFile::default();
    };
    let mut parsed = serde_json::from_str::<QuarantineStateFile>(&raw).unwrap_or_default();
    for entry in parsed.strategies.values_mut() {
        entry.status = match entry.status.as_str() {
            "quarantined" => "quarantined".to_string(),
            "throttled" => "throttled".to_string(),
            _ => "active".to_string(),
        };
        entry.throttle_mult = if entry.status == "quarantined" {
            0.0
        } else if entry.status == "throttled" {
            finite_or(entry.throttle_mult, 0.5).clamp(0.05, 1.0)
        } else {
            1.0
        };
        entry.rolling_markout_30s_cents = finite_or(entry.rolling_markout_30s_cents, 0.0);
        entry.rolling_fill_rate_pct = finite_or(entry.rolling_fill_rate_pct, 100.0).clamp(0.0, 100.0);
        entry.rolling_net_pnl_dollars = finite_or(entry.rolling_net_pnl_dollars, 0.0);
    }
    parsed
}

pub fn save_quarantine_state(data_dir: &Path, state: &QuarantineStateFile) -> Result<(), String> {
    if data_dir.as_os_str().is_empty() {
        return Ok(());
    }
    if let Ok(m) = std::fs::symlink_metadata(data_dir) {
        if m.file_type().is_symlink() || !m.file_type().is_dir() {
            return Err("refusing to write under symlinked data_dir".to_string());
        }
    } else {
        std::fs::create_dir_all(data_dir).map_err(|e| format!("create data dir: {e}"))?;
    }
    let path = quarantine_state_path(data_dir);
    if let Ok(m) = std::fs::symlink_metadata(&path) {
        if m.file_type().is_symlink() {
            return Err("refusing to overwrite symlink quarantine_state.json".to_string());
        }
    }
    let body = serde_json::to_vec_pretty(state).map_err(|e| format!("serialize quarantine_state: {e}"))?;
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .map_err(|e| format!("open temp quarantine_state: {e}"))?;
        f.write_all(&body)
            .map_err(|e| format!("write temp quarantine_state: {e}"))?;
        let _ = f.flush();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename quarantine_state.json: {e}")
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn append_markout_sample(data_dir: &Path, rec: &MarkoutSampleRecord) {
    if data_dir.as_os_str().is_empty() || !rec.yes_mid_cents.is_finite() {
        return;
    }
    let path = markout_samples_path(data_dir);
    if let Ok(m) = std::fs::symlink_metadata(&path) {
        if m.file_type().is_symlink() {
            return;
        }
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut line) = serde_json::to_string(rec) else {
        return;
    };
    line.push('\n');
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    if let Ok(mut f) = opts.open(&path) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn read_shadow_ledger(data_dir: &Path) -> Vec<LedgerEvent> {
    let path = data_dir.join("shadow_ledger.jsonl");
    if !matches!(check_regular_non_symlink(data_dir, &path), Ok(true)) {
        return Vec::new();
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|l| {
            let t = l.trim();
            if t.is_empty() {
                None
            } else {
                serde_json::from_str::<LedgerEvent>(t).ok()
            }
        })
        .collect()
}

#[derive(Deserialize)]
struct RawLiveExecLine {
    #[serde(flatten)]
    base: LiveExecutionRecord,
    #[serde(default)]
    strategy: Option<String>,
}

fn read_live_executions(data_dir: &Path) -> Vec<RawLiveExecLine> {
    let path = data_dir.join("live_executions.jsonl");
    if !matches!(check_regular_non_symlink(data_dir, &path), Ok(true)) {
        return Vec::new();
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|l| {
            let t = l.trim();
            if t.is_empty() {
                None
            } else {
                serde_json::from_str::<RawLiveExecLine>(t).ok()
            }
        })
        .collect()
}

fn read_orderbook_midpoints(
    data_dir: &Path,
    midpoints: &mut HashMap<String, Vec<MidpointObs>>,
) {
    #[derive(Deserialize)]
    struct RawOb {
        ts_ms: i64,
        ticker: String,
        #[serde(default)]
        yes_bids: Vec<[u64; 2]>,
        #[serde(default)]
        no_bids: Vec<[u64; 2]>,
        #[serde(default)]
        best_yes_bid: Option<u8>,
        #[serde(default)]
        best_yes_ask: Option<u8>,
        #[serde(default)]
        mid_cents: Option<f64>,
    }

    let path = data_dir.join("orderbooks.jsonl");
    if !matches!(check_regular_non_symlink(data_dir, &path), Ok(true)) {
        return;
    }
    let Ok(meta) = std::fs::metadata(&path) else {
        return;
    };
    let Ok(mut file) = std::fs::File::open(&path) else {
        return;
    };
    let mut skip_first = false;
    if meta.len() > MAX_JSONL_SCAN_BYTES {
        let start = meta.len().saturating_sub(MAX_JSONL_SCAN_BYTES);
        if file.seek(SeekFrom::Start(start)).is_ok() {
            skip_first = true;
        }
    }
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        if skip_first {
            skip_first = false;
            line.clear();
            continue;
        }
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if let Ok(raw) = serde_json::from_str::<RawOb>(trimmed) {
                let best_yb = raw
                    .best_yes_bid
                    .map(|v| v as f64)
                    .or_else(|| raw.yes_bids.iter().map(|x| x[0]).max().map(|v| v as f64));
                let best_ya = raw
                    .best_yes_ask
                    .map(|v| v as f64)
                    .or_else(|| raw.no_bids.iter().map(|x| x[0]).max().map(|v| (100u64.saturating_sub(v)) as f64));
                let mid = raw
                    .mid_cents
                    .filter(|v| v.is_finite())
                    .or(match (best_yb, best_ya) {
                        (Some(b), Some(a)) => Some((b + a) * 0.5),
                        (Some(b), None) => Some(b),
                        (None, Some(a)) => Some(a),
                        (None, None) => None,
                    });
                if let Some(m) = mid.filter(|v| v.is_finite() && (0.0..=100.0).contains(v)) {
                    midpoints.entry(raw.ticker).or_default().push(MidpointObs {
                        ts_secs: (raw.ts_ms as f64) / 1000.0,
                        yes_mid_cents: m,
                    });
                }
            }
        }
        line.clear();
    }
}

fn read_markout_samples(
    data_dir: &Path,
    midpoints: &mut HashMap<String, Vec<MidpointObs>>,
) {
    let path = markout_samples_path(data_dir);
    if !matches!(check_regular_non_symlink(data_dir, &path), Ok(true)) {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return;
    };
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(rec) = serde_json::from_str::<MarkoutSampleRecord>(trimmed) {
            if rec.yes_mid_cents.is_finite() && (0.0..=100.0).contains(&rec.yes_mid_cents) {
                midpoints.entry(rec.ticker).or_default().push(MidpointObs {
                    ts_secs: (rec.ts_ms as f64) / 1000.0,
                    yes_mid_cents: rec.yes_mid_cents,
                });
            }
        }
    }
}

fn read_snapshots_fallback(
    data_dir: &Path,
    midpoints: &mut HashMap<String, Vec<MidpointObs>>,
    settlements: &mut HashMap<String, String>,
) {
    #[derive(Deserialize)]
    struct RawSnap {
        fetched_at: i64,
        ticker: String,
        #[serde(default)]
        result: String,
        #[serde(default)]
        yes_bid: Option<f64>,
        #[serde(default)]
        yes_ask: Option<f64>,
        #[serde(default)]
        no_bid: Option<f64>,
        #[serde(default)]
        no_ask: Option<f64>,
        #[serde(default)]
        last_price: Option<f64>,
    }

    let path = data_dir.join("snapshots.jsonl");
    if !matches!(check_regular_non_symlink(data_dir, &path), Ok(true)) {
        return;
    }
    let Ok(file) = std::fs::File::open(&path) else {
        return;
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            if let Ok(s) = serde_json::from_str::<RawSnap>(trimmed) {
                let res = s.result.trim().to_ascii_lowercase();
                if res == "yes" || res == "no" {
                    settlements.insert(s.ticker.clone(), res);
                } else {
                    let yes_mid = match (s.yes_bid, s.yes_ask) {
                        (Some(yb), Some(ya)) if yb.is_finite() && ya.is_finite() => {
                            Some((yb + ya) * 50.0)
                        }
                        _ => match (s.no_bid, s.no_ask) {
                            (Some(nb), Some(na)) if nb.is_finite() && na.is_finite() => {
                                Some(100.0 - (nb + na) * 50.0)
                            }
                            _ => s.last_price.filter(|v| v.is_finite()).map(|v| v * 100.0),
                        },
                    };
                    if let Some(m) = yes_mid.filter(|v| v.is_finite() && (0.0..=100.0).contains(v)) {
                        midpoints.entry(s.ticker).or_default().push(MidpointObs {
                            ts_secs: s.fetched_at as f64,
                            yes_mid_cents: m,
                        });
                    }
                }
            }
        }
        line.clear();
    }
}

fn find_markout_at_horizon(
    mids: &[MidpointObs],
    fill_ts: i64,
    horizon_secs: i64,
    max_lag_secs: f64,
    side: &str,
    fill_cents: u8,
) -> Option<f64> {
    if mids.is_empty() {
        return None;
    }
    let target = (fill_ts + horizon_secs) as f64;
    let min_ts = target - 1.5;
    let max_ts = target + max_lag_secs;

    let mut best_after: Option<(f64, f64)> = None;
    let mut best_before: Option<(f64, f64)> = None;
    for obs in mids {
        if obs.ts_secs < min_ts {
            continue;
        }
        if obs.ts_secs > max_ts {
            break;
        }
        if obs.ts_secs >= target {
            let dist = obs.ts_secs - target;
            if best_after.is_none_or(|(bd, _)| dist < bd) {
                best_after = Some((dist, obs.yes_mid_cents));
            }
        } else {
            let dist = target - obs.ts_secs;
            if best_before.is_none_or(|(bd, _)| dist < bd) {
                best_before = Some((dist, obs.yes_mid_cents));
            }
        }
    }

    let yes_mid = best_after.or(best_before)?.1;
    let side_mid = if side == "no" {
        100.0 - yes_mid
    } else {
        yes_mid
    };
    finite_opt(side_mid - (fill_cents as f64))
}

#[derive(Clone)]
struct InternalFill {
    seq: u64,
    record: FillAuditRecord,
}

fn avg_opt<I: Iterator<Item = Option<f64>>>(iter: I) -> Option<f64> {
    let mut sum = 0.0;
    let mut cnt = 0u64;
    for v in iter.flatten() {
        if v.is_finite() {
            sum += v;
            cnt += 1;
        }
    }
    if cnt > 0 {
        finite_opt(sum / cnt as f64)
    } else {
        None
    }
}

pub fn evaluate_execution_health(
    data_dir: &Path,
    orderbook: Option<&OrderbookStore>,
    tape_midpoints: Option<&HashMap<String, Vec<MidpointObs>>>,
    tape_settlements: Option<&HashMap<String, String>>,
    persist_quarantine: bool,
) -> ExecutionHealthReport {
    let ledger = read_shadow_ledger(data_dir);
    let live_execs = read_live_executions(data_dir);
    let mut q_state = load_quarantine_state(data_dir);
    let prev_q_json = serde_json::to_string(&q_state).unwrap_or_default();

    let mut midpoints: HashMap<String, Vec<MidpointObs>> = HashMap::new();
    let mut settlements: HashMap<String, String> = HashMap::new();

    if let Some(tm) = tape_midpoints {
        for (t, list) in tm {
            midpoints.entry(t.clone()).or_default().extend(list.iter().cloned());
        }
    }
    if let Some(ts) = tape_settlements {
        for (t, r) in ts {
            settlements.insert(t.clone(), r.clone());
        }
    }
    if tape_midpoints.is_none() || tape_settlements.is_none() {
        let mut fb_mids = HashMap::new();
        let mut fb_settle = HashMap::new();
        read_snapshots_fallback(data_dir, &mut fb_mids, &mut fb_settle);
        if tape_midpoints.is_none() {
            for (t, list) in fb_mids {
                midpoints.entry(t).or_default().extend(list);
            }
        }
        if tape_settlements.is_none() {
            for (t, r) in fb_settle {
                settlements.entry(t).or_insert(r);
            }
        }
    }

    read_orderbook_midpoints(data_dir, &mut midpoints);
    read_markout_samples(data_dir, &mut midpoints);

    let mut quotes_by_strat: BTreeMap<String, Vec<(i64, u64)>> = BTreeMap::new();
    let mut quote_meta_by_order: HashMap<String, (String, u8, String)> = HashMap::new();
    let mut quote_meta_by_live_id: HashMap<String, (String, u8, String)> = HashMap::new();
    let mut settled_by_order: HashMap<String, (String, f64)> = HashMap::new();
    let mut fill_tickers: HashSet<String> = HashSet::new();
    let mut shadow_live_fill_keys: HashSet<(String, i64, u8, u32)> = HashSet::new();
    let mut max_seq_seen = 0u64;

    for ev in &ledger {
        if ev.seq > max_seq_seen {
            max_seq_seen = ev.seq;
        }
        match ev.event.as_str() {
            "QuotePosted" => {
                if !ev.strategy.is_empty() {
                    quotes_by_strat
                        .entry(ev.strategy.clone())
                        .or_default()
                        .push((ev.ts, ev.seq));
                    if !ev.order_id.is_empty() {
                        quote_meta_by_order.insert(
                            ev.order_id.clone(),
                            (ev.strategy.clone(), ev.limit_cents, ev.execution_mode.clone()),
                        );
                    }
                    if let Some(ref lid) = ev.live_order_id {
                        quote_meta_by_live_id.insert(
                            lid.clone(),
                            (ev.strategy.clone(), ev.limit_cents, ev.execution_mode.clone()),
                        );
                    }
                }
            }
            "OrderFilled" => {
                if !ev.ticker.is_empty() {
                    fill_tickers.insert(ev.ticker.clone());
                }
                if let Some(ref lid) = ev.live_order_id {
                    let fc = ev.fill_cents.unwrap_or(ev.limit_cents).clamp(1, 99);
                    shadow_live_fill_keys.insert((lid.clone(), ev.ts, fc, ev.contracts));
                }
            }
            "WindowSettled" => {
                if let Some(ref res) = ev.result {
                    let r = res.trim().to_ascii_lowercase();
                    if r == "yes" || r == "no" {
                        if !ev.ticker.is_empty() {
                            settlements.entry(ev.ticker.clone()).or_insert_with(|| r.clone());
                        }
                        if !ev.order_id.is_empty() {
                            settled_by_order.insert(
                                ev.order_id.clone(),
                                (r, ev.realized_pnl_dollars.unwrap_or(0.0)),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut live_submitted_meta: HashMap<String, (Option<String>, u8, Option<bool>)> = HashMap::new();
    for rec in &live_execs {
        match rec.base.kind.as_str() {
            "order_submitted" => {
                live_submitted_meta.insert(
                    rec.base.order_id.clone(),
                    (
                        rec.base.client_order_id.clone(),
                        rec.base.price_cents,
                        rec.base.is_taker,
                    ),
                );
            }
            "fill" => {
                if !rec.base.ticker.is_empty() {
                    fill_tickers.insert(rec.base.ticker.clone());
                }
            }
            "settlement" => {
                if !rec.base.ticker.is_empty() && !settlements.contains_key(&rec.base.ticker) {
                    if let Some(rev) = rec.base.revenue_cents {
                        if rec.base.count > 0 {
                            let won = rev > 0;
                            let res = if (rec.base.side == "yes" && won)
                                || (rec.base.side == "no" && !won)
                            {
                                "yes"
                            } else {
                                "no"
                            };
                            settlements.insert(rec.base.ticker.clone(), res.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if let Some(ob_store) = orderbook {
        for t in &fill_tickers {
            let snap = ob_store.snapshot(t);
            if snap.source != "empty" && !snap.needs_resync {
                if let Some(mid) = snap.mid_cents.filter(|v| v.is_finite() && (0.0..=100.0).contains(v)) {
                    midpoints.entry(t.clone()).or_default().push(MidpointObs {
                        ts_secs: (snap.updated_ms as f64) / 1000.0,
                        yes_mid_cents: mid,
                    });
                }
            }
        }
    }

    for list in midpoints.values_mut() {
        list.sort_by(|a, b| a.ts_secs.total_cmp(&b.ts_secs));
    }

    let mut fills_by_strat: BTreeMap<String, Vec<InternalFill>> = BTreeMap::new();
    for ev in &ledger {
        if ev.event != "OrderFilled" || ev.strategy.is_empty() || ev.contracts == 0 {
            continue;
        }
        let fill_cents = ev.fill_cents.unwrap_or(ev.limit_cents).clamp(1, 99);
        let side = if ev.side.eq_ignore_ascii_case("no") {
            "no".to_string()
        } else {
            "yes".to_string()
        };
        let exec_mode = if ev.execution_mode.eq_ignore_ascii_case("taker") {
            "taker".to_string()
        } else {
            "maker".to_string()
        };
        let expected_slippage_cents = if exec_mode == "taker" {
            DEFAULT_TAKER_SLIPPAGE_CENTS
        } else {
            0.0
        };
        let realized_slippage_cents = finite_or(
            ev.slippage_cents
                .unwrap_or((fill_cents as f64) - (ev.limit_cents as f64)),
            0.0,
        );
        let fee_dollars = finite_or(ev.fee_dollars.unwrap_or(0.0).max(0.0), 0.0);
        let mids = midpoints.get(&ev.ticker).map(Vec::as_slice).unwrap_or(&[]);
        let m10 = find_markout_at_horizon(mids, ev.ts, 10, 25.0, &side, fill_cents);
        let m30 = find_markout_at_horizon(mids, ev.ts, 30, 45.0, &side, fill_cents);
        let m60 = find_markout_at_horizon(mids, ev.ts, 60, 60.0, &side, fill_cents);

        let settle_res = settled_by_order
            .get(&ev.order_id)
            .map(|(r, _)| r.clone())
            .or_else(|| settlements.get(&ev.ticker).cloned());
        let (won, settlement_markout_cents, realized_net_pnl_dollars) =
            if let Some(res) = settle_res {
                let w = side == res;
                let payout_cents = if w { 100.0 } else { 0.0 };
                let s_markout = payout_cents - (fill_cents as f64);
                let pnl = (payout_cents - (fill_cents as f64)) / 100.0 * (ev.contracts as f64)
                    - fee_dollars;
                (Some(w), finite_opt(s_markout), finite_opt(pnl))
            } else {
                (None, None, None)
            };

        let predicted_prob = finite_opt((ev.limit_cents as f64 / 100.0).clamp(0.01, 0.99));

        fills_by_strat
            .entry(ev.strategy.clone())
            .or_default()
            .push(InternalFill {
                seq: ev.seq,
                record: FillAuditRecord {
                    order_id: ev.order_id.clone(),
                    strategy: ev.strategy.clone(),
                    ticker: ev.ticker.clone(),
                    series: ev.series.clone(),
                    mode: ev.mode.clone(),
                    execution_mode: exec_mode,
                    side,
                    ts: ev.ts,
                    fill_cents,
                    contracts: ev.contracts,
                    fee_dollars,
                    markout_10s_cents: m10,
                    markout_30s_cents: m30,
                    markout_60s_cents: m60,
                    settlement_markout_cents,
                    predicted_prob,
                    expected_slippage_cents,
                    realized_slippage_cents,
                    realized_net_pnl_dollars,
                    won,
                },
            });
    }

    for rec in &live_execs {
        if rec.base.kind != "fill" || rec.base.count <= 0 {
            continue;
        }
        let contracts = u32::try_from(rec.base.count).unwrap_or(0);
        if contracts == 0 {
            continue;
        }
        let fill_cents = rec.base.price_cents.clamp(1, 99);
        if shadow_live_fill_keys.contains(&(rec.base.order_id.clone(), rec.base.ts, fill_cents, contracts)) {
            continue;
        }
        let sub_meta = live_submitted_meta.get(&rec.base.order_id);
        let q_meta = quote_meta_by_live_id
            .get(&rec.base.order_id)
            .or_else(|| {
                rec.base
                    .client_order_id
                    .as_deref()
                    .and_then(|cid| quote_meta_by_order.get(cid))
            })
            .or_else(|| {
                sub_meta
                    .and_then(|(cid, _, _)| cid.as_deref())
                    .and_then(|cid| quote_meta_by_order.get(cid))
            });
        let strategy = rec
            .strategy
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| q_meta.map(|(s, _, _)| s.clone()))
            .unwrap_or_else(|| "live_direct".to_string());
        let side = if rec.base.side.eq_ignore_ascii_case("no") {
            "no".to_string()
        } else {
            "yes".to_string()
        };
        let is_taker = rec
            .base
            .is_taker
            .or_else(|| sub_meta.and_then(|(_, _, t)| *t))
            .unwrap_or_else(|| q_meta.is_some_and(|(_, _, m)| m == "taker"));
        let exec_mode = if is_taker { "taker" } else { "maker" }.to_string();
        let limit_cents = q_meta
            .map(|(_, l, _)| *l)
            .or_else(|| sub_meta.map(|(_, l, _)| *l))
            .unwrap_or(fill_cents);
        let expected_slippage_cents = if is_taker {
            DEFAULT_TAKER_SLIPPAGE_CENTS
        } else {
            0.0
        };
        let realized_slippage_cents = (fill_cents as f64) - (limit_cents as f64);
        let fee_dollars = finite_or(rec.base.fee_dollars.max(0.0), 0.0);

        let mids = midpoints
            .get(&rec.base.ticker)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let m10 = find_markout_at_horizon(mids, rec.base.ts, 10, 25.0, &side, fill_cents);
        let m30 = find_markout_at_horizon(mids, rec.base.ts, 30, 45.0, &side, fill_cents);
        let m60 = find_markout_at_horizon(mids, rec.base.ts, 60, 60.0, &side, fill_cents);

        let (won, settlement_markout_cents, realized_net_pnl_dollars) =
            if let Some(res) = settlements.get(&rec.base.ticker) {
                let w = side == *res;
                let payout_cents = if w { 100.0 } else { 0.0 };
                let s_markout = payout_cents - (fill_cents as f64);
                let pnl = (payout_cents - (fill_cents as f64)) / 100.0 * (contracts as f64)
                    - fee_dollars;
                (Some(w), finite_opt(s_markout), finite_opt(pnl))
            } else {
                (None, None, None)
            };

        let series = rec
            .base
            .ticker
            .split('-')
            .next()
            .unwrap_or("")
            .to_string();

        fills_by_strat
            .entry(strategy.clone())
            .or_default()
            .push(InternalFill {
                seq: max_seq_seen,
                record: FillAuditRecord {
                    order_id: rec.base.order_id.clone(),
                    strategy,
                    ticker: rec.base.ticker.clone(),
                    series,
                    mode: "live".to_string(),
                    execution_mode: exec_mode,
                    side,
                    ts: rec.base.ts,
                    fill_cents,
                    contracts,
                    fee_dollars,
                    markout_10s_cents: m10,
                    markout_30s_cents: m30,
                    markout_60s_cents: m60,
                    settlement_markout_cents,
                    predicted_prob: finite_opt((limit_cents as f64 / 100.0).clamp(0.01, 0.99)),
                    expected_slippage_cents,
                    realized_slippage_cents,
                    realized_net_pnl_dollars,
                    won,
                },
            });
    }

    let mut all_strats: BTreeMap<String, ()> = BTreeMap::new();
    for s in fills_by_strat.keys() {
        all_strats.insert(s.clone(), ());
    }
    for s in quotes_by_strat.keys() {
        all_strats.insert(s.clone(), ());
    }
    for s in q_state.strategies.keys() {
        all_strats.insert(s.clone(), ());
    }

    let now = now_unix();
    let mut strategy_reports: Vec<StrategyExecutionHealth> = Vec::with_capacity(all_strats.len());
    let mut all_fills_flat: Vec<&FillAuditRecord> = Vec::new();
    let mut total_quotes_posted = 0u64;
    let mut total_assumed_fill_weight = 0.0f64;

    for strat in all_strats.keys() {
        let strat_fills = fills_by_strat.get(strat).map(Vec::as_slice).unwrap_or(&[]);
        let strat_quotes = quotes_by_strat.get(strat).map(Vec::as_slice).unwrap_or(&[]);

        let quotes_posted = strat_quotes.len() as u64;
        let fills_count = strat_fills.len() as u64;
        total_quotes_posted = total_quotes_posted.saturating_add(quotes_posted);

        let maker_fills = strat_fills
            .iter()
            .filter(|f| f.record.execution_mode == "maker")
            .count();
        let assumed_fill_rate_pct = if fills_count > 0 && maker_fills == 0 {
            100.0
        } else {
            DEFAULT_ASSUMED_MAKER_FILL_RATE_PCT
        };
        let weight_q = quotes_posted.max(fills_count).max(1) as f64;
        total_assumed_fill_weight += assumed_fill_rate_pct * weight_q;

        let actual_fill_rate_pct = if quotes_posted > 0 {
            ((fills_count as f64 / quotes_posted as f64) * 100.0).clamp(0.0, 100.0)
        } else if fills_count > 0 {
            100.0
        } else {
            0.0
        };
        let fill_rate_residual_pct = finite_or(actual_fill_rate_pct - assumed_fill_rate_pct, 0.0);

        let settled_vec: Vec<&InternalFill> = strat_fills
            .iter()
            .filter(|f| f.record.won.is_some())
            .collect();
        let settled_fills = settled_vec.len() as u64;

        let predicted_win_rate_pct = avg_opt(
            settled_vec
                .iter()
                .map(|f| f.record.predicted_prob.map(|p| p * 100.0)),
        )
        .or_else(|| {
            avg_opt(
                strat_fills
                    .iter()
                    .map(|f| f.record.predicted_prob.map(|p| p * 100.0)),
            )
        });
        let realized_win_rate_pct = if settled_fills > 0 {
            let wins = settled_vec.iter().filter(|f| f.record.won == Some(true)).count();
            finite_opt((wins as f64 / settled_fills as f64) * 100.0)
        } else {
            None
        };
        let win_rate_residual_pct = match (realized_win_rate_pct, predicted_win_rate_pct) {
            (Some(r), Some(p)) => finite_opt(r - p),
            _ => None,
        };

        let expected_slippage_cents = avg_opt(
            strat_fills
                .iter()
                .map(|f| finite_opt(f.record.expected_slippage_cents)),
        )
        .unwrap_or(0.0);
        let realized_slippage_cents = avg_opt(
            strat_fills
                .iter()
                .map(|f| finite_opt(f.record.realized_slippage_cents)),
        )
        .unwrap_or(0.0);
        let slippage_residual_cents = finite_or(realized_slippage_cents - expected_slippage_cents, 0.0);

        let m10 = avg_opt(strat_fills.iter().map(|f| f.record.markout_10s_cents));
        let m30 = avg_opt(strat_fills.iter().map(|f| f.record.markout_30s_cents));
        let m60 = avg_opt(strat_fills.iter().map(|f| f.record.markout_60s_cents));
        let m_settle = avg_opt(strat_fills.iter().map(|f| f.record.settlement_markout_cents));
        let total_net_pnl_dollars: f64 = strat_fills
            .iter()
            .filter_map(|f| f.record.realized_net_pnl_dollars)
            .sum();

        for f in strat_fills {
            all_fills_flat.push(&f.record);
        }

        // Evaluate rolling decay since any manual reset.
        let prev_entry = q_state.strategies.get(strat).cloned();
        let reset_ts = prev_entry.as_ref().and_then(|e| e.reset_after_ts);
        let reset_seq = prev_entry.as_ref().and_then(|e| e.reset_after_seq);

        let post_reset_fills: Vec<&InternalFill> = strat_fills
            .iter()
            .filter(|f| {
                let ts_ok = reset_ts.is_none_or(|rt| f.record.ts > rt);
                let seq_ok = reset_seq.is_none_or(|rs| f.seq > rs || f.seq == 0);
                ts_ok && seq_ok
            })
            .collect();
        let rolling_start = post_reset_fills.len().saturating_sub(ROLLING_WINDOW_FILLS);
        let rolling_slice = &post_reset_fills[rolling_start..];
        let rolling_fills = rolling_slice.len() as u64;
        let rolling_m30_vals: Vec<f64> = rolling_slice
            .iter()
            .filter_map(|f| f.record.markout_30s_cents)
            .collect();
        let rolling_markout_30s_cents = if rolling_m30_vals.is_empty() {
            0.0
        } else {
            finite_or(
                rolling_m30_vals.iter().sum::<f64>() / rolling_m30_vals.len() as f64,
                0.0,
            )
        };
        let rolling_settled: Vec<&&InternalFill> = rolling_slice
            .iter()
            .filter(|f| f.record.realized_net_pnl_dollars.is_some())
            .collect();
        let rolling_settled_count = rolling_settled.len() as u64;
        let rolling_net_pnl_dollars: f64 = rolling_settled
            .iter()
            .filter_map(|f| f.record.realized_net_pnl_dollars)
            .sum();
        let rolling_settled_contracts: u64 = rolling_settled
            .iter()
            .map(|f| f.record.contracts as u64)
            .sum();
        let rolling_avg_net_ev_cents = if rolling_settled_contracts > 0 {
            finite_or(
                (rolling_net_pnl_dollars * 100.0) / (rolling_settled_contracts as f64),
                0.0,
            )
        } else {
            0.0
        };

        let post_reset_quotes = strat_quotes
            .iter()
            .filter(|(qts, qseq)| {
                let ts_ok = reset_ts.is_none_or(|rt| *qts > rt);
                let seq_ok = reset_seq.is_none_or(|rs| *qseq > rs || *qseq == 0);
                ts_ok && seq_ok
            })
            .count() as u64;
        let rolling_fill_rate_pct = if post_reset_quotes > 0 {
            ((post_reset_fills.len() as f64 / post_reset_quotes as f64) * 100.0).clamp(0.0, 100.0)
        } else {
            100.0
        };

        let next_entry = match prev_entry {
            Some(mut prev) if prev.manual_quarantine => {
                prev.status = "quarantined".to_string();
                prev.throttle_mult = 0.0;
                prev.rolling_fills = rolling_fills;
                prev.rolling_markout_30s_cents = rolling_markout_30s_cents;
                prev.rolling_fill_rate_pct = rolling_fill_rate_pct;
                prev.rolling_net_pnl_dollars = rolling_net_pnl_dollars;
                prev
            }
            Some(mut prev)
                if prev.status == "quarantined"
                    && prev.reset_after_ts.is_none_or(|rt| rt < prev.triggered_at) =>
            {
                prev.throttle_mult = 0.0;
                prev.rolling_fills = rolling_fills;
                prev.rolling_markout_30s_cents = rolling_markout_30s_cents;
                prev.rolling_fill_rate_pct = rolling_fill_rate_pct;
                prev.rolling_net_pnl_dollars = rolling_net_pnl_dollars;
                prev
            }
            prev_opt => {
                let m30_n = rolling_m30_vals.len() as u64;
                let prev_triggered = prev_opt.as_ref().map_or(0, |p| p.triggered_at);
                let prev_status = prev_opt
                    .as_ref()
                    .map(|p| p.status.as_str())
                    .unwrap_or("active");

                let (status, throttle_mult, reason) = if m30_n >= MIN_DECAY_FILLS
                    && rolling_markout_30s_cents <= QUARANTINE_MARKOUT_30S_CENTS
                {
                    (
                        "quarantined",
                        0.0,
                        format!(
                            "rolling 30s adverse markout breach ({rolling_markout_30s_cents:+.2}c <= {QUARANTINE_MARKOUT_30S_CENTS:.2}c)"
                        ),
                    )
                } else if rolling_settled_count >= MIN_DECAY_FILLS
                    && rolling_net_pnl_dollars < 0.0
                    && rolling_avg_net_ev_cents <= QUARANTINE_NET_EXPECTANCY_CENTS
                {
                    (
                        "quarantined",
                        0.0,
                        format!(
                            "negative rolling realized net expectancy ({rolling_avg_net_ev_cents:+.2}c/ct, PnL ${rolling_net_pnl_dollars:+.2})"
                        ),
                    )
                } else if rolling_settled_count >= MIN_DECAY_FILLS
                    && rolling_net_pnl_dollars < 0.0
                    && m30_n >= MIN_DECAY_FILLS
                    && rolling_markout_30s_cents < THROTTLE_MARKOUT_30S_CENTS
                {
                    (
                        "quarantined",
                        0.0,
                        format!(
                            "combined negative realized PnL (${rolling_net_pnl_dollars:+.2}) and adverse 30s markout ({rolling_markout_30s_cents:+.2}c)"
                        ),
                    )
                } else if post_reset_quotes >= 10
                    && rolling_fill_rate_pct < DEFAULT_MIN_FILL_RATE_PCT * 0.5
                {
                    (
                        "quarantined",
                        0.0,
                        format!(
                            "rolling fill rate collapse ({rolling_fill_rate_pct:.1}% < {:.1}%)",
                            DEFAULT_MIN_FILL_RATE_PCT * 0.5
                        ),
                    )
                } else if m30_n >= MIN_DECAY_FILLS
                    && rolling_markout_30s_cents < THROTTLE_MARKOUT_30S_CENTS
                {
                    (
                        "throttled",
                        0.5,
                        format!(
                            "adverse 30s markout warning ({rolling_markout_30s_cents:+.2}c < {THROTTLE_MARKOUT_30S_CENTS:.2}c)"
                        ),
                    )
                } else if rolling_settled_count >= MIN_DECAY_FILLS
                    && rolling_net_pnl_dollars < 0.0
                {
                    (
                        "throttled",
                        0.5,
                        format!("negative rolling realized net PnL (${rolling_net_pnl_dollars:+.2})"),
                    )
                } else if post_reset_quotes >= MIN_DECAY_FILLS
                    && rolling_fill_rate_pct < DEFAULT_MIN_FILL_RATE_PCT
                {
                    (
                        "throttled",
                        0.5,
                        format!(
                            "rolling fill rate below gate ({rolling_fill_rate_pct:.1}% < {DEFAULT_MIN_FILL_RATE_PCT:.1}%)"
                        ),
                    )
                } else {
                    ("active", 1.0, String::new())
                };

                let triggered_at = if status == "active" {
                    0
                } else if prev_status == status && prev_triggered > 0 {
                    prev_triggered
                } else {
                    now
                };

                StrategyQuarantineEntry {
                    status: status.to_string(),
                    throttle_mult,
                    reason,
                    triggered_at,
                    rolling_fills,
                    rolling_markout_30s_cents,
                    rolling_fill_rate_pct,
                    rolling_net_pnl_dollars,
                    reset_after_ts: reset_ts,
                    reset_after_seq: reset_seq,
                    manual_quarantine: false,
                }
            }
        };

        let mut recent_fills: Vec<FillAuditRecord> = strat_fills
            .iter()
            .rev()
            .take(MAX_RECENT_FILLS_PER_STRATEGY)
            .map(|f| f.record.clone())
            .collect();
        recent_fills.reverse();

        strategy_reports.push(StrategyExecutionHealth {
            strategy: strat.clone(),
            quotes_posted,
            fills: fills_count,
            rolling_fills: next_entry.rolling_fills,
            settled_fills,
            predicted_prob: predicted_win_rate_pct.map(|p| p / 100.0),
            realized_win_rate: realized_win_rate_pct.map(|p| p / 100.0),
            predicted_win_rate_pct,
            realized_win_rate_pct,
            win_rate_residual_pct,
            assumed_fill_rate_pct,
            actual_fill_rate_pct,
            realized_fill_rate_pct: actual_fill_rate_pct,
            fill_rate_residual_pct,
            expected_slippage_cents,
            realized_slippage_cents,
            slippage_residual_cents,
            markout_10s_cents: m10,
            markout_30s_cents: m30,
            markout_60s_cents: m60,
            settlement_markout_cents: m_settle,
            realized_net_pnl_dollars: finite_or(total_net_pnl_dollars, 0.0),
            rolling_net_pnl_dollars: next_entry.rolling_net_pnl_dollars,
            status: next_entry.status.clone(),
            throttle_mult: next_entry.throttle_mult,
            reason: next_entry.reason.clone(),
            quarantine_reason: next_entry.reason.clone(),
            recent_fills,
        });

        q_state.strategies.insert(strat.clone(), next_entry);
    }

    let mut active_strategies = 0usize;
    let mut throttled_strategies = 0usize;
    let mut quarantined_strategies = 0usize;
    for e in q_state.strategies.values() {
        match e.status.as_str() {
            "quarantined" => quarantined_strategies += 1,
            "throttled" => throttled_strategies += 1,
            _ => active_strategies += 1,
        }
    }

    let total_fills = all_fills_flat.len() as u64;
    let settled_all: Vec<&&FillAuditRecord> = all_fills_flat
        .iter()
        .filter(|f| f.won.is_some())
        .collect();
    let settled_fills = settled_all.len() as u64;
    let predicted_win_rate_pct = avg_opt(
        settled_all
            .iter()
            .map(|f| f.predicted_prob.map(|p| p * 100.0)),
    )
    .or_else(|| {
        avg_opt(
            all_fills_flat
                .iter()
                .map(|f| f.predicted_prob.map(|p| p * 100.0)),
        )
    });
    let realized_win_rate_pct = if settled_fills > 0 {
        let wins = settled_all.iter().filter(|f| f.won == Some(true)).count();
        finite_opt((wins as f64 / settled_fills as f64) * 100.0)
    } else {
        None
    };

    let total_weight: f64 = strategy_reports
        .iter()
        .map(|s| s.quotes_posted.max(s.fills).max(1) as f64)
        .sum();
    let assumed_fill_rate_pct = if total_weight > 0.0 {
        finite_or(total_assumed_fill_weight / total_weight, DEFAULT_ASSUMED_MAKER_FILL_RATE_PCT)
    } else {
        DEFAULT_ASSUMED_MAKER_FILL_RATE_PCT
    };
    let actual_fill_rate_pct = if total_quotes_posted > 0 {
        ((total_fills as f64 / total_quotes_posted as f64) * 100.0).clamp(0.0, 100.0)
    } else if total_fills > 0 {
        100.0
    } else {
        0.0
    };
    let expected_slippage_cents = avg_opt(
        all_fills_flat
            .iter()
            .map(|f| finite_opt(f.expected_slippage_cents)),
    )
    .unwrap_or(0.0);
    let realized_slippage_cents = avg_opt(
        all_fills_flat
            .iter()
            .map(|f| finite_opt(f.realized_slippage_cents)),
    )
    .unwrap_or(0.0);
    let slippage_residual_cents = finite_or(realized_slippage_cents - expected_slippage_cents, 0.0);

    let overall = OverallExecutionHealth {
        quotes_posted: total_quotes_posted,
        total_fills,
        settled_fills,
        predicted_win_rate_pct,
        realized_win_rate_pct,
        assumed_fill_rate_pct,
        actual_fill_rate_pct,
        expected_slippage_cents,
        realized_slippage_cents,
        slippage_residual_cents,
        markout_10s_cents: avg_opt(all_fills_flat.iter().map(|f| f.markout_10s_cents)),
        markout_30s_cents: avg_opt(all_fills_flat.iter().map(|f| f.markout_30s_cents)),
        markout_60s_cents: avg_opt(all_fills_flat.iter().map(|f| f.markout_60s_cents)),
        settlement_markout_cents: avg_opt(
            all_fills_flat.iter().map(|f| f.settlement_markout_cents),
        ),
        realized_net_pnl_dollars: finite_or(
            all_fills_flat
                .iter()
                .filter_map(|f| f.realized_net_pnl_dollars)
                .sum(),
            0.0,
        ),
        active_strategies,
        throttled_strategies,
        quarantined_strategies,
    };

    if persist_quarantine && !q_state.strategies.is_empty() {
        let mut compare = q_state.clone();
        compare.updated_at = 0;
        let mut prev_cmp: QuarantineStateFile =
            serde_json::from_str(&prev_q_json).unwrap_or_default();
        prev_cmp.updated_at = 0;
        let changed = serde_json::to_string(&compare).ok() != serde_json::to_string(&prev_cmp).ok();
        if changed || !quarantine_state_path(data_dir).exists() {
            q_state.updated_at = now;
            let _ = save_quarantine_state(data_dir, &q_state);
        }
    }

    ExecutionHealthReport {
        ok: true,
        strategies: strategy_reports,
        quarantine_state: q_state,
        overall,
    }
}

pub fn apply_quarantine_override(
    data_dir: &Path,
    strategy: &str,
    action: &str,
    reason: Option<&str>,
) -> Result<(StrategyQuarantineEntry, QuarantineStateFile), String> {
    let clean_strat = strategy.trim();
    if clean_strat.is_empty()
        || clean_strat.len() > 64
        || !clean_strat
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err("invalid strategy identifier".to_string());
    }
    let clean_action = action.trim().to_ascii_lowercase();
    if clean_action != "reset" && clean_action != "quarantine" {
        return Err(format!(
            "action must be \"reset\" or \"quarantine\", got \"{action}\""
        ));
    }

    let mut q_state = load_quarantine_state(data_dir);
    let ledger = read_shadow_ledger(data_dir);
    let max_seq = ledger.iter().map(|e| e.seq).max().unwrap_or(0);
    let max_ts = ledger.iter().map(|e| e.ts).max().unwrap_or(0);
    let now = now_unix();
    let existing = q_state.strategies.get(clean_strat).cloned().unwrap_or_default();

    let updated = if clean_action == "reset" {
        let note = reason
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("operator manual reset")
            .to_string();
        StrategyQuarantineEntry {
            status: "active".to_string(),
            throttle_mult: 1.0,
            reason: note,
            triggered_at: now,
            rolling_fills: 0,
            rolling_markout_30s_cents: 0.0,
            rolling_fill_rate_pct: 100.0,
            rolling_net_pnl_dollars: 0.0,
            reset_after_ts: Some(now.max(max_ts)),
            reset_after_seq: Some(max_seq),
            manual_quarantine: false,
        }
    } else {
        let note = reason
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("operator manual quarantine")
            .to_string();
        StrategyQuarantineEntry {
            status: "quarantined".to_string(),
            throttle_mult: 0.0,
            reason: note,
            triggered_at: now,
            rolling_fills: existing.rolling_fills,
            rolling_markout_30s_cents: existing.rolling_markout_30s_cents,
            rolling_fill_rate_pct: existing.rolling_fill_rate_pct,
            rolling_net_pnl_dollars: existing.rolling_net_pnl_dollars,
            reset_after_ts: existing.reset_after_ts,
            reset_after_seq: existing.reset_after_seq,
            manual_quarantine: true,
        }
    };

    q_state.updated_at = now;
    q_state
        .strategies
        .insert(clean_strat.to_string(), updated.clone());
    save_quarantine_state(data_dir, &q_state)?;
    Ok((updated, q_state))
}

pub fn strategy_exists_in_audit(data_dir: &Path, strategy: &str) -> bool {
    let q = load_quarantine_state(data_dir);
    if q.find_entry(strategy).is_some() {
        return true;
    }
    let ledger = read_shadow_ledger(data_dir);
    ledger.iter().any(|e| {
        e.strategy == strategy
            || e.strategy == strategy.replace('-', "_")
            || e.strategy == strategy.replace('_', "-")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markouts_and_rolling_quarantine_persist_and_override_resets() {
        let dir = std::env::temp_dir().join(format!("ob-exec-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let base_ts: i64 = 1_800_000_000;
        let mut ledger_lines = String::new();
        let mut ob_lines = String::new();
        for i in 0..6i64 {
            let ts = base_ts + i * 200;
            let ticker = format!("KXBTC15M-AUDIT{i}");
            let oid = format!("ob-audit-{i}");
            ledger_lines.push_str(&format!(
                "{{\"seq\":{},\"ts\":{},\"event\":\"QuotePosted\",\"mode\":\"shadow\",\"slot_id\":1,\"strategy\":\"decay-strat\",\"ticker\":\"{ticker}\",\"series\":\"KXBTC15M\",\"order_id\":\"{oid}\",\"side\":\"yes\",\"execution_mode\":\"maker\",\"limit_cents\":60,\"contracts\":5}}\n",
                i * 3 + 1,
                ts - 5
            ));
            ledger_lines.push_str(&format!(
                "{{\"seq\":{},\"ts\":{},\"event\":\"OrderFilled\",\"mode\":\"shadow\",\"slot_id\":1,\"strategy\":\"decay-strat\",\"ticker\":\"{ticker}\",\"series\":\"KXBTC15M\",\"order_id\":\"{oid}\",\"side\":\"yes\",\"execution_mode\":\"maker\",\"limit_cents\":60,\"fill_cents\":60,\"contracts\":5,\"slippage_cents\":0.0,\"fee_dollars\":0.02}}\n",
                i * 3 + 2,
                ts
            ));
            ledger_lines.push_str(&format!(
                "{{\"seq\":{},\"ts\":{},\"event\":\"WindowSettled\",\"mode\":\"shadow\",\"slot_id\":1,\"strategy\":\"decay-strat\",\"ticker\":\"{ticker}\",\"series\":\"KXBTC15M\",\"order_id\":\"{oid}\",\"side\":\"yes\",\"execution_mode\":\"maker\",\"limit_cents\":60,\"fill_cents\":60,\"contracts\":5,\"fee_dollars\":0.02,\"realized_pnl_dollars\":-3.02,\"result\":\"no\"}}\n",
                i * 3 + 3,
                ts + 120
            ));

            // Orderbook midpoints drop after fill: +10s=58c (-2c), +30s=54c (-6c), +60s=50c (-10c)
            ob_lines.push_str(&format!(
                "{{\"ts_ms\":{},\"ticker\":\"{ticker}\",\"seq\":1,\"source\":\"ws\",\"yes_bids\":[[57,10]],\"no_bids\":[[41,10]]}}\n",
                (ts + 10) * 1000
            ));
            ob_lines.push_str(&format!(
                "{{\"ts_ms\":{},\"ticker\":\"{ticker}\",\"seq\":2,\"source\":\"ws\",\"yes_bids\":[[53,10]],\"no_bids\":[[45,10]]}}\n",
                (ts + 30) * 1000
            ));
            ob_lines.push_str(&format!(
                "{{\"ts_ms\":{},\"ticker\":\"{ticker}\",\"seq\":3,\"source\":\"ws\",\"yes_bids\":[[49,10]],\"no_bids\":[[49,10]]}}\n",
                (ts + 60) * 1000
            ));
        }
        std::fs::write(dir.join("shadow_ledger.jsonl"), ledger_lines).unwrap();
        std::fs::write(dir.join("orderbooks.jsonl"), ob_lines).unwrap();

        let rep = evaluate_execution_health(&dir, None, None, None, true);
        assert!(rep.ok);
        assert_eq!(rep.overall.total_fills, 6);
        assert!((rep.overall.markout_10s_cents.unwrap() - (-2.0)).abs() < 1e-6);
        assert!((rep.overall.markout_30s_cents.unwrap() - (-6.0)).abs() < 1e-6);
        assert!((rep.overall.markout_60s_cents.unwrap() - (-10.0)).abs() < 1e-6);
        assert!((rep.overall.settlement_markout_cents.unwrap() - (-60.0)).abs() < 1e-6);

        let qs = rep.quarantine_state.strategies.get("decay-strat").unwrap();
        assert_eq!(qs.status, "quarantined");
        assert_eq!(qs.throttle_mult, 0.0);
        assert_eq!(qs.rolling_fills, 6);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(quarantine_state_path(&dir))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }

        // Reload and verify quarantine persists
        let reloaded = load_quarantine_state(&dir);
        assert!(reloaded.is_quarantined("decay-strat"));

        // Override reset -> active, stays active on re-eval because old pre-reset fills are skipped
        let (entry, _) = apply_quarantine_override(&dir, "decay-strat", "reset", Some("re-enabled")).unwrap();
        assert_eq!(entry.status, "active");
        assert_eq!(entry.throttle_mult, 1.0);
        let rep2 = evaluate_execution_health(&dir, None, None, None, true);
        assert_eq!(
            rep2.quarantine_state.strategies.get("decay-strat").unwrap().status,
            "active"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
