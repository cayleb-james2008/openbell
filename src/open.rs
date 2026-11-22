//! Opens the desk as a standalone chromeless desktop window (`chromium --app=`
//! on Linux, `msedge.exe --app=` on Windows, Chrome/Edge `--app=` on macOS)
//! once per process start, when a person is there to see it — falling back
//! cleanly to the platform's default browser opener (`xdg-open`, `open`, or
//! `cmd /c start`) when no Chromium-family desktop runtime is present.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Whether the desktop window should open on this start. `args` are the
/// arguments after the program name; `get` reads one environment variable;
/// `os` is the platform name (`std::env::consts::OS`).
pub fn wanted(args: &[String], get: &dyn Fn(&str) -> Option<String>, os: &str) -> bool {
    let flag = |f: &str| args.iter().any(|a| a == f);
    if flag("--no-open") || flag("--service") || get("OPENBELL_NO_OPEN").as_deref() == Some("1") {
        return false;
    }
    // A service manager started this process: nobody is sitting at a desktop.
    if get("INVOCATION_ID").is_some() {
        return false;
    }
    // Linux without a display cannot show a window.
    if os == "linux" && get("DISPLAY").is_none() && get("WAYLAND_DISPLAY").is_none() {
        return false;
    }
    true
}

/// Find the earliest directory index (and full path) on `PATH` containing `bin`.
fn find_on_path_with_index(path_env: &str, bin: &str, sep: char) -> Option<(usize, PathBuf)> {
    for (idx, dir) in path_env.split(sep).enumerate() {
        if dir.is_empty() {
            continue;
        }
        let candidate = Path::new(dir).join(bin);
        if candidate.is_file() {
            return Some((idx, candidate));
        }
    }
    None
}

/// Resolve the preferred desktop launcher `(program, args)` for `(os, url)`.
///
/// On Linux, prefers Chromium-family `--app=<url>` for a chromeless desktop
/// window, unless an explicit `xdg-open` override appears strictly earlier on
/// `PATH` (preserving isolated test shims) or no Chromium binary is installed.
/// On Windows, prefers `msedge.exe --app=<url>` (standard install paths or
/// `PATH`), then `chrome.exe --app=<url>`, falling back to `cmd /c start`.
pub fn resolve_launcher(
    url: &str,
    os: &str,
    path_env: Option<&str>,
    extra_probe: &dyn Fn(&Path) -> bool,
) -> (String, Vec<String>) {
    let app_arg = format!("--app={url}");
    match os {
        "windows" => {
            let win_candidates = [
                r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
                r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
                r"C:\Program Files\Google\Chrome\Application\chrome.exe",
                r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
            ];
            for path_str in win_candidates {
                if extra_probe(Path::new(path_str)) {
                    return (path_str.to_string(), vec![app_arg, "--new-window".to_string()]);
                }
            }
            if let Some(p) = path_env {
                for bin in ["msedge.exe", "chrome.exe"] {
                    if find_on_path_with_index(p, bin, ';').is_some() {
                        return (bin.to_string(), vec![app_arg, "--new-window".to_string()]);
                    }
                }
            }
            (
                "cmd".to_string(),
                vec!["/c".into(), "start".into(), String::new(), url.to_string()],
            )
        }
        "macos" => {
            let mac_candidates = [
                "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
                "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
                "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
            ];
            for path_str in mac_candidates {
                if extra_probe(Path::new(path_str)) {
                    return (path_str.to_string(), vec![app_arg]);
                }
            }
            ("open".to_string(), vec![url.to_string()])
        }
        _ => {
            let path_str = path_env.unwrap_or("");
            let xdg_idx = find_on_path_with_index(path_str, "xdg-open", ':').map(|(i, _)| i);
            let mut best_app: Option<(usize, String)> = None;
            for bin in [
                "chromium",
                "chromium-browser",
                "google-chrome",
                "google-chrome-stable",
                "brave",
                "brave-browser",
                "microsoft-edge",
            ] {
                if let Some((idx, _)) = find_on_path_with_index(path_str, bin, ':') {
                    if best_app.as_ref().is_none_or(|(b_idx, _)| idx < *b_idx) {
                        best_app = Some((idx, bin.to_string()));
                    }
                }
            }
            if let Some((app_idx, bin)) = best_app {
                // If an explicit xdg-open shim sits strictly earlier on PATH than the
                // browser binary, honor xdg-open so test harnesses never spawn a GUI window.
                if xdg_idx.is_none_or(|xi| xi >= app_idx) {
                    return (
                        bin,
                        vec![
                            app_arg,
                            "--new-window".to_string(),
                            "--class=OpenBell".to_string(),
                        ],
                    );
                }
            }
            ("xdg-open".to_string(), vec![url.to_string()])
        }
    }
}

