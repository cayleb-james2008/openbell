//! Hard fail-closed Risk Rails & Emergency Kill Switch for live Kalshi v2 trading.
//!
//! Core invariants:
//! - Disarmed by default (`live_armed == false`): server restarts ALWAYS boot disarmed.
//! - Arming requires BOTH valid configured Kalshi credentials AND explicit `confirm_arm: true`.
//! - Every live order passes ALL hard caps (`max_order_contracts`, `max_order_dollars`,
//!   `max_daily_loss_dollars`, `max_open_exposure_dollars`, `allowed_ticker_prefix`,
//!   whole-cent `1..=99` price check) or is rejected fail-closed with every violated rail.
//! - `emergency_kill_switch()` disarms immediately before issuing `cancel_all_orders()`,
//!   guaranteeing no further orders can pass even if upstream network calls fail.

#![allow(dead_code)]

use crate::kalshi_live::{
    mask_api_key_id, write_secret_file_0600, CancelAllReport, KalshiLiveClient, KalshiOrderRequest,
    PersistedLiveConfig,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn default_max_order_contracts() -> u32 {
    25
}
fn default_max_order_dollars() -> f64 {
    25.0
}
fn default_max_daily_loss_dollars() -> f64 {
    50.0
}
fn default_max_open_exposure_dollars() -> f64 {
    100.0
}
fn default_allowed_ticker_prefix() -> String {
    "KX".to_string()
}

/// Configurable hard risk limits enforced before every live order submission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RiskRailsConfig {
    #[serde(default = "default_max_order_contracts")]
    pub max_order_contracts: u32,
    #[serde(default = "default_max_order_dollars")]
    pub max_order_dollars: f64,
    #[serde(default = "default_max_daily_loss_dollars")]
    pub max_daily_loss_dollars: f64,
    #[serde(default = "default_max_open_exposure_dollars")]
    pub max_open_exposure_dollars: f64,
    #[serde(default = "default_allowed_ticker_prefix")]
    pub allowed_ticker_prefix: String,
}

impl Default for RiskRailsConfig {
    fn default() -> Self {
        Self {
            max_order_contracts: default_max_order_contracts(),
            max_order_dollars: default_max_order_dollars(),
            max_daily_loss_dollars: default_max_daily_loss_dollars(),
            max_open_exposure_dollars: default_max_open_exposure_dollars(),
            allowed_ticker_prefix: default_allowed_ticker_prefix(),
        }
    }
}

impl RiskRailsConfig {
    /// Strict validation rejecting zero, negative, non-finite, or empty-prefix caps.
    pub fn validate(&self) -> Result<Self, Vec<String>> {
        let mut errs = Vec::new();
        if self.max_order_contracts == 0 || self.max_order_contracts > 10_000 {
            errs.push("max_order_contracts must be between 1 and 10,000".to_string());
        }
        if !self.max_order_dollars.is_finite()
            || self.max_order_dollars < 1.0
            || self.max_order_dollars > 100_000.0
        {
            errs.push("max_order_dollars must be finite between $1.00 and $100,000.00".to_string());
        }
        if !self.max_daily_loss_dollars.is_finite()
            || self.max_daily_loss_dollars < 1.0
            || self.max_daily_loss_dollars > 100_000.0
        {
            errs.push(
                "max_daily_loss_dollars must be finite between $1.00 and $100,000.00".to_string(),
            );
        }
        if !self.max_open_exposure_dollars.is_finite()
            || self.max_open_exposure_dollars < 1.0
            || self.max_open_exposure_dollars > 500_000.0
        {
            errs.push(
                "max_open_exposure_dollars must be finite between $1.00 and $500,000.00"
                    .to_string(),
            );
        }
        let prefixes: Vec<String> = self
            .allowed_ticker_prefix
            .split(',')
            .map(|s| s.trim().to_ascii_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        if prefixes.is_empty() {
            errs.push("allowed_ticker_prefix cannot be empty (e.g. \"KX\" or \"KXBTC15M,KXETH15M\")".to_string());
        } else if !prefixes.iter().all(|p| {
            p.len() <= 32
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        }) {
            errs.push("allowed_ticker_prefix contains invalid characters".to_string());
        }
        if !errs.is_empty() {
            return Err(errs);
        }
        Ok(Self {
            max_order_contracts: self.max_order_contracts,
            max_order_dollars: self.max_order_dollars,
            max_daily_loss_dollars: self.max_daily_loss_dollars,
            max_open_exposure_dollars: self.max_open_exposure_dollars,
            allowed_ticker_prefix: prefixes.join(","),
        })
    }

    pub fn ticker_allowed(&self, ticker: &str) -> bool {
        let up = ticker.trim().to_ascii_uppercase();
        if up.is_empty() {
            return false;
        }
        self.allowed_ticker_prefix
            .split(',')
            .map(|p| p.trim().to_ascii_uppercase())
            .filter(|p| !p.is_empty())
            .any(|prefix| up.starts_with(&prefix))
    }
}

/// Incoming live order intent validated by `RiskRailsState::check_order`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveOrderIntent {
    pub ticker: String,
    pub action: String,
    pub side: String,
    pub count: u32,
    pub price_cents: u8,
    #[serde(default)]
    pub client_order_id: Option<String>,
    #[serde(default)]
    pub dry_run: bool,
}

