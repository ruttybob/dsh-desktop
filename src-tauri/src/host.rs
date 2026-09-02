//! Node host sidecar lifecycle.
//!
//! The bundled host lives under `resources/host/` inside the app resource
//! directory (`main.mjs` + a Node runtime + `node_modules` with
//! `@deepseek-ai/dsh`). We spawn `node main.mjs`, watch stdout for the
//! `dsh web: http://127.0.0.1:<port>` line the web-app bundle prints after the
//! loader tree settles, and navigate the WebView there. Logs from both streams
//! are forwarded through the `log` facade (tauri-plugin-log).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use tauri::{AppHandle, Manager, Url, WebviewWindow};

/// Owns the host child process; `stop()` is called on app exit.
pub struct HostManager {
    child: Mutex<Option<Child>>,
}

impl HostManager {
    /// Locate the bundled host and spawn it. Returns a manager even when the
    /// spawn failed (the window stays on the loading screen; logs explain why).
    pub fn spawn(app: AppHandle, window: WebviewWindow) -> Self {
        let host_dirs = resolve_host_dirs(&app);
        let Some(host_dir) = host_dirs.iter().find(|dir| dir.join("main.mjs").exists()) else {
            log::error!(
                "[host] bundled host missing under any of: {}",
                host_dirs
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            return Self {
                child: Mutex::new(None),
            };
        };
        // A GUI launch inherits launchd's stub environment; restore the login
        // shell's full exported environment so the sidecar sees the same
        // DSH_HOME, proxy, and provider variables a terminal `dsh web` gets.
        let login_env = restore_login_shell_env();
        let login_path = login_env
            .as_ref()
            .and_then(|vars| lookup_var(vars, "PATH"))
            .map(|value| value.to_string());
        let node = strip_verbatim_prefix(match host_node_binary(host_dir) {
            Some(bundled) => bundled,
            None => fallback_node_binary(login_path.as_deref()),
        });
        let entry = strip_verbatim_prefix(host_dir.join("main.mjs"));
        let workspace = ensure_workspace_dir();
        log::info!(
            "[host] spawn node={} entry={} cwd={}",
            node.display(),
            entry.display(),
            workspace.display()
        );

        let mut cmd = Command::new(&node);
        cmd.arg(entry)
            .current_dir(&workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Environment layering (see `sidecar_env`): ambient base, then the
        // login-shell exports — they win over launchd's stubs, then nothing
        // else. The app's mandated variables are part of `sidecar_env` and
        // therefore land above every login export; a profile can never
        // intercept the sidecar contract.
        cmd.envs(sidecar_env(std::env::vars_os(), login_env.as_deref().unwrap_or(&[])));
        // No console window on Windows (WebView2 app shell).
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        // Own process group on Unix so teardown can kill the whole tree: the
        // web host may spawn helper processes (tool runners, session workers)
        // that must not outlive the app as orphans holding the port.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                log::error!("[host] failed to spawn {node:?} main.mjs: {error}");
                return Self {
                    child: Mutex::new(None),
                };
            }
        };
        let stdout = child.stdout.take().expect("piped host stdout");
        let stderr = child.stderr.take().expect("piped host stderr");
        let manager = Self {
            child: Mutex::new(Some(child)),
        };

        // Surface the bundled harness release in the log at boot so the user
        // always knows which build is booting; the loading-screen line is
        // patched by the page-load hook (lib.rs) once the page's DOM is
        // deterministically ready.
        let version = bundled_host_version(host_dir);
        log::info!(
            "[host] bundled harness version: {}",
            version.as_deref().unwrap_or("unknown")
        );

        // stderr reader: forward everything (harness logs, boot errors),
        // redacted like stdout (see `log_redacted`).
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log_redacted(&line);
            }
        });

        // stdout reader: forward, and navigate the WebView on the URL line.
        // Forwarded lines are token-redacted; only the in-memory navigation
        // URL keeps the launch token.
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                log_redacted(&line);
                if let Some(url) = parse_url_line(&line) {
                    match Url::parse(&url) {
                        Ok(url) => {
                            log::info!("[host] navigating to {}", redact_token(url.as_str()));
                            if let Err(fallback) = navigate_through_cookie_proxy(&url, &window) {
                                if let Err(error) = window.navigate(fallback) {
                                    log::error!("[host] navigate failed: {error}");
                                }
                            }
                        }
                        Err(error) => log::error!("[host] malformed URL line: {error}"),
                    }
                }
            }
        });

        manager
    }

    /// Terminate the host process (hard kill; the harness persists its own
    /// state on disk, so no graceful shutdown is required).
    pub fn stop(&self) {
        if let Some(mut child) = self.child.lock().expect("host child lock").take() {
            log::info!("[host] stopping sidecar pid {}", child.id());
            // Kill the host's whole process group (Unix): direct-child kill
            // alone leaves any host descendants running as orphans.
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            log::info!("[host] sidecar stopped");
        }
    }
}