/// Starts the platform's desktop app window (or fallback browser) on `url` and returns at once.
pub fn open_in_browser(url: &str) {
    let path_env = std::env::var("PATH").ok();
    let (program, args) = resolve_launcher(
        url,
        std::env::consts::OS,
        path_env.as_deref(),
        &|p| p.is_file(),
    );
    std::thread::spawn(move || {
        let _ = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |k: &str| owned.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_person_at_a_display_gets_the_page_opened() {
        assert!(wanted(&args(&[]), &env_of(&[("DISPLAY", ":0")]), "linux"));
        assert!(wanted(
            &args(&[]),
            &env_of(&[("WAYLAND_DISPLAY", "wayland-1")]),
            "linux"
        ));
        assert!(
            wanted(&args(&[]), &env_of(&[]), "macos"),
            "macOS has no display variable to check"
        );
        assert!(wanted(&args(&[]), &env_of(&[]), "windows"));
    }

    #[test]
    fn every_opt_out_wins() {
        let display = [("DISPLAY", ":0")];
        assert!(!wanted(&args(&["--no-open"]), &env_of(&display), "linux"));
        assert!(!wanted(&args(&["--service"]), &env_of(&display), "linux"));
        assert!(!wanted(
            &args(&[]),
            &env_of(&[("OPENBELL_NO_OPEN", "1"), ("DISPLAY", ":0")]),
            "linux"
        ));
        assert!(
            wanted(
                &args(&[]),
                &env_of(&[("OPENBELL_NO_OPEN", "0"), ("DISPLAY", ":0")]),
                "linux"
            ),
            "only the value 1 opts out"
        );
        assert!(
            !wanted(
                &args(&[]),
                &env_of(&[("INVOCATION_ID", "abc"), ("DISPLAY", ":0")]),
                "linux"
            ),
            "a service manager started it"
        );
        assert!(!wanted(&args(&[]), &env_of(&[]), "linux"), "no display on Linux");
        assert!(!wanted(
            &args(&[]),
            &env_of(&[("INVOCATION_ID", "abc")]),
            "macos"
        ));
    }

    #[test]
    fn chromeless_desktop_launcher_prefers_app_window_and_respects_shim_and_windows_edge() {
        // Windows: msedge.exe --app= when installed, fallback to cmd /c start otherwise.
        let (prog_w, args_w) = resolve_launcher(
            "http://127.0.0.1:8472",
            "windows",
            None,
            &|p| p.to_string_lossy().contains("msedge.exe"),
        );
        assert!(prog_w.ends_with("msedge.exe"));
        assert_eq!(args_w[0], "--app=http://127.0.0.1:8472");

        let (prog_fb, args_fb) = resolve_launcher("http://127.0.0.1:8472", "windows", None, &|_| false);
        assert_eq!(prog_fb, "cmd");
        assert_eq!(args_fb.last().unwrap(), "http://127.0.0.1:8472");

        // Linux: chromium --app= when chromium & xdg-open share directory; xdg-open when shim sits earlier.
        let dir = std::env::temp_dir().join(format!("ob-open-{}", std::process::id()));
        let bin_shim = dir.join("shim");
        let bin_sys = dir.join("sys");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&bin_shim).unwrap();
        std::fs::create_dir_all(&bin_sys).unwrap();
        std::fs::write(bin_sys.join("chromium"), "#!/bin/sh\n").unwrap();
        std::fs::write(bin_sys.join("xdg-open"), "#!/bin/sh\n").unwrap();
        std::fs::write(bin_shim.join("xdg-open"), "#!/bin/sh\n").unwrap();

        let normal_path = bin_sys.to_string_lossy().to_string();
        let (p_linux, a_linux) = resolve_launcher(
            "http://127.0.0.1:8472",
            "linux",
            Some(&normal_path),
            &|_| false,
        );
        assert_eq!(p_linux, "chromium");
        assert_eq!(a_linux[0], "--app=http://127.0.0.1:8472");

        let shim_path = format!("{}:{}", bin_shim.display(), bin_sys.display());
        let (p_shim, a_shim) = resolve_launcher(
            "http://127.0.0.1:8472",
            "linux",
            Some(&shim_path),
            &|_| false,
        );
        assert_eq!(p_shim, "xdg-open");
        assert_eq!(a_shim, vec!["http://127.0.0.1:8472"]);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
