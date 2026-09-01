# Managed Mode research: discovering and spawning the user's own `dsh web`

Date: 2026-09-01.
Scope: read-only investigation of the deepseek-harness checkout
(`/Users/sergeykostrov/pets/harnesses-ai/ya-ow/deepseek-harness`, below `<harness>`)
and the live machine state, to pin the facts needed for the desktop shell's
"Managed Mode" (spawn the user's own `dsh web` as a child, parse the ready
line, navigate to it). The live server inspected: PID 94112 on 127.0.0.1:3080.
No servers were started or stopped; probes were limited to `--version`,
`--help`, `ps`, `lsof`, and file reads.

All harness paths below are relative to `<harness>` unless marked absolute.
`$DSH_HOME` defaults to `~/.dsh` (verified below); the machine's home is
`/Users/sergeykostrov/.dsh`.

---

## 1. Locating a user's dsh install

### What exists on this machine

| Form | Path / command | Status |
|---|---|---|
| PATH binary | `/Users/sergeykostrov/.local/bin/dsh` | **exists** — a 605-byte `sh` wrapper script (regular file, executable) |
| Checkout pnpm script | `pnpm dsh <args>` run with cwd = `<harness>` root | **works** (this is what the live instance uses) |
| Direct node | `node --import tsx/esm apps/cli/src/bin.ts <args>` with cwd = `<harness>` root | **works** (the same, without pnpm) |
| Built bin | `node apps/cli/lib/bin.js` | **absent** — `<harness>/apps/cli/lib/` does not exist (verified: `ls` → `No such file or directory`) |
| npm-global | `npm root -g` = `~/.local/lib/node_modules`; scopes `@deepseek-ai/` and `@deepseek-harness-tui/` exist but are **empty directories** | **no global dsh install** |
| pnpm-global | `~/.local/share/pnpm/global/5/node_modules` is empty | **no global dsh install** |

`command -v dsh` / `which -a dsh` → `/Users/sergeykostrov/.local/bin/dsh` (only hit).

### The wrapper script `~/.local/bin/dsh` (read in full)

```sh
#!/bin/sh
REPO="$HOME/pets/harnesses-ai/ya-ow/deepseek-harness"
cd "$REPO" || { echo "dsh: checkout not found: $REPO" >&2; exit 1 }
...
exec "$PNPM" dsh "$@"
```

So the PATH binary is *not* an installed package — it hardcodes this checkout
and execs `pnpm dsh "$@"` from the repo root. Consequence for detection: a
`dsh` on PATH implies a checkout install on this machine.

### The npm-script indirection

`<harness>/package.json` line 153:

```json
"dsh": "node --import tsx/esm apps/cli/src/bin.ts",
```

The root manifest has **no `bin` field** (`private: true`).
`<harness>/apps/cli/package.json` declares `bin: { "dsh": "lib/bin.js" }`
(lines 14–16) for the *published* npm package `@deepseek-ai/dsh` (version
`0.1.2-alpha.3`) — but that artifact is not installed anywhere on this
machine and `lib/` is not built in the checkout.

### How the live instance is actually running (ps, verified)

```
PID   PPID  COMMAND
94087 1     node /Users/sergeykostrov/.local/bin/pnpm dsh web
94092 94087 node /Users/sergeykostrov/.local/share/pnpm/.tools/pnpm/11.7.0/bin/pnpm dsh web
94112 94092 node --import tsx/esm apps/cli/src/bin.ts web
```

- The final server process (94112) has **cwd = `<harness>` root** (verified
  with `lsof -a -p 94112 -d cwd`). cwd must be the checkout root: the
  `--import tsx/esm` specifier and the layered `.env` loading resolve from
  there (see `apps/cli/src/bin.ts` line 30, `loadLayeredEnv('dsh')`; the
  `~/.local/bin/dsh` wrapper's own comment states the repo `.env` resolves
  relative to the checkout).
- The outer two processes are pnpm shims (the second is pnpm's versioned
  tool shim under `~/.local/share/pnpm/.tools/pnpm/11.7.0`).

### Reliable detection recipe (facts only)