/// Navigate the WebView to the sidecar through the loopback cookie-injecting
/// proxy (`cookie_proxy`).
///
/// Why: the harness mints the browser-session cookie on a 303 response to the
/// tokenized root request, and WebKit has been observed never to send that
/// cookie back on subsequent requests — the window then strands on the
/// harness `401` page ("dsh web authentication required"), no matter whether
/// the cookie arrived via the redirect or was injected into the WebView
/// cookie store. The smoke test's token→cookie dance over plain HTTP is
/// immune to that, so the shell runs the same dance here (GET the tokenized
/// URL, never following redirects) and fronts the sidecar with a proxy that
/// attaches the minted cookie to every request. The WebView never touches
/// authentication again.
///
/// On any failure the `Err` carries the original tokenized URL so the caller
/// falls back to the plain browser-side mint path. The minted value is a
/// bearer-equivalent session secret and is never logged.
fn navigate_through_cookie_proxy(url: &Url, window: &WebviewWindow) -> Result<(), Url> {
    let (name, value) = match mint_browser_session_cookie(url) {
        Ok(pair) => pair,
        Err(error) => {
            log::warn!("[host] browser-session cookie not minted: {error}");
            return Err(url.clone());
        }
    };
    let sidecar_port = url.port_or_known_default().unwrap_or(80);
    let proxy_port = match crate::cookie_proxy::spawn(sidecar_port, format!("{name}={value}")) {
        Ok(proxy_port) => proxy_port,
        Err(error) => {
            log::warn!("[host] cookie proxy not started: {error}");
            return Err(url.clone());
        }
    };
    // The shell's one observable auth signal: a plain GET through the proxy
    // must come back 200 (proxy injects the minted cookie, sidecar accepts
    // it). The WebView cannot be trusted to report auth failures — a 401
    // renders as a page and surfaces nothing — so this log line is the
    // greppable proof the auth path works before the window is handed over.
    if let Err(error) = verify_proxy_auth(proxy_port) {
        log::warn!("[host] auth verification failed; continuing with proxied navigation: {error}");
    }
    // The navigation target is the PROXY origin (same loopback host, proxy
    // port, clean root) — the proxy attaches the minted cookie to every
    // request it forwards to the sidecar.
    let target = Url::parse(&format!("http://127.0.0.1:{proxy_port}/"))
        .map_err(|_| url.clone())?;
    log::info!("[host] navigating via cookie proxy on 127.0.0.1:{proxy_port}");
    match window.navigate(target) {
        Ok(()) => Ok(()),
        Err(error) => {
            log::warn!("[host] proxied navigation failed: {error}");
            Err(url.clone())
        }
    }
}

/// A plain GET of the proxy root must come back 200: the proxy injects the
/// minted cookie and the sidecar validates it, so this one request proves the
/// whole auth chain (dance, injection, rewrite, authority binding) before the
/// window is handed over. Returns `Err` with the observed status line on any
/// other answer.
fn verify_proxy_auth(proxy_port: u16) -> Result<(), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", proxy_port))
        .map_err(|error| format!("proxy connect failed: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| format!("read timeout setup failed: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| format!("write timeout setup failed: {error}"))?;
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .map_err(|error| format!("probe write failed: {error}"))?;
    let mut raw = Vec::new();
    stream
        .take(16 * 1024)
        .read_to_end(&mut raw)
        .map_err(|error| format!("probe read failed: {error}"))?;
    let status_line = String::from_utf8_lossy(&raw)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    if status_line.contains(" 200") {
        log::info!("[host] auth path verified through proxy (200)");
        Ok(())
    } else {
        Err(format!("unexpected answer: {status_line}"))
    }
}

/// Run the token→cookie dance over a plain HTTP GET of `url` without
/// following redirects: connect to the sidecar, read the 303's `Set-Cookie`,
/// and return the `(name, value)` pair (value cut at the first attribute
/// delimiter, mirroring what the browser stores).
fn mint_browser_session_cookie(url: &Url) -> Result<(String, String), String> {
    let host = url.host_str().unwrap_or_default().to_string();
    if host.is_empty() {
        return Err("ready URL carries no host".to_string());
    }
    let port = url.port_or_known_default().unwrap_or(80);
    // The Host header IS the authority the server signs the cookie for, and
    // the browser sends it host:port for this non-default port — mirror it
    // exactly so the injected cookie matches the WebView's later requests.
    let authority = format!("{host}:{port}");
    let path = url.path();
    let path_with_query = match url.query() {
        Some(query) => format!("{path}?{query}"),
        None => path.to_string(),
    };
    let request = format!(
        "GET {path_with_query} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n"
    );

    let mut stream = TcpStream::connect((host.as_str(), port))
        .map_err(|error| format!("connect failed: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| format!("read timeout setup failed: {error}"))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(3)))
        .map_err(|error| format!("write timeout setup failed: {error}"))?;
    stream
        .write_all(request.as_bytes())
        .map_err(|error| format!("request write failed: {error}"))?;

    let mut raw = Vec::new();
    stream
        .take(64 * 1024)
        .read_to_end(&mut raw)
        .map_err(|error| format!("response read failed: {error}"))?;
    let response = String::from_utf8_lossy(&raw);
    let (status_line, headers) = response.split_once("\r\n").ok_or("malformed response")?;
    if !status_line.contains(" 303") {
        return Err(format!("expected a 303, got {:.32}", status_line));
    }
    extract_set_cookie_pair(headers)
        .ok_or_else(|| "the 303 carried no Set-Cookie".to_string())
}

/// Pull the first `Set-Cookie` header out of a raw header block and cut its
/// `name=value` pair off before the first attribute delimiter. Case-
/// insensitive on the header name; the value is returned raw (it is
/// base64url-plus-signature and cookie-safe).
fn extract_set_cookie_pair(headers: &str) -> Option<(String, String)> {
    for line in headers.split("\r\n") {
        let Some((field, value)) = line.split_once(':') else {
            continue;
        };
        if !field.trim().eq_ignore_ascii_case("set-cookie") {
            continue;
        }
        let pair = value.trim();
        let (name, cookie_value) = pair.split_once('=')?;
        let cookie_value = cookie_value.split(';').next().unwrap_or_default().trim();
        return Some((name.trim().to_string(), cookie_value.to_string()));
    }
    None
}

/// The bundled harness release version: the top-level `version` field of
/// `@deepseek-ai/dsh/package.json` inside the bundle. A missing, unreadable,
/// or non-JSON manifest yields `None` (the version line is then "unknown").
/// The manifest is parsed as JSON, so nested `version` keys are ignored and
/// field spacing is irrelevant — only the manifest's own field counts.
fn bundled_host_version(host_dir: &Path) -> Option<String> {
    let manifest = host_dir
        .join("node_modules")
        .join("@deepseek-ai/dsh")
        .join("package.json");
    let raw = std::fs::read_to_string(manifest).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    let version = value.get("version")?.as_str()?;
    (!version.is_empty()).then(|| version.to_string())
}

/// Patch the bundled harness version into the loading screen's `#status`
/// line ("loading-screen line or log" — the log always gets it in `spawn`).
/// Called from the page-load hook in lib.rs when the loading page has
/// finished loading its DOM — deterministic, no delay to guess. The URL
/// guard there keeps this off the harness page the ready line navigates to.
/// A character whitelist on the version keeps the injected JS literal inert.
pub(crate) fn surface_version(webview: &tauri::Webview<tauri::Wry>) {
    let Some(host_dir) = resolve_host_dirs(webview.app_handle())
        .into_iter()
        .find(|dir| dir.join("main.mjs").exists())
    else {
        return;
    };
    let Some(version) = bundled_host_version(&host_dir) else {
        return;
    };
    let safe: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
        .collect();
    let script = format!(
        "const s = document.getElementById('status'); \
         if (s) {{ s.textContent = 'Starting the sidecar host v{safe}…'; }}"
    );
    if let Err(error) = webview.eval(&script) {
        log::warn!("[host] version line on the loading screen failed: {error}");
    }
}

/// Candidate host roots: bundled resource layouts (prod + dev) and the source tree.
fn resolve_host_dirs(app: &AppHandle) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        dirs.push(resource_dir.join("host"));
        dirs.push(resource_dir.join("resources").join("host"));
    }
    // Dev fallback: the checkout's assembled resources (npm run host:bundle).
    dirs.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("host"),
    );
    dirs
}

