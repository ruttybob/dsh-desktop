# Context

Desktop shell for DeepSeek Harness: a self-contained Tauri window that launches its bundled dsh sidecar and shows the web GUI.

## Glossary

### Sidecar host
The app-packaged dsh server (`resources/host`: `main.mjs` + a bundled Node runtime + `@deepseek-ai/dsh`) — the shell's only server story: spawned on launch, followed into the WebView via the [ready line](#ready-line), killed on exit. No user-installed dsh is ever discovered, adopted, or spawned (ADR-0004).
_Avoid_: managed mode, bundled fallback, own-server mode, sidecar mode.

### Ready line
The line a dsh web process prints once on stdout after the server is bound: `dsh web: http://127.0.0.1:<port>/?token=…`. The shell watches the [sidecar host](#sidecar-host)'s output for it and navigates the WebView to it; the exact string is best-effort, not a versioned contract.
_Avoid_: ready message, boot line.

### Launch token
A bearer secret every dsh web process generates at startup and prints in its [ready line](#ready-line); it stays valid for the server process's whole lifetime and authenticates by minting the [browser-session cookie](#browser-session-cookie). The shell reads it from the ready line, navigates with it, and never stores it in its own stores.
_Avoid_: auth token, API token, fixed token.

### Browser-session cookie
The signed, authority-bound cookie a dsh web server mints on a successful [launch-token](#launch-token) exchange. The sidecar's port is OS-assigned, so the authority changes every launch: a cookie is minted fresh at each launch from the ready-line token, and none survives across launches.
The dance's reference implementation is `scripts/smoke-host.mjs` (server side: harness `packages/client/connection/src/browser-auth.ts`). WebKit never sends this cookie back — the shell mints it in Rust and fronts the sidecar with `src-tauri/src/cookie_proxy.rs`, which attaches it to every request and rewrites Host/Origin/Referer to the sidecar authority (the cookie name and signed payload hash the authority).
_Avoid_: session token, login cookie.

### Loading screen
The transient placeholder shown while the [sidecar host](#sidecar-host) boots; it carries no choices and takes no input — the connect form, the mode picker, and the Change Server surface are all retired.
_Avoid_: splash screen, welcome screen, connect form.

### Command palette
The Cmd+K overlay: two tabbed views — **Actions** (commands and the model/effort/agent picks) and **Sessions** (every live session, the current one marked); Tab cycles the views, the palette opens on Sessions, and picking a session jumps to it. Its key bindings are remappable from inside the palette itself. It coexists with the composer's slash-command menu — opening one never force-closes the other.

### Update check
The background probe the shell runs at launch (`[update]` log prefix, `src-tauri/src/lib.rs`): it asks the release feed (`latest.json` on the fork's GitHub releases), and on a newer version shows a blocking Restart/Later dialog — accepting downloads and installs the signed bundle and restarts the shell. Every failure (offline, no release yet, rate limits, declined dialog) is a logged warning only; launch never waits on it and the running [sidecar host](#sidecar-host) is never taken down by a failed update.
_Avoid_: auto-update silently, updater polling.

## Non-goals recorded in this context

- Discovering, adopting, or spawning a user-installed dsh — the [sidecar host](#sidecar-host) is the only server story (ADR-0004).
- A background server or menu-bar (tray) control — the sidecar lives exactly as long as the window. (One sanctioned exception: an accepted [update check](#update-check) stops the sidecar a moment before the shell restarts into the new version — the window is still up, but the restart is already committed.)
- The dsh webserver never attaches outward to another listener; multi-host convergence happens client-side (a second surface pointing at one host), not server-side.
