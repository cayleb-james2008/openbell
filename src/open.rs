//! Opens the desk's page in the browser once per process start, when a person
//! is there to see it. The only process started here is the platform's opener
//! (`xdg-open`, `open`, or `cmd /c start`), run in the background; its failure
//! is silent, because the start-up banner already printed the address.

use std::process::{Command, Stdio};

/// Whether the browser should open on this start. `args` are the arguments
/// after the program name; `get` reads one environment variable; `os` is the
/// platform name (`std::env::consts::OS`).
pub fn wanted(args: &[String], get: &dyn Fn(&str) -> Option<String>, os: &str) -> bool {
    let flag = |f: &str| args.iter().any(|a| a == f);
    if flag("--no-open") || flag("--service") || get("OPENBELL_NO_OPEN").as_deref() == Some("1") {
        return false;
    }
    // A service manager started this process: nobody is sitting at a browser.
    if get("INVOCATION_ID").is_some() {
        return false;
    }
    // Linux without a display cannot show a page.
    if os == "linux" && get("DISPLAY").is_none() && get("WAYLAND_DISPLAY").is_none() {
        return false;
    }
    true
}

/// Starts the platform's opener on `url` and returns at once.
pub fn open_in_browser(url: &str) {
    let (program, args): (&str, Vec<String>) = match std::env::consts::OS {
        "macos" => ("open", vec![url.to_string()]),
        "windows" => ("cmd", vec!["/c".into(), "start".into(), String::new(), url.to_string()]),
        _ => ("xdg-open", vec![url.to_string()]),
    };
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
        let owned: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k: &str| owned.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone())
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_person_at_a_display_gets_the_page_opened() {
        assert!(wanted(&args(&[]), &env_of(&[("DISPLAY", ":0")]), "linux"));
        assert!(wanted(&args(&[]), &env_of(&[("WAYLAND_DISPLAY", "wayland-1")]), "linux"));
        assert!(wanted(&args(&[]), &env_of(&[]), "macos"), "macOS has no display variable to check");
        assert!(wanted(&args(&[]), &env_of(&[]), "windows"));
    }

    #[test]
    fn every_opt_out_wins() {
        let display = [("DISPLAY", ":0")];
        assert!(!wanted(&args(&["--no-open"]), &env_of(&display), "linux"));
        assert!(!wanted(&args(&["--service"]), &env_of(&display), "linux"));
        assert!(!wanted(&args(&[]), &env_of(&[("OPENBELL_NO_OPEN", "1"), ("DISPLAY", ":0")]), "linux"));
        assert!(wanted(&args(&[]), &env_of(&[("OPENBELL_NO_OPEN", "0"), ("DISPLAY", ":0")]), "linux"), "only the value 1 opts out");
        assert!(!wanted(&args(&[]), &env_of(&[("INVOCATION_ID", "abc"), ("DISPLAY", ":0")]), "linux"), "a service manager started it");
        assert!(!wanted(&args(&[]), &env_of(&[]), "linux"), "no display on Linux");
        assert!(!wanted(&args(&[]), &env_of(&[("INVOCATION_ID", "abc")]), "macos"));
    }
}
