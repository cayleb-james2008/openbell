//! The binary answers `--version` before it does anything else: it must not
//! bind a port, read the tape, or start the collector.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn version_flag_prints_name_and_version_and_exits() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_openbell-rs"))
        .arg("--version")
        .env("PORT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn openbell-rs");
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("--version did not exit: the binary started serving instead");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), format!("openbell-rs {}", env!("CARGO_PKG_VERSION")));
    assert_eq!(env!("CARGO_PKG_VERSION"), "0.35.0");
}
