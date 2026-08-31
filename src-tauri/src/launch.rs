//! Attach-launch signal resolution.
//!
//! Two independent signals can request Attach Mode, in which the shell skips
//! spawning the bundled sidecar host and navigates straight to an already
//! running web server:
//!
//! - the `DSH_DESKTOP_ATTACH_URL` environment variable, and
//! - a `--attach-url <url>` / `--attach-url=<url>` command-line argument.
//!
//! # Precedence
//!
//! When both signals are present the environment wins. Rationale: the env var
//! is the more explicit, positional signal (an argv attach value can be
//! displaced or confused by unrelated trailing arguments, and launcher scripts
//! that export the env var do so as the deliberate final word); resolving env
//! first also makes the outcome independent of argv ordering. The argv signal
//! is only consulted when the env variable carries no attach value at all.
//!
//! # Invalid URLs
//!
//! A present-but-invalid signal (unparsable URL, non-http(s) scheme, missing
//! `--attach-url` value) never navigates and never crashes: the resolver logs
//! a clear error and degrades to [`LaunchMode::Sidecar`], so the app still
//! boots normally and remains usable — silently spawning the sidecar is the
//! safest recovery, and the log line explains why attach did not happen.
//!
//! A whitespace-only env value counts as "no signal" and falls through to
//! argv; surrounding whitespace on a real value is trimmed.

use tauri::Url;

/// How this launch should reach a web UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchMode {
    /// Navigate to an already running server instead of spawning the sidecar.
    Attach {
        /// The validated, trimmed attach URL (scheme is http or https).
        url: Url,
    },
    /// The classic launch: spawn the bundled host and follow its
    /// `dsh web:` stdout marker.
    Sidecar,
}

/// The env variable checked for an attach URL.
pub const ATTACH_URL_ENV: &str = "DSH_DESKTOP_ATTACH_URL";

/// The argv flag requesting attach, space-separated form (`--attach-url <url>`).
const ATTACH_URL_FLAG: &str = "--attach-url";
/// `--attach-url=<url>` prefix form.
const ATTACH_URL_FLAG_EQ: &str = "--attach-url=";

/// Resolve the launch mode from an optional env value and the process argv.
///
/// `env_value` is the raw `DSH_DESKTOP_ATTACH_URL` value; `args` is the full
/// argv including the program name at index 0.
pub fn resolve_launch_mode(env_value: Option<&str>, args: &[String]) -> LaunchMode {
    // Env first (see the module docs for why): a whitespace-only value means
    // "no attach signal", anything else is authoritative — including invalid,
    // which degrades to Sidecar rather than falling through to argv.
    if let Some(raw) = env_value {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            log::info!("[launch] {ATTACH_URL_ENV} is empty; ignoring it");
        } else {
            return match parse_attach_url(trimmed) {
                Ok(url) => LaunchMode::Attach { url },
                Err(error) => {
                    log::error!(
                        "[launch] invalid {ATTACH_URL_ENV} value {raw:?}: {error}; \
                         falling back to sidecar launch"
                    );
                    LaunchMode::Sidecar
                }
            };
        }
    }
    match parse_attach_arg(args) {
        Some(Ok(url)) => LaunchMode::Attach { url },
        Some(Err(error)) => {
            log::error!(
                "[launch] invalid {ATTACH_URL_FLAG} argument: {error}; \
                         falling back to sidecar launch"
            );
            LaunchMode::Sidecar
        }
        None => LaunchMode::Sidecar,
    }
}