- `command -v dsh` → wrapper; if present, the checkout is at the path in the
  script (here `$HOME/pets/harnesses-ai/ya-ow/deepseek-harness`).
- Checkout markers, without executing anything:
  `<harness>/apps/cli/src/bin.ts` exists; `<harness>/package.json` has
  `scripts.dsh == "node --import tsx/esm apps/cli/src/bin.ts"`;
  `<harness>/apps/cli/package.json` has `name == "@deepseek-ai/dsh"`.
- Executing forms (all require cwd = checkout root for forms 2–3):
  1. `<dsh on PATH> <args>` (wrapper handles cwd itself),
  2. `pnpm dsh <args>` from the checkout root,
  3. `node --import tsx/esm apps/cli/src/bin.ts <args>` from the checkout root.

### Version probe (executed, verbatim)

```
$ pnpm dsh --version        # from <harness> root
$ node --import tsx/esm apps/cli/src/bin.ts --version
0.1.2-alpha.3
```

`--version` is handled by the launcher's commander setup before any profile
boots (`apps/cli/src/bin.ts` lines 17–24, `apps/cli/src/args.ts` line 119
`.version(version, '-V, --version')`); it reads the version from
`<harness>/apps/cli/package.json` (`bin.ts` lines 17–22). No files are
written. Equivalent zero-exec probe: read `apps/cli/package.json` →
`"version": "0.1.2-alpha.3"`.

`pnpm dsh --help` prints only the launcher help and exits (captured in full
in the appendix; no boot, no writes).

---

## 2. Profile selection

### There is no `--profile` in the live argv because `web` is a subcommand alias

`apps/cli/src/args.ts`:

- Line 13–14 (docblock): *"`web` is a hardcoded alias for `--profile web`"*
- Lines 156–169: the `web` commander subcommand resolves
  `resolveBoot(web, 'web', options, args)` — the profile name `web` is
  hardcoded.
- Line 131: the root option is `--profile <name>` — *"the profile under
  $DSH_HOME/profiles to boot"*.

So both invocation shapes boot the same profile:

- `dsh web [inner args...]` (what the live instance runs), and
- `dsh --profile web [inner args...]`.

### Flag position rules (args.ts)

- Launcher flags come **first**; the first token the launcher does not
  recognize starts the inner app arguments (docblock lines 7–11 and
  implementation lines 123–130). `dsh --profile tui --resume abc` boots
  `tui` with inner args `--resume abc`; `dsh --profile web -h` prints the
  **web app's** help, not the launcher's.
