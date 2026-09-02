//! Launch-transition e2e (dsh-fni): the real shell must carry the window
//! from the loading screen into the authenticated harness GUI, and the
//! persisted app log must never carry a raw launch token.
//!
//! Assertions, all against observable artifacts of a real launch:
//! 1. the app log gains the launch markers — sidecar spawned, `auth path
//!    verified through proxy (200)`, proxied navigation;
//! 2. no `token=` value anywhere in the persisted log is anything other than
//!    the literal `[redacted]` (the persisted-log redaction boundary);
//! 3. a graceful quit stops the sidecar (`sidecar stopped` in the log) and
//!    the process is gone.
//!
//! macOS with a GUI session required: the test launches the debug `.app`
//! bundle through LaunchServices and quits it via AppleScript. Gated behind
//! `#[ignore]`; run with:
//!
//! ```text
//! cargo test --manifest-path src-tauri/Cargo.toml --test app_launch_transition -- --ignored --nocapture
//! ```
//!
//! Prerequisite: a debug bundle (`npm run build:debug`) — a bare binary has
//! no LaunchServices registration, so AppleScript cannot quit it — and an
//! assembled host bundle for the app to serve (both are produced by
//! `npm run build:debug`).

#![cfg(target_os = "macos")]

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LOG_RELATIVE: &str = "Library/Logs/com.dsh.desktop/dsh-desktop.log";
const APP_NAME: &str = "dsh-desktop";
const MARKERS: [&str; 3] = [
    "auth path verified through proxy (200)",
    "navigating via cookie proxy on",
    "harness page loaded (",
];
const FAILURE_MARKERS: [&str; 2] = ["navigate failed", "proxied navigation failed"];

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().expect("repo root").to_path_buf()
}

fn debug_app() -> PathBuf {
    workspace()
        .join("src-tauri")
        .join("target")
        .join("debug")
        .join("bundle")
        .join("macos")
        .join(format!("{APP_NAME}.app"))
}

fn app_log() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME set on macOS");
    PathBuf::from(home).join(LOG_RELATIVE)
}

fn read_log() -> String {
    let path = app_log();
    let Ok(mut file) = std::fs::File::open(&path) else {
        return String::new();
    };
    let mut raw = Vec::new();
    let _ = file.read_to_end(&mut raw);
    String::from_utf8_lossy(&raw).into_owned()
}

/// Everything the current log holds beyond the snapshot: after a rotation or
/// truncation the whole file is new, so the snapshot is matched as a prefix
/// and stripped only when it is still a prefix.
fn new_log_since(baseline: &str) -> String {
    let current = read_log();
    if baseline.len() <= current.len() && current.starts_with(baseline) {
        current[baseline.len()..].to_string()
    } else {
        current
    }
}

fn wait_for_new_log(baseline: &str, markers: &[&str], what: &str, budget: Duration) -> String {
    let deadline = Instant::now() + budget;
    loop {
        let new_log = new_log_since(baseline);
        if markers.iter().all(|marker| new_log.contains(marker)) {
            return new_log;
        }
        if Instant::now() > deadline {
            panic!("timeout waiting for {what}; new log:\n{new_log}");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The persisted-log redaction boundary: every `token=` value in the log
/// must be the literal `[redacted]`. A raw token is long base64url; a
/// redacted one is exactly the placeholder.
fn assert_no_raw_token_in(log: &str) {
    for line in log.lines() {
        let mut from = 0;
        while let Some(at) = line[from..].find("token=") {
            let value_start = from + at + "token=".len();
            let value_end = line[value_start..]
                .find(|c: char| c == '&' || c.is_whitespace() || c == ')')
                .map(|end| value_start + end)
                .unwrap_or(line.len());
            assert_eq!(
                &line[value_start..value_end],
                "[redacted]",
                "a raw launch token leaked into the persisted log: {line}"
            );
            from = value_end;
        }
    }
}

fn app_process_count() -> u32 {
    Command::new("pgrep")
        .arg("-f")
        .arg(format!("{APP_NAME}.app/Contents/MacOS"))
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count() as u32
        })
        .unwrap_or(0)
}

/// Quit-and-kill guard: whatever happens in the test (panic included), no
/// dsh-desktop instance or sidecar is left behind to trip the next run's
/// precondition.
struct AppGuard;

impl AppGuard {
    fn arm() -> Self {
        AppGuard
    }
}

impl Drop for AppGuard {
    fn drop(&mut self) {
        if app_process_count() == 0 {
            return;
        }
        let _ = Command::new("osascript")
            .arg("-e")
            .arg(format!("quit app \"{APP_NAME}\""))
            .status();
        let deadline = Instant::now() + Duration::from_secs(10);
        while app_process_count() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
        }
        if app_process_count() > 0 {
            let _ = Command::new("pkill")
                .arg("-9")
                .arg("-f")
                .arg(format!("{APP_NAME}.app/Contents/MacOS"))
                .status();
        }
        let _ = Command::new("pkill")
            .arg("-9")
            .arg("-f")
            .arg("resources/host/main.mjs")
            .status();
    }
}

