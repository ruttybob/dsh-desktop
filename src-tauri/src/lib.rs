//! dsh-desktop shell: create the window (splash), spawn the Node host sidecar,
//! navigate the WebView to the `dsh web:` loopback URL once the host reports
//! readiness, and terminate the host when the app exits.

mod host;
mod launch;
mod splash;

use tauri::Manager;

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
            log::info!("[single-instance] second instance argv: {argv:?}");
            let args: Vec<String> = argv.to_vec();
            if let Some(url) = launch::resolve_relaunch_attach(&args) {
                log::info!("[single-instance] forwarding attach to {url}",);
                // The main window may not exist yet when the callback fires
                // during first-instance startup, so resolve it here rather
                // than capturing it from setup.
                match app.get_webview_window("main") {
                    Some(window) => {
                        if let Err(error) = window.navigate(url) {
                            log::error!("[single-instance] attach navigate failed: {error}");
                        }
                    }
                    None => log::error!(
                        "[single-instance] no main window yet; attach request dropped"
                    ),
                }
            }
        }))
        .plugin(tauri_plugin_log::Builder::new().build())
        // Splash connect-form commands (dsh-df4): the page decides nothing
        // itself — validation, remember store, and navigation are host-side.
        .invoke_handler(tauri::generate_handler![
            splash::splash_connect,
            splash::splash_get_remembered,
            splash::splash_forget
        ])
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .expect("main webview window from tauri.conf.json");
            // Attach Mode (DSH_DESKTOP_ATTACH_URL / --attach-url): navigate
            // straight to an already running server and skip the sidecar
            // entirely — no HostManager is managed, and the exit hook's
            // try_state() below simply finds nothing to stop.
            match launch::resolve_launch_mode(
                std::env::var(launch::ATTACH_URL_ENV).ok().as_deref(),
                &std::env::args().collect::<Vec<_>>(),
            ) {
                launch::LaunchMode::Attach { url } => {
                    log::info!("[launch] attach mode: navigating to {}", url.as_str());
                    if let Err(error) = window.navigate(url) {
                        log::error!("[launch] attach navigate failed: {error}");
                    }
                }
                launch::LaunchMode::Sidecar => {
                    let manager = host::HostManager::spawn(app.handle().clone(), window);
                    app.manage(manager);
                }
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
