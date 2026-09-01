//! Node host sidecar lifecycle.
//!
//! The bundled host lives under `resources/host/` inside the app resource
//! directory (`main.mjs` + a Node runtime + `node_modules` with
//! `@deepseek-ai/dsh`). We spawn `node main.mjs`, watch stdout for the
//! `dsh web: http://127.0.0.1:<port>` line the web-app bundle prints after the
//! loader tree settles, and navigate the WebView there. Logs from both streams
//! are forwarded through the `log` facade (tauri-plugin-log).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::thread;

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
        // A GUI launch sees launchd's stub PATH; restore the login shell's
        // PATH first so both the bare-node fallback and the spawned host see
        // the user's own CLI locations (Homebrew, nvm, ~/.local/bin).
        let restored_path = restore_login_shell_path();
        let node = strip_verbatim_prefix(match host_node_binary(host_dir) {
            Some(bundled) => bundled,
            None => fallback_node_binary(restored_path.as_deref()),
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
            .env("DSH_DESKTOP_PORT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(path) = restored_path {
            cmd.env("PATH", path);
        }
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

        // Surface the bundled harness release on the loading screen (and in
        // the log) so the user always knows which build is booting.
        let version = bundled_host_version(host_dir);
        log::info!(
            "[host] bundled harness version: {}",
            version.as_deref().unwrap_or("unknown")
        );
        surface_version(&window, version);

        // stderr reader: forward everything (harness logs, boot errors).
        thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                log::info!("[host] {line}");
            }
        });

        // stdout reader: forward, and navigate the WebView on the URL line.
        // Forwarded lines are token-redacted; only the in-memory navigation
        // URL keeps the launch token.
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                log::info!("[host] {}", redact_token(&line));
                if let Some(url) = parse_url_line(&line) {
                    match Url::parse(&url) {
                        Ok(url) => {
                            log::info!("[host] navigating to {}", redact_token(url.as_str()));
                            if let Err(error) = window.navigate(url) {
                                log::error!("[host] navigate failed: {error}");
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
            // Kill the host's whole process group (Unix): direct-child kill
            // alone leaves any host descendants running as orphans.
            #[cfg(unix)]
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The bundled harness release version: the `version` field of
/// `@deepseek-ai/dsh/package.json` inside the bundle. A missing or
/// unreadable manifest yields `None` (the version line is then "unknown");
/// extraction is a plain scan, so no JSON dependency is kept for this.
fn bundled_host_version(host_dir: &Path) -> Option<String> {
    let manifest = host_dir
        .join("node_modules")
        .join("@deepseek-ai/dsh")
        .join("package.json");
    let raw = std::fs::read_to_string(manifest).ok()?;
    let marker = "\"version\":";
    let at = raw.find(marker)? + marker.len();
    let rest = raw[at..].trim_start();
    let value = rest.strip_prefix('"')?;
    let end = value.find('"')?;
    let version = &value[..end];
    (!version.is_empty()).then(|| version.to_string())
}

/// Show the bundled harness version on the loading screen ("loading-screen
/// line or log" — the log always gets it in `spawn`). The screen is static
/// HTML, so the version is patched into its `#status` line shortly after
/// setup; by then the page's DOM is long since ready. A character whitelist
/// on the version keeps the injected JS literal inert.
fn surface_version(window: &WebviewWindow, version: Option<String>) {
    let Some(version) = version else {
        return;
    };
    let safe: String = version
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | '_'))
        .collect();
    let window = window.clone();
    thread::spawn(move || {
        thread::sleep(std::time::Duration::from_millis(600));
        let script = format!(
            "const s = document.getElementById('status'); \
             if (s) {{ s.textContent = '正在启动主机服务 v{safe}…'; }}"
        );
        if let Err(error) = window.eval(&script) {
            log::warn!("[host] version line on the loading screen failed: {error}");
        }
    });
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
// A macOS GUI launch inherits launchd's four-directory stub PATH
// (/usr/bin:/bin:/usr/sbin:/sbin): Homebrew, nvm, and home-local binaries —
// `bd` among them — stay invisible to the host process and every session
// shell beneath it. Before spawning the sidecar we ask the user's login
// shell to evaluate its profile scripts and print its PATH (the VS Code
// approach); on any failure the ambient environment is kept, so the app
// never boots slower than the probe budget below.

/// How long the login-shell PATH probe may run before the ambient PATH wins:
/// a wedged profile script must not stall app boot.
#[cfg(unix)]
const SHELL_PATH_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Upper bound of accepted probe output. Real PATH values sit far below this;
/// more means a misbehaving profile, not a longer PATH.
#[cfg(unix)]
const SHELL_PATH_MAX_OUTPUT_BYTES: u64 = 64 * 1024;

/// Marker wrapping the probe payload so profile startup noise cannot corrupt
/// the parsed value.
#[cfg(unix)]
const SHELL_PATH_MARKER: &str = "__DSH_PATH__";

/// Restore the user's login-shell `PATH` by running `<SHELL> -ilc 'printf …'`.
///
/// Returns `None` when `SHELL` is unset, the shell cannot be spawned, or the
/// probe times out, fails, or yields nothing usable — every failure keeps the
/// inherited environment rather than blocking launch.
#[cfg(unix)]
fn restore_login_shell_path() -> Option<String> {
    let shell = PathBuf::from(std::env::var_os("SHELL")?);
    use std::time::{Duration, Instant};
    let mut child = Command::new(&shell)
        .args(["-ilc", &format!("printf '{SHELL_PATH_MARKER}%s' \"$PATH\"")])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .inspect_err(|error| log::warn!("[host] PATH probe via {shell:?} failed to start: {error}"))
        .ok()?;
    let deadline = Instant::now() + SHELL_PATH_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                log::warn!(
                    "[host] PATH probe via {shell:?} exceeded {} ms; keeping ambient PATH",
                    SHELL_PATH_PROBE_TIMEOUT.as_millis(),
                );
                reap_probe_child(&mut child);
                return None;
            }
            Ok(None) => thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                log::warn!("[host] PATH probe wait failed: {error}; keeping ambient PATH");
                reap_probe_child(&mut child);
                return None;
            }
        }
    }
    // Drain only after the shell has exited. A profile that prints more than
    // one pipe capacity before exiting blocks on write instead of exiting,
    // trips the deadline above, and is killed — losing its PATH, which is the
    // accepted degradation: real payloads sit far below one pipe buffer.
    use std::io::Read as _;
    let mut captured = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        let _ = stdout
            .take(SHELL_PATH_MAX_OUTPUT_BYTES)
            .read_to_end(&mut captured);
    }
    let path = extract_probe_path(&String::from_utf8_lossy(&captured))?;
    log::info!(
        "[host] restored login-shell PATH ({} entries)",
        path.split(':').count()
    );
    Some(path)
}

