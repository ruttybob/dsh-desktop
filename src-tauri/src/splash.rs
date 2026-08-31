//! Splash connect form backend (Attach Mode, ticket dsh-df4).
//!
//! When a launch carries no attach signal (env/argv — resolved by the launch
//! module from dsh-tfd) and no remembered server, the window shows the splash
//! connect form (`ui/splash.html`). This module owns two things:
//!
//! 1. The remembered-server store. It is deliberately shell-owned and lives
//!    OUTSIDE the sidecar's data root (never under `<DSH_HOME>/storages`):
//!    the whole point of Attach Mode is to survive sidecar replacement, and
//!    the sidecar must never see or race this file. Location:
//!    `<DSH_HOME>/desktop/attach-server.json` (DSH_HOME defaults to
//!    `~/.dsh`, mirroring `ensure_workspace_dir` in the host module).
//! 2. The Tauri command surface the splash page calls over IPC: read the
//!    remembered URL (prefill), connect (validate + remember + navigate),
//!    and forget (clear the entry, which returns the next launch to the
//!    form).
//!
//! Launch-mode resolution (attach signal vs remembered auto-connect vs form)
//! stays with dsh-tfd's resolver; `read_remembered_url` is the public seam it
//! consumes for the auto-connect branch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{Url, WebviewWindow};

/// One remembered server choice. Kept minimal and versioned so a future
/// schema change can be detected instead of silently misread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RememberedServer {
    version: u32,
    url: String,
}

const STORE_VERSION: u32 = 1;

/// Path of the remembered-server file under the shell-owned config dir.
/// `None` only when the OS gives us no home directory at all.
pub fn remembered_server_file() -> Option<PathBuf> {
    let home = std::env::var_os("DSH_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".dsh")))?;
    Some(home.join("desktop").join("attach-server.json"))
}

/// Read the remembered server URL for the auto-connect branch of the launch
/// resolver. Any problem (missing file, corrupt JSON, foreign version) is
/// logged and reported as `None`: a broken remember entry must degrade to the
/// connect form, never block the launch.
pub fn read_remembered_url() -> Option<String> {
    let path = remembered_server_file()?;
    match std::fs::read_to_string(&path) {
        Ok(raw) => match serde_json::from_str::<RememberedServer>(&raw) {
            // Reject foreign versions instead of guessing their shape.
            Ok(entry) if entry.version == STORE_VERSION => validate_attach_url(&entry.url)
                .map(|url| url.to_string())
                .map_err(|error| {
                    log::warn!("[splash] remembered URL no longer valid: {error}");
                    error
                })
                .ok(),
            Ok(entry) => {
                log::warn!(
                    "[splash] ignoring remembered server with unknown version {}",
                    entry.version
                );
                None
            }
            Err(error) => {
                log::warn!(
                    "[splash] corrupt remembered-server file {}: {error}",
                    path.display()
                );
                None
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            log::warn!("[splash] cannot read {}: {error}", path.display());
            None
        }
    }
}

/// Persist the remembered choice (`url` is expected pre-validated), creating
/// the parent directory on demand.
fn save_remembered_at(path: &std::path::Path, url: &str) -> Result<(), String> {
    let entry = RememberedServer {
        version: STORE_VERSION,
        url: url.to_string(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let raw = serde_json::to_string_pretty(&entry).map_err(|error| error.to_string())?;
    std::fs::write(path, raw).map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Remove the remembered entry. A missing file is already "cleared".
fn clear_remembered_at(path: &std::path::Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
    }
}

/// Validate a user-typed attach URL. Mirrors dsh-tfd's resolver rule: http(s)
/// only, non-empty host. Returns the normalized URL used for navigation.
fn validate_attach_url(input: &str) -> Result<Url, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("URL is empty".into());
    }
    let url = Url::parse(trimmed).map_err(|error| format!("invalid URL: {error}"))?;
    match url.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!(
                "unsupported scheme {other:?}: only http/https are allowed"
            ))
        }
    }
    // Reject scheme-less input that Url::parse salvages as a path
    // (e.g. "127.0.0.1:3080" parses as scheme "127.0.0.1").
    if url.host_str().is_none() {
        return Err("URL has no host".into());
    }
    Ok(url)
}

