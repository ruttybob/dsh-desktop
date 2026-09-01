//! dsh-desktop shell: the bundled sidecar is the only server story (ADR-0004).
//! Launch unconditionally spawns `resources/host` (main.mjs + a bundled Node
//! runtime + `@deepseek-ai/dsh`), follows the `dsh web: http://127.0.0.1:<port>`
//! ready line into the WebView (host.rs + the loading screen ui/index.html),
//! and kills the sidecar process group on exit. No user-installed dsh is
//! discovered, adopted, or spawned; no connect UI exists.

mod host;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Single-instance guard MUST be the first plugin registered (per the
        // plugin docs): it claims the identity socket early so a second launch
        // is refused before any other plugin/setup can spawn the sidecar. The
        // guard is also the only protection against two sidecars racing
        // ~/.dsh/storages with last-write-wins. There is deliberately no
        // relaunch forwarding: the second instance is only logged (its argv
        // redacted — command lines may carry tokens).
        .plugin(tauri_plugin_single_instance::init(|_app, argv, _cwd| {
            let redacted_argv: Vec<String> =
                argv.iter().map(|arg| host::redact_token(arg)).collect();
            log::info!("[single-instance] second instance refused; argv: {redacted_argv:?}");
        }))
        .plugin(tauri_plugin_log::Builder::new().build())
        .setup(|app| {
            let window = app
                .get_webview_window("main")
                .expect("main webview window from tauri.conf.json");
            // The only launch path (ADR-0004): spawn the bundled sidecar and
            // follow its ready line. The window stays on the loading screen
            // until that navigation; a spawn failure is logged there and the
            // screen's own hint points at the log.
            let manager = host::HostManager::spawn(app.handle().clone(), window);
            app.manage(manager);
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