/// Non-Unix platforms keep the inherited environment.
#[cfg(not(unix))]
fn restore_login_shell_path() -> Option<String> {
    None
}

/// Kill and reap a wedged or failed probe shell so no child is left behind.
#[cfg(unix)]
fn reap_probe_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Parse `"<marker><path>"` out of whatever else a profile printed. One
/// trailing newline is stripped, interior whitespace is preserved, and the
/// value must carry at least two colon-separated entries.
#[cfg(unix)]
fn extract_probe_path(payload: &str) -> Option<String> {
    let start = payload.find(SHELL_PATH_MARKER)? + SHELL_PATH_MARKER.len();
    let remainder = &payload[start..];
    let end = remainder.find(['\r', '\n']).unwrap_or(remainder.len());
    let value = remainder[..end].trim();
    (!value.is_empty() && value.contains(':')).then(|| value.to_string())
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

/// Replace every `token=<value>` occurrence (value runs to the next `&`,
/// whitespace, or end of string) with `token=[redacted]`. Applied to host
/// stdout before it reaches the log: the launch token is a bearer secret and
/// the unredacted URL must live only in memory, used solely for navigation.
/// `pub(crate)`: the single-instance callback in lib.rs applies the same
/// redaction to a refused second instance's argv.
pub(crate) fn redact_token(line: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::{parse_url_line, redact_token};

    #[cfg(unix)]
    mod shell_path {
        use super::super::{extract_probe_path, resolve_in_path};
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
        fn extracts_the_path_past_profile_startup_noise() {
            let payload = "Last login: Fri Aug 26 on ttys000\nloading nvm...\n\
                 __DSH_PATH__/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/Users/me/.local/bin";
            assert_eq!(
                extract_probe_path(payload),
                Some(
                    "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/Users/me/.local/bin"
                        .to_string()
                ),
            );
        }

        #[test]
        fn strips_a_single_trailing_newline_after_the_path() {
            assert_eq!(
                extract_probe_path("__DSH_PATH__/bin:/usr/bin\n"),
                Some("/bin:/usr/bin".to_string()),
            );
        }

        #[test]
        fn rejects_missing_empty_or_componentless_probe_payloads() {
            assert_eq!(extract_probe_path("profile printed nothing useful"), None);
            assert_eq!(extract_probe_path("__DSH_PATH__"), None);
            assert_eq!(extract_probe_path("__DSH_PATH__   \n"), None);
            assert_eq!(extract_probe_path("__DSH_PATH__/only-one-directory"), None);
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