/// True when the URL points at loopback, where the server does not need
/// `--trusted-host` (the splash hint is shown for every other host).
/// `pub(crate)`: the unreachable-stub probe (dsh-cxq) reuses the same rule
/// for its diagnostics hint.
pub(crate) fn is_loopback_url(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost",
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// IPC: the remembered URL, or `null` when nothing valid is stored. The splash
/// page uses it to prefill the form; the launch resolver uses
/// `read_remembered_url` directly (no IPC round-trip at startup).
#[tauri::command]
pub fn splash_get_remembered() -> Option<String> {
    read_remembered_url()
}

/// IPC: forget the remembered server. The next launch without an attach
/// signal shows the connect form again.
#[tauri::command]
pub fn splash_forget() -> Result<(), String> {
    let Some(path) = remembered_server_file() else {
        return Err("no home directory: cannot locate the remember store".into());
    };
    clear_remembered_at(&path)?;
    log::info!("[splash] cleared remembered server at {}", path.display());
    Ok(())
}

/// IPC: validate the form input, apply the remember choice, and navigate the
/// window to the server. Unchecking "remember" also clears any previously
/// stored entry, so the checkbox state is the whole truth after every connect.
#[tauri::command]
pub fn splash_connect(
    app: tauri::AppHandle,
    window: WebviewWindow,
    url: String,
    remember: bool,
) -> Result<(), String> {
    let url = validate_attach_url(&url)?;
    let Some(path) = remembered_server_file() else {
        return Err("no home directory: cannot locate the remember store".into());
    };
    if remember {
        save_remembered_at(&path, url.as_str())?;
        log::info!("[splash] remembered server {}", url.as_str());
    } else {
        clear_remembered_at(&path)?;
        log::info!("[splash] remember cleared on connect to {}", url.as_str());
    }
    let non_loopback = !is_loopback_url(&url);
    log::info!(
        "[splash] navigating to {}{}",
        url.as_str(),
        if non_loopback {
            " (non-loopback: server must run with --trusted-host)"
        } else {
            ""
        }
    );
    // A splash connect points the window at an external server: start
    // unreachable-probing for it (dsh-cxq). The guard inside start_monitor is
    // a no-op in Sidecar mode, where the HostManager owns the window target.
    crate::stub::start_monitor(app, url.clone());
    window
        .navigate(url)
        .map_err(|error| format!("navigate failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{
        clear_remembered_at, is_loopback_url, read_remembered_url, save_remembered_at,
        validate_attach_url, RememberedServer, STORE_VERSION,
    };
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!(
            "dsh-desktop-splash-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn validate_accepts_http_and_https() {
        assert_eq!(
            validate_attach_url("http://127.0.0.1:3080")
                .unwrap()
                .to_string(),
            "http://127.0.0.1:3080/"
        );
        assert!(validate_attach_url("  https://dsh.example.com  ").is_ok());
    }

    #[test]
    fn validate_rejects_non_http_schemes_and_garbage() {
        for bad in [
            "ftp://127.0.0.1:3080",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "127.0.0.1:3080",
            "not a url",
            "",
            "   ",
        ] {
            assert!(
                validate_attach_url(bad).is_err(),
                "expected reject: {bad:?}"
            );
        }
    }

    #[test]
    fn loopback_detection() {
        assert!(is_loopback_url(
            &validate_attach_url("http://127.0.0.1:3080").unwrap()
        ));
        assert!(is_loopback_url(
            &validate_attach_url("http://localhost:3080").unwrap()
        ));
        assert!(is_loopback_url(
            &validate_attach_url("http://[::1]:3080").unwrap()
        ));
        assert!(!is_loopback_url(
            &validate_attach_url("http://192.168.1.10:3080").unwrap()
        ));
        assert!(!is_loopback_url(
            &validate_attach_url("https://dsh.example.com").unwrap()
        ));
    }

    #[test]
    fn remember_roundtrip_and_clear() {
        let dir = scratch("roundtrip");
        let path = dir.join("desktop").join("attach-server.json");
        assert!(!path.exists(), "fresh scratch must have no store");

        assert!(save_remembered_at(&path, "http://127.0.0.1:3080/").is_ok());
        let raw = std::fs::read_to_string(&path).unwrap();
        let entry: RememberedServer = serde_json::from_str(&raw).unwrap();
        assert_eq!(entry.version, STORE_VERSION);
        assert_eq!(entry.url, "http://127.0.0.1:3080/");

        assert!(clear_remembered_at(&path).is_ok());
        assert!(!path.exists());
        // Clearing twice is fine: the second is already "cleared".
        assert!(clear_remembered_at(&path).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn remembered_read_tolerates_missing_and_corrupt_file() {
        // Missing file via a DSH_HOME pointing at an empty scratch dir.
        let dir = scratch("missing");
        std::env::set_var("DSH_HOME", &dir);
        assert_eq!(read_remembered_url(), None);

        // Corrupt JSON degrades to None, file is left in place for inspection.
        let path = dir.join("desktop").join("attach-server.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(read_remembered_url(), None);
        assert!(path.exists());

        // Foreign version is ignored, not guessed.
        std::fs::write(&path, r#"{"version":99,"url":"http://127.0.0.1:9"}"#).unwrap();
        assert_eq!(read_remembered_url(), None);

        // Valid entry survives and comes back normalized.
        std::fs::write(
            &path,
            format!(r#"{{"version":{STORE_VERSION},"url":"http://127.0.0.1:3080"}}"#),
        )
        .unwrap();
        assert_eq!(
            read_remembered_url().as_deref(),
            Some("http://127.0.0.1:3080/")
        );

        // Stored URL that no longer validates (e.g. file edited by hand) -> None.
        std::fs::write(
            &path,
            format!(r#"{{"version":{STORE_VERSION},"url":"ftp://x"}}"#),
        )
        .unwrap();
        assert_eq!(read_remembered_url(), None);

        std::env::remove_var("DSH_HOME");
        let _ = std::fs::remove_dir_all(dir);
    }
}