/// Outcome of evaluating an order intent against every risk rail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskCheckResult {
    pub allowed: bool,
    pub order_notional_dollars: f64,
    pub projected_exposure_dollars: f64,
    pub rail_violations: Vec<String>,
}

/// Outcome returned by `emergency_kill_switch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KillSwitchOutcome {
    pub ok: bool,
    pub disarmed: bool,
    pub killed: bool,
    pub kill_reason: String,
    pub attempted: usize,
    pub cancelled_count: usize,
    pub cancelled_ids: Vec<String>,
    pub errors: Vec<String>,
}

/// Public JSON status snapshot for `GET /api/live/status` (never exposes private keys!).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveStatusSnapshot {
    pub configured: bool,
    pub environment: String,
    pub api_base_url: String,
    pub key_id_masked: Option<String>,
    pub key_path_configured: bool,
    pub connected: bool,
    pub mode: String,
    pub live_armed: bool,
    pub killed: bool,
    pub kill_reason: Option<String>,
    pub rails: RiskRailsConfig,
    pub daily_realized_loss_dollars: f64,
    pub open_exposure_dollars: f64,
    pub orders_placed_today: u32,
    pub last_error: Option<String>,
}

/// Runtime risk rail state. Always boots disarmed (`live_armed == false`).
#[derive(Debug, Clone)]
pub struct RiskRailsState {
    pub live_armed: bool,
    pub killed: bool,
    pub kill_reason: Option<String>,
    pub rails: RiskRailsConfig,
    pub daily_realized_loss_dollars: f64,
    pub open_exposure_dollars: f64,
    pub orders_placed_today: u32,
    pub utc_day_num: u64,
    pub last_error: Option<String>,
}

impl Default for RiskRailsState {
    fn default() -> Self {
        Self::new(RiskRailsConfig::default())
    }
}

fn current_utc_day() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0)
}

pub fn rails_config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("risk_rails.json")
}

/// Load persisted risk rail caps from disk (always returns disarmed state!).
pub fn load_rails_config(data_dir: &Path) -> RiskRailsConfig {
    let path = rails_config_path(data_dir);
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<RiskRailsConfig>(&raw) {
            if let Ok(valid) = cfg.validate() {
                return valid;
            }
        }
    }
    RiskRailsConfig::default()
}

/// Persist validated risk rail caps atomically (`0600`). Never persists `live_armed`.
pub fn save_rails_config(data_dir: &Path, cfg: &RiskRailsConfig) -> Result<RiskRailsConfig, String> {
    let valid = cfg.validate().map_err(|errs| errs.join("; "))?;
    let body = serde_json::to_vec_pretty(&valid).map_err(|e| format!("serialize rails: {e}"))?;
    write_secret_file_0600(&rails_config_path(data_dir), &body)?;
    Ok(valid)
}

impl RiskRailsState {
    pub fn new(rails: RiskRailsConfig) -> Self {
        Self {
            live_armed: false,
            killed: false,
            kill_reason: None,
            rails: rails.validate().unwrap_or_default(),
            daily_realized_loss_dollars: 0.0,
            open_exposure_dollars: 0.0,
            orders_placed_today: 0,
            utc_day_num: current_utc_day(),
            last_error: None,
        }
    }