/// The bundled Node binary, or `None` to resolve bare `node` against the
/// restored login PATH (dev convenience; release builds always bundle).
fn host_node_binary(host_dir: &Path) -> Option<PathBuf> {
    let bundled = if cfg!(windows) {
        host_dir.join("node").join("node.exe")
    } else {
        host_dir.join("node").join("bin").join("node")
    };
    bundled.exists().then_some(bundled)
}

// ── login-shell environment restoration ─────────────────────────────────────
//
// A macOS GUI launch inherits launchd's stub environment
// (/usr/bin:/bin:/usr/sbin:/sbin for PATH, nothing user-configured besides):
// Homebrew, nvm, and home-local binaries — `bd` among them — stay invisible
// to the host process, and a DSH_HOME or proxy variable exported in the
// user's profile never reaches the sidecar, quietly splitting the desktop
// and terminal deployments into two data homes. Before spawning the sidecar
// we ask the user's login shell to evaluate its profile scripts and print
// its whole exported environment (the VS Code approach, generalized from
// PATH to every variable); on any failure the ambient environment is kept,
// so the app never boots slower than the probe budget below.

/// How long the login-shell environment probe may run before the ambient
/// environment wins: a wedged profile script must not stall app boot.
#[cfg(unix)]
const SHELL_ENV_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Upper bound of accepted probe output. Real environments sit far below
/// this; more means a misbehaving profile, not a bigger environment.
#[cfg(unix)]
const SHELL_ENV_MAX_OUTPUT_BYTES: u64 = 64 * 1024;

/// Marker printed before the probe payload so profile startup noise cannot
/// corrupt the parse; a repeated marker means the last payload wins.
#[cfg(unix)]
const SHELL_ENV_MARKER: &str = "__DSH_ENV__";

/// Restore the user's login-shell exported environment by running
/// `<SHELL> -ilc 'printf <marker>; env -0'`.
///
/// Returns the `KEY=VALUE` pairs printed after the last marker, or `None`
/// when `SHELL` is unset, the shell cannot be spawned, or the probe times
/// out, fails, or yields nothing usable — every failure keeps the inherited
/// environment rather than blocking launch. Values never reach the log:
/// callers may log the variable count and names only.
#[cfg(unix)]
fn restore_login_shell_env() -> Option<Vec<(String, String)>> {
    let shell = PathBuf::from(std::env::var_os("SHELL")?);
    use std::time::{Duration, Instant};
    let mut child = Command::new(&shell)
        .args(["-ilc", &format!("printf '{SHELL_ENV_MARKER}'; env -0")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .inspect_err(|error| {
            log::warn!("[host] environment probe via {shell:?} failed to start: {error}")
        })
        .ok()?;
    let deadline = Instant::now() + SHELL_ENV_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                log::warn!(
                    "[host] environment probe via {shell:?} exceeded {} ms; keeping ambient environment",
                    SHELL_ENV_PROBE_TIMEOUT.as_millis(),
                );
                reap_probe_child(&mut child);
                return None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                log::warn!("[host] environment probe wait failed: {error}; keeping ambient environment");
                reap_probe_child(&mut child);
                return None;
            }
        }
    }
    // Drain only after the shell has exited. A profile that prints more than
    // one pipe capacity before exiting blocks on write instead of exiting,
    // trips the deadline above, and is killed — losing its environment, which
    // is the accepted degradation: real payloads sit far below one pipe
    // buffer.
    use std::io::Read as _;
    let mut captured = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout
            .take(SHELL_ENV_MAX_OUTPUT_BYTES)
            .read_to_end(&mut captured);
    }
    let vars = extract_probe_env(&String::from_utf8_lossy(&captured))?;
    log::info!("[host] restored login-shell environment ({} vars)", vars.len());
    // Names only, never values — and only at trace, so even debug logs carry
    // the count alone (the names alone reveal which secrets a machine holds).
    log::trace!(
        "[host] login-shell environment names: {}",
        vars.iter().map(|(key, _)| key.as_str()).collect::<Vec<_>>().join(" ")
    );
    Some(vars)
}

