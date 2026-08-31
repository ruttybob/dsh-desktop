//! Unreachable-server stub: interval probing, diagnostics, Retry / Quit
//! (Attach Mode, ticket dsh-cxq).
//!
//! Tauri/wry does not report WebView navigation failures, so a dead attach
//! server would otherwise leave a blank window with no signal at all. This
//! module is the only detector: a Rust-side probe of the attach URL on an
//! interval, and when the verdict flips to dead the window is navigated to
//! `ui/unreachable.html` (mono diagnostics, Retry, Quit).
//!
//! # Probe transport (documented decision)
//!
//! The probe uses `std::net::TcpStream` — dependency-free, and everything the
//! probe needs (connect with timeout, read with timeout) is in std. `reqwest`
//! appears in Cargo.lock only as a transitive dependency built WITHOUT any TLS
//! backend, so depending on it directly would either drag in a TLS stack for a
//! two-line check or fail on https targets. For `http://` URLs the probe does
//! a real `GET / HTTP/1.0` and classifies the status line; for `https://` a
//! successful TCP connect is the alive verdict (TLS is not spoken — a plain
//! socket to a live TLS listener still connects, which is exactly the liveness
//! question being asked).
//!
//! Verdict rule (per the ticket): ANY HTTP response means alive, including
//! 4xx/5xx — the question is "is something listening", not "is it healthy".
//! Connect refused / timeout / unresolvable host means dead.
//!
//! # Threading model (documented decision)
//!
//! One daemon `std::thread` per process, spawned lazily by [`start_monitor`]
//! and owned by the current run: it exits as soon as the monitored target is
//! cleared, and the process exit at app close kills it regardless, so no
//! thread outlives the app. The target lives in a `Mutex` managed as
//! [`ProbeMonitor`]; a late probe for a replaced target is discarded by URL
//! comparison, never applied.
//!
//! # Quit never stops the external server (documented decision)
//!
//! Attach mode manages no [`crate::host::HostManager`] state, so the
//! `RunEvent::Exit` hook's `try_state::<HostManager>()` finds nothing and the
//! attached external server is never touched. [`stub_quit`] additionally uses
//! `AppHandle::exit(0)` and nothing else — there is deliberately no kill,
//! signal, or child reaping anywhere in this module. Probing is also never
//! started when a HostManager exists (Sidecar mode), which keeps the sidecar
//! path byte-identical and prevents the stub from fighting the sidecar the
//! host module itself manages.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{Manager, Url, WebviewWindow};

/// Per-probe connect and read timeout.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Poll interval while the server is alive (death-detection latency).
const ALIVE_POLL_INTERVAL: Duration = Duration::from_secs(10);
/// Local page shown on a dead verdict.
const STUB_PAGE: &str = "unreachable.html";

/// Outcome of the probe's TCP layer, kept data-like so classification stays
/// pure and unit-testable without real sockets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectOutcome {
    /// Socket connected (and, for http, how the HTTP read went).
    Connected(HttpRead),
    /// Connection actively refused.
    Refused,
    /// Connect or read timed out.
    Timeout,
    /// Host could not be resolved to an address.
    Unresolved,
    /// Any other transport error.
    Io(String),
}

/// Outcome of writing the GET and reading the response head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpRead {
    /// A parseable `HTTP/1.x NNN` status line was read.
    Status(u16),
    /// A live TLS listener accepted the TCP connect (https is not spoken in
    /// plain sockets — see module docs).
    TlsAccepted,
    /// Connected but no usable status line came back (empty, garbage, EOF).
    NoStatus(String),
}

/// The probe's final verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProbeVerdict {
    pub alive: bool,
    pub detail: String,
}

/// Parse an HTTP status line ("HTTP/1.1 200 OK") into the status code.
/// Pure: anything that is not `HTTP/<digit...> <3 digits>` is `None`.
fn parse_status_line(line: &str) -> Option<u16> {
    let rest = line.strip_prefix("HTTP/")?;
    let space = rest.find(' ')?;
    let code = rest.get(space + 1..space + 4)?;
    if code.bytes().all(|byte| byte.is_ascii_digit()) {
        code.parse().ok()
    } else {
        None
    }
}