    pub fn load(data_dir: &Path) -> Self {
        Self::new(load_rails_config(data_dir))
    }

    /// Roll over daily loss / order count automatically at UTC midnight.
    pub fn refresh_utc_day(&mut self) {
        let today = current_utc_day();
        if today != self.utc_day_num {
            self.utc_day_num = today;
            self.daily_realized_loss_dollars = 0.0;
            self.orders_placed_today = 0;
        }
    }

    pub fn update_rails(&mut self, cfg: &RiskRailsConfig) -> Result<RiskRailsConfig, Vec<String>> {
        let valid = cfg.validate()?;
        self.rails = valid.clone();
        Ok(valid)
    }

    /// Arm live trading. Requires valid credentials AND explicit `confirm_arm == true`.
    pub fn arm(&mut self, confirm_arm: bool, configured: bool) -> Result<(), String> {
        if !configured {
            return Err(
                "Cannot arm live trading: Kalshi API key ID and private key PEM are not configured."
                    .to_string(),
            );
        }
        if !confirm_arm {
            return Err(
                "Explicit confirmation (confirm_arm: true) is required to arm live trading."
                    .to_string(),
            );
        }
        self.live_armed = true;
        self.killed = false;
        self.kill_reason = None;
        Ok(())
    }

    /// Disarm live trading immediately.
    pub fn disarm(&mut self) {
        self.live_armed = false;
    }

    /// Update live exposure and realized loss metrics from fresh venue data.
    pub fn sync_venue_metrics(&mut self, open_exposure_dollars: f64, daily_realized_loss_dollars: f64) {
        self.refresh_utc_day();
        if open_exposure_dollars.is_finite() && open_exposure_dollars >= 0.0 {
            self.open_exposure_dollars = open_exposure_dollars;
        }
        if daily_realized_loss_dollars.is_finite() && daily_realized_loss_dollars >= 0.0 {
            self.daily_realized_loss_dollars = daily_realized_loss_dollars;
        }
    }

    /// Fail-closed check of a `LiveOrderIntent` against every configured risk rail.
    ///
    /// When `intent.dry_run == true`, evaluates every numeric/prefix/kill-switch rail
    /// without requiring `live_armed == true` so operators can inspect pre-trade rail
    /// compliance before arming; non-dry-run submissions ALWAYS require `live_armed == true`.
    pub fn check_order(&mut self, intent: &LiveOrderIntent) -> RiskCheckResult {
        self.refresh_utc_day();
        let mut violations = Vec::new();

        if self.killed {
            violations.push(format!(
                "emergency kill switch is engaged ({})",
                self.kill_reason
                    .as_deref()
                    .unwrap_or("operator halt")
            ));
        }
        if !intent.dry_run && !self.live_armed {
            violations.push(
                "live trading is disarmed (live_armed=false); arm explicitly before submitting orders"
                    .to_string(),
            );
        }

        let action = intent.action.trim().to_ascii_lowercase();
        if action != "buy" && action != "sell" {
            violations.push(format!(
                "action must be \"buy\" or \"sell\", got \"{}\"",
                intent.action
            ));
        }
        let side = intent.side.trim().to_ascii_lowercase();
        if side != "yes" && side != "no" {
            violations.push(format!(
                "side must be \"yes\" or \"no\", got \"{}\"",
                intent.side
            ));
        }

        let ticker = intent.ticker.trim();
        if ticker.is_empty()
            || !ticker
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            violations.push("ticker is empty or contains invalid characters".to_string());
        } else if !self.rails.ticker_allowed(ticker) {
            violations.push(format!(
                "ticker \"{ticker}\" does not match allowed_ticker_prefix \"{}\"",
                self.rails.allowed_ticker_prefix
            ));
        }

        if !(1..=99).contains(&intent.price_cents) {
            violations.push(format!(
                "price_cents must be whole-cent 1..=99, got {}",
                intent.price_cents
            ));
        }

        if intent.count == 0 {
            violations.push("count must be >= 1 contract".to_string());
        } else if intent.count > self.rails.max_order_contracts {
            violations.push(format!(
                "count {} exceeds max_order_contracts ({})",
                intent.count, self.rails.max_order_contracts
            ));
        }