/// Non-Unix platforms keep the inherited environment.
#[cfg(not(unix))]
fn restore_login_shell_env() -> Option<Vec<(String, String)>> {
    None
}

/// Kill and reap a wedged or failed probe shell so no child is left behind.
#[cfg(unix)]
fn reap_probe_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Parse the NUL-separated `KEY=VALUE` entries printed after the LAST
/// `SHELL_ENV_MARKER`. Profile noise before the marker is dropped by the
/// marker cut; a repeated marker means the last payload wins; noise trailing
/// the payload (after the final NUL) is skipped as segments that name no
/// variable. Values keep their `=` characters whole; segments that are empty
/// or carry no `=` are skipped; a value may be empty (`KEY=`).
///
/// Returns `None` when the marker is missing or no usable entry follows it —
/// the caller keeps the ambient environment in that case.
fn extract_probe_env(payload: &str) -> Option<Vec<(String, String)>> {
    let start = payload.rfind(SHELL_ENV_MARKER)? + SHELL_ENV_MARKER.len();
    let mut vars = Vec::new();
    for segment in payload[start..].split('\0') {
        if segment.is_empty() {
            continue; // consecutive NULs or the payload's trailing NUL
        }
        let Some((key, value)) = segment.split_once('=') else {
            continue; // not an assignment — profile noise tail
        };
        if key.is_empty() {
            continue; // "=value" names no variable
        }
        vars.push((key.to_string(), value.to_string()));
    }
    (!vars.is_empty()).then_some(vars)
}

/// The sidecar's child environment, layered: the ambient process environment
/// is the base, every login-shell export wins over it (a profile's DSH_HOME,
/// proxy, or provider settings must reach the sidecar exactly as a terminal
/// launch sees them), and the app's mandated variables are applied last so a
/// profile can never intercept the sidecar contract (the port handshake).
/// Later layers win per key; keys no layer sets pass through from ambient.
fn sidecar_env(
    ambient: impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
    login: &[(String, String)],
) -> std::collections::HashMap<String, String> {
    let mut env: std::collections::HashMap<String, String> = ambient
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();
    for (key, value) in login {
        env.insert(key.clone(), value.clone());
    }
    env.insert("DSH_DESKTOP_PORT".to_string(), "0".to_string());
    env
}

/// Value of one variable in a parsed probe payload.
fn lookup_var<'a>(vars: &'a [(String, String)], key: &str) -> Option<&'a str> {
    vars.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// The `node` binary used when no runtime is bundled: searched along the
/// restored login PATH when available; otherwise the bare name keeps dev
/// checkouts working through the ambient lookup (a GUI launch resolves it
/// only if the stub PATH covers it).
#[cfg(unix)]
fn fallback_node_binary(restored_path: Option<&str>) -> PathBuf {
    restored_path
        .and_then(|value| resolve_in_path(value, "node"))
        .unwrap_or_else(|| PathBuf::from("node"))
}

#[cfg(not(unix))]
fn fallback_node_binary(_restored_path: Option<&str>) -> PathBuf {
    PathBuf::from("node")
}

/// Locate `binary` along a concrete PATH value: `Command`'s own program lookup
/// searches this process's PATH, which a GUI launch leaves launchd-stubbed —
/// the restored value must be walked explicitly.
#[cfg(unix)]
fn resolve_in_path(path_value: &str, binary: &str) -> Option<PathBuf> {
    path_value
        .split(':')
        .filter(|entry| !entry.is_empty())
        .map(|entry| Path::new(entry).join(binary))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Tauri returns resource paths with the Windows extended-length prefix
/// (`\\?\C:\...`). Node's module loader does not understand that prefix and
/// mangles it down to the bare drive letter, so child-process paths must be
/// plain `C:\...` form.
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    if cfg!(windows) {
        if let Some(rest) = path.to_string_lossy().strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

/// The dsh working directory for desktop sessions: `$DSH_HOME/workspace`
/// (harness default home is `~/.dsh`). Created when missing.
fn ensure_workspace_dir() -> PathBuf {
    let home = std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".dsh")))
        .unwrap_or_else(|| PathBuf::from("."));
    let workspace = home.join("workspace");
    if let Err(error) = std::fs::create_dir_all(&workspace) {
        log::warn!("[host] cannot create workspace dir {workspace:?}: {error}");
    }
    workspace
}

/// Extract the full loopback URL (including any query string) from the host's
/// `dsh web: <url>` line. Only the first whitespace token after the marker is
/// taken, so a trailing LAN suffix is dropped; only `http://127.0.0.1:` with at
/// least one port digit is accepted, so non-loopback authorities (0.0.0.0 and
/// any other host) resolve to `None`.
fn parse_url_line(line: &str) -> Option<String> {
    const PREFIX: &str = "dsh web: http://127.0.0.1:";
    let trimmed = line.trim();
    let start = trimmed.find(PREFIX)?;
    let rest = &trimmed[start + PREFIX.len()..];
    let end = rest
        .find(char::is_whitespace)
        .unwrap_or(rest.len());
    let token = &rest[..end];
    let port: String = token.chars().take_while(|c| c.is_ascii_digit()).collect();
    if port.is_empty() {
        return None;
    }
    Some(format!("http://127.0.0.1:{token}"))
}

/// Internal helper: replace every `token=<value>` occurrence (value runs to
/// the next `&`, whitespace, or end of string) with `token=[redacted]`. Every
/// host-output line that reaches the log passes through it — the stdout
/// ready-line/URL lines and the stderr forward alike — because the launch
/// token is a bearer secret: the unredacted URL lives only in memory, used
/// solely for navigation.
fn redact_token(line: &str) -> String {
    const MARKER: &str = "token=";
    let mut result = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(at) = rest.find(MARKER) {
        result.push_str(&rest[..at + MARKER.len()]);
        rest = &rest[at + MARKER.len()..];
        let end = rest.find(['&', ' ']).unwrap_or(rest.len());
        rest = &rest[end..];
        result.push_str("[redacted]");
    }
    result.push_str(rest);
    result
}

