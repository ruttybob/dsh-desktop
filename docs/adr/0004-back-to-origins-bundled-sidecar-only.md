# Back to origins: the bundled sidecar is the only server story

The standalone-launcher redraw (ADR-0002, ADR-0003) aimed the shell at the user's own dsh: discover it, adopt-or-spawn it on a fixed port, guide the user when it is missing. Working the no-dsh fallback decision (dsh-u3m.4) surfaced that the fallback question only exists because of the user-dsh concept at all; on 2026-09-01 the maintainer redrew the destination instead of answering it. The shell returns to the origins design (c1aff67): on launch it always spawns its bundled sidecar (`resources/host` — `main.mjs` + bundled Node runtime + `@deepseek-ai/dsh`), follows the ready line into the WebView, and kills the sidecar on exit. No user-installed dsh is ever discovered, adopted, or spawned; no connect UI exists.

Supersedes **ADR-0002 entirely** (adopt-or-fail lifecycle, fixed port, adoption record) and **the managed half of ADR-0003** (standalone redraw, tray contract). Survives from ADR-0003: Attach Mode and its whole surface stay retired — and the connect UI dies with them (splash connect form, remembered-connection store, Change Server menu, single-instance forwarding).

Carried over from the fallback grilling (dsh-u3m.4, before the redraw): the port stays `--port 0` (OS-assigned; the ready line is the authority for the actual URL, and every launch mints a fresh browser-session cookie from its token); the lifecycle is the origins one — kill on exit, no record, no tray substrate; the bundle is an **actively maintained** second distribution, rebuilt with harness releases; a missing `web` profile keeps auto-initializing silently inside the child.

## Considered options

- **Managed mode with the bundled sidecar as an explicit fallback button** — rejected: keeps the whole discovery/adoption/fixed-port machinery alive for a case the maintainer would rather not have at all; the origins design is strictly less code and one story.
- **Managed mode with install-guidance-only fallback** (the recommended option during the grilling) — rejected: the maintainer prefers a self-contained app that always works over a launcher that depends on a user-side install.

## Consequences

- ~445 MB of `resources/host` ship in every .app and must be refreshed with harness releases — an active rebuild obligation, not a frozen snapshot.
- The bundled dsh shares the user's `~/.dsh` home (sessions, settings, profiles, plugins). Version skew between the bundle and any user-side dsh tooling is the normal condition, guarded only by rebuilding promptly after harness releases.
- The adoption record, the tray contract, the fixed-port cookie-authority story, and the no-dsh guidance surfaces fall out of active use. The discovery research (`docs/research/managed-mode-dsh-discovery.md`) stays true as facts about the harness but no longer drives shell behavior.
- The probe monitor and unreachable stub go with the attach surface; a mid-session sidecar death leaves the WebView's own connection error. A second app instance spawns its own sidecar on its own OS-assigned port (the origins behavior).
- Token redaction before host stdout reaches the log, the login-shell PATH restore before spawn, and the process-group kill on exit all survive unchanged.