/// Classify a TCP connect outcome into a verdict. Pure and socket-free.
/// Any HTTP status counts as alive — even 4xx/5xx (see module docs).
fn classify_connect(outcome: &ConnectOutcome) -> ProbeVerdict {
    match outcome {
        ConnectOutcome::Connected(HttpRead::Status(code)) => ProbeVerdict {
            alive: true,
            detail: format!("HTTP {code} response"),
        },
        ConnectOutcome::Connected(HttpRead::TlsAccepted) => ProbeVerdict {
            alive: true,
            detail: "TLS port accepting connections".into(),
        },
        ConnectOutcome::Connected(HttpRead::NoStatus(what)) => ProbeVerdict {
            alive: false,
            detail: format!("no HTTP status ({what})"),
        },
        ConnectOutcome::Refused => ProbeVerdict {
            alive: false,
            detail: "connection refused".into(),
        },
        ConnectOutcome::Timeout => ProbeVerdict {
            alive: false,
            detail: "timeout".into(),
        },
        ConnectOutcome::Unresolved => ProbeVerdict {
            alive: false,
            detail: "host could not be resolved".into(),
        },
        ConnectOutcome::Io(error) => ProbeVerdict {
            alive: false,
            detail: format!("error: {error}"),
        },
    }
}

/// Backoff schedule: 1s, 2s, 4s, 8s, then a 15s cap. Pure so the stub page
/// and the probe thread always agree on the next retry moment.
pub fn backoff_delay_ms(attempt: u32) -> u64 {
    // attempt counts consecutive failures; step doubles from 1s and is
    // capped so a long outage never grows an unbounded sleep.
    const CAP_MS: u64 = 15_000;
    let step = 1_000u64.saturating_mul(1 << (attempt.saturating_sub(1).min(4)));
    step.min(CAP_MS)
}

/// One blocking probe of `url`. Runs on the probe thread or on the Retry
/// command; the pure classification above does the actual deciding.
pub fn probe_once(url: &Url) -> ProbeVerdict {
    let outcome = probe_connect(url);
    classify_connect(&outcome)
}

fn probe_connect(url: &Url) -> ConnectOutcome {
    let Some(host) = url.host_str() else {
        return ConnectOutcome::Unresolved;
    };
    let port = url.port_or_known_default().unwrap_or(80);
    let Ok(mut addresses) = (host, port).to_socket_addrs() else {
        return ConnectOutcome::Unresolved;
    };
    let Some(address) = addresses.next() else {
        return ConnectOutcome::Unresolved;
    };
    let stream = match TcpStream::connect_timeout(&address, PROBE_TIMEOUT) {
        Ok(stream) => stream,
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            return ConnectOutcome::Refused;
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::TimedOut
                || error.kind() == std::io::ErrorKind::WouldBlock =>
        {
            return ConnectOutcome::Timeout;
        }
        Err(error) => return ConnectOutcome::Io(error.to_string()),
    };
    // https is not spoken in plain sockets (see module docs): a live TLS
    // listener accepting the TCP connect IS the liveness signal.
    if url.scheme() == "https" {
        return ConnectOutcome::Connected(HttpRead::TlsAccepted);
    }
    probe_http(stream, url.host_str().unwrap_or_default(), port)
}