/// Every host-output line that reaches the log passes through here: the
/// launch token is a bearer secret, so redaction happens before logging.
fn log_redacted(line: &str) {
    log::info!("[host] {}", redact_token(line));
}

#[cfg(test)]
mod tests {
    use super::{parse_url_line, redact_token};

    mod shell_env {
        use super::super::{extract_probe_env, sidecar_env};

        #[test]
        fn extracts_entries_past_profile_startup_noise() {
            let payload = "Last login: Fri Aug 26 on ttys000\nloading nvm...\
                 \n__DSH_ENV__DSH_HOME=/Users/me/dsh\0HTTP_PROXY=http://127.0.0.1:7890\0PATH=/opt/homebrew/bin:/usr/bin\0";
            assert_eq!(
                extract_probe_env(payload),
                Some(vec![
                    ("DSH_HOME".to_string(), "/Users/me/dsh".to_string()),
                    ("HTTP_PROXY".to_string(), "http://127.0.0.1:7890".to_string()),
                    ("PATH".to_string(), "/opt/homebrew/bin:/usr/bin".to_string()),
                ]),
            );
        }

        #[test]
        fn repeated_marker_means_the_last_payload_wins() {
            let payload = "__DSH_ENV__STALE=1\0noise\n__DSH_ENV__FRESH=2\0";
            assert_eq!(
                extract_probe_env(payload),
                Some(vec![("FRESH".to_string(), "2".to_string())]),
            );
        }

        #[test]
        fn values_keep_equals_signs_and_may_be_empty() {
            let payload = "__DSH_ENV__EQ=a=b=c\0EMPTY=\0";
            assert_eq!(
                extract_probe_env(payload),
                Some(vec![
                    ("EQ".to_string(), "a=b=c".to_string()),
                    ("EMPTY".to_string(), String::new()),
                ]),
            );
        }

        #[test]
        fn skips_empty_and_garbage_segments() {
            let payload = "__DSH_ENV__GOOD=1\0\0\0no_assignment\0=orphan_value\0TAIL=2\0trailing noise";
            assert_eq!(
                extract_probe_env(payload),
                Some(vec![
                    ("GOOD".to_string(), "1".to_string()),
                    ("TAIL".to_string(), "2".to_string()),
                ]),
            );
        }

        #[test]
        fn rejects_missing_marker_or_marker_without_entries() {
            assert_eq!(extract_probe_env("profile printed nothing useful"), None);
            assert_eq!(extract_probe_env("__DSH_ENV__"), None);
            assert_eq!(extract_probe_env("__DSH_ENV__\0\0"), None);
            assert_eq!(extract_probe_env("__DSH_ENV__nothing useful printed"), None);
        }

        /// A fixed ambient base so merge assertions never depend on the real
        /// process environment.
        fn ambient<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Iterator<Item = (std::ffi::OsString, std::ffi::OsString)> + 'a {
            pairs
                .iter()
                .map(|(key, value)| ((*key).into(), (*value).into()))
        }

        #[test]
        fn login_wins_ambient_only_is_kept_login_only_is_added() {
            let env = sidecar_env(
                ambient(&[("PATH", "/usr/bin"), ("AMBIENT_ONLY", "keep"), ("DSH_HOME", "/stub")]),
                &[
                    ("DSH_HOME".to_string(), "/Users/me/dsh".to_string()),
                    ("HTTP_PROXY".to_string(), "http://127.0.0.1:7890".to_string()),
                ],
            );
            assert_eq!(env.get("DSH_HOME").map(String::as_str), Some("/Users/me/dsh"), "login beats ambient");
            assert_eq!(env.get("AMBIENT_ONLY").map(String::as_str), Some("keep"), "process-only survives");
            assert_eq!(env.get("HTTP_PROXY").map(String::as_str), Some("http://127.0.0.1:7890"), "login-only is added");
        }

