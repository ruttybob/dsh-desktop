# Tray, resident mode and launch-at-login: Tauri 2 API facts (macOS)

Date: 2026-09-01.
Scope: read-only API research for the desktop shell's "resident" mode (the app
keeps living in the menu bar after its window is closed). Resolves bd issue
`dsh-j2t.1` on the wayfinder map `dsh-j2t`. Companion doc:
`docs/research/managed-mode-dsh-discovery.md` (server-side discovery).

Primary sources, in priority order:

1. **Vendored crate sources** from this machine's cargo registry
   (`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/…`), i.e. the
   exact code this repo compiles. Below `<reg>` abbreviates that prefix.
   The repo's `src-tauri/Cargo.toml` asks for `tauri = "2.10"` (a semver
   *requirement*, not a pin) and `tauri-plugin-single-instance = "=2.4.3"`;
   the lockfile (`src-tauri/Cargo.lock`) resolves:
   `tauri 2.11.5`, `tauri-runtime 2.11.3`, `tauri-runtime-wry 2.11.4`,
   `tao 0.35.3`, `tray-icon 0.24.2`, `muda 0.19.3`,
   `tauri-plugin-single-instance 2.4.3`. `tauri-plugin-autostart` is not yet
   a dependency; the registry copy and the latest published release are
   both **2.5.1** (crates.io, verified 2026-09-01).
2. docs.rs rendered docs for the pinned versions (URLs below; note that
   macOS-only methods are only rendered in the `x86_64-apple-darwin`
   target docs — the default docs.rs build target omits them).
3. Official guides at v2.tauri.app and Apple developer documentation for the
   AppKit behaviors underneath.

Cross-check note: `tauri-plugin-single-instance` 2.4.3 itself declares
`tauri = "2.10"` (`<reg>/tauri-plugin-single-instance-2.4.3/Cargo.toml`
lines 69–71) — that is where the repo's "^2.10" comment comes from.
`tauri-plugin-autostart` 2.5.1 declares `tauri = "2.8.2"`, so it is
compatible with the locked tauri 2.11.5.

---

## Verdict summary

Everything needed for a macOS resident app exists in the locked versions:

- **Tray**: enable the `tray-icon` feature; `TrayIconBuilder` + `muda` menus
  are fully supported. On macOS the **default is menu-on-left-click**; setting
  `show_menu_on_left_click(false)` makes the left click fire a
  `TrayIconEvent::Click` instead, which is exactly the hook for
  left-click-shows-window. Template images are supported
  (`icon_as_template`); the icon is force-rendered at **18 pt height**.
- **Hide instead of exit**: closing the last window first fires
  `WindowEvent::CloseRequested` (interceptable with `api.prevent_close()`),
  and only if not prevented destroys the window and then fires
  `RunEvent::ExitRequested` with `api.prevent_exit()`. For "close = hide"
  the CloseRequested hook is the right one — at ExitRequested the window is
  **already destroyed**.
- **Accessory ⇄ Regular**: runtime switching works and is supported on all
  stable 2.x — `AppHandle::set_activation_policy` was added in
  2.0.0-beta.20 and AppKit allows setting any policy since macOS 10.9.
  Bonus: `AppHandle::set_dock_visibility` exists for Dock-only toggling.
- **Single-instance**: the 2.4.3 callback fires on a unix-domain socket
  listener that is independent of window state — a hidden window changes
  nothing. But the plugin does **not** show/focus the window itself; the
  callback must call `window.show()` + `set_focus()`. The existing
  `--attach-url` forwarding composes unchanged.
- **Autostart**: tauri-plugin-autostart 2.5.1 uses **no SMAppService** — on
  macOS it writes a `~/Library/LaunchAgents/<name>.plist` (default,
  `MacosLauncher::LaunchAgent`, `RunAtLoad`) or creates an AppleScript login
  item. macOS passes **no special argument** by itself; the supported way to
  detect "launched at login" is to register your own sentinel argument
  (`Builder::arg`) and check argv at startup — in LaunchAgent mode arbitrary
  args are passed through the plist's `ProgramArguments`.

The main correction to the usual assumptions: **`prevent_exit()` cannot
revive the closed window** (the window is destroyed before ExitRequested);
and the tray icon left click **does open the menu by default on macOS**
(both left and right), so left-click-show-window requires the explicit
`show_menu_on_left_click(false)` opt-out.

