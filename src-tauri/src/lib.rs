//! dsh-desktop shell: the bundled sidecar is the only server story (ADR-0004).
//! Launch unconditionally spawns `resources/host` (main.mjs + a bundled Node
//! runtime + `@deepseek-ai/dsh`), follows the `dsh web: http://127.0.0.1:<port>`
//! ready line into the WebView (host.rs + the loading screen ui/index.html),
//! and kills the sidecar process group on exit. No user-installed dsh is
//! discovered, adopted, or spawned; no connect UI exists. There is no
//! single-instance guard either: a second app instance opens its own window
//! and spawns its own sidecar on its own OS-assigned port (origins behavior,
//! ADR-0004) — no forwarding, no refusal.

mod cookie_proxy;
mod host;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        // Deterministic loading-screen version line: patch `#status` when the
        // loading page has Finished loading its DOM — no delay to guess. The
        // URL guard keeps the patch off the harness page the ready line
        // navigates to later.
        .on_page_load(|webview, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished)
                && payload.url().as_str().contains("index.html")
            {
                host::surface_version(webview);
            }
        })
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
            // The sidecar must die on EVERY graceful exit path. Empirically
            // (2026-09-01, macOS): closing the last window tears the run loop
            // down WITHOUT delivering RunEvent::Exit — the sidecar survived
            // as an orphan (ppid 1). ExitRequested covers the window-close
            // path; Exit covers direct app.exit()/terminate paths that skip
            // ExitRequested. stop() is idempotent (the child is taken out of
            // the manager), so handling both is safe.
            match event {
                tauri::RunEvent::ExitRequested { .. } => {
                    log::info!("[launch] exit requested; stopping sidecar");
                    if let Some(manager) = app_handle.try_state::<host::HostManager>() {
                        manager.stop();
                    }
                }
                tauri::RunEvent::Exit => {
                    if let Some(manager) = app_handle.try_state::<host::HostManager>() {
                        manager.stop();
                    }
                }
                _ => {}
            }
        });
}
