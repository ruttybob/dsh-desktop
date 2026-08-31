//! dsh-desktop shell: create the window (splash), spawn the Node host sidecar,
//! navigate the WebView to the `dsh web:` loopback URL once the host reports
//! readiness, and terminate the host when the app exits.

mod host;
mod launch;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
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