---

## 1. Tray (`tray-icon` feature)

### Enabling and construction

- The feature is opt-in: `tauri = { version = "2.10", features = ["tray-icon"] }`.
  In tauri's manifest the feature is `tray-icon = ["dep:tray-icon"]` — it
  pulls the `tray-icon` crate (0.24.2 locked), which in turn uses `muda`
  (0.19.3 locked) for menus. Verified: `<reg>/tauri-2.11.5/Cargo.toml`
  line 129 and the crates.io 2.11.5 feature map;
  guide: https://v2.tauri.app/learn/system-tray/ (Configuration section).
- Rust-only construction is enough — a tray built in Rust needs **no
  capability/permission entry** (capabilities gate the *JavaScript* tray/menu
  API, not the Rust one; the guide's capability examples are for JS calls).
- Builder: `tauri::tray::TrayIconBuilder` (`with_id`, `icon`, `tooltip`,
  `title`, `menu`, `icon_as_template`, `show_menu_on_left_click`,
  `on_menu_event`, `on_tray_icon_event`), then `.build(manager)` —
  https://docs.rs/tauri/2.11.5/tauri/tray/struct.TrayIconBuilder.html.
  Handlers receive the app handle / tray icon directly; the same events also
  arrive centrally as `RunEvent::MenuEvent` / `RunEvent::TrayIconEvent`
  (https://docs.rs/tauri/2.11.5/x86_64-apple-darwin/tauri/enum.RunEvent.html)
  and via `AppHandle::on_menu_event` / `AppHandle::on_tray_icon_event`
  (`<reg>/tauri-2.11.5/src/app.rs` lines 2000–2045).
- `TrayIconBuilder::menu_on_left_click` is deprecated since tauri 2.2.0 in
  favor of `show_menu_on_left_click` (`<reg>/tauri-2.11.5/src/tray/mod.rs`
  lines 309–326; same deprecation in the config:
  `TrayIconConfig.menu_on_left_click`, `<reg>/tauri-utils-2.9.3/src/config.rs`
  line 3140).
- **Lifetime**: tauri keeps a clone of every built tray in app state
  (`register()` pushes it into `resources_table` and `manager.tray.icons`,
  `<reg>/tauri-2.11.5/src/tray/mod.rs` lines 398–450). The underlying
  tray-icon type is reference-counted and the icon disappears when the last
  instance drops (`<reg>/tray-icon-0.24.2/src/lib.rs` lines 340–347 and
  `Drop` in `<reg>/tray-icon-0.24.2/src/platform_impl/macos/mod.rs` lines
  277–281). So the icon survives dropping the local `TrayIcon` value; lookup
  and removal later: `AppHandle::tray_by_id` /
  `AppHandle::remove_tray_by_id` (`<reg>/tauri-2.11.5/src/app.rs` lines
  828–842). `AppHandle::cleanup_before_exit` clears the trays
  (`<reg>/tauri-2.11.5/src/app.rs` line 1105).
- A config-only tray also exists: `app > trayIcon` in `tauri.conf.json`
  (`TrayIconConfig`: `id` default `"main"`, `iconPath`, `iconAsTemplate`,
  `showMenuOnLeftClick`… — `<reg>/tauri-utils-2.9.3/src/config.rs`
  lines 3069, 3121+). For dsh-desktop, building in `setup()` is more
  flexible (we need runtime menu updates anyway).

### Native menus

- `tauri::menu` re-exports muda's item types: `Menu`, `Submenu`,
  `MenuItem`, `CheckMenuItem`, `IconMenuItem`, `PredefinedMenuItem` and the
  `MenuBuilder`/`SubmenuBuilder` builders
  (`<reg>/tauri-2.11.5/src/menu/mod.rs` line 18 `pub use builders::*`, files
  `normal.rs`, `check.rs`, `predefined.rs`, `submenu.rs`;
  https://docs.rs/tauri/2.11.5/tauri/menu/index.html).
- `PredefinedMenuItem` provides native macOS items: `separator`, `copy`,
  `cut`, `paste`, `select_all`, `undo`, `redo`, `minimize`, `maximize`,
  `fullscreen`, `hide`, `hide_others`, `show_all`, `close_window`, `quit`,
  `about`, `services`, `bring_all_to_front`
  (`<reg>/muda-0.19.3/src/items/predefined.rs` lines 38–170). The repo
  already uses `PredefinedMenuItem::quit` for the window menu
  (`src-tauri/src/lib.rs` line 122).
- Item ids: `MenuItem::with_id(app, id, text, enabled, accelerator)` and the
  event's `event.id()` (the repo already does this for the app menu,
  `src-tauri/src/lib.rs` lines 115–135). Tray menu events carry
  `MenuEvent.id()` the same way; `TrayIconBuilder::on_menu_event` is global —
  "this handler is called for any menu event, whether it is coming from this
  window, another window or from the tray icon menu"
  (`<reg>/tauri-2.11.5/src/tray/mod.rs` lines 328–335) — dispatch on the id,
  as the repo's `on_menu_event` already does.

### Clicks on the icon itself (macOS)

- Defaults: menu shows on **both** left and right click on macOS
  (`TrayIconAttributes::menu_on_left_click: true`,
  `menu_on_right_click: true`, defaults at
  `<reg>/tray-icon-0.24.2/src/lib.rs` lines 216–217; the official guide
  states it: "By default the menu is displayed on both left and right
  clicks", https://v2.tauri.app/learn/system-tray/#add-a-menu).
- Mechanics (`<reg>/tray-icon-0.24.2/src/platform_impl/macos/mod.rs`): an
  overlay `NSView` (`TrayTarget`) is added to the status item's button and
  intercepts `mouseDown:`/`rightMouseDown:` (lines 336–380). On a click,
  `on_tray_click` (lines 489–513) calls `NSStatusBar` button
  `performClick(nil)` — which pops the attached menu — *only when* the
  matching `menu_on_*_click` flag is set **and the menu has items**
  (`menu.numberOfItems() > 0`, lines 499–508); otherwise it only highlights
  the button. **Consequences**:
  - With `show_menu_on_left_click(false)`, a left click does NOT open the
    menu; instead `TrayIconEvent::Click { button: Left, state: Down }` is
    sent (the mouse handlers always emit the event, lines 337–365).
    Handler: `TrayIconBuilder::on_tray_icon_event` or
    `AppHandle::on_tray_icon_event`. In it, calling
    `window.show()` + `window.set_focus()` implements
    **left-click-shows-window**, the standard macOS tray-app pattern. The
    menu remains available on right click (`menu_on_right_click` stays
    `true`).
  - **An empty menu never pops**: with zero items the click only highlights
    (lines 499–508). Relevant if we build the tray menu asynchronously.
- `TrayIconEvent` variants: `Click` (with `button`, `button_state`,
  `position`, `rect`), `DoubleClick` (**Windows only**), `Enter`/`Move`/
  `Leave` (emitted on macOS via a tracking area; **Linux unsupported** for
  the whole event enum) — `<reg>/tray-icon-0.24.2/src/lib.rs` lines 546–609
  and macOS mouse handlers lines 427–440.
- Manual menu opening: `TrayIcon::show_menu()` — on macOS implemented as
  `button.performClick(nil)`, so it opens **anchored under the status item**
  (not at an arbitrary cursor position) and Linux is unsupported
  (`<reg>/tray-icon-0.24.2/src/lib.rs` lines 497–508; macos impl lines
  254–261). `TrayIcon::rect()` gives the icon's screen rect for positioning
  a window near the tray (macOS supported, lines 510–517 /
  macos `get_tray_rect`, 515–528).

### Icon: size, format, template

- Input format is raw RGBA: `Image::from_rgba` (or `Image::from_bytes` /
  `include_image!`, which need the `image-png` feature for PNG input —
  `<reg>/tauri-2.11.5/Cargo.toml` line 98 `image-png = ["image/png"]`,
  `Image::from_bytes`/`from_path` in `<reg>/tauri-2.11.5/src/image/mod.rs`
  lines 76–96).
- **The icon is force-rendered at 18 pt height** on macOS: the RGBA buffer
  is re-encoded to PNG and `NSImage.setSize(NSSize(width·18/height, 18))`
  (`<reg>/tray-icon-0.24.2/src/platform_impl/macos/mod.rs`
  `set_icon_for_ns_status_item_button`, lines 283–317, `icon_height = 18.0`
  at line 296). Width follows the aspect ratio, so supply a square image.
  For crisp rendering on Retina, provide a **36×36 px** source (2× of
  18 pt); an 18×18 source will be upscaled and look blurry.
- Template images: `TrayIconBuilder::icon_as_template(true)` /
  `TrayIcon::set_icon_as_template` set `NSImage.isTemplate`
  (`macos/mod.rs` lines 207–218, 310). Apple defines template images as
  black-and-transparent images that AppKit automatically recolors for the
  menu bar appearance (light/dark, highlight) —
  https://developer.apple.com/documentation/appkit/nsimage/1520017-template
  (the exact URL the crate source cites). Practical recipe: monochrome
  black + alpha PNG at 36×36, `icon_as_template(true)`.
- The status item is created with `NSVariableStatusItemLength` (width fits
  content; `mod.rs` line 54–56), and `TrayIcon::set_title` can show text
  next to the icon (lines 169–191) — the supported way to surface a short
  server status/port next to the icon.

## 2. Hiding the window instead of exiting

### Event order when the last window closes (exact, from the runtime)

`tauri-runtime-wry 2.11.4` (`<reg>/tauri-runtime-wry-2.11.4/src/lib.rs`,
`handle_event`, lines 4311–4326 and `on_close_requested` lines 4438–4470):

1. User close (red button / Cmd+W) → `TaoWindowEvent::CloseRequested` →
   tauri fires **`RunEvent::WindowEvent { event: WindowEvent::CloseRequested
   { api: CloseRequestApi } }`** (`<reg>/tauri-2.11.5/src/app.rs` lines
   118–121, 177–179). `api.prevent_close()` aborts the close entirely.
   This is the hook where "close = hide" must be implemented:
   `api.prevent_close()` then `window.hide()`.
2. If not prevented, the window is destroyed; on
   `TaoWindowEvent::Destroyed`, **when the windows map becomes empty** the
   runtime fires **`RunEvent::ExitRequested { code: None, api }`**; unless
   the handler calls `api.prevent_exit()`, `ControlFlow::Exit` ends the app.
   **The window no longer exists at this point** — `prevent_exit()` keeps
   the *process* alive but cannot bring the closed window back; a resident
   design that goes through ExitRequested must re-create the window itself.
3. Programmatic `AppHandle::exit(code)` / `restart()` arrive as
   `Message::RequestExit` → `ExitRequested { code: Some(code) }`
   (`lib.rs` lines 4349–4365). `prevent_exit()` **is honored** for
   `AppHandle::exit(...)` too; the only exception is restart, whose code is
   `RESTART_EXIT_CODE = i32::MAX` — `prevent_exit` explicitly ignores it
   (`<reg>/tauri-2.11.5/src/app.rs` lines 76–94). After the request the
   runtime proceeds to **`RunEvent::Exit`** ("Event loop is exiting",
   `app.rs` line 221) — the repo's sidecar teardown already lives there
   (`src-tauri/src/lib.rs` lines 185–191).

### Hide/show

- `WebviewWindow::hide()` / `show()` / `set_focus()` /
  `is_visible()` (`<reg>/tauri-2.11.5/src/webview/webview_window.rs`
  lines 1800, 2207–2270). On macOS `hide` is a synchronous
  `[NSWindow orderOut]` and `show` is `makeKeyAndOrderFront`
  (`<reg>/tao-0.35.3/src/platform_impl/macos/window.rs` lines 668–676) —
  the window and its webview survive; only visibility changes. `show()`
  also makes the window key (focus).
- Application-level equivalents exist: `AppHandle::hide()` /
  `AppHandle::show()` (macOS; NSApp-level hide/unhide, "Shows the
  application, but does not automatically focus it" —
  `<reg>/tauri-2.11.5/src/app.rs` lines 1086–1107;
  https://docs.rs/tauri/2.11.5/x86_64-apple-darwin/tauri/struct.AppHandle.html).
- App menu and focus after hiding the last window: AppKit keeps the
  application frontmost with its (Regular-policy) menu bar even with zero
  visible windows — the menu bar belongs to NSApp, not the window, so the
  app menu (including the repo's "Change Server…" item) stays live.
  Dock icon and Cmd+Tab entry remain as long as the policy is Regular —
  which is why §3 exists. Tauri/tao does not install
  `applicationShouldTerminateAfterLastWindowClosed` (no occurrence in
  `<reg>/tao-0.35.3/src/platform_impl/macos/`), so the default AppKit
  terminate-on-last-close is never in play; exit is entirely governed by
  the ExitRequested flow above.
- macOS bonus event: `RunEvent::Reopen { has_visible_windows }` fires when
  the user clicks the Dock/Finder reopen (`applicationShouldHandleReopen`)
  — the natural hook for "Dock click → show the hidden window"
  (`<reg>/tauri-2.11.5/src/app.rs` lines 297–311).

## 3. ActivationPolicy::Accessory ⇄ Regular at runtime

- Type: `tauri::ActivationPolicy` = `Regular` | `Accessory` | `Prohibited`
  (macOS only, `#[non_exhaustive]`), re-exported from tauri-runtime
  (`<reg>/tauri-2.11.5/src/lib.rs` line 204;
  `<reg>/tauri-runtime-2.11.3/src/lib.rs` lines 260–270;
  https://docs.rs/tauri/2.11.5/x86_64-apple-darwin/tauri/enum.ActivationPolicy.html).
- Two setters exist in tauri 2.11.5:
  - `App::set_activation_policy(&mut self, policy)` — the pre-run API
    (v1-era, added 2021-08); since 2.x it falls back to the AppHandle path
    if the runtime has already started (`<reg>/tauri-2.11.5/src/app.rs`
    lines 1273–1291). Usual place: `.setup()`, before `.run()`.
  - **`AppHandle::set_activation_policy(&self, policy) -> Result<()>` —
    runtime switching.** It posts `Message::SetActivationPolicy` to the
    event loop, which calls tao's `set_activation_policy_at_runtime`,
    which performs `NSApplication.setActivationPolicy(...)` directly
    (`<reg>/tauri-2.11.5/src/app.rs` lines 627–644;
    `<reg>/tauri-runtime-wry-2.11.4/src/lib.rs` lines 1566, 2736–2742,
    3337–3339; `<reg>/tao-0.35.3/src/platform/macos.rs` lines 428–443).
  - **Since when**: added in tauri **2.0.0-beta.20** (PR #9842, "Added
    `AppHandle::set_activation_policy` for macOS" — crates/tauri/CHANGELOG.md,
    https://github.com/tauri-apps/tauri). So it is available in
    **every stable 2.x, including 2.10 and 2.11.5**. On the AppKit side,
    Apple's documentation states "You can set any activation policy in
    macOS 10.9 and later" —
    https://developer.apple.com/documentation/appkit/nsapplication/setactivationpolicy(_:)
    (verified against the page content).
- Effects of `Accessory` (Apple's NSApplication.ActivationPolicy docs,
  https://developer.apple.com/documentation/appkit/nsapplication/activationpolicy):
  no Dock icon, not in Cmd+Tab (Force Quit) list, and **no main menu bar**
  while accessory — windows are still allowed. `Regular` = Dock icon +
  menu bar. Flipping back to `Regular` at runtime re-creates Dock/Cmd+Tab
  presence; the menu bar reappears once the app activates/shows a window
  (AppKit applies the policy to the running NSApp; menu-bar ownership is
  tied to the policy, not to the menu object).
- Ordering pitfall: the app menu must exist *before* returning to Regular
  if you want a menu bar — the repo already installs its menu in `setup()`
  via `app.set_menu(...)` (`src-tauri/src/lib.rs` line 124), which is
  compatible with switching policy later. Switching to Accessory does not
  destroy the installed NSApp menu; it is simply not displayed while
  accessory.
- Dock-only alternative: `AppHandle::set_dock_visibility(bool)` (macOS,
  `<reg>/tauri-2.11.5/src/app.rs` lines 646–660; tao implements it with
  `TransformProcessType`-based hide/show plus a 1 s anti-race guard against
  duplicated Dock icons — `<reg>/tao-0.35.3/src/platform_impl/macos/dock.rs`
  lines 33–55). Useful when we want the tray visible but keep the process
  "Regular" without a full policy flip.

## 4. Single-instance interplay (tauri-plugin-single-instance 2.4.3)

- Transport on macOS (`<reg>/tauri-plugin-single-instance-2.4.3/src/platform_impl/macos.rs`):
  a **unix-domain socket** at
  `/tmp/<bundle-identifier-with-.-and---replaced-by-_>_si.sock`
  (`socket_path`, lines 88–99; no version segment unless the `semver`
  feature is on). The first instance binds a tokio `UnixListener` in setup
  (`listen_for_other_instances`, lines 101–140). The second instance
  connects with `UnixStream`, writes `cwd + "\0\0" + args joined by "\0"`,
  and immediately `std::process::exit(0)` (`notify_singleton`, lines 74–86 +
  setup lines 25–37).
- Callback signature: `FnMut(&AppHandle<R>, argv: Vec<String>, cwd: String)`
  (`<reg>/tauri-plugin-single-instance-2.4.3/src/lib.rs` lines 36–40) —
  `argv` is the second process's **full argv including argv[0]** (the
  join of `std::env::args()`), so index-based parsing must account for it;
  the repo's `launch::resolve_relaunch_attach` scans rather than indexes,
  so it is unaffected.
- **What the callback gets when the window is hidden: the same thing as
  always.** The listener task lives in the first process's async runtime,
  independent of window state; nothing about a hidden window blocks
  delivery. There is no automatic reveal: the plugin does not touch windows
  at all — its own README example only logs/emits
  (`<reg>/tauri-plugin-single-instance-2.4.3/README.md`, Usage section).
- Required addition for resident mode — in the callback:
  `if let Some(w) = app.get_webview_window("main") { w.show()?; w.set_focus()?; }`
  before/in addition to the existing `--attach-url` handling
  (`src-tauri/src/lib.rs` lines 53–88). Composition with attach-forwarding
  is natural: show first, then retarget (`attach_to_server`) — the
  callback already resolves the window lazily (lines 78–86), so it works
  even when the window was hidden rather than absent.
- Registration order stays critical: "make sure that this plugin is
  registered first" (README, same note as the repo comment at
  `src-tauri/src/lib.rs` lines 46–52) — the socket must be claimed before
  anything else can spawn work. The socket file is cleaned on
  `RunEvent::Exit` (macos.rs `on_event`, lines 40–46).
- Note for dev runs: the socket path derives from `config.identifier`
  (tauri.conf.json), so dev and packaged builds of the same identifier
  collide intentionally — a second launch focuses the first. (Directly
  implied by `socket_path`, macos.rs lines 88–99.)

## 5. Launch at login (tauri-plugin-autostart)

- Version/compat: latest release is **2.5.1** (crates.io, published
  2025-10-27; the vendored copy is identical); requires `tauri = "2.8.2"`
  (`<reg>/tauri-plugin-autostart-2.5.1/Cargo.toml` lines 64–66) — fine
  with the locked tauri 2.11.5. Rust-only usage needs no npm package and
  no capability (capabilities only gate the JS commands `enable`/`disable`/
  `is_enabled`; default permission set = `allow-enable`, `allow-disable`,
  `allow-is-enabled` — `<reg>/tauri-plugin-autostart-2.5.1/permissions/default.toml`).
  Guide: https://v2.tauri.app/plugin/autostart/.
- **Mechanism on macOS — no SMAppService anywhere.** The plugin wraps the
  `auto-launch` 0.5 crate (`Cargo.toml` lines 54–56), whose macOS backend
  (`<reg>/auto-launch-0.5.0/src/macos.rs`) offers exactly two modes,
  selected by the plugin's `MacosLauncher` enum
  (`<reg>/tauri-plugin-autostart-2.5.1/src/lib.rs` lines 26–30):
  - **`MacosLauncher::LaunchAgent` (default)** — `enable()` writes
    `~/Library/LaunchAgents/<app_name>.plist` with `Label`,
    `ProgramArguments = [app_path, ...args]` and `RunAtLoad = true`;
    `disable()` deletes the file; `is_enabled()` = file exists
    (macos.rs lines 63–113, 155–158). launchd launches the binary
    directly (argv[0] = the binary path inside the .app), at every login.
  - **`MacosLauncher::AppleScript`** — `enable()` runs
    `osascript -e 'tell application "System Events" to make login item at
    end with properties {name, path, hidden}'`; only the literal args
    `--hidden`/`--minimized` are recognized (they set the login item's
    `hidden` property, which makes macOS launch the app without bringing
    it forward); any other args are ignored. `is_enabled()` scans
    `get the name of every login item` (macos.rs lines 115–146, 34–39).
  Legacy LaunchAtLogin/`SMAppService` APIs are **not used**; AppleScript
  login items are the "System Settings → General → Login Items" visible
  form, LaunchAgents appear there too (as "Allow in Background").
- Builder (`<reg>/tauri-plugin-autostart-2.5.1/src/lib.rs` lines 102–236):
  `Builder::new().arg("…").args([...]).macos_launcher(MacosLauncher::…)
  .app_name("…").build()`; `app_name` defaults to `package_info().name`;
  on macOS `app_path` = canonicalized `current_exe()` (AppleScript mode
  trims to the `.app` bundle so Login Items don't show a "Unix Executable",
  lines 197–215).
- **"Launched at login" detection — the plugin exposes nothing; macOS
  passes no special argument by itself.** The supported pattern is to
  register a sentinel argument at plugin construction and check argv at
  startup: `Builder::new().arg("--dsh-autostart").build()`; in
  LaunchAgent mode the plist's `ProgramArguments` carries it, so the
  launched instance sees it in `std::env::args()` and can start hidden
  (show nothing, activate Accessory, tray only). The builder's doc comment
  says exactly this purpose: "Adds an argument to pass to your app on
  startup" (lib.rs lines 123–138). In AppleScript mode the sentinel is
  *not* delivered (only `--hidden`/`--minimized` are meaningful), so
  **LaunchAgent mode is the one that supports custom-arg detection**.
  Alternative without any arg: `auto_launch.is_enabled()` at startup says
  "the entry exists", not "this launch came from it" — insufficient alone,
  since the user can also start the app manually.
- Enable/disable toggle: call `AutoLaunchManager::enable()/disable()/
  is_enabled()` from Rust (state-managed by the plugin, lib.rs lines
  49–67) or expose the JS commands. Flipping the toggle just
  writes/removes the plist (LaunchAgent) — no daemon reload needed
  (`RunAtLoad` only matters at login/load).
- Interplay with single-instance: the login-launched instance goes through
  the same socket handshake — if the app is already running, the
  autostarted process exits(0) and its argv (including the sentinel) lands
  in the first instance's callback. The callback should ignore the
  autostart sentinel (it must not be treated as an attach URL), and the
  hidden-start path only applies when the autostart instance *is* the
  first instance.

## 6. Pitfalls (NSStatusItem / hidden-window specifics)

- **Menu tracking blocks the main thread.** While a status-item NSMenu is
  open (the `performClick` path), AppKit runs the main runloop in event
  tracking mode; Tauri event-loop work (menu item updates, window.show,
  IPC) will not run until the menu closes. Populate/status-update menu
  items *before* the menu opens; do not expect live updates while the user
  holds the menu open. (AppKit runloop mode behavior —
  https://developer.apple.com/documentation/appkit/nsapplication; the
  open-menu path is `performClick` in
  `<reg>/tray-icon-0.24.2/src/platform_impl/macos/mod.rs` lines 489–513.)
- **Empty tray menus never open** on macOS (guarded in `on_tray_click`,
  see §1) — and the Linux variant is documented in the crate docs:
  "Sometimes the icon won't be visible unless a menu is set. Setting an
  empty `Menu` is enough" (carried through tauri's builder docs,
  `<reg>/tauri-2.11.5/src/tray/mod.rs` lines 216–229). For us: build the
  tray only when the menu is ready, or with a disabled placeholder item.
- **Runtime menu updates are supported**: `MenuItem::set_text`/
  `set_enabled`, `CheckMenuItem::set_checked`/`is_checked`,
  `PredefinedMenuItem::set_text` (`<reg>/muda-0.19.3/src/items/normal.rs`
  lines 87–101; `check.rs` lines 101–141; `predefined.rs` line 196), and
  the whole menu can be swapped via `TrayIcon::set_menu`
  (`<reg>/tray-icon-0.24.2/src/lib.rs` lines 391–398). Status display in
  the tray menu ("server: running on :3080") is a `set_text` away; tray
  icon badge-style text is `TrayIcon::set_title` (§1).
- **Icon size**: 18 pt fixed height (see §1) — do not ship a huge app icon
  as the tray icon; the config docs explicitly warn to keep
  `trayIcon.iconPath` small or "it's going to bloat your final executable"
  (`<reg>/tauri-utils-2.9.3/src/config.rs` lines 3124–3128, relevant if we
  embed via `include_image!`).
- **`TrayIconEvent::DoubleClick` is Windows-only**; on macOS only Click /
  Enter / Move / Leave arrive (§1) — don't design double-click-to-show.
- **App Nap**: a hidden-window app with no visible UI is eligible for App
  Nap (timer/API throttling) while macOS deems it inactive. If the hidden
  shell must keep the sidecar probe polling at full rate, hold a
  `ProcessInfo.beginActivity(options:reason:)` token
  (`NSActivityIdleSystemSleepDisabled` or `NSActivityUserInitiated`
  options) — https://developer.apple.com/documentation/foundation/processinfo/beginactivity(options:reason:).
  Tauri exposes no API for this; it would be a small `objc2-foundation`
  call in the shell. (Observe first: an app with an active socket + child
  process may never actually nap.)
- **`RunEvent::ExitRequested` fires per exit request** — with
  `prevent_exit()` the app lives on; when the user later opens a new
  window and closes it, the sequence repeats (windows-map-empty → new
  ExitRequested), so the handler must be idempotent. Also remember
  `prevent_exit()` cannot block `restart()`
  (`RESTART_EXIT_CODE` bypass, §2).
- **Accessory + no windows = invisible process**: with Accessory and no
  tray icon (e.g. tray creation failed or `set_visible(false)`), the user
  has no way to reach the app; ensure the tray exists before going
  Accessory (`set_visible` re-creates the status item from stored attrs if
  it was hidden — `<reg>/tray-icon-0.24.2/src/platform_impl/macos/mod.rs`
  lines 193–205).
- **Tray/menu construction must be on the main thread**: tray-icon
  requires `MainThreadMarker` and returns `Error::NotMainThread`
  otherwise (`mod.rs` lines 34–47); tauri's `TrayIconBuilder::build`
  already hops to the main thread via `run_on_main_thread`
  (`<reg>/tauri-2.11.5/src/tray/mod.rs` lines 386–396) — building inside
  `setup()` is safe.

## Negative results (explicit "not possible / not provided")

- **No SMAppService autostart** in tauri-plugin-autostart 2.5.1 (or in
  auto-launch 0.5.0) — macOS 13+ SMAppService is simply not implemented;
  the plugin is plist/osascript based (§5).
- **No "launched-at-login" flag from the OS or the plugin** — macOS login
  items pass no marker argument; detection requires the app's own sentinel
  arg in LaunchAgent mode (§5).
- **`prevent_exit()` cannot prevent window destruction** — it runs after
  `Destroyed`; window revival requires the CloseRequested hook (§2).
- **`AppHandle::exit()` can be intercepted** by `prevent_exit()` (code
  `Some`), but **`restart()` cannot** (§2).
- **No per-click left-click menu suppression** — the
  `show_menu_on_left_click` switch is global to the tray, not per event
  (§1).
- **No tray-icon double-click on macOS** (`DoubleClick` is Windows-only,
  §6).
- **No multi-resolution (HiDPI set) tray icon API** — a single RGBA buffer
  is passed; macOS scales it to 18 pt; there is no @1x/@2x selection
  (§1).
- **tauri exposes no App Nap suppression API** (§6).

## Open questions

1. Whether `TrayIconEvent::Enter/Move/Leave` (active on macOS) are worth
   wiring (hover feedback) or should be ignored — event volume is small
   but non-zero; no repo decision yet.
2. LaunchAgent mode launches the raw binary, bypassing `open(1)`/LaunchServices
   GUI launch: first-run at login may have a different TCC state (no
   notification/screen-recording prompts shown by the system at that
   point). Not verifiable from sources alone — needs an on-machine probe
   before relying on TCC-gated features at login time.
3. If "Quit" lives in the tray menu (`PredefinedMenuItem::quit`), confirm
   it triggers the same `ExitRequested → Exit` flow the sidecar teardown
   depends on — the predefined item maps to AppKit's terminate:; asserting
   the exact runtime path needs a probe rather than a source read.
