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
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons};
use tauri_plugin_updater::UpdaterExt;

/// One shared way to stop the sidecar (stop() is idempotent): both exit
/// paths and the updater's accepted-update path go through here.
fn stop_sidecar(app: &tauri::AppHandle) {
    if let Some(manager) = app.try_state::<host::HostManager>() {
        manager.stop();
    }
}

/// Background update check at launch (dsh-3r8): check → confirm dialog →
/// download+install → restart. Spawned onto the async runtime, never the
/// main thread (`blocking_show` and the updater's blocking download would
/// freeze the UI there). Every failure path — offline, 404 before the first
/// release exists, rate limits, declined dialog, failed install — is a
/// logged warning and the shell carries on untouched.
fn spawn_update_check(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        log::info!("[update] checking for updates");
        let updater = match app.updater_builder().build() {
            Ok(updater) => updater,
            Err(e) => {
                log::warn!("[update] check failed (updater config): {e}");
                return;
            }
        };
        let update = match updater.check().await {
            Ok(Some(update)) => {
                log::info!(
                    "[update] available v{} (current v{})",
                    update.version,
                    update.current_version
                );
                update
            }
            Ok(None) => {
                log::info!("[update] up to date (v{})", app.package_info().version);
                return;
            }
            Err(e) => {
                log::warn!("[update] check failed: {e}");
                return;
            }
        };
        // Modal on a worker thread: blocks this task only. Declining must
        // keep the current version running as-is.
        let restart_now = app
            .dialog()
            .message(format!(
                "dsh-desktop v{} is available (installed: v{}).\nRestart now to update?",
                update.version, update.current_version
            ))
            .title("dsh-desktop update")
            .buttons(MessageDialogButtons::OkCancelCustom(
                "Restart".into(),
                "Later".into(),
            ))
            .blocking_show();
        if !restart_now {
            log::info!("[update] postponed by the user");
            return;
        }
        log::info!("[update] downloading v{}", update.version);
        match update
            .download_and_install(
                |_chunk, _total| {},
                || log::info!("[update] download finished; installing"),
            )
            .await
        {
            Ok(()) => {
                log::info!("[update] installed v{}; restarting", update.version);
                // Only now stop the sidecar: a failed download or install
                // must leave the running harness untouched. The restarted
                // process spawns a fresh sidecar of the new version; the
                // install replaces the bundle the old one ran from (a
                // running process keeps its unlinked inodes, so stopping
                // this late is safe on Unix).
                stop_sidecar(&app);
                app.restart();
            }
            Err(e) => log::warn!("[update] download/install failed: {e}"),
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        // Deterministic loading-screen version line: patch `#status` when the
        // loading page has Finished loading its DOM — no delay to guess. The
        // URL guard keeps the patch off the harness page the ready line
        // navigates to later.
        .on_page_load(|webview, payload| {
            if matches!(payload.event(), tauri::webview::PageLoadEvent::Finished) {
                let url = payload.url().as_str();
                if url.contains("index.html") {
                    host::surface_version(webview);
                } else if url.starts_with("http://127.0.0.1") {
                    // The transition's completion signal: a page from the
                    // sidecar's origin finished loading in the WebView. The
                    // launch e2e waits for this line — without it a window
                    // stranded on the loading screen passes every other
                    // marker.
                    log::info!("[launch] harness page loaded ({url})");
                }
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
            // Fire-and-forget updater probe: async, never blocks launch. With
            // no release published yet the endpoint 404s → warn and continue.
            spawn_update_check(app.handle().clone());
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
                    stop_sidecar(app_handle);
                }
                tauri::RunEvent::Exit => {
                    stop_sidecar(app_handle);
                }
                _ => {}
            }
        });
}
