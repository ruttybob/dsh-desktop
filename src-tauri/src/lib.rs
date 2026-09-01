//! dsh-desktop shell: create the window and connect it to a web UI. With an
//! attach signal (env/argv) or a remembered server the window navigates
//! straight to that server (probe-monitored); otherwise the splash connect
//! form is shown. The classic sidecar spawn + `dsh web:` marker flow lives in
//! host.rs and the loading page (ui/index.html) but no longer serves the
//! no-signal launch (replaced by splash per dsh-df4); the host is terminated
//! on exit when one is running.

mod host;
mod launch;
mod splash;
mod stub;

use tauri::Manager;

/// Attach the main window to a server: start the unreachable-probe monitor
/// first, navigate second. This is the shared wiring for every attach entry
/// point (env/argv launch, remembered auto-connect, splash connect, and
/// single-instance forwarding) — wry reports no navigation failures, so the
/// probe is the only detector for a dead server.
fn attach_to_server(app: &tauri::AppHandle, window: &tauri::WebviewWindow, url: tauri::Url) {
    // The attach URL may carry a ?token=<bearer-secret>; the in-memory URL
    // keeps it (navigation only), every log line is redacted (host.rs).
    log::info!(
        "[launch] attach mode: navigating to {}",
        host::redact_token(url.as_str())
    );
    stub::start_monitor(app.clone(), url.clone());
    if let Err(error) = window.navigate(url) {
        log::error!("[launch] attach navigate failed: {error}");
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance guard MUST be the first plugin registered (per the
        // plugin docs): it claims the identity socket early so a second launch
        // is refused before any other plugin/setup can spawn the sidecar. The
        // guard is also the only protection against two sidecars racing
        // ~/.dsh/storages with last-write-wins. On relaunch the second
        // instance's argv (not env — LaunchServices drops it) arrives here; an
        // --attach-url in it re-targets the running window.
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            // The second instance's argv may carry --attach-url with a token;
            // redact each argument before the argv reaches the log.
            let redacted_argv: Vec<String> =
                argv.iter().map(|arg| host::redact_token(arg)).collect();
            log::info!("[single-instance] second instance argv: {redacted_argv:?}");
            let args: Vec<String> = argv.to_vec();
            if let Some(url) = launch::resolve_relaunch_attach(&args) {
                log::info!(
                "[single-instance] forwarding attach to {}",
                host::redact_token(url.as_str())
            );
                // The launch mode is fixed only at startup; forwarding may
                // retarget an instance that launched in sidecar mode. Stop
                // the sidecar first so the window detaches from it cleanly
                // and probe monitoring is allowed to start (start_monitor
                // refuses while a sidecar child is running).
                if let Some(manager) = app.try_state::<host::HostManager>() {
                    if manager.is_running() {
                        log::info!(
                            "[single-instance] stopping sidecar host before attach retarget"
                        );
                        manager.stop();
                    }
                }
                // The main window may not exist yet when the callback fires
                // during first-instance startup, so resolve it here rather
                // than capturing it from setup.
                match app.get_webview_window("main") {
                    Some(window) => attach_to_server(app, &window, url),
                    None => {
                        log::error!("[single-instance] no main window yet; attach request dropped")
                    }
                }
            }
        }))
        .plugin(tauri_plugin_log::Builder::new().build())
        // Splash connect-form commands (dsh-df4): the page decides nothing
        // itself — validation, remember store, and navigation are host-side.
        .invoke_handler(tauri::generate_handler![
            splash::splash_connect,
            splash::splash_is_loopback,
            splash::splash_get_remembered,
            splash::splash_forget,
            stub::stub_retry,
            stub::stub_quit,
            stub::stub_diagnostics
        ])
        // Managed on the Builder (before setup and before any single-instance
        // callback) so every attach path can reach the shared probe state.
        .manage(stub::ProbeMonitor::empty())
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .expect("main webview window from tauri.conf.json");
            // Launch priority (dsh-nbe / dsh-df4), resolved here so the
            // dsh-tfd resolver stays pure:
            //   1. env/argv attach signal        → attach (no spawn),
            //   2. remembered server (dsh-df4)   → attach (no spawn),
            //   3. otherwise                     → splash connect form.
            // The classic sidecar spawn therefore no longer serves the
            // no-signal case — splash replaced it per dsh-df4 AC1 ("Splash
            // без env/argv показывает форму"); the HostManager machinery in
            // host.rs stays byte-identical and both the probe guard and the
            // exit hook tolerate its absence. A sidecar can still come back
            // under this window later only via a cleared splash state (the
            // single-instance forwarding path above stops it explicitly).
            match launch::resolve_launch_mode(
                std::env::var(launch::ATTACH_URL_ENV).ok().as_deref(),
                &std::env::args().collect::<Vec<_>>(),
            ) {
                launch::LaunchMode::Attach { url } => {
                    attach_to_server(app.handle(), &window, url);
                }
                launch::LaunchMode::Sidecar => match splash::resolve_no_signal_action() {
                    splash::NoSignalAction::Attach { url } => {
                        log::info!(
                    "[launch] remembered server: attaching to {}",
                    host::redact_token(url.as_str())
                );
                        attach_to_server(app.handle(), &window, url);
                    }
                    splash::NoSignalAction::ShowForm => {
                        // The initial window URL (tauri.conf.json "index.html")
                        // is the classic sidecar loading screen; the no-signal
                        // launch must land on the splash connect form instead,
                        // so navigate explicitly right after setup.
                        log::info!(
                            "[launch] no attach signal and no remembered server: \
                             showing the splash connect form"
                        );
                        let splash_url = tauri::Url::parse("tauri://localhost/splash.html")
                            .expect("splash page URL");
                        if let Err(error) = window.navigate(splash_url) {
                            log::error!("[launch] splash navigate failed: {error}");
                        }
                    }
                },
            }
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building dsh-desktop")
        .run(|app_handle, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(manager) = app_handle.try_state::<host::HostManager>() {
                    manager.stop();
                }
            }
        });
}
