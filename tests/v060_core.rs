//! Integration tests for OpenBell v0.6.0 HTTP endpoints & top-of-book depth:
//! - Public collector top-of-book depth (`yes_bid_size` / `no_bid_size`) -> `observed_median_depth`
//! - `GET /api/analytics/recalibration` (PAV isotonic regression IS 60% / OOS 40% + EV gate)
//! - `POST /api/analytics/scale-plan` (`$500/day` paper capital scaling, liquidity ceiling, 5 gates)
//! - 100,000-tick / 2,000-window scale + 30 concurrent clients under 300ms p95 budget

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct ServerGuard {
    child: Child,
    dir: PathBuf,
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn http_req(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect server");
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let req = if body.is_empty() {
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n")
    } else {
        format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    };
    s.write_all(req.as_bytes()).unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let (hdr, b) = raw.split_once("\r\n\r\n").expect("valid HTTP response");
    let status: u16 = hdr
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    (status, b.to_string())
}

fn spawn_server(dir: &Path, port: u16, extra_path: Option<&Path>) -> ServerGuard {
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let strat = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("strategies");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_openbell-rs"));
    cmd.env("PORT", port.to_string())
        .env("OPENBELL_DATA", &data)
        .env("OPENBELL_STRATEGIES", &strat)
        .env("OPENBELL_COLLECT_SECS", "3600")
        .env("OPENBELL_COLLECT_PACE_MS", "0")
        .env("OPENBELL_UPDATE_MODE", "off")
        .env("OPENBELL_NO_OPEN", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(bin_dir) = extra_path {
        let cur = std::env::var("PATH").unwrap_or_default();
        cmd.env("PATH", format!("{}:{cur}", bin_dir.display()));
    }
    let child = cmd.spawn().expect("spawn openbell-rs");
    let guard = ServerGuard {
        child,
        dir: dir.to_path_buf(),
    };
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return guard;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("openbell-rs did not start on port {port}");
}

#[test]
fn depth_collection_recalibration_and_scale_plan_work_end_to_end() {
    let dir = std::env::temp_dir().join(format!("ob-v060-depth-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("curl");
    let script = r#"#!/bin/sh
url="$*"
case "$url" in
  *status=open*)
    echo '{"markets":[{"ticker":"KXBTC15M-DEPTH-1","event_ticker":"KXBTC15M-DEPTH","close_time":"2026-12-31T23:45:00Z","status":"open","result":"","yes_bid_dollars":"0.52","yes_ask_dollars":"0.54","no_bid_dollars":"0.46","no_ask_dollars":"0.48","last_price_dollars":"0.53","yes_bid_size_fp":"420.50","yes_ask_size_fp":"180.00"}]}'
    ;;
  *)
    echo '{"markets":[]}'
    ;;
esac
"#;
    std::fs::write(&shim, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let port = free_port();
    let _srv = spawn_server(&dir, port, Some(&bin));

    // Trigger one public collector pass via fake Kalshi shim to capture top-of-book depth.
    let (st_col, body_col) = http_req(port, "POST", "/api/collect", "{}");
    assert_eq!(st_col, 200, "collect failed: {body_col}");

    // Capabilities expose v0.6.0 routes.
    let (st_home, body_home) = http_req(port, "GET", "/api/home", "");
    assert_eq!(st_home, 200);
    let home: serde_json::Value = serde_json::from_str(&body_home).unwrap();
    let caps = home["capabilities"].as_array().unwrap();
    assert!(caps.iter().any(|c| c == "recalibration"));
    assert!(caps.iter().any(|c| c == "scale_plan"));

    // Collected market carries live top-of-book depth fields.
    let (st_mk, body_mk) = http_req(port, "GET", "/api/markets", "");
    assert_eq!(st_mk, 200);
    let mk: serde_json::Value = serde_json::from_str(&body_mk).unwrap();
    let first = &mk.as_array().unwrap()[0];
    assert!((first["yes_bid_size"].as_f64().unwrap() - 420.5).abs() < 1e-6);
    assert!((first["no_bid_size"].as_f64().unwrap() - 180.0).abs() < 1e-6);

    // Scale plan on empty settled tape honestly refuses required bankroll and surfaces observed depth.
    let (st_sp, body_sp) = http_req(port, "POST", "/api/analytics/scale-plan", r#"{"target_daily_dollars":500}"#);
    assert_eq!(st_sp, 200, "scale-plan failed: {body_sp}");
    let sp: serde_json::Value = serde_json::from_str(&body_sp).unwrap();
    assert_eq!(sp["target_daily_dollars"], 500.0);
    assert!(sp["required_bankroll_equal_slots"].is_null());
    assert_eq!(sp["readiness_gates"]["verdict"], "PAPER_RESEARCH_ONLY");
    let obs_depth = sp["liquidity_ceiling"]["observed_median_depth"].as_f64().unwrap();
    assert!((obs_depth - 300.25).abs() < 1e-6, "expected median(420.5, 180.0)=300.25, got {obs_depth}");

    // Error handling: bad JSON -> 400; unknown dataset -> 404.
    let (st_bad, _) = http_req(port, "POST", "/api/analytics/scale-plan", "not-json");
    assert_eq!(st_bad, 400);
    let (st_404_r, _) = http_req(port, "GET", "/api/analytics/recalibration?dataset=nope", "");
    assert_eq!(st_404_r, 404);
    let (st_404_s, _) = http_req(port, "POST", "/api/analytics/scale-plan?dataset=nope", "{}");
    assert_eq!(st_404_s, 404);
}

#[test]
fn v060_recalibration_and_scale_plan_100k_ticks_30_concurrent_under_300ms_p95() {
    let dir = std::env::temp_dir().join(format!("ob-v060-scale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let data = dir.join("data");
    std::fs::create_dir_all(&data).unwrap();

    // 100,000 ticks across 2,000 settled 15m windows with realistic top-of-book depth.
    let mut buf = String::with_capacity(28 * 1024 * 1024);
    let base_epoch: i64 = 1_790_812_800;
    let series = ["KXBTC15M", "KXETH15M", "KXSOL15M", "KXXRP15M", "KXDOGE15M"];
    for w in 0..2000usize {
        let ser = series[w % series.len()];
        let ticker = format!("{ser}-W{w:04}");
        let close_epoch = base_epoch + (w as i64) * 900;
        let close_time = format!(
            "2026-10-{:02}T{:02}:{:02}:00Z",
            1 + (w / 96),
            (w / 4) % 24,
            (w % 4) * 15
        );
        let result = if w % 4 != 0 { "yes" } else { "no" };
        for t in 0..50usize {
            let fetched_at = close_epoch - 900 + (t as i64) * 17;
            let st = if t == 49 { "finalized" } else { "open" };
            let res = if t == 49 { result } else { "" };
            let ya = 0.55 + ((t % 10) as f64) * 0.02;
            let yb = ya - 0.02;
            let na = (1.0 - yb).clamp(0.01, 0.99);
            let nb = (1.0 - ya).clamp(0.01, 0.99);
            buf.push_str(&format!(
                "{{\"fetched_at\":{fetched_at},\"ticker\":\"{ticker}\",\"event_ticker\":\"{ticker}\",\"series\":\"{ser}\",\"close_time\":\"{close_time}\",\"status\":\"{st}\",\"result\":\"{res}\",\"yes_bid\":{yb:.2},\"yes_ask\":{ya:.2},\"no_bid\":{nb:.2},\"no_ask\":{na:.2},\"last_price\":{ya:.2},\"yes_bid_size\":320.0,\"no_bid_size\":280.0}}\n"
            ));
        }
    }
    std::fs::write(data.join("snapshots.jsonl"), &buf).unwrap();
    drop(buf);

    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let shim = bin.join("curl");
    std::fs::write(&shim, "#!/bin/sh\necho '{\"error\":{\"code\":\"offline\"}}'\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let port = free_port();
    let _srv = spawn_server(&dir, port, Some(&bin));

    // Warm & validate GET /api/analytics/recalibration
    let (st_rec, body_rec) = http_req(port, "GET", "/api/analytics/recalibration?slippage_cents=1.0", "");
    assert_eq!(st_rec, 200, "recalibration failed: {body_rec}");
    let rec: serde_json::Value = serde_json::from_str(&body_rec).unwrap();
    assert_eq!(rec["windows_settled"], 2_000);
    assert_eq!(rec["portfolio"]["oos_buckets"].as_array().unwrap().len(), 20);
    assert!(rec["portfolio"]["is_count"].as_u64().unwrap() > 0);
    assert!(rec["portfolio"]["oos_count"].as_u64().unwrap() > 0);

    // Warm & validate POST /api/analytics/scale-plan (empirical + what-if sensitivity)
    let sp_body = r#"{"target_daily_dollars":500,"starting_bankroll":250,"slippage_cents":1.0,"max_contracts_per_window":250,"active_series_count":4,"sizing_mode":"quarter_kelly","paths":1000,"seed":20261122,"assumed_edge_cents":3.5}"#;
    let (st_sp, body_sp) = http_req(port, "POST", "/api/analytics/scale-plan", sp_body);
    assert_eq!(st_sp, 200, "scale-plan failed: {body_sp}");
    let sp: serde_json::Value = serde_json::from_str(&body_sp).unwrap();
    assert_eq!(sp["compounding_projection"]["mode_label"], "Empirical Tape");
    assert_eq!(sp["sensitivity_projection"]["mode_label"], "Sensitivity (Assumed Edge)");
    assert_eq!(sp["readiness_gates"]["gates"].as_array().unwrap().len(), 5);
    assert_eq!(sp["liquidity_ceiling"]["observed_median_depth"], 300.0);

    // 30 concurrent readers across recalibration + scale-plan under <300ms p95 budget.
    let mut handles = Vec::new();
    for i in 0..30usize {
        let req_str = sp_body.to_string();
        handles.push(std::thread::spawn(move || {
            let t0 = Instant::now();
            let (st, b) = if i % 2 == 0 {
                http_req(port, "GET", "/api/analytics/recalibration?slippage_cents=1.0", "")
            } else {
                http_req(port, "POST", "/api/analytics/scale-plan", &req_str)
            };
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(st, 200, "concurrent v060 call failed: {b}");
            ms
        }));
    }
    let mut lats: Vec<f64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    lats.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p95 = lats[(lats.len() * 95 / 100).min(lats.len() - 1)];
    assert!(
        p95 < 300.0,
        "v0.6.0 100k-tick 30-client p95 {p95:.1}ms exceeded 300ms budget (lats: {lats:?})"
    );
}
