<div align="center">

# dsh-desktop

**DeepSeek Harness desktop client** — a Tauri native window wrapper around [deepseek-harness](https://github.com/deepseek-ai/deepseek-harness)

[![Release](https://img.shields.io/github/v/release/kyorakuyk/dsh-desktop?label=release)](https://github.com/kyorakuyk/dsh-desktop/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/kyorakuyk/dsh-desktop/ci.yml?label=ci)](https://github.com/kyorakuyk/dsh-desktop/actions/workflows/ci.yml)
[![License](https://img.shields.io/github/license/kyorakuyk/dsh-desktop)](LICENSE)
[![Topic](https://img.shields.io/badge/topic-dsh--plugin-blue)](#)

Windows · macOS · Linux — works out of the box, no Node.js installation required

[Русская версия](README.ru.md)

</div>

---

## What is this

dsh-desktop puts the DeepSeek Harness Web GUI (`dsh web`) inside a native desktop window:

- A **Tauri 2 (Rust) shell** handles the window, host process lifecycle, and packaging;
- An **embedded Node.js host (sidecar)** runs [`@deepseek-ai/dsh`](https://www.npmjs.com/package/@deepseek-ai/dsh) published on npm (`dsh web --port 0`);
- **The WebView loads `http://127.0.0.1:<random port>` directly**, fully reusing the harness's
  `window.__DSH_BOOT__` injection, plugin bundle serving, `/api` JSON-RPC, and WebSocket event stream — **without changing a single line of deepseek-harness**.

The installer bundles the Node runtime and all harness dependencies, so no runtime needs to be preinstalled on the user's machine.

## ✨ Features

| Capability | Description |
| --- | --- |
| 🖥️ Native window | Windows WebView2 / macOS WKWebView / Linux WebKitGTK |
| 📦 Zero-dependency install | The installer ships a Node runtime + the full `@deepseek-ai/dsh` stack |
| 🔌 Full harness capabilities | Sessions, tool calls, plugins, model config, workspace — identical to `dsh web` |
| 🚀 Ready on launch | Splash screen → host assembly (~30 plugin lines) → straight into the GUI |
| 🤖 Model config | Configure your API key in the GUI under Settings → Model |
| 🔄 Cross-platform releases | GitHub Actions builds installers for all three platforms on tag and publishes to Releases |
| 📋 Logging | Host and shell logs are written through a unified pipeline (tauri-plugin-log) |

## 📥 Download & install

Download the installer for your platform from [Releases](https://github.com/kyorakuyk/dsh-desktop/releases/latest):

| Platform | File | Notes |
| --- | --- | --- |
| Windows | `dsh-desktop_<version>_x64-setup.exe` | NSIS installer, double-click to install |
| macOS (Apple Silicon) | `dsh-desktop_<version>_aarch64.dmg` | Drag into Applications |
| macOS (Intel) | `dsh-desktop_<version>_x64.dmg` | Same as above |
| Linux | `dsh-desktop_<version>_amd64.deb` | Debian / Ubuntu: `sudo dpkg -i` |
| Linux | `dsh-desktop-<version>-1.x86_64.rpm` | Fedora / RHEL: `sudo rpm -i` |

> **First run**: wait a few seconds after launch (host assembly), then set your API key under Settings → Model to start chatting.
> Data is stored in `~/.dsh` by default (sessions, settings, profile; shared with the dsh CLI).

## 🏗️ Architecture

```
┌─────────────────────────────────────────────┐
│ Tauri window (WebView2 / WKWebView)         │
│  └─ loads http://127.0.0.1:<random port>    │
├─────────────────────────────────────────────┤
│ Rust shell (src-tauri)                      │
│  ├─ spawns/monitors the sidecar, parses the │
│  │   `dsh web:` URL line                    │
│  ├─ working dir ~/.dsh/workspace            │
│  ├─ terminates the host process on exit     │
│  └─ logging (tauri-plugin-log)              │
├─────────────────────────────────────────────┤
│ Sidecar: bundled Node runtime + @deepseek-  │
│ ai/dsh                                      │
│  └─ node host/main.mjs → dsh web --port 0   │
│     ├─ __DSH_BOOT__ injection (dsh-client-  │
│     │   modules scans dsh.client on node)   │
│     ├─ /plugins/<id>/client.js plugin bundle│
│     ├─ /api JSON-RPC gateway                │
│     └─ /api/events.mux|host WebSocket stream│
└─────────────────────────────────────────────┘
```

### Startup sequence

1. The app starts and the window shows the **splash screen** (loading UI while the host assembles);
2. The Rust shell spawns the bundled `node.exe` → `host/main.mjs` → `dsh web --port 0` runs in that process;
3. `dsh web` assembles the Cordis plugin tree (the `@deepseek-ai/dsh-base` + `@deepseek-ai/dsh-web-app`
   bundles, ~30 plugin lines), and the webserver binds to an **OS-assigned random port**;
4. Once the Loader tree settles, the web-app bundle prints `dsh web: http://127.0.0.1:<port>`;
5. The Rust shell parses that line → the WebView navigates to the URL → the harness GUI fully loads
   (boot manifest, plugin bundle prefetch, `/api` connection, WebSocket event stream);
6. The user closes the window → the app exits → the host process is terminated (session data has already been persisted to disk).

### Engineering notes

- **Zero port conflicts**: `--port 0` lets the OS assign the port; Rust parses the real address from stdout;
- **Windows `\\?\` prefix**: Tauri resource paths carry the extended-length prefix, which the Node loader cannot resolve;
  it is stripped before being passed to the child process (`strip_verbatim_prefix`);
- **Lean bundle**: pnpm installs with a `hoisted` layout (no symlinks after copying); the `.pnpm` store (~250 MB)
  and npm/npx/corepack from the Node distribution (~30 MB) are excluded at packaging time;
- **Keyless smoke test**: `npm run smoke` verifies the whole host chain in both CI and locally
  (start → URL line → index 200 + shell HTML), no API key needed.

## 📁 Repository layout

```
dsh-desktop/
├── src-tauri/            # Rust shell (window, sidecar lifecycle, packaging config, icons)
│   ├── src/host.rs       # sidecar spawn / URL parsing / process termination
│   ├── src/lib.rs        # Tauri app assembly and exit hooks
│   └── resources/        # build-time assembled artifacts (gitignored):
│                         #   host/{main.mjs, node/, node_modules/}
├── host/                 # Node host entry
│   ├── main.mjs          # runs dsh web in-process (argv redirection + file:// import)
│   └── pnpm-workspace.yaml  # pnpm 11 settings (hoisted / allowBuilds / publish age gate)
├── scripts/
│   ├── fetch-node.mjs    # downloads the official Node runtime (v22 LTS, per platform/arch)
│   ├── bundle-host.mjs   # assembles resources/host (excludes .pnpm and npm dist files)
│   └── smoke-host.mjs    # keyless host smoke test (prefers packaged artifacts)
├── ui/                   # splash screen page (pure static, no build step)
├── .github/workflows/
│   ├── release.yml       # tag → tauri-action three-platform build → GitHub Releases
│   └── ci.yml            # PR/push: host smoke + Windows/Linux cargo check
└── package.json          # convenience scripts (see below)
```

## 🛠️ Development

### Prerequisites

| Dependency | Version | Notes |
| --- | --- | --- |
| Node.js | ≥ 22.19 | includes npm |
| pnpm | ≥ 11 | used to install host dependencies |
| Rust toolchain | ≥ 1.77 | cargo / rustc |
| Extra Linux dependencies | — | `libwebkit2gtk-4.1-dev` etc., see the [Tauri docs](https://tauri.app/start/prerequisites/) |

### Quick start

```sh
# 1. Install dependencies and assemble the host resources (Node runtime + @deepseek-ai/dsh and its deps)
npm install
npm run host:install        # internally: pnpm -C host install --prod
npm run host:bundle         # output: src-tauri/resources/host/ (gitignored)

# 2. Run in dev mode (opens the window; the host is spawned by Rust automatically)
npm run tauri dev
```

### Common scripts

| Script | Purpose |
| --- | --- |
| `npm run host:install` | Install host dependencies (`@deepseek-ai/dsh` pinned) |
| `npm run host:fetch-node` | Download/trim the official Node runtime |
| `npm run host:bundle` | Assemble `src-tauri/resources/host/` |
| `npm run smoke` | Keyless host smoke test (prefers packaged artifacts) |
| `npm run tauri dev` | Run in development mode |
| `npm run build` | Production build (bundle host + tauri build) |

### Building installers

```sh
npm run build
# Windows: src-tauri/target/release/bundle/nsis/*.exe
# macOS:   bundle/macos/*.app + bundle/dmg/*.dmg
# Linux:   bundle/deb/*.deb + bundle/rpm/*.rpm (AppImage on hold, see known limitations)
```

## 🚀 Publishing to GitHub

The repo ships with `.github/workflows/release.yml`. Two ways to trigger it:

```sh
# Option 1: push a tag (recommended)
git tag v0.2.0
git push origin v0.2.0

# Option 2: trigger the release workflow manually from the Actions page
```

The workflow builds installers on Windows / macOS (arm64+x64) / Linux and uploads them to GitHub
Releases (as a draft; promote it manually after verifying).

> Optional secrets (only needed to enable auto-update, see below):
> `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.

## 🧪 CI checks (ci.yml)

| Job | What it does |
| --- | --- |
| Host boot smoke | On Ubuntu, installs host dependencies and starts `dsh web`, asserting the URL line + shell HTML |
| cargo check | Compile checks on Windows + Ubuntu (including build.rs resource validation) |

## 🔄 Auto-update (roadmap)

The current build does not compile in `tauri-plugin-updater`. To enable it:

1. `cargo add tauri-plugin-updater` and register it in `src-tauri/src/lib.rs`;
2. Generate a keypair with `npx tauri signer generate -w ~/.tauri/dsh-desktop.key`,
   and put the public key in `tauri.conf.json → plugins.updater.pubkey`;
3. Configure `TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` in the repo Secrets;
4. In `tauri.conf.json`, point `plugins.updater.endpoints` at
   `https://github.com/kyorakuyk/dsh-desktop/releases/latest/download/latest.json`;
5. Push a new tag; `tauri-action` automatically uploads the update manifest and signed artifacts.

## 🔍 Troubleshooting

| Symptom | Fix |
| --- | --- |
| Stuck on the splash screen | Check the app logs to find why the host failed to start |
| Host startup error | Logs: Windows `%LOCALAPPDATA%\com.dsh.desktop\logs\dsh-desktop.log`; macOS/Linux see `~/Library/Logs` and `~/.local/share/com.dsh.desktop/logs` |
| Attach Mode: "dsh web authentication required" | The server requires its launch token. The first connect must use the **whole** URL from the `dsh web: http://…?token=…` line (the token rotates on every server restart, but is needed once — a signed cookie takes over afterwards). The **Change Server…** menu item (⌘/Ctrl+Shift+C) returns to the connect form from any page; the form never remembers a URL whose auth handshake failed |
| Model not responding | Check the API key and model config under Settings → Model |
| A CLI visible in the terminal (e.g. `bd`) is missing in the app | The GUI inherits launchd's four-directory stub `PATH` at startup; the host probes the login shell at startup and restores your `PATH`. If a CLI is still missing, make sure it is on the login shell's `PATH` and that its profile script doesn't block for more than 5 seconds |
| Build says host bundle missing | Run `npm run host:bundle` first (`build.rs` reports this proactively) |

## ⚠️ Known limitations

- First launch takes a few seconds (the host assembles ~30 plugin lines + prefetches frontend bundles); the splash screen shows meanwhile;
- Installer size is large (it bundles the Node runtime and all harness dependencies, on the order of ~100 MB);
- If the host exits abnormally, the window stays on the last page (the exit reason is visible in the logs);
- No Linux AppImage for now: `linuxdeploy` packaging fails in CI (deb/rpm work fine;
  it's an AppImage toolchain issue), to be fixed in a later release;
- Auto-update is not enabled (see the roadmap above).

## 📄 License

[MIT](LICENSE) — same as deepseek-harness.

> **Disclaimer**: this project is a community desktop wrapper, not affiliated with DeepSeek;
> DeepSeek Harness and related trademarks belong to their respective owners.