/// Resolve a *relaunch* attach request from the second instance's argv.
///
/// This is the single-instance forwarding seam: when a second app instance is
/// launched with `--attach-url`, its argv is delivered to the first instance's
/// single-instance callback, which navigates the existing window to the URL.
/// The env variable is deliberately NOT consulted here — on macOS relaunches
/// via LaunchServices the environment of the second process is not forwarded
/// anyway, so honoring it would only create a platform-dependent behavior
/// difference.
///
/// `Some(url)` — a well-formed attach request to apply; `None` — no attach
/// flag in argv, or one that failed validation (already logged), in which
/// case the running instance simply keeps its current target.
pub fn resolve_relaunch_attach(args: &[String]) -> Option<Url> {
    match parse_attach_arg(args) {
        Some(Ok(url)) => Some(url),
        Some(Err(error)) => {
            log::error!("[launch] ignoring invalid relaunch {ATTACH_URL_FLAG} argument: {error}");
            None
        }
        None => None,
    }
}

/// Validate a candidate attach URL: must parse and carry an http(s) scheme.
fn parse_attach_url(raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|error| format!("not a valid URL: {error}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        other => Err(format!(
            "unsupported scheme {other:?}: only http:// and https:// are accepted"
        )),
    }
}

/// Scan argv (skipping the program name) for `--attach-url`.
///
/// `Some(Ok(url))` — a well-formed attach request; `Some(Err(_))` — an attach
/// request that failed validation (flag present with no value or a bad URL,
/// already logged by the caller); `None` — no attach flag in argv.
fn parse_attach_arg(args: &[String]) -> Option<Result<Url, String>> {
    let mut iter = args.iter().skip(1).map(String::as_str);
    while let Some(arg) = iter.next() {
        if let Some(value) = arg.strip_prefix(ATTACH_URL_FLAG_EQ) {
            return Some(finish_attach_value(value));
        }
        if arg == ATTACH_URL_FLAG {
            return Some(match iter.next() {
                Some(value) => finish_attach_value(value),
                None => Err("flag given without a value".to_string()),
            });
        }
    }
    None
}

/// Validate one attach URL candidate (after flag/value splitting, trimmed).
fn finish_attach_value(value: &str) -> Result<Url, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("flag given with an empty value".to_string());
    }
    parse_attach_url(trimmed)
}

#[cfg(test)]
mod tests {
    use super::{resolve_launch_mode, LaunchMode, ATTACH_URL_ENV};

    fn args(list: &[&str]) -> Vec<String> {
        std::iter::once("dsh-desktop")
            .chain(list.iter().copied())
            .map(String::from)
            .collect()
    }

    fn attach(url: &str) -> LaunchMode {
        LaunchMode::Attach {
            url: tauri::Url::parse(url).expect("test URL"),
        }
    }

    #[test]
    fn defaults_to_sidecar_without_any_signal() {
        assert_eq!(resolve_launch_mode(None, &args(&[])), LaunchMode::Sidecar);
        assert_eq!(
            resolve_launch_mode(None, &args(&["--verbose", "file.txt"])),
            LaunchMode::Sidecar,
            "unknown/unrelated arguments are ignored"
        );
    }

    #[test]
    fn attaches_from_a_valid_env_value() {
        assert_eq!(
            resolve_launch_mode(Some("http://127.0.0.1:3080?token=abc"), &args(&[])),
            attach("http://127.0.0.1:3080?token=abc"),
        );
        assert_eq!(
            resolve_launch_mode(Some("https://dsh.example.com"), &args(&[])),
            attach("https://dsh.example.com"),
        );
    }

    #[test]
    fn trims_surrounding_whitespace_from_the_env_value() {
        assert_eq!(
            resolve_launch_mode(Some("  http://127.0.0.1:3080 \n"), &args(&[])),
            attach("http://127.0.0.1:3080"),
        );
    }

    #[test]
    fn treats_a_whitespace_only_env_value_as_no_signal() {
        assert_eq!(
            resolve_launch_mode(Some("   "), &args(&[])),
            LaunchMode::Sidecar
        );
        assert_eq!(
            resolve_launch_mode(Some("   "), &args(&["--attach-url", "http://h:1"])),
            attach("http://h:1"),
            "an empty env value falls through to argv",
        );
    }

