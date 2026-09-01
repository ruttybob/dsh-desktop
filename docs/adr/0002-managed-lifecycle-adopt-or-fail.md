# Managed server lifecycle: adopt-or-fail, never kill, log-file stdio

Managed Mode keeps exactly one server story (dsh-u3m.3). A single adoption record — `~/.dsh/desktop/managed-server.json` (`version, profile, port, pid, spawned_at`) — decides everything at launch: record alive (pid running and the port answers a probe) → navigate to it, the browser-session cookie carries auth across restarts, so the spawn never needs a token on re-adoption; no live record → spawn the chosen profile on the fixed default port (3080, never OS-assigned) and follow the ready line in the log; the port is taken by anything else, a shell-started dsh included → the spawn fails with its natural EADDRINUSE and the window shows an error stub (log detail, Retry = redo adopt-or-spawn, Quit). No port bumping, no adopting a foreign server, one record — not one per profile. The shell never kills a managed server on any exit path (quit, crash, single-instance attach-retarget). The child's stdout and stderr go to `~/.dsh/desktop/managed-server.log` from the start and the ready line is read by tailing that file: app-held pipes kill the server with an unhandled EPIPE on its first post-exit stdout write (verified on Node 22), and a relay process to avoid the file was judged over-engineered.

## Considered options

- **Auto-bump to a free port / deterministic per-profile ports** — rejected: silently displacing whoever owns the port hides a conflict only the user can resolve; a loud failure is the honest answer.
- **Adopt any same-profile dsh regardless of who started it** — rejected: safe only with argv-identity forensics (PID-reuse checks, ownership proofs); the app's own record is the only adoption authority.
- **Per-profile records, many live managed servers** — rejected: one record, one port, one story.
- **Detached relay process holding the pipes so the token never lands on disk** — rejected: the user's own `dsh-web` supervisor already persists the same token in its log; the relay buys ritual, not safety.

## Consequences

- Scoped exception to the standing "launch token memory-only" rule: the ready-line token persists in `~/.dsh/desktop/managed-server.log` (mode 0600) — the exposure the user's existing `dsh-web.log` already carries.
- A shell-started dsh on the port blocks Managed Mode until the user stops it — deliberate friction: one loud error instead of cleverness.
- The fixed port is what makes token-free re-adoption work at all: a new OS-assigned port would change the cookie's authority and strand the next launch on a 401.
- Switching profiles in-session replaces the record; the previous server keeps running as a deliberately unmanaged orphan (logged, never killed).
- Liveness stays with the existing unreachable-probe monitor — the managed URL is watched exactly like an attached one; death → the existing unreachable stub.
