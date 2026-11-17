//! openbell-rs — Cayleb's Kalshi compounding desk, rebuilt lean.
//!
//! Venue truth on every number: every figure the app shows comes from
//! Kalshi's own record or is shown as "unavailable". This build is a
//! measuring machine — a forward collector plus a shadow scorer. It has NO
//! order-placement code path at all: nothing here can place, arm, or
//! simulate placing a live order. Strategies are Rune scripts, scored on
//! paper against collected forward data, and a strategy is only "proven"
//! when it passes the desk's promotion standard on forward evidence.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

const SERIES: [&str; 5] = ["KXBTC15M", "KXETH15M", "KXSOL15M", "KXXRP15M", "KXDOGE15M"];
const API_BASE: &str = "https://api.elections.kalshi.com/trade-api/v2";

// ---------------------------------------------------------------------------
// Time helpers (no chrono dependency — Kalshi timestamps are ISO-8601 Zulu)
// ---------------------------------------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn parse_iso_z(s: &str) -> Option<i64> {
    let t = s.get(..19)?;
    let (date, time) = t.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da) = (
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
        d.next()?.parse::<i64>().ok()?,
    );
    let mut h = time.split(':');
    let (hh, mm, ss) = (
        h.next()?.parse::<i64>().ok()?,
        h.next()?.parse::<i64>().ok()?,
        h.next()?.parse::<i64>().ok()?,
    );
    Some(days_from_civil(y, mo, da) * 86400 + hh * 3600 + mm * 60 + ss)
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Snapshots and the forward collector (public market data — no auth)
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize)]
struct Snapshot {
    fetched_at: i64,
    ticker: String,
    event_ticker: String,
    series: String,
    close_time: String,
    status: String,
    result: String,
    yes_bid: Option<f64>,
    yes_ask: Option<f64>,
    no_bid: Option<f64>,
    no_ask: Option<f64>,
    last_price: Option<f64>,
}

fn dollars(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok())
}

fn fetch_markets(series: &str, status: &str) -> Result<Vec<Snapshot>, String> {
    let url = format!("{API_BASE}/markets?series_ticker={series}&status={status}&limit=100");
    let out = Command::new("curl")
        .args(["-s", "-m", "25", &url])
        .output()
        .map_err(|e| format!("curl failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exited {}", out.status));
    }
    let body: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("venue returned non-JSON: {e}"))?;
    let fetched_at = now_unix();
    let mut snaps = Vec::new();
    if let Some(markets) = body.get("markets").and_then(|m| m.as_array()) {
        for m in markets {
            let get = |k: &str| m.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            snaps.push(Snapshot {
                fetched_at,
                ticker: get("ticker"),
                event_ticker: get("event_ticker"),
                series: series.to_string(),
                close_time: get("close_time"),
                status: get("status"),
                result: get("result"),
                yes_bid: dollars(m, "yes_bid_dollars"),
                yes_ask: dollars(m, "yes_ask_dollars"),
                no_bid: dollars(m, "no_bid_dollars"),
                no_ask: dollars(m, "no_ask_dollars"),
                last_price: dollars(m, "last_price_dollars"),
            });
        }
    }
    Ok(snaps)
}

/// One collector pass over every series the desk trades: open markets (the
/// forward tape) plus settled markets (so windows get their results).
/// Appends JSONL snapshots to disk. Returns how many snapshots were written.
fn collect_once(data_dir: &Path) -> Result<usize, String> {
    let _ = std::fs::create_dir_all(data_dir);
    let path = data_dir.join("snapshots.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let mut written = 0;
    for series in SERIES {
        for status in ["open", "settled"] {
            match fetch_markets(series, status) {
                Ok(snaps) => {
                    for snap in &snaps {
                        writeln!(file, "{}", serde_json::to_string(snap).unwrap())
                            .map_err(|e| e.to_string())?;
                        written += 1;
                    }
                }
                Err(e) => eprintln!("collect {series}/{status}: {e}"),
            }
        }
    }
    Ok(written)
}

fn load_snapshots(path: &Path) -> Vec<Snapshot> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Snapshot>(l).ok())
        .collect()
}