    #[test]
    fn rejects_invalid_or_non_http_env_values_into_sidecar() {
        for bad in [
            "not a url",
            "ftp://127.0.0.1:3080",
            "file:///etc/passwd",
            "http://",
        ] {
            assert_eq!(
                resolve_launch_mode(Some(bad), &args(&[])),
                LaunchMode::Sidecar,
                "env value {bad:?} must not navigate"
            );
        }
    }

    #[test]
    fn an_invalid_env_value_does_not_fall_through_to_argv() {
        assert_eq!(
            resolve_launch_mode(Some("ftp://x"), &args(&["--attach-url", "http://h:1"])),
            LaunchMode::Sidecar,
            "env takes precedence even when invalid",
        );
    }

    #[test]
    fn env_takes_precedence_over_a_valid_argv_signal() {
        assert_eq!(
            resolve_launch_mode(
                Some("http://from-env:1"),
                &args(&["--attach-url", "http://from-argv:2"])
            ),
            attach("http://from-env:1"),
        );
    }

    #[test]
    fn attaches_from_space_separated_argv() {
        assert_eq!(
            resolve_launch_mode(None, &args(&["--attach-url", "http://127.0.0.1:41237"])),
            attach("http://127.0.0.1:41237"),
        );
    }

    #[test]
    fn attaches_from_equals_form_argv() {
        assert_eq!(
            resolve_launch_mode(None, &args(&["--attach-url=http://127.0.0.1:41237"])),
            attach("http://127.0.0.1:41237"),
        );
        assert_eq!(
            resolve_launch_mode(None, &args(&["--attach-url=  https://h:9  "])),
            attach("https://h:9"),
            "the equals form is trimmed too",
        );
    }

    #[test]
    fn rejects_argv_flag_without_a_value() {
        assert_eq!(
            resolve_launch_mode(None, &args(&["--attach-url"])),
            LaunchMode::Sidecar,
        );
    }

    #[test]
    fn rejects_invalid_or_non_http_argv_values_into_sidecar() {
        for bad in ["--attach-url=javascript:alert(1)", "--attach-url=nope"] {
            assert_eq!(
                resolve_launch_mode(None, &args(&[bad])),
                LaunchMode::Sidecar
            );
        }
        assert_eq!(
            resolve_launch_mode(None, &args(&["--attach-url", "https://"])),
            LaunchMode::Sidecar,
        );
    }

    #[test]
    fn uses_the_first_attach_flag_only() {
        assert_eq!(
            resolve_launch_mode(
                None,
                &args(&[
                    "--attach-url",
                    "http://first:1",
                    "--attach-url",
                    "http://second:2"
                ])
            ),
            attach("http://first:1"),
        );
    }

    #[test]
    fn env_constant_matches_the_documented_name() {
        assert_eq!(ATTACH_URL_ENV, "DSH_DESKTOP_ATTACH_URL");
    }

    #[test]
    fn relaunch_forwarding_extracts_a_valid_attach_url() {
        assert_eq!(
            super::resolve_relaunch_attach(&args(&["--attach-url", "http://127.0.0.1:41237"])),
            Some(tauri::Url::parse("http://127.0.0.1:41237").expect("test URL")),
        );
        assert_eq!(
            super::resolve_relaunch_attach(&args(&["--attach-url=https://h:9"])),
            Some(tauri::Url::parse("https://h:9").expect("test URL")),
        );
    }

    #[test]
    fn relaunch_forwarding_ignores_unrelated_or_invalid_argv() {
        // No attach flag at all: plain relaunch keeps the current target.
        assert_eq!(super::resolve_relaunch_attach(&args(&[])), None);
        assert_eq!(
            super::resolve_relaunch_attach(&args(&["--verbose"])),
            None,
            "unrelated arguments are not a forward request"
        );
        // An invalid URL must never navigate the running instance anywhere.
        assert_eq!(
            super::resolve_relaunch_attach(&args(&["--attach-url=javascript:alert(1)"])),
            None,
        );
        assert_eq!(
            super::resolve_relaunch_attach(&args(&["--attach-url"])),
            None
        );
    }
}
