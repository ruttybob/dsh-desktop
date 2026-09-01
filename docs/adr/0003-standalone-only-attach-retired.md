# Standalone-only: Attach Mode retired

The shell becomes a single-mode standalone launcher (map redraw, 2026-09-01): on launch it ensures the user's own dsh web server (profile `web`, fixed default port) is running — adopting the live one or spawning it per the adopt-or-fail lifecycle of ADR-0002 — and follows the ready line into the WebView. The whole Attach surface is removed: the `DSH_DESKTOP_ATTACH_URL` env signal, the `--attach-url` argv flag and its single-instance relaunch forwarding, the splash connect form with its pre-flight probe, the remembered-connection store (`attach-server.json`, v1 schema and its planned v2 mode+payload migration), the mode picker, the Connection… menu, and profile-selection UI — the profile is fixed to `web` (the field stays in the record schema; UI returns only when a second profile is actually needed).

When navigation lands on a 401 (browser-session cookie lost), the shell re-reads the latest ready line from `~/.dsh/desktop/managed-server.log` and navigates with `?token=` to mint a fresh cookie. This is legal because the launch token stays valid for the server process's whole lifetime (harness `browser-auth.ts` compares it across the process lifetime, minting a cookie per presentation) — and it works no matter who spawned the server, the shell or the tray icon.

**Tray contract** (binding for the separate menu-bar-icon effort, so both sides stay compatible): there is exactly one shared adoption record, `~/.dsh/desktop/managed-server.json`; every writer replaces it atomically (temp file + rename); the record is a hint, never authority — pid and port are verified before acting on it; stop means kill the recorded process group (the `process_group(0)` kept in ADR-0002 exists for exactly this) and then clear the record; both sides spawn the server with stdio redirected to `~/.dsh/desktop/managed-server.log`, so the ready line and token are readable by either. This is the whole race story: atomic rename means no torn reads, probe verification means a lost update cannot mislead, and clearing a stale record is always safe.

## Considered options

- **Keep Attach dormant behind a flag** — rejected: dead code kept "just in case" is the over-engineering this redraw retires; git history is the archive.
- **Keep profile-selection UI with a single profile** — rejected: UI with one option is ceremony; the record schema already carries the field.
- **Per-writer record files (one for the shell, one for the tray)** — rejected: two files invite divergent formats and exactly the write races the single-file convention avoids.

## Consequences

- `DSH_DESKTOP_ATTACH_URL` / `--attach-url` become unprocessed arguments; splash.html shrinks to a transient "starting the server" screen; the unreachable stub remains the liveness surface of the managed server.
- ADR-0001 (two-face picker, remembered connection) is superseded; its auto-apply rule survives only as "the launcher has nothing to ask".
- The tray icon effort owns start/stop through the contract above; the shell itself still never kills the server on its own exit paths.
- A shell-started `dsh web` on the port keeps blocking the launch with a loud error (ADR-0002), tray or no tray.