        #[test]
        fn mandated_port_contract_beats_every_profile() {
            let env = sidecar_env(
                ambient(&[("DSH_DESKTOP_PORT", "4242")]),
                &[("DSH_DESKTOP_PORT".to_string(), "9999".to_string())],
            );
            assert_eq!(
                env.get("DSH_DESKTOP_PORT").map(String::as_str),
                Some("0"),
                "a profile must not intercept the sidecar port contract",
            );
        }
    }

    #[cfg(unix)]
    mod path_lookup {
        use super::super::resolve_in_path;
        use std::path::{Path, PathBuf};

        /// A unique scratch root that cleans itself up, safe under parallel runs.
        struct Scratch(PathBuf);

        impl Scratch {
            fn new(tag: &str) -> Self {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or_default();
                let dir = std::env::temp_dir().join(format!(
                    "dsh-desktop-host-{tag}-{}-{nanos}",
                    std::process::id(),
                ));
                std::fs::create_dir_all(&dir).expect("create scratch root");
                Self(dir)
            }
        }

        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        /// Materialize `path` as an executable file.
        fn executable_file(path: &Path) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(path, "#!/bin/sh\n").expect("write fixture script");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
                .expect("chmod fixture script");
        }

        #[test]
        fn resolves_the_binary_through_path_order() {
            let scratch = Scratch::new("resolve-order");
            let earlier = scratch.0.join("earlier");
            let later = scratch.0.join("later");
            std::fs::create_dir_all(&earlier).expect("mkdir earlier");
            std::fs::create_dir_all(&later).expect("mkdir later");
            executable_file(&later.join("bd"));
            let path_value = format!("{}:{}", earlier.display(), later.display());
            assert_eq!(resolve_in_path(&path_value, "bd"), Some(later.join("bd")));
        }

        #[test]
        fn skips_non_executable_files_directory_decoys_and_misses() {
            let scratch = Scratch::new("resolve-skip");
            let plain_only = scratch.0.join("plain-only");
            let decoy_dirs = scratch.0.join("decoy-dirs");
            let carries_it = scratch.0.join("carries-it");
            std::fs::create_dir_all(&plain_only).expect("mkdir plain-only");
            std::fs::create_dir_all(decoy_dirs.join("bd")).expect("mkdir directory decoy");
            std::fs::create_dir_all(&carries_it).expect("mkdir carries-it");
            std::fs::write(plain_only.join("bd"), "not executable").expect("write plain file");
            executable_file(&carries_it.join("bd"));
            let path_value = format!(
                "{}:{}:{}",
                plain_only.display(),
                decoy_dirs.display(),
                carries_it.display()
            );
            assert_eq!(
                resolve_in_path(&path_value, "bd"),
                Some(carries_it.join("bd")),
                "a non-executable file and a same-named directory must not win over PATH order",
            );
            assert_eq!(
                resolve_in_path(&plain_only.display().to_string(), "absent"),
                None
            );
        }
    }

    mod bundled_host_version {
        use super::super::bundled_host_version;
        use std::path::{Path, PathBuf};

        /// A unique scratch host root that cleans itself up, safe under
        /// parallel runs.
        struct Scratch(PathBuf);

        impl Scratch {
            fn new(tag: &str) -> Self {
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or_default();
                let dir = std::env::temp_dir().join(format!(
                    "dsh-desktop-host-version-{tag}-{}-{nanos}",
                    std::process::id(),
                ));
                std::fs::create_dir_all(&dir).expect("create scratch root");
                Self(dir)
            }
        }

        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        /// Lay down the bundled-host manifest the extraction scans.
        fn write_manifest(host_dir: &Path, body: &str) {
            let package_dir = host_dir
                .join("node_modules")
                .join("@deepseek-ai")
                .join("dsh");
            std::fs::create_dir_all(&package_dir).expect("mkdir package dir");
            std::fs::write(package_dir.join("package.json"), body).expect("write manifest");
        }

        #[test]
        fn extracts_the_bundled_manifest_version() {
            let scratch = Scratch::new("basic");
            write_manifest(
                &scratch.0,
                r#"{"name":"@deepseek-ai/dsh","version":"1.2.3-beta.4"}"#,
            );
            assert_eq!(
                bundled_host_version(&scratch.0),
                Some("1.2.3-beta.4".to_string())
            );
        }

        #[test]
        fn ignores_nested_version_fields() {
            // Only the manifest's top-level `version` field counts: a nested
            // key (here a devDependencies-style object) must not be picked up
            // no matter where it sits in the document.
            let scratch = Scratch::new("nested");
            write_manifest(
                &scratch.0,
                r#"{"devDependencies":{"version":"2.0.0-rc.1"},"version":"1.2.3"}"#,
            );
            assert_eq!(
                bundled_host_version(&scratch.0),
                Some("1.2.3".to_string())
            );
        }

        #[test]
        fn parses_the_manifest_regardless_of_field_spacing() {
            // The manifest is parsed as JSON, not scanned for a marker, so
            // spacing around the colon cannot hide the field.
            let scratch = Scratch::new("spacing");
            write_manifest(&scratch.0, r#"{ "name" : "@deepseek-ai/dsh" , "version" : "1.2.3" }"#);
            assert_eq!(bundled_host_version(&scratch.0), Some("1.2.3".to_string()));
        }

        #[test]
        fn returns_none_for_a_non_json_manifest() {
            let scratch = Scratch::new("non-json");
            write_manifest(&scratch.0, "not json at all");
            assert_eq!(bundled_host_version(&scratch.0), None);
        }

        #[test]
        fn returns_none_when_the_manifest_is_missing() {
            let scratch = Scratch::new("missing");
            assert_eq!(bundled_host_version(&scratch.0), None);
        }

        #[test]
        fn returns_none_for_an_empty_version_value() {
            let scratch = Scratch::new("empty");
            write_manifest(&scratch.0, r#"{"name":"@deepseek-ai/dsh","version":""}"#);
            assert_eq!(bundled_host_version(&scratch.0), None);
        }
    }

    mod browser_session_cookie {
        use super::super::{extract_set_cookie_pair, mint_browser_session_cookie, verify_proxy_auth};
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use tauri::Url;

        const CANNED_303: &str = "HTTP/1.1 303 See Other\r\n\
             cache-control: no-store\r\n\
             location: /\r\n\
             set-cookie: dsh-auth-AbCd=v1.cGF5bG9hZA.wijd; Max-Age=86400; Path=/; HttpOnly; SameSite=Strict\r\n\
             \r\n";

        #[test]
        fn extracts_the_name_value_pair_cut_before_attributes() {
            let headers = "cache-control: no-store\r\n\
                 set-cookie: dsh-auth-AbCd=v1.cGF5bG9hZA.wijd; Max-Age=86400; Path=/; HttpOnly\r\n";
            assert_eq!(
                extract_set_cookie_pair(headers),
                Some(("dsh-auth-AbCd".to_string(), "v1.cGF5bG9hZA.wijd".to_string()))
            );
        }

        #[test]
        fn matches_the_header_name_case_insensitively() {
            let headers = "SET-COOKIE: dsh-auth-x=v1.a.sig\r\n";
            assert_eq!(
                extract_set_cookie_pair(headers),
                Some(("dsh-auth-x".to_string(), "v1.a.sig".to_string()))
            );
        }

        #[test]
        fn returns_none_without_a_set_cookie_header() {
            assert_eq!(extract_set_cookie_pair("location: /\r\n\r\n"), None);
        }

        /// A one-shot local server answering the canned 303, so the mint runs
        /// its real socket path against a loopback peer.
        #[test]
        fn mints_from_a_canned_303_over_a_real_socket() {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let port = listener.local_addr().expect("local addr").port();
            std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().expect("accept");
                // A real server answers once the request head is complete; it
                // never waits for the client's EOF (the connection stays
                // half-open while the response travels).
                let mut request = Vec::new();
                let mut buf = [0u8; 512];
                loop {
                    let read = socket.read(&mut buf).expect("read the probe request");
                    request.extend_from_slice(&buf[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n") || read == 0 {
                        break;
                    }
                }
                assert!(
                    request.starts_with(b"GET /?token=abc HTTP/1.1\r\n"),
                    "{:?}",
                    String::from_utf8_lossy(&request)
                );
                assert!(
                    request.windows(17).any(|window| window == b"Connection: close"),
                    "{:?}",
                    String::from_utf8_lossy(&request)
                );
                socket.write_all(CANNED_303.as_bytes()).expect("write 303");
                socket
                    .shutdown(std::net::Shutdown::Write)
                    .expect("close for EOF");
            });
            let url = Url::parse(&format!("http://127.0.0.1:{port}/?token=abc")).expect("url");
            assert_eq!(
                mint_browser_session_cookie(&url),
                Ok(("dsh-auth-AbCd".to_string(), "v1.cGF5bG9hZA.wijd".to_string()))
            );
        }

        #[test]
        fn refuses_a_non_303_answer() {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let port = listener.local_addr().expect("local addr").port();
            std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().expect("accept");
                let mut request = String::new();
                let _ = socket.read_to_string(&mut request);
                socket
                    .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 0\r\n\r\n")
                    .expect("write 401");
            });
            let url = Url::parse(&format!("http://127.0.0.1:{port}/?token=abc")).expect("url");
            assert!(mint_browser_session_cookie(&url).is_err());
        }

        /// A one-shot local server answering `status` to any single request.
        fn serve_status(status: &'static str) -> u16 {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
            let port = listener.local_addr().expect("local addr").port();
            std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().expect("accept");
                let mut buf = [0u8; 512];
                let _ = socket.read(&mut buf);
                let response = format!("{status}\r\ncontent-length: 0\r\n\r\n");
                socket.write_all(response.as_bytes()).expect("write answer");
                socket.shutdown(std::net::Shutdown::Write).expect("close");
            });
            port
        }

        #[test]
        fn verification_passes_on_a_200_answer() {
            let port = serve_status("HTTP/1.1 200 OK");
            assert_eq!(verify_proxy_auth(port), Ok(()));
        }

        #[test]
        fn verification_fails_loudly_on_a_non_200_answer() {
            let port = serve_status("HTTP/1.1 401 Unauthorized");
            let error = verify_proxy_auth(port).expect_err("must fail");
            assert!(error.contains("401"), "{error}");
        }
    }

    /// The real sidecar's output pushed through the real redaction boundary
    /// (`redact_token` — the boundary function the shell applies to every
    /// logged line; that the application happens is proven end-to-end by the
    /// launch e2e's persisted-log scan): no raw launch token may survive.
    /// This is the persisted-log guarantee (spec dsh-u3m.7 US#17) exercised
    /// against the exact entry the app ships.
    #[cfg(unix)]
    mod sidecar_redaction_boundary {
        use super::super::{parse_url_line, redact_token};
        use std::io::{BufRead, BufReader};
        use std::path::PathBuf;
        use std::process::{Command, Stdio};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, Mutex};
        use std::thread;
        use std::time::{Duration, Instant};

        /// Locate a runnable sidecar: the assembled bundle first (its node
        /// runtime included), then the checkout's `host/` directory on the
        /// PATH `node`. `None` skips the test where neither is prepared (a
        /// bare checkout without `npm -C host install --prod`).
        fn resolve_sidecar() -> Option<(String, PathBuf)> {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let bundled = manifest.join("resources").join("host");
            let node = bundled.join("node").join("bin").join("node");
            if bundled.join("main.mjs").exists()
                && bundled.join("node_modules").exists()
                && node.exists()
            {
                return Some((node.to_string_lossy().into_owned(), bundled.join("main.mjs")));
            }
            let checkout = manifest.parent()?.join("host");
            if checkout.join("main.mjs").exists() && checkout.join("node_modules").exists() {
                return Some(("node".to_string(), checkout.join("main.mjs")));
            }
            None
        }

        fn scratch_home(tag: &str) -> PathBuf {
            scratch_home_guard(tag).0
        }

        /// The scratch home plus its self-cleaning guard: the directory is
        /// removed whenever the guard drops, panic or not.
        fn scratch_home_guard(tag: &str) -> (PathBuf, ScratchGuard) {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!(
                "dsh-desktop-redaction-{tag}-{}-{nanos}",
                std::process::id(),
            ));
            std::fs::create_dir_all(&dir).expect("create scratch home");
            (dir.clone(), ScratchGuard(dir))
        }

        struct ScratchGuard(PathBuf);

        impl Drop for ScratchGuard {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        #[test]
        fn no_raw_launch_token_survives_the_logging_boundary() {
            let Some((node_bin, entry)) = resolve_sidecar() else {
                eprintln!(
                    "skipping: no runnable sidecar (run `npm run host:bundle` or `npm -C host install --prod`)"
                );
                return;
            };
            let (home, _home_guard) = scratch_home_guard("boundary");
            let mut child = Command::new(&node_bin)
                .arg(&entry)
                .env("DSH_DESKTOP_PORT", "0")
                .env("DSH_HOME", &home)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn the sidecar");
            let stdout = child.stdout.take().expect("piped stdout");
            let stderr = child.stderr.take().expect("piped stderr");

            // Both streams are collected exactly as the shell forwards them:
            // line by line, through the redaction boundary.
            let collect = |pipe: Box<dyn BufRead + Send>| -> (Arc<Mutex<Vec<String>>>, Arc<AtomicBool>) {
                let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
                let done = Arc::new(AtomicBool::new(false));
                let lines_w = Arc::clone(&lines);
                let done_w = Arc::clone(&done);
                thread::spawn(move || {
                    for line in pipe.lines().map_while(Result::ok) {
                        lines_w.lock().expect("line lock").push(line);
                    }
                    done_w.store(true, Ordering::SeqCst);
                });
                (lines, done)
            };
            let (out_lines, out_done) = collect(Box::new(BufReader::new(stdout)));
            let (err_lines, _err_done) = collect(Box::new(BufReader::new(stderr)));

            // Wait for the ready line, then a short drain so trailing boot
            // output (the lines most likely to quote the URL) is collected.
            let deadline = Instant::now() + Duration::from_secs(90);
            let ready_url = loop {
                let found = out_lines
                    .lock()
                    .expect("line lock")
                    .iter()
                    .find_map(|line| parse_url_line(line));
                if let Some(url) = found {
                    break url;
                }
                if out_done.load(Ordering::SeqCst) || Instant::now() > deadline {
                    child.kill().ok();
                    panic!("sidecar printed no ready line within the budget");
                }
                thread::sleep(Duration::from_millis(200));
            };
            thread::sleep(Duration::from_millis(1500));
            child.kill().ok();
            let _ = child.wait();
            let _ = std::fs::remove_dir_all(&home);

            // The raw token, straight from the ready line: this exact byte
            // sequence must not appear in any redacted line.
            let raw_token = ready_url
                .split("token=")
                .nth(1)
                .unwrap_or_default()
                .to_string();
            assert!(
                raw_token.len() >= 20,
                "ready URL carries no plausible token: {ready_url}"
            );
            let redacted_ready = redact_token(&format!("dsh web: {ready_url}"));
            assert!(
                redacted_ready.contains("token=[redacted]"),
                "the ready line must redact: {redacted_ready}"
            );

            let all = out_lines
                .lock()
                .expect("line lock")
                .iter()
                .chain(err_lines.lock().expect("line lock").iter())
                .map(|line| redact_token(line))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !all.contains(&raw_token),
                "a raw launch token survived the redaction boundary"
            );
            let raw_stream = format!(
                "{}\n{}",
                out_lines.lock().expect("line lock").join("\n"),
                err_lines.lock().expect("line lock").join("\n")
            );
            assert!(
                raw_stream.contains(&raw_token),
                "vacuous test: the sidecar never printed the raw token"
            );
        }
    }

    #[test]
    fn redacts_the_token_value() {
        assert_eq!(
            redact_token("dsh web: http://127.0.0.1:3080?token=abc123"),
            "dsh web: http://127.0.0.1:3080?token=[redacted]"
        );
        // A trailing LAN suffix (whitespace delimiter) is left untouched.
        assert_eq!(
            redact_token("dsh web: http://127.0.0.1:3080?token=abc123 (LAN: http://192.168.1.2:3080)"),
            "dsh web: http://127.0.0.1:3080?token=[redacted] (LAN: http://192.168.1.2:3080)"
        );
    }

    #[test]
    fn leaves_lines_without_a_query_untouched() {
        assert_eq!(
            redact_token("dsh web: http://127.0.0.1:3080"),
            "dsh web: http://127.0.0.1:3080"
        );
        assert_eq!(redact_token("some other log line"), "some other log line");
        assert_eq!(redact_token(""), "");
    }

    #[test]
    fn redacts_every_token_and_honors_the_query_delimiter() {
        // Multiple occurrences, and `&` ends the value so remaining query
        // parameters survive.
        assert_eq!(
            redact_token("GET /?token=abc&next=1 x token=def"),
            "GET /?token=[redacted]&next=1 x token=[redacted]"
        );
    }

    #[test]
    fn parses_the_url_line() {
        assert_eq!(
            parse_url_line("dsh web: http://127.0.0.1:3080"),
            Some("http://127.0.0.1:3080".to_string())
        );
        assert_eq!(
            parse_url_line("dsh web: http://127.0.0.1:41237 (LAN: http://192.168.1.2:41237)"),
            Some("http://127.0.0.1:41237".to_string())
        );
        // Legacy no-query strings parse exactly as before.
        assert_eq!(
            parse_url_line("dsh web: http://127.0.0.1:2060"),
            Some("http://127.0.0.1:2060".to_string())
        );
    }

    #[test]
    fn keeps_the_whole_tokenized_url_and_drops_the_lan_suffix() {
        // The full URL, query (launch token) included, is preserved verbatim.
        assert_eq!(
            parse_url_line("dsh web: http://127.0.0.1:3080?token=abc123"),
            Some("http://127.0.0.1:3080?token=abc123".to_string())
        );
        // A trailing LAN suffix is whitespace-separated and dropped.
        assert_eq!(
            parse_url_line(
                "dsh web: http://127.0.0.1:41237?token=xyz (LAN: http://192.168.1.2:41237)"
            ),
            Some("http://127.0.0.1:41237?token=xyz".to_string())
        );
    }

    #[test]
    fn ignores_other_lines() {
        assert_eq!(parse_url_line("some other log line"), None);
        assert_eq!(parse_url_line(""), None);
        // Missing port digit.
        assert_eq!(parse_url_line("dsh web: http://127.0.0.1:"), None);
        // Non-loopback authorities are rejected, tokenized or not.
        assert_eq!(parse_url_line("dsh web: http://0.0.0.0:3080"), None);
        assert_eq!(parse_url_line("dsh web: http://0.0.0.0:3080?token=abc"), None);
        assert_eq!(parse_url_line("dsh web: http://192.168.1.2:3080?token=abc"), None);
        assert_eq!(parse_url_line("dsh web: http://localhost:3080?token=abc"), None);
    }
}