- `dsh --profile web web` is an **error**: the `web` subcommand rejects any
  parent `--profile/--patch/--dump-config/--dump-default-config` seen before
  it (`rejectParentOptions`, args.ts lines 147–154, message: *"error: web
  takes none of parent --profile, --patch, --dump-config, or
  --dump-default-config"*). `--profile` is **not** accepted after `web`
  either — it would pass through as an inner app argument (the subcommand
  sets `allowUnknownOption()`/`passThroughOptions()`).
- A missing profile is an error, not a default: *"error: --profile <name> is
  required"* (args.ts line 140). There is **no default profile**.

### Env vars

- **No `DSH_PROFILE` env var exists.** `grep -rn "DSH_PROFILE" apps packages`
  over the harness checkout → zero hits. The running server's environment
  (read via `ps eww`, see §4) contains **no** `DSH_*` variables at all.
- `DSH_HOME` exists and is the only home override:
  `<harness>/packages/util/home-paths/src/index.ts` —
  `DSH_HOME_ENV = 'DSH_HOME'` (line 18), `DSH_HOME_DIR_NAME = '.dsh'`
  (line 12), and `resolveDshHome()` precedence: explicit configured argument
  → `$DSH_HOME` (empty/whitespace treated as unset) → `~/.dsh`
  (lines 87–91). `~` prefixes are expanded (lines 70–74).
- No settings.yaml key selects a profile: the live `~/.dsh/settings.yaml`
  top-level keys are `ui-onboarding, ui-theme, agent-presets, permission,
  agent-default-model, llm-pi-ai, provider-quotas, dsh-better-sidebar` —
  no profile/web/port keys.

### Profile layout and resolution

`<harness>/packages/boot/app-boot/src/profile.ts`:

- Line 5–13 (docblock) and lines 41, 127–134: a profile is a directory
  `$DSH_HOME/profiles/<name>` (`PROFILES_DIR = 'profiles'`) holding a
  `package.json` (manifest `dsh.profile.bundles` — ordered bundle list — and
  `dsh.profile.patchReload`: `'live' | 'startup'`) plus the user's patch
  layer `cordis.patch.yml`. `resolveProfileDir()` (lines 127–134) rejects
  names `''`, containing `/` or `\`, `.`, `..`, and the reserved
  `node_modules`.
- `loadProfile()` (lines 805–844): if `<profile>/package.json` is missing,
  a **shipped template is auto-initialized**; an unknown name throws
  *"profile … does not exist; create it with 'dsh plugin --profile <name> add
  <package>'"* (lines 810–818).
- Shipped templates (lines 137–158): `acp`, `web`
  (bundles `['@deepseek-ai/dsh-base', '@deepseek-ai/dsh-web-app']`,
  `patchReload: 'live'`), `headless`, `sdk`, `sdk-minimal`. Custom profiles
  default to `dsh-base` only (line 166).
- Side effect to know: every profile load rewrites
  `<profile>/cordis.yml` with a constant empty-root document
  (`prepareProfile`, `<harness>/apps/cli/src/profile-boot.ts` lines 80–84 and
  118–122). Verified: the live `~/.dsh/profiles/web/cordis.yml` equals that
  constant (`# dsh profile root — an empty entry list…` + `[]`). So
  `dsh … --dump-config` is *boot-free but not write-free* on `$DSH_HOME`
  (it rewrites cordis.yml and may auto-create a shipped profile dir).
- The launcher also stacks a home-level user layer
  `$DSH_HOME/cordis.patch.yml` over every profile
  (`apps/cli/src/profile-boot.ts` lines 63–71); none exists on this machine
  today (`ls ~/.dsh` shows no `cordis.patch.yml`).

### `~/.dsh` layout observed (ls)

```
.agent-presets  .anonymous-user-id  .credentials.yaml  .env  AGENTS.md
attachments  desktop  profiles  sessions  settings.yaml
settings.yaml.backup-*  skills  storages  workspace
```

`profiles/` contains `default/`, `headless/`, `web/`, and the shared
`node_modules/` (the installation dependency closure mounted under all
profiles; profile.ts docblock lines 17–22 and `healProfilesModuleFallback`,
lines 569–586 — note it takes a `withFileLock` on that modules dir, which
creates a transient sibling `<dir>.lock` file recording the holder PID).

The live `web` profile adds user plugins on top of the shipped template:
`~/.dsh/profiles/web/package.json` `dsh.profile.bundles` =
`[@deepseek-ai/dsh-base, @deepseek-ai/dsh-web-app, owsty-foundation,
dsh-better-sidebar, dsh-context, dshmarket, @ybg/*… ]`.

### Listing available profiles

- There is **no CLI list command**: `apps/cli/src/args.ts` offers only the
  root boot, the `web` alias, `plugin`, and `--dump-config`/`--dump-default-config`
  (per-profile, not an enumeration).
- A picker would have to scan the filesystem: list directories under
  `$DSH_HOME/profiles/` that contain a `package.json` (the exact existence
  test `loadProfile` uses, line 810), excluding `node_modules` and
  `.dsh-module-fallback` (profile-private link dir, line 47). Bootability of
  an arbitrary directory is not checked until `loadProfile` parses the
  manifest and resolves each bundle.

---

## 3. Ready line, port, and auth

### Ready line — verbatim and stream

`<harness>/packages/bundle/web-app/src/index.ts` line 363:

```ts
console.log(`dsh web: ${authenticatedUrl}${lanUrl === undefined ? '' : ` (LAN: ${lanUrl})`}`)
```

- **stdout** (`console.log`), printed **once**, only after the Loader tree
  settles and the server is bound: the row awaits
  `ctx.get('loader')?.await()` before announcing (index.ts lines 344–390;
  the code comments call the line a *readiness signal* that supervisors RPC
  on). A per-root `ANNOUNCED_ROOTS` WeakSet dedupes re-announcements.
- `authenticatedUrl()` is
  `new URL(baseUrl)` with `pathname='/'`, `search=''` and
  `searchParams.set('token', launchToken)` →
  `<harness>/packages/client/connection/src/browser-auth.ts` lines 15
  (`const TOKEN_QUERY = 'token'`) and 223–230.
- Actual line captured from the running instance
  (`~/.local/state/dsh-web/dsh-web.log`, token masked):

  ```
  $ node --import tsx/esm apps/cli/src/bin.ts web
  dsh web: http://127.0.0.1:3080/?token=<MASKED>
  dsh web: opening the default browser; pass --no-open to disable
  (node:94112) ExperimentalWarning: SQLite is an experimental feature …
  ```

  (The `$ …` first line is pnpm's own script echo; the SQLite warning is
  stderr. The ready line itself is stdout.)
- LAN variant: when the server binds all interfaces, the line gains
  ` (LAN: http://<lan-ipv4>:<port>/?token=<t>)` (index.ts lines 356–363).
  Not reachable through the CLI today because `--host 0.0.0.0` is rejected
  (below); it would require a config patch setting the webserver row's host.
- A second stdout line follows when the browser handoff fires:
  `dsh web: opening the default browser; pass --no-open to disable`
  (index.ts line 366). If opening fails, a diagnostic goes to **stderr** and
  the server keeps running (line 369).

### Port and host selection

- Defaults are in the web-app bundle patch
  `<harness>/packages/bundle/web-app/cordis.patch.yml` lines 116–124
  (webserver row):

  ```yaml
  - id: webserver
    name: '@deepseek-ai/dsh-host-webserver'
    inject: [webStartup]
    config:
      host: !!js ctx.webStartup.host ?? '127.0.0.1'
      port: !!js ctx.webStartup.port ?? 3080
  ```

  So: **default 127.0.0.1:3080**, overridable only by the inner app's CLI
  flags or by a patch/config override of this row — **no `PORT` /
  `DSH_WEB_PORT` env var exists** (nothing in the chain reads one; the
  running server's env has no such variable either).
- Flags (inner web app, `<harness>/packages/bundle/web-app/src/startup.ts`
  lines 50–54): `--host <host>`, `--no-open`, `--port <port>`
  (*"pass 0 to let the OS pick a free one"*), `--trusted-host <authority...>`
  (repeatable). Usage: they follow the profile selection —
  `dsh web --no-open --port 8080` or `dsh --profile web --no-open`.
  `--host 0.0.0.0` is **rejected** as a usage error (startup.ts lines
  74–76: *"intentionally not supported yet for safety"*); non-numeric
  `--port` is rejected too (lines 77–79).
- Port `0` → OS-assigned; the **actual** bound port is what the ready line
  prints (the webserver records `(server.address()).port`,
  `<harness>/packages/host/webserver/src/index.ts` lines 149–150, 294–297).
- **Busy port does not fall back.** The boot awaits
  `server.listen(port, host)` with `server.once('error', reject)`
  (webserver `index.ts` lines 292–300) — `EADDRINUSE` rejects the boot, the
  launcher's fail-loud handling reports the failed fiber
  (`apps/cli/src/profile-boot.ts` line 226 `installFailLoud`; webserver
  comment at line 122). No retry, no automatic port bump. A managed-mode
  child that dies immediately after spawn may simply have lost the port.
- Another startup failure mode relevant to spawning from a checkout: the
  frontend dist is checked for staleness at activation and the boot **throws**
  if `dist/index.html` predates its inputs (`assertFreshFrontendDist`,
  web-app `index.ts` lines 287–307; message: *"frontend dist older than its
  inputs; run `pnpm run build` before launch"*).

### Browser suppression (headless spawn)

`--no-open` (startup.ts line 52; sets `openBrowser: false`) is the CLI way.
The handoff is also suppressed automatically when the process was launched
through SSH (`SSH_CONNECTION`/`SSH_TTY` present at launch,
`launchedThroughSsh`, index.ts lines 87–93, 319). Config fallbacks:
`openBrowser` default `true`, `printUrl` default `true`
(index.ts lines 62–67); the bundle patch pins `printUrl: true`
(cordis.patch.yml line 140). **Always pass `--no-open` when spawning the
child from the desktop app**, or the child will open a browser tab.

### Auth model (for navigation)

- The ready-line URL carries a fresh **process token** as the sole auth
  input: opening `/?token=<t>` mints a signed cookie and redirects to clean
  `/` (`browser-auth.ts` lines 232–239; README
  `<harness>/packages/bundle/web-app/README.md` line 37). The signing secret
  is durable per harness home (`initializeSecret`, browser-auth.ts
  lines 210–216; stored under `~/.dsh/.credentials.yaml`).
- The API/WebSocket surface is the single `/api` channel, token/cookie
  authenticated (`<harness>/packages/client/connection/src/api-path.ts`
  line 7: `API_PATH = '/api'`). No unauthenticated endpoints were found.

### Config-dump probes (read-only-ish, boot-free)

`dsh web --dump-config` / `dsh --profile <name> --dump-config` print the
composed patch tree without booting (`apps/cli/src/args.ts` lines 31–37,
92–102). Caveat from §2: they still rewrite `<profile>/cordis.yml` and may
auto-create a shipped profile directory. `--dump-default-config` is the
variant without the user layer. Neither prints the resolved port/token
(those exist only at bind time).

---

## 4. Recognizing a running instance

### Process signature (verified on the live instance)

The server process argv is one of:

```
node --import tsx/esm apps/cli/src/bin.ts web                 # web alias
node --import tsx/esm apps/cli/src/bin.ts --profile <name> …  # other profiles
```

matched with cwd = the harness checkout root. When launched through pnpm
(or the `dsh` wrapper), the parent chain is
`node <…>/bin/pnpm dsh [web|--profile <name>]` → pnpm tool shim → the node
server. `pgrep -f 'apps/cli/src/bin.ts'` is the most precise argv anchor;
the profile name is directly readable from argv (trailing `web` positional
or `--profile <name>`).

### Env readability (`ps eww`) — works, with a privacy caveat

`ps eww -p 94112 -o command` on this machine **does** dump the full
environment of the same-user server process (macOS, SIP permitting for
one's own processes). Verified contents include the checkout pointers
`INIT_CWD` and `PNPM_SCRIPT_SRC_DIR` (both = `<harness>`) and **no**
`DSH_*` variables — but also live secrets (`DEEPSEEK_API_KEY`,
`JIRA_TOKEN`, etc.). Anything the desktop app reads from `ps eww` must be
treated as sensitive and never logged.

### No pidfile / runfile / lockfile under `~/.dsh`

`find ~/.dsh -maxdepth 2 -name '*.pid' -o -name '*.lock' -o -name '*.sock'`
→ zero results. The server writes none. The only transient lock dsh creates
is `<profiles-dir>/node_modules.lock` while `healProfilesModuleFallback`
holds `withFileLock` (profile.ts line 586; `<filename>.lock` sibling
recording the creator PID — `packages/util/atomic-write/README.zh.md`
line 84); it is removed by the holder in `finally`, so it is not a reliable
runfile.

### The user's own supervisor: `~/.local/bin/dsh-web`

A user-authored bash script (not part of the harness) already implements a
mini version of managed mode; its conventions are useful precedent:

- State: `~/.local/state/dsh-web/dsh-web.pid` and `dsh-web.log`.
- The pidfile records the **pnpm shim PID** (94087), not the node server
  (94112); the script kills the whole descendant tree on stop.
- It waits for the ready line by grepping the log for
  `http://[0-9A-Za-z.:/?=_&-]*` (line 25) — i.e. it scrapes the same
  `dsh web: <url>?token=…` line, and its comments already document
  `--no-open` / `--port` pass-through (lines 6–7, 23–24).

### Port probing

The live listener is findable with
`lsof -nP -iTCP:3080 -sTCP:LISTEN` → `node 94112`. But the port is not
fixed (the user may run `--port`/`--port 0`, or another port via a config
override), so port probing alone cannot identify a dsh instance or its
profile; combine an argv match with the port owner PID.

### Runtime exposure of the profile

- No plain HTTP endpoint exposes the profile (or any server metadata):
  the only registered API surface is the authenticated `/api` RPC channel;
  the `plugin-inventory` row (`@deepseek-ai/dsh-host-plugin-inventory`,
  web-app cordis.patch.yml lines 86–88) exposes **Loader entries** to
  trusted client RPCs — not the profile name. Nothing like `/health` exists.
- Practical identification of a running instance's profile: read it from
  the process argv (`web` positional / `--profile <name>`), with `$DSH_HOME`
  (via `ps eww` env if non-default) to know which home's profiles dir
  applies.
- For an instance the desktop app spawned itself, the ready line is the
  authoritative URL+token source — no detection needed.

---

## Open questions

1. **`~/.dsh/desktop/`** — an empty directory exists (created 2026-09-01)
   but no harness package references a `desktop` home subdir (grep over
   `packages/util`, `packages/boot`, `apps/*/src` → zero hits). Origin
   unknown; possibly an artifact of earlier dsh-desktop experiments. If the
   desktop shell wants state under the harness home, this path is free but
   has no existing convention.
2. **Multi-instance semantics** — nothing prevents two `dsh web` processes
   on different ports with different profiles simultaneously (no lock, no
   singleton). How the desktop app should scope "the" instance (per profile?
   per port?) is a design decision, not a discovered fact.
3. **Published-install shape** — `@deepseek-ai/dsh` declares
   `bin: { dsh: lib/bin.js }`, so an npm-installed dsh would have a real
   global binary with different argv (`…/lib/bin.js web`). No such install
   exists on this machine; argv-matching heuristics should tolerate
   `bin.js`/`bin.ts` both.
4. **Ready-line stability** — the exact string `dsh web: ` is hardcoded in
   `packages/bundle/web-app/src/index.ts` line 363 (and asserted in its
   tests, lines 147–152). It is not a documented, versioned contract;
   a parser should treat it as best-effort.
5. Whether `pnpm dsh …` output (the `$ …` script echo line pnpm prints
   first) is guaranteed on stdout for all pnpm versions is unverified; the
   desktop parser should scan all merged child output for the ready line
   rather than assuming line 1.

---

## Appendix: captured probe outputs

`pnpm dsh --version` (from `<harness>`, cwd = checkout root):

```
$ node --import tsx/esm apps/cli/src/bin.ts --version
0.1.2-alpha.3
```

`pnpm dsh --help` (launcher only; no boot):

```
Usage: dsh [options] [command] [args...]

dsh: boot a DeepSeek Harness profile — an ordered stack of plugin-bundle patch
layers under your own overrides.

Arguments:
  args                        arguments for the booted profile's app (see: dsh
                              --profile <name> --help)

Options:
  -V, --version               output the version number
  --profile <name>            the profile under $DSH_HOME/profiles to boot
  --patch <path>              extra patch-list overlay applied after the profile
                              layer (repeatable)
  --dump-config               print the composed profile tree and exit
  --dump-default-config       print the profile tree without its user layer or
                              --patch overlays and exit

Commands:
  web [options] [args...]     boot the web profile (alias of --profile web); the
                              web app's own flags follow
  plugin [options] [args...]  manage a profile's plugins by forwarding the
                              remaining arguments to pnpm in the profile
                              directory

Examples:
  dsh --profile web                          boot the web profile (same as: dsh web)
  ...
```

The inner web app's own flags (from `startup.ts` lines 50–60; `dsh web
--help` prints these without binding a server — web-app cordis.patch.yml
line 12 comment):

```
--host <host>                       bind host
--no-open                           do not open the Web UI in the default browser
--port <port>                       listen port; pass 0 to let the OS pick a free one
--trusted-host <authority...>       extra authority the /api browser-trust fence accepts
                                    (host or host:port; repeatable)
```

`dsh-web.log` (ready-line evidence, token masked) — see §3.

`lsof -nP -i :3080` (listener evidence): `node 94112 … TCP 127.0.0.1:3080 (LISTEN)`.