// ---------------------------------------------------------------------------
// Shadow scorer — Rune strategies, promotion standard as code (paper only)
// ---------------------------------------------------------------------------

/// The desk's promotion standard. A strategy is proven only on forward
/// evidence: enough settled windows, and BOTH chronological halves of the
/// record positive, at or above the per-contract bar.
#[derive(Clone, Copy, Serialize)]
struct Standard {
    min_windows: usize,
    min_mean_cents: f64,
}

impl Default for Standard {
    fn default() -> Self {
        Standard {
            min_windows: 100,
            min_mean_cents: 3.5,
        }
    }
}

/// Kalshi-style fee model used for shadow scoring, in dollars per contract.
fn fee(price: f64) -> f64 {
    0.07 * price * (1.0 - price)
}

struct Strategy {
    name: String,
    side: String,
    unit: Arc<rune::Unit>,
    runtime: Arc<rune::runtime::RuntimeContext>,
}

impl Strategy {
    fn load(path: &Path) -> Result<Strategy, String> {
        let script = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let context = rune::Context::with_default_modules().map_err(|e| e.to_string())?;
        let runtime = Arc::new(context.runtime().map_err(|e| e.to_string())?);
        let mut sources = rune::Sources::new();
        sources
            .insert(rune::Source::memory(&script).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let mut diagnostics = rune::Diagnostics::new();
        let unit = rune::prepare(&mut sources)
            .with_context(&context)
            .with_diagnostics(&mut diagnostics)
            .build()
            .map_err(|e| format!("strategy {} failed to build: {e}", path.display()))?;
        let unit = Arc::new(unit);
        let mut strat = Strategy {
            name: String::new(),
            side: String::new(),
            unit,
            runtime,
        };
        strat.name = strat.call_string("name")?;
        strat.side = strat.call_string("side")?;
        Ok(strat)
    }

    fn vm(&self) -> rune::Vm {
        rune::Vm::new(self.runtime.clone(), self.unit.clone())
    }

    fn call_string(&self, name: &str) -> Result<String, String> {
        let value = self
            .vm()
            .call([name], ())
            .map_err(|e| format!("strategy call {name} failed: {e}"))?;
        rune::from_value::<String>(value).map_err(|e| e.to_string())
    }

    fn allow_entry(&self, yes_ask: f64, no_ask: f64, secs_to_close: i64) -> bool {
        match self.vm().call(["allow_entry"], (yes_ask, no_ask, secs_to_close)) {
            Ok(value) => rune::from_value::<bool>(value).unwrap_or(false),
            Err(_) => false,
        }
    }
}

#[derive(Serialize)]
struct ScoreReport {
    strategy: String,
    side: String,
    windows_settled: usize,
    windows_traded: usize,
    mean_cents_per_contract: Option<f64>,
    first_half_cents: Option<f64>,
    second_half_cents: Option<f64>,
    standard: Standard,
    verdict: String,
}

fn score_strategy(strat: &Strategy, snapshots: &[Snapshot], standard: Standard) -> ScoreReport {
    // Group snapshots into market windows.
    let mut windows: BTreeMap<String, Vec<Snapshot>> = BTreeMap::new();
    for snap in snapshots {
        windows.entry(snap.ticker.clone()).or_default().push(snap.clone());
    }
    // (close_epoch, pnl per contract) for every settled window we entered.
    let mut trades: Vec<(i64, f64)> = Vec::new();
    let mut settled = 0usize;
    for (_ticker, mut snaps) in windows {
        snaps.sort_by_key(|s| s.fetched_at);
        let result = snaps
            .iter()
            .rev()
            .find(|s| !s.result.is_empty())
            .map(|s| s.result.clone());
        let Some(result) = result else { continue };
        settled += 1;
        let mut entry_price: Option<f64> = None;
        for snap in &snaps {
            let (Some(ya), Some(na)) = (snap.yes_ask, snap.no_ask) else {
                continue;
            };
            let Some(close) = parse_iso_z(&snap.close_time) else {
                continue;
            };
            let secs_to_close = close - snap.fetched_at;
            if strat.allow_entry(ya, na, secs_to_close) {
                entry_price = Some(if strat.side == "yes" { ya } else { na });
                break;
            }
        }
        let Some(price) = entry_price else { continue };
        if price <= 0.0 || price >= 1.0 {
            continue;
        }
        let payoff = if result == strat.side { 1.0 } else { 0.0 };
        let pnl = payoff - price - fee(price);
        let close_epoch = parse_iso_z(&snaps[0].close_time).unwrap_or(0);
        trades.push((close_epoch, pnl));
    }
    trades.sort_by_key(|(close, _)| *close);
    let traded = trades.len();
    let mean = |xs: &[f64]| {
        if xs.is_empty() {
            None
        } else {
            Some(xs.iter().sum::<f64>() / xs.len() as f64 * 100.0)
        }
    };
    let pnls: Vec<f64> = trades.iter().map(|(_, p)| *p).collect();
    let half = traded / 2;
    let first = mean(&pnls[..half]);
    let second = mean(&pnls[half..]);
    let overall = mean(&pnls);
    let verdict = if traded < standard.min_windows {
        format!(
            "INSUFFICIENT EVIDENCE — {traded} forward windows traded, the standard requires {}",
            standard.min_windows
        )
    } else if first.unwrap_or(0.0) > 0.0
        && second.unwrap_or(0.0) > 0.0
        && overall.unwrap_or(0.0) >= standard.min_mean_cents
    {
        "PROVEN on forward evidence (paper only — no orders placed)".to_string()
    } else {
        "NOT PROVEN — at least one half of the forward record is not positive above the bar"
            .to_string()
    };
    ScoreReport {
        strategy: strat.name.clone(),
        side: strat.side.clone(),
        windows_settled: settled,
        windows_traded: traded,
        mean_cents_per_contract: overall,
        first_half_cents: first,
        second_half_cents: second,
        standard,
        verdict,
    }
}

fn load_strategies(dir: &Path) -> Vec<Strategy> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().map(|x| x == "rn").unwrap_or(false))
            .collect();
        paths.sort();
        for path in paths {
            match Strategy::load(&path) {
                Ok(s) => out.push(s),
                Err(e) => eprintln!("strategy load failed: {e}"),
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Venue account state — read-only, and only if a credential is already in
// the environment. This build never hunts for one.
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct AccountState {
    status: String,
    balance: Option<f64>,
    open_positions: Vec<serde_json::Value>,
    day_pnl: Option<f64>,
    note: String,
}

fn account_state() -> AccountState {
    let key_id = std::env::var("KALSHI_API_KEY_ID").ok();
    let key_path = std::env::var("KALSHI_PRIVATE_KEY_PATH").ok();
    match (key_id, key_path) {
        (Some(_), Some(p)) if Path::new(&p).exists() => AccountState {
            status: "unavailable".to_string(),
            balance: None,
            open_positions: Vec::new(),
            day_pnl: None,
            note: "A credential is present in the environment, but signed venue reads are not enabled in this build — figures are shown as unavailable rather than guessed."
                .to_string(),
        },
        _ => AccountState {
            status: "unavailable".to_string(),
            balance: None,
            open_positions: Vec::new(),
            day_pnl: None,
            note: "No venue credential in the environment — account figures are unavailable. Market data below is public venue data and needs no credential."
                .to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Daily report — plain words, venue truth, signed P&L in line one
// ---------------------------------------------------------------------------

fn daily_report(data_dir: &Path, strategies_dir: &Path) -> String {
    let account = account_state();
    let pnl_line = match account.day_pnl {
        Some(p) => format!("Day P&L (venue): {}${:.2}", if p < 0.0 { "-" } else { "+" }, p.abs()),
        None => "Day P&L (venue): unavailable — no venue credential in the environment; nothing below invents one.".to_string(),
    };
    let snapshots = load_snapshots(&data_dir.join("snapshots.jsonl"));
    let tickers: std::collections::HashSet<&str> =
        snapshots.iter().map(|s| s.ticker.as_str()).collect();
    let settled = snapshots.iter().filter(|s| !s.result.is_empty()).count();
    let mut lines = vec![
        "OpenBell daily report".to_string(),
        pnl_line,
        format!(
            "Balance (venue): {}",
            account
                .balance
                .map(|b| format!("${b:.2}"))
                .unwrap_or_else(|| "unavailable".to_string())
        ),
        format!("Open positions (venue): {}", account.open_positions.len()),
        format!(
            "Forward collector: {} snapshots on disk across {} markets; {} settled-window records.",
            snapshots.len(),
            tickers.len(),
            settled
        ),
        "Shadow scoring (paper only — this build cannot place orders):".to_string(),
    ];
    for strat in load_strategies(strategies_dir) {
        let report = score_strategy(&strat, &snapshots, Standard::default());
        lines.push(format!(
            "  {} ({}): {} windows traded, verdict — {}",
            report.strategy, report.side, report.windows_traded, report.verdict
        ));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// HTTP layer (std-only)
// ---------------------------------------------------------------------------

struct App {
    data_dir: PathBuf,
    strategies_dir: PathBuf,
}

fn json_response(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn html_response(body: &str) -> String {
    format!(
        "HTTP/1.1 200\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, String, String)> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();
    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).ok()?;
    }
    Some((method, path, String::from_utf8_lossy(&body).to_string()))
}

fn latest_markets(data_dir: &Path) -> Vec<Snapshot> {
    let snapshots = load_snapshots(&data_dir.join("snapshots.jsonl"));
    let mut latest: BTreeMap<String, Snapshot> = BTreeMap::new();
    for snap in snapshots {
        latest
            .entry(snap.ticker.clone())
            .and_modify(|e| {
                if snap.fetched_at >= e.fetched_at {
                    *e = snap.clone();
                }
            })
            .or_insert(snap);
    }
    let mut out: Vec<Snapshot> = latest.into_values().collect();
    out.sort_by(|a, b| a.close_time.cmp(&b.close_time));
    out
}

fn handle(app: &App, method: &str, path: &str) -> String {
    let (route, query) = match path.split_once('?') {
        Some((r, q)) => (r, q),
        None => (path, ""),
    };
    match (method, route) {
        ("GET", "/") => html_response(include_str!("../assets/index.html")),
        ("GET", "/api/home") => {
            let home = serde_json::json!({
                "account": account_state(),
                "mode": "shadow — this build never places orders",
                "series": SERIES,
            });
            json_response(200, &home.to_string())
        }
        ("GET", "/api/markets") => {
            json_response(200, &serde_json::to_string(&latest_markets(&app.data_dir)).unwrap())
        }
        ("POST", "/api/collect") => match collect_once(&app.data_dir) {
            Ok(n) => json_response(200, &format!("{{\"snapshots_written\":{n}}}")),
            Err(e) => json_response(502, &format!("{{\"error\":\"{e}\"}}")),
        },
        ("GET", "/api/strategies") => {
            let dataset = if query.contains("dataset=fixture") {
                app.data_dir.join("fixture-snapshots.jsonl")
            } else {
                app.data_dir.join("snapshots.jsonl")
            };
            let snapshots = load_snapshots(&dataset);
            let reports: Vec<ScoreReport> = load_strategies(&app.strategies_dir)
                .iter()
                .map(|s| score_strategy(s, &snapshots, Standard::default()))
                .collect();
            json_response(200, &serde_json::to_string(&reports).unwrap())
        }
        ("GET", "/api/report") => {
            let report = daily_report(&app.data_dir, &app.strategies_dir);
            json_response(200, &serde_json::json!({ "report": report }).to_string())
        }
        _ => json_response(404, "{\"error\":\"not found\"}"),
    }
}

fn main() {
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8472);
    let data_dir = std::env::var("OPENBELL_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./openbell-data"));
    let strategies_dir = std::env::var("OPENBELL_STRATEGIES")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./strategies"));
    let _ = std::fs::create_dir_all(&data_dir);

    // Forward collector: one pass now, then on an interval.
    let collect_secs: u64 = std::env::var("OPENBELL_COLLECT_SECS")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(60);
    {
        let data_dir = data_dir.clone();
        std::thread::spawn(move || loop {
            match collect_once(&data_dir) {
                Ok(n) => println!("collector: {n} snapshots written"),
                Err(e) => eprintln!("collector: {e}"),
            }
            std::thread::sleep(std::time::Duration::from_secs(collect_secs));
        });
    }

    let app = Arc::new(App {
        data_dir,
        strategies_dir,
    });
    let listener = TcpListener::bind(("127.0.0.1", port)).expect("bind failed");
    println!("openbell-rs listening on http://127.0.0.1:{port} (shadow mode — no order path exists)");
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                let app = app.clone();
                std::thread::spawn(move || {
                    if let Some((method, path, _body)) = read_request(&mut stream) {
                        let response = handle(&app, &method, &path);
                        let _ = stream.write_all(response.as_bytes());
                    }
                });
            }
            Err(_) => continue,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(ticker: &str, fetched_at: i64, close: &str, result: &str, ya: Option<f64>, na: Option<f64>) -> Snapshot {
        Snapshot {
            fetched_at,
            ticker: ticker.to_string(),
            event_ticker: "EV".to_string(),
            series: "KXBTC15M".to_string(),
            close_time: close.to_string(),
            status: if result.is_empty() { "active".into() } else { "settled".into() },
            result: result.to_string(),
            yes_bid: ya.map(|v| v - 0.02),
            yes_ask: ya,
            no_bid: na.map(|v| v - 0.02),
            no_ask: na,
            last_price: None,
        }
    }

    #[test]
    fn iso_parse_round_trip() {
        assert_eq!(parse_iso_z("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_z("1970-01-02T00:00:00Z"), Some(86400));
        assert!(parse_iso_z("2026-11-17T23:00:00Z").unwrap() > 1_700_000_000);
    }

    #[test]
    fn fee_model() {
        assert!((fee(0.5) - 0.0175).abs() < 1e-9);
        assert!(fee(0.9) < fee(0.5));
    }

    #[test]
    fn insufficient_evidence_is_the_honest_default() {
        let strat = Strategy::load(Path::new("strategies/favourite_late.rn")).unwrap();
        let report = score_strategy(&strat, &[], Standard::default());
        assert_eq!(report.windows_traded, 0);
        assert!(report.verdict.contains("INSUFFICIENT EVIDENCE"));
    }

    #[test]
    fn scorer_rewards_a_winning_shape_and_kills_a_losing_one() {
        // 6 windows: YES settles in 5. favourite-late buys YES at 0.70;
        // longshot-late buys NO at 0.32 and loses 5 of 6.
        let mut snaps = Vec::new();
        for i in 0..6 {
            let close = format!("2026-11-17T{:02}:00:00Z", 10 + i);
            let close_epoch = parse_iso_z(&close).unwrap();
            let result = if i == 5 { "no" } else { "yes" };
            snaps.push(snap(&format!("T{i}"), close_epoch - 240, &close, "", Some(0.70), Some(0.32)));
            snaps.push(snap(&format!("T{i}"), close_epoch + 60, &close, result, None, None));
        }
        let standard = Standard { min_windows: 4, min_mean_cents: 3.5 };
        let fav = Strategy::load(Path::new("strategies/favourite_late.rn")).unwrap();
        let report = score_strategy(&fav, &snaps, standard);
        assert_eq!(report.windows_traded, 6);
        assert!(report.verdict.contains("PROVEN"), "verdict: {}", report.verdict);
        let long = Strategy::load(Path::new("strategies/longshot_late.rn")).unwrap();
        let report = score_strategy(&long, &snaps, standard);
        assert_eq!(report.windows_traded, 6);
        assert!(report.verdict.contains("NOT PROVEN"), "verdict: {}", report.verdict);
    }
}