        let order_notional = (intent.count as f64) * (intent.price_cents as f64 / 100.0);
        if !order_notional.is_finite() {
            violations.push("order notional is not finite".to_string());
        } else if order_notional > self.rails.max_order_dollars + 1e-9 {
            violations.push(format!(
                "order notional ${order_notional:.2} exceeds max_order_dollars (${:.2})",
                self.rails.max_order_dollars
            ));
        }

        if self.daily_realized_loss_dollars >= self.rails.max_daily_loss_dollars - 1e-9 {
            violations.push(format!(
                "daily realized loss ${:.2} has reached max_daily_loss_dollars (${:.2})",
                self.daily_realized_loss_dollars, self.rails.max_daily_loss_dollars
            ));
        }

        let projected_exposure = if action == "buy" {
            self.open_exposure_dollars + order_notional
        } else {
            (self.open_exposure_dollars - order_notional).max(0.0)
        };
        if !projected_exposure.is_finite() {
            violations.push("projected exposure is not finite".to_string());
        } else if action == "buy" && projected_exposure > self.rails.max_open_exposure_dollars + 1e-9 {
            violations.push(format!(
                "projected open exposure ${projected_exposure:.2} exceeds max_open_exposure_dollars (${:.2})",
                self.rails.max_open_exposure_dollars
            ));
        }