fn probe_http(mut stream: TcpStream, host: &str, port: u16) -> ConnectOutcome {
    let _ = stream.set_read_timeout(Some(PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(PROBE_TIMEOUT));
    let request = format!("GET / HTTP/1.0\r\nHost: {host}:{port}\r\n\r\n");
    if let Err(error) = stream.write_all(request.as_bytes()) {
        return ConnectOutcome::Io(error.to_string());
    }
    let mut head = [0u8; 256];
    let read = match stream.read(&mut head) {
        Ok(read) => read,
        Err(error)
            if error.kind() == std::io::ErrorKind::TimedOut
                || error.kind() == std::io::ErrorKind::WouldBlock =>
        {
            return ConnectOutcome::Timeout;
        }
        Err(error) => return ConnectOutcome::Io(error.to_string()),
    };
    let text = String::from_utf8_lossy(&head[..read]);
    let line = text.lines().next().unwrap_or_default();
    match parse_status_line(line) {
        Some(code) => ConnectOutcome::Connected(HttpRead::Status(code)),
        None => {
            ConnectOutcome::Connected(HttpRead::NoStatus(format!("unexpected response {line:?}")))
        }
    }
}

/// Render the verdict label shown in the stub's diagnostics block.
fn verdict_label(verdict: &ProbeVerdict) -> String {
    if verdict.alive {
        format!("alive ({})", verdict.detail)
    } else {
        format!("dead — {}", verdict.detail)
    }
}

/// Current monitored target plus its probe history.
#[derive(Debug)]
struct MonitorTarget {
    url: Url,
    non_loopback: bool,
    /// Consecutive failed probes since the last successful one.
    attempt: u32,
    last_verdict: String,
    /// Whether the window is currently showing the stub page.
    on_stub: bool,
}

impl MonitorTarget {
    fn new(url: Url, non_loopback: bool) -> Self {
        Self {
            url,
            non_loopback,
            attempt: 0,
            last_verdict: "probing…".into(),
            on_stub: false,
        }
    }
}

/// Shared probe state, managed on the app. The daemon probe thread and the
/// stub commands both go through this single `Mutex`.
pub struct ProbeMonitor {
    inner: Mutex<MonitorInner>,
}

struct MonitorInner {
    target: Option<MonitorTarget>,
    thread_started: bool,
}

impl ProbeMonitor {
    pub fn empty() -> Self {
        Self {
            inner: Mutex::new(MonitorInner {
                target: None,
                thread_started: false,
            }),
        }
    }
}

/// JSON shape returned by `stub_diagnostics` to the stub page.
#[derive(Debug, Serialize)]
pub struct StubDiagnostics {
    pub url: String,
    pub verdict: String,
    pub attempt: u32,
    pub next_retry_ms: u64,
    pub on_stub: bool,
    pub non_loopback: bool,
}

/// Start (or retarget) attach-URL monitoring. Called on the attach launch
/// path, on `--attach-url` forwarding, and after `splash_connect` — but only
/// when no HostManager exists, so Sidecar mode is never probed.
pub fn start_monitor(app: tauri::AppHandle, url: Url) {
    if app.try_state::<crate::host::HostManager>().is_some() {
        log::debug!("[probe] sidecar mode: probing not started");
        return;
    }
    let non_loopback = !crate::splash::is_loopback_url(&url);
    let first = {
        let managed = app.state::<ProbeMonitor>();
        let mut inner = managed.inner.lock().expect("probe monitor lock");
        match inner.target.as_ref() {
            // Same target: nothing to change, keep attempt history.
            Some(target) if target.url == url => return,
            _ => {
                inner.target = Some(MonitorTarget::new(url.clone(), non_loopback));
            }
        }
        let first = !inner.thread_started;
        inner.thread_started = true;
        first
    };
    if first {
        // Daemon-style thread, owned by this run: exits when the target is
        // cleared, and process exit at app close reaps it regardless (see the
        // module docs for why nothing here outlives the app).
        std::thread::spawn(move || probe_loop(app));
    }
    log::info!("[probe] monitoring {}", url.as_str());
}

/// Stop monitoring (target cleared). Nothing currently calls this on the
/// happy path — attach runs for the app's lifetime — but the probe thread
/// exits cleanly if the target ever goes away.
fn probe_loop(app: tauri::AppHandle) {
    loop {
        let sample = {
            let managed = app.state::<ProbeMonitor>();
            let inner = managed.inner.lock().expect("probe monitor lock");
            inner.target.as_ref().map(|target| target.url.clone())
        };
        let Some(url) = sample else {
            log::debug!("[probe] no target: probe thread exiting");
            return;
        };
        let verdict = probe_once(&url);
        let navigate_to = {
            let managed = app.state::<ProbeMonitor>();
            let mut inner = managed.inner.lock().expect("probe monitor lock");
            let Some(target) = inner.target.as_mut() else {
                return;
            };
            if target.url != url {
                // The target was replaced while we probed: drop the stale
                // result instead of stubbing for a server we no longer watch.
                continue;
            }
            target.last_verdict = verdict_label(&verdict);
            if verdict.alive {
                target.attempt = 0;
                if target.on_stub {
                    target.on_stub = false;
                    Some(url.clone())
                } else {
                    None
                }
            } else {
                if !target.on_stub {
                    target.on_stub = true;
                    Some(
                        Url::parse(&format!("tauri://localhost/{STUB_PAGE}"))
                            .expect("static stub page URL"),
                    )
                } else {
                    None
                }
            }
        };
        if let Some(target) = navigate_to {
            if let Some(window) = app.get_webview_window("main") {
                log::info!(
                    "[probe] navigating main window to {} (verdict: {})",
                    target.as_str(),
                    if target.as_str().ends_with(STUB_PAGE) {
                        "dead"
                    } else {
                        "alive"
                    }
                );
                if let Err(error) = window.navigate(target) {
                    log::error!("[probe] navigate failed: {error}");
                }
            }
        }
        let sleep_for = {
            let managed = app.state::<ProbeMonitor>();
            let inner = managed.inner.lock().expect("probe monitor lock");
            match inner.target.as_ref() {
                None => return,
                Some(target) if !verdict.alive => {
                    Duration::from_millis(backoff_delay_ms(target.attempt + 1))
                }
                Some(_) => ALIVE_POLL_INTERVAL,
            }
        };
        std::thread::sleep(sleep_for);
    }
}

/// IPC: probe again right now. On success the window is navigated back to the
/// server; on failure the stub stays and the attempt counter / backoff step
/// shown in diagnostics keep growing.
#[tauri::command]
pub fn stub_retry(app: tauri::AppHandle, window: WebviewWindow) -> Result<bool, String> {
    let (url, attempt) = {
        let managed = app.state::<ProbeMonitor>();
        let inner = managed.inner.lock().expect("probe monitor lock");
        let Some(target) = inner.target.as_ref() else {
            return Err("no monitored server".into());
        };
        (target.url.clone(), target.attempt)
    };
    let verdict = probe_once(&url);
    log::info!("[probe] manual retry: {}", verdict_label(&verdict));
    {
        let managed = app.state::<ProbeMonitor>();
        let mut inner = managed.inner.lock().expect("probe monitor lock");
        let Some(target) = inner.target.as_mut() else {
            return Err("no monitored server".into());
        };
        target.last_verdict = verdict_label(&verdict);
        if verdict.alive {
            target.attempt = 0;
            target.on_stub = false;
        } else {
            target.attempt = attempt + 1;
        }
    }
    if verdict.alive {
        window
            .navigate(url)
            .map_err(|error| format!("navigate failed: {error}"))?;
    }
    Ok(verdict.alive)
}

/// IPC: quit the app. The attached external server is NEVER stopped — attach
/// mode has no HostManager, so the exit hook has nothing to kill; this command
/// adds nothing on top of a plain `exit(0)` by design.
#[tauri::command]
pub fn stub_quit(app: tauri::AppHandle) -> Result<(), String> {
    log::info!("[probe] quit requested from the stub page");
    app.exit(0);
    // exit() only signals the run loop; this returns once more, harmlessly.
    Ok(())
}

/// IPC: current probe state for the stub page's diagnostics block. The page
/// polls this so the attempt counter and backoff step keep growing live.
#[tauri::command]
pub fn stub_diagnostics(app: tauri::AppHandle) -> Option<StubDiagnostics> {
    let managed = app.state::<ProbeMonitor>();
    let inner = managed.inner.lock().expect("probe monitor lock");
    inner.target.as_ref().map(|target| StubDiagnostics {
        url: target.url.to_string(),
        verdict: target.last_verdict.clone(),
        attempt: target.attempt,
        next_retry_ms: if target.attempt == 0 {
            0
        } else {
            backoff_delay_ms(target.attempt + 1)
        },
        on_stub: target.on_stub,
        non_loopback: target.non_loopback,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        backoff_delay_ms, classify_connect, parse_status_line, verdict_label, ConnectOutcome,
        HttpRead, ProbeVerdict,
    };

    #[test]
    fn backoff_schedule_steps_up_and_caps() {
        // 1s, 2s, 4s, 8s, then capped at 15s forever.
        let expected = [1_000, 2_000, 4_000, 8_000, 15_000, 15_000, 15_000];
        for (index, &milliseconds) in expected.iter().enumerate() {
            assert_eq!(
                backoff_delay_ms(index as u32 + 1),
                milliseconds,
                "attempt {}",
                index + 1
            );
        }
        // Far-future attempts stay capped, never overflow.
        assert_eq!(backoff_delay_ms(1_000), 15_000);
        assert_eq!(backoff_delay_ms(u32::MAX), 15_000);
    }

    #[test]
    fn backoff_attempt_zero_is_first_step() {
        // An attempt of 0 (no failure recorded yet) must not panic or wrap.
        assert_eq!(backoff_delay_ms(0), 1_000);
    }

    #[test]
    fn status_line_parsing() {
        assert_eq!(parse_status_line("HTTP/1.1 200 OK"), Some(200));
        assert_eq!(parse_status_line("HTTP/1.0 404 Not Found"), Some(404));
        assert_eq!(parse_status_line("HTTP/2 500"), Some(500));
        assert_eq!(parse_status_line("HTTP/1.1 100 Continue"), Some(100));
        assert_eq!(parse_status_line(""), None);
        assert_eq!(parse_status_line("GET / HTTP/1.1"), None);
        assert_eq!(parse_status_line("HTTP/1.1 abc"), None);
        assert_eq!(parse_status_line("HTTP/1.1 20"), None);
    }

    #[test]
    fn verdict_classification_covers_all_outcomes() {
        // ANY HTTP status is alive — 4xx/5xx included, per the ticket.
        for code in [200u16, 301, 401, 404, 500, 503] {
            let verdict = classify_connect(&ConnectOutcome::Connected(HttpRead::Status(code)));
            assert!(verdict.alive, "HTTP {code} must count as alive");
        }
        assert!(classify_connect(&ConnectOutcome::Connected(HttpRead::TlsAccepted)).alive);
        assert!(
            !classify_connect(&ConnectOutcome::Connected(HttpRead::NoStatus(
                "unexpected response \"\"".into()
            )))
            .alive
        );
        assert_eq!(
            classify_connect(&ConnectOutcome::Refused),
            ProbeVerdict {
                alive: false,
                detail: "connection refused".into()
            }
        );
        assert!(!classify_connect(&ConnectOutcome::Timeout).alive);
        assert!(!classify_connect(&ConnectOutcome::Unresolved).alive);
        assert!(!classify_connect(&ConnectOutcome::Io("broken pipe".into())).alive);
    }

    #[test]
    fn verdict_labels_for_diagnostics() {
        let alive = verdict_label(&ProbeVerdict {
            alive: true,
            detail: "HTTP 200 response".into(),
        });
        assert_eq!(alive, "alive (HTTP 200 response)");
        let dead = verdict_label(&ProbeVerdict {
            alive: false,
            detail: "connection refused".into(),
        });
        assert_eq!(dead, "dead — connection refused");
    }
}