#[test]
#[ignore = "drives the real window on macOS; run with -- --ignored"]
fn launch_reaches_the_authenticated_gui_and_the_log_stays_token_free() {
    let app = debug_app();
    assert!(
        app.join("Contents").join("MacOS").exists(),
        "no debug bundle at {} — run `npm run build:debug` first",
        app.display()
    );
    assert_eq!(
        app_process_count(),
        0,
        "a dsh-desktop instance is already running; quit it before this test"
    );
    // Whatever happens from here (panic included), nothing is left behind.
    let _guard = AppGuard;

    // Archive the previous log so this launch's markers cannot be satisfied
    // by a previous session's lines (the rotation fallback in new_log_since
    // would otherwise make the wait vacuous). The archive keeps the history.
    if app_log().exists() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or_default();
        let archived = app_log().with_extension(format!("log.{stamp}.bak"));
        std::fs::rename(&app_log(), archived).expect("archive the previous log");
    }
    let baseline = String::new();
    let opened = Command::new("open")
        .arg("-n")
        .arg(&app)
        .status()
        .expect("run open");
    assert!(opened.success(), "`open` failed for {}", app.display());

    // The transition: loading screen → authenticated harness, observed
    // through the shell's own launch markers.
    let new_log = wait_for_new_log(
        &baseline,
        &MARKERS,
        "the launch to reach the authenticated harness",
        Duration::from_secs(60),
    );
    assert!(
        new_log.contains("bundled harness version:"),
        "the bundled version line must be logged"
    );
    // Navigation actually completed — the shell logs its failures, and a
    // stranded loading screen would never produce the harness page-load
    // marker at all.
    for failure in FAILURE_MARKERS {
        assert!(
            !new_log.contains(failure),
            "navigation failure logged: {failure}"
        );
    }
    // Non-vacuous token scan: the ready line guarantees a redacted token is
    // in the log; its absence would mean nothing was scanned.
    assert!(
        new_log.contains("token=[redacted]"),
        "vacuous scan: no ready line in the log region"
    );
    assert_no_raw_token_in(&read_log());

    // Graceful quit through the normal user path: LaunchServices terminate →
    // ExitRequested → sidecar stopped.
    let quit = Command::new("osascript")
        .arg("-e")
        .arg(format!("quit app \"{APP_NAME}\""))
        .status()
        .expect("run osascript");
    assert!(quit.success(), "AppleScript quit failed");
    wait_for_new_log(
        &baseline,
        &["sidecar stopped"],
        "the sidecar to stop after the quit",
        Duration::from_secs(15),
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while app_process_count() > 0 {
        assert!(
            Instant::now() < deadline,
            "the shell process survived its own quit"
        );
        std::thread::sleep(Duration::from_millis(500));
    }

    assert_no_raw_token_in(&read_log());
}