        RiskCheckResult {
            allowed: violations.is_empty(),
            order_notional_dollars: if order_notional.is_finite() { order_notional } else { 0.0 },
            projected_exposure_dollars: if projected_exposure.is_finite() { projected_exposure } else { self.open_exposure_dollars },
            rail_violations: violations,
        }
    }

    /// Record an accepted live order into local exposure and daily order counters.
    pub fn record_submitted_order(&mut self, action: &str, notional_dollars: f64) {
        self.refresh_utc_day();
        self.orders_placed_today = self.orders_placed_today.saturating_add(1);
        if action.eq_ignore_ascii_case("buy") && notional_dollars.is_finite() && notional_dollars > 0.0 {
            self.open_exposure_dollars += notional_dollars;
        }
    }

    /// Build a validated `KalshiOrderRequest` if and only if every hard risk rail passes.
    pub fn authorize_order(
        &mut self,
        intent: &LiveOrderIntent,
    ) -> Result<(KalshiOrderRequest, RiskCheckResult), RiskCheckResult> {
        let check = self.check_order(intent);
        if !check.allowed {
            return Err(check);
        }
        match KalshiOrderRequest::limit(
            &intent.ticker,
            &intent.action,
            &intent.side,
            intent.count,
            intent.price_cents,
            intent.client_order_id.clone(),
        ) {
            Ok(req) => Ok((req, check)),
            Err(e) => Err(RiskCheckResult {
                allowed: false,
                order_notional_dollars: check.order_notional_dollars,
                projected_exposure_dollars: check.projected_exposure_dollars,
                rail_violations: vec![e],
            }),
        }
    }

    /// Emergency kill switch: disarms IMMEDIATELY, latches `killed = true`, and
    /// cancels every resting order on Kalshi if a live client is provided.
    pub fn emergency_kill_switch(
        &mut self,
        reason: Option<&str>,
        client: Option<&KalshiLiveClient>,
    ) -> KillSwitchOutcome {
        let why = reason
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("operator emergency kill switch")
            .to_string();

        // Fail-closed FIRST: disarm and latch killed BEFORE any network call.
        self.live_armed = false;
        self.killed = true;
        self.kill_reason = Some(why.clone());

        let report = match client {
            Some(c) => c.cancel_all_orders(),
            None => CancelAllReport {
                attempted: 0,
                cancelled_count: 0,
                cancelled_ids: Vec::new(),
                errors: Vec::new(),
            },
        };

        if !report.errors.is_empty() {
            self.last_error = Some(report.errors.join("; "));
        }

        KillSwitchOutcome {
            ok: true,
            disarmed: true,
            killed: true,
            kill_reason: why,
            attempted: report.attempted,
            cancelled_count: report.cancelled_count,
            cancelled_ids: report.cancelled_ids,
            errors: report.errors,
        }
    }

    /// Build the exact `GET /api/live/status` JSON snapshot (never exposes secrets!).
    pub fn status_snapshot(
        &self,
        cfg: &PersistedLiveConfig,
        connected: bool,
    ) -> LiveStatusSnapshot {
        let configured = cfg.is_configured();
        let mode = if self.live_armed && configured {
            "live".to_string()
        } else {
            "paper".to_string()
        };
        LiveStatusSnapshot {
            configured,
            environment: cfg.environment.as_str().to_string(),
            api_base_url: cfg.effective_base_url(),
            key_id_masked: mask_api_key_id(&cfg.api_key_id),
            key_path_configured: !cfg.private_key_path.trim().is_empty()
                && Path::new(cfg.private_key_path.trim()).is_file(),
            connected,
            mode,
            live_armed: self.live_armed,
            killed: self.killed,
            kill_reason: self.kill_reason.clone(),
            rails: self.rails.clone(),
            daily_realized_loss_dollars: self.daily_realized_loss_dollars,
            open_exposure_dollars: self.open_exposure_dollars,
            orders_placed_today: self.orders_placed_today,
            last_error: self.last_error.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Comprehensive Fail-Closed Risk Rail & Kill-Switch Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kalshi_live::{tests::generate_ephemeral_test_rsa, KalshiEnv};
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;

    fn sample_buy() -> LiveOrderIntent {
        LiveOrderIntent {
            ticker: "KXBTC15M-26NOV22-T1200".to_string(),
            action: "buy".to_string(),
            side: "yes".to_string(),
            count: 10,
            price_cents: 65,
            client_order_id: None,
            dry_run: false,
        }
    }

    #[test]
    fn disarmed_by_default_and_explicit_confirm_arm_required() {
        let mut state = RiskRailsState::default();
        assert!(!state.live_armed, "must boot disarmed by default");
        assert!(!state.killed);

        // Live order rejected while disarmed.
        let res = state.check_order(&sample_buy());
        assert!(!res.allowed);
        assert!(res.rail_violations.iter().any(|v| v.contains("disarmed")));

        // Arming without credentials refused.
        assert!(state.arm(true, false).is_err());
        assert!(!state.live_armed);

        // Arming without explicit confirm_arm=true refused.
        assert!(state.arm(false, true).is_err());
        assert!(!state.live_armed);

        // Arming with credentials + confirm_arm=true succeeds.
        assert!(state.arm(true, true).is_ok());
        assert!(state.live_armed);
        assert!(state.check_order(&sample_buy()).allowed);

        // Disarming immediately blocks orders again.
        state.disarm();
        assert!(!state.live_armed);
        assert!(!state.check_order(&sample_buy()).allowed);
    }

    #[test]
    fn every_hard_risk_rail_blocks_fail_closed() {
        let mut state = RiskRailsState::new(RiskRailsConfig {
            max_order_contracts: 20,
            max_order_dollars: 15.0,
            max_daily_loss_dollars: 30.0,
            max_open_exposure_dollars: 40.0,
            allowed_ticker_prefix: "KXBTC15M,KXETH15M".to_string(),
        });
        state.arm(true, true).unwrap();

        // 1. Valid order passes.
        assert!(state.authorize_order(&sample_buy()).is_ok());

        // 2. Whole-cent price bounds: 0 and 100 MUST fail.
        for bad_px in [0u8, 100, 255] {
            let mut o = sample_buy();
            o.price_cents = bad_px;
            let r = state.check_order(&o);
            assert!(!r.allowed, "price_cents={bad_px} must fail");
        }

        // 3. Zero count or > max_order_contracts MUST fail.
        let mut o0 = sample_buy();
        o0.count = 0;
        assert!(!state.check_order(&o0).allowed);

        let mut o_big = sample_buy();
        o_big.count = 21;
        o_big.price_cents = 50;
        let r_big = state.check_order(&o_big);
        assert!(!r_big.allowed);
        assert!(r_big.rail_violations.iter().any(|v| v.contains("max_order_contracts")));

        // 4. Order notional > max_order_dollars ($15) MUST fail (20 * 80c = $16).
        let mut o_dol = sample_buy();
        o_dol.count = 20;
        o_dol.price_cents = 80;
        let r_dol = state.check_order(&o_dol);
        assert!(!r_dol.allowed);
        assert!(r_dol.rail_violations.iter().any(|v| v.contains("max_order_dollars")));

        // 5. Disallowed ticker prefix MUST fail.
        let mut o_pfx = sample_buy();
        o_pfx.ticker = "KXSOL15M-26NOV22".to_string();
        let r_pfx = state.check_order(&o_pfx);
        assert!(!r_pfx.allowed);
        assert!(r_pfx.rail_violations.iter().any(|v| v.contains("allowed_ticker_prefix")));

        // 6. Open exposure cap ($40): existing $36 + $6.50 buy = $42.50 -> blocked;
        //    but a reducing sell order IS allowed!
        state.sync_venue_metrics(36.0, 0.0);
        let r_exp = state.check_order(&sample_buy());
        assert!(!r_exp.allowed);
        assert!(r_exp.rail_violations.iter().any(|v| v.contains("max_open_exposure_dollars")));

        let mut o_sell = sample_buy();
        o_sell.action = "sell".to_string();
        assert!(state.check_order(&o_sell).allowed, "exposure-reducing sell should pass");

        // 7. Daily loss cap ($30): once reached, ALL orders blocked.
        state.sync_venue_metrics(0.0, 30.0);
        let r_loss = state.check_order(&sample_buy());
        assert!(!r_loss.allowed);
        assert!(r_loss.rail_violations.iter().any(|v| v.contains("max_daily_loss_dollars")));
    }

    #[test]
    fn emergency_kill_switch_disarms_and_cancels_all_resting_orders() {
        let (pkcs8, _, _) = generate_ephemeral_test_rsa();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let srv = std::thread::spawn(move || {
            for stream in listener.incoming().take(3) {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut req_line = String::new();
                let _ = reader.read_line(&mut req_line);
                let mut parts = req_line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                }
                let body = if method == "GET" && path.starts_with("/trade-api/v2/portfolio/orders") {
                    serde_json::json!({
                        "orders": [
                            { "order_id": "ks-ord-1", "ticker": "KXBTC15M-A", "status": "resting", "action": "buy", "side": "yes" },
                            { "order_id": "ks-ord-2", "ticker": "KXETH15M-B", "status": "resting", "action": "buy", "side": "no" }
                        ]
                    })
                    .to_string()
                } else {
                    serde_json::json!({ "status": "canceled" }).to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let client = KalshiLiveClient::from_parts(
            "kill-test-key-1234",
            &pkcs8,
            KalshiEnv::Demo,
            &format!("http://{addr}/trade-api/v2"),
        )
        .unwrap();

        let mut state = RiskRailsState::default();
        state.arm(true, true).unwrap();
        assert!(state.live_armed);

        let outcome = state.emergency_kill_switch(Some("manual panic test"), Some(&client));
        assert!(outcome.ok && outcome.disarmed && outcome.killed);
        assert_eq!(outcome.attempted, 2);
        assert_eq!(outcome.cancelled_count, 2);
        assert!(outcome.errors.is_empty());
        assert!(!state.live_armed);
        assert!(state.killed);
        assert!(!state.check_order(&sample_buy()).allowed);

        // Even dry_run reports killed status violation.
        let mut dry = sample_buy();
        dry.dry_run = true;
        assert!(!state.check_order(&dry).allowed);

        srv.join().unwrap();
    }

    #[test]
    fn persisted_rails_never_persist_live_armed_and_status_never_leaks_secrets() {
        let dir = std::env::temp_dir().join(format!("ob-rails-persist-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = RiskRailsConfig {
            max_order_contracts: 15,
            max_order_dollars: 12.5,
            max_daily_loss_dollars: 25.0,
            max_open_exposure_dollars: 60.0,
            allowed_ticker_prefix: "KXBTC15M".to_string(),
        };
        save_rails_config(&dir, &cfg).unwrap();
        let reloaded = RiskRailsState::load(&dir);
        assert!(!reloaded.live_armed, "reloaded state must ALWAYS boot disarmed");
        assert_eq!(reloaded.rails.max_order_contracts, 15);

        let live_cfg = PersistedLiveConfig {
            api_key_id: "kls_live_secret_987654321".to_string(),
            private_key_path: "/nonexistent/key.pem".to_string(),
            environment: KalshiEnv::Prod,
            api_base_url: None,
        };
        let snap = reloaded.status_snapshot(&live_cfg, false);
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("secret_987654321"));
        assert!(!json.contains("BEGIN"));
        assert_eq!(snap.key_id_masked.as_deref(), Some("kls_...4321"));
        assert_eq!(snap.mode, "paper");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
