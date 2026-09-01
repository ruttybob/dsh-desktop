/**
 * Host boot smoke test (keyless): spawn the host entry, wait for the
 * `dsh web: http://127.0.0.1:<port>?token=...` URL line the web-app bundle prints
 * once the loader tree settles, run the token->cookie->index auth dance (or the
 * legacy direct-200 path for bundles without token auth), assert the shell HTML
 * is served, then terminate the host and exit 0.
 *
 * Prefers the assembled bundle (`src-tauri/resources/host/`, the exact shipped
 * artifact) when present, falling back to the checkout's `host/` directory.
 * Set `DSH_DESKTOP_REQUIRE_BUNDLE=1` to forbid the fallback: without a
 * complete bundle the test fails instead of silently exercising the checkout.
 *
 * Sidecar output echoes into this log line-by-line and token-redacted — the
 * ready URL carries the bearer launch token, so raw chunks are never written.
 *
 * Usage: node scripts/smoke-host.mjs   (from the repo root)
 * Env:   DSH_DESKTOP_SMOKE_TIMEOUT_MS  default 120000
 *        DSH_DESKTOP_REQUIRE_BUNDLE    "1" forbids the checkout fallback
 */
import { existsSync, mkdtempSync } from 'node:fs'
import { spawn } from 'node:child_process'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('..', import.meta.url))
const timeoutMs = Number(process.env.DSH_DESKTOP_SMOKE_TIMEOUT_MS ?? 120_000)

const bundled = join(root, 'src-tauri', 'resources', 'host')
const useBundled = existsSync(join(bundled, 'node_modules')) && existsSync(join(bundled, 'main.mjs'))
// Runtime binary path inside node/: node.exe on Windows, bin/node elsewhere.
const nodeRuntimeRel = process.platform === 'win32' ? join('node.exe') : join('bin', 'node')
const nodeBin = useBundled ? join(bundled, 'node', nodeRuntimeRel) : process.execPath
const hostEntry = useBundled ? join(bundled, 'main.mjs') : join(root, 'host', 'main.mjs')

// The checkout fallback exists for fast local loops; CI (and anyone asserting
// the shipped artifact) forbids it so a stale or missing bundle fails loudly.
const requireBundle = process.env.DSH_DESKTOP_REQUIRE_BUNDLE === '1'
if (requireBundle && !useBundled) {
  console.error('[smoke] DSH_DESKTOP_REQUIRE_BUNDLE=1 but src-tauri/resources/host is incomplete — run `npm run host:bundle`')
  process.exit(1)
}

// Capture the whole first whitespace token after the `dsh web:` marker, so a
// query string (`?token=...`) is preserved rather than truncated at the port.
// Keep in sync with `parse_url_line` in src-tauri/src/host.rs — the two define
// the same ready-line grammar in different languages.
const URL_LINE = /dsh web: (https?:\/\/127\.0\.0\.1:\d+\S*)/

// Hermetic boot: an isolated DSH_HOME keeps user-installed plugins (built
// against whatever harness the user runs) out of the loader tree, so the smoke
// exercises only the pinned bundled harness.
const isolatedHome = mkdtempSync(join(tmpdir(), 'dsh-desktop-smoke-'))

console.log(`[smoke] booting ${nodeBin} ${hostEntry} (${useBundled ? 'bundled' : 'checkout'} host, timeout ${timeoutMs} ms)`)
const child = spawn(nodeBin, [hostEntry], {
  cwd: root,
  stdio: ['ignore', 'pipe', 'pipe'],
  env: { ...process.env, DSH_DESKTOP_PORT: '0', DSH_HOME: isolatedHome },
  windowsHide: true,
})

let stdout = ''
let stderr = ''
let url
const deadline = Date.now() + timeoutMs

const timer = setTimeout(() => {
  console.error(`[smoke] timed out after ${timeoutMs} ms`)
  child.kill()
  process.exit(1)
}, timeoutMs)

// Sidecar output echoes line-by-line and token-redacted: a chunk boundary can
// split a `token=` value, so a trailing partial line waits for its newline and
// raw chunks are never written.
let stdoutForwarded = 0
let stderrForwarded = 0

child.stdout.on('data', (chunk) => {
  stdout += chunk.toString()
  stdoutForwarded = forwardRedacted(stdout, stdoutForwarded, process.stdout)
  const match = URL_LINE.exec(stdout)
  if (match !== null && url === undefined) {
    url = match[1]
    console.log(`[smoke] host URL: ${redactUrlForLog(url)}`)
  }
})
child.stderr.on('data', (chunk) => {
  stderr += chunk.toString()
  stderrForwarded = forwardRedacted(stderr, stderrForwarded, process.stderr)
})
child.on('exit', (code, signal) => {
  if (url === undefined && Date.now() < deadline) {
    console.error(`[smoke] host exited before printing the URL (code=${code} signal=${signal})`)
    process.exit(1)
  }
})

async function waitForUrl() {
  while (url === undefined) {
    if (Date.now() > deadline) return false
    await new Promise((resolve) => setTimeout(resolve, 250))
  }
  return true
}

if (!(await waitForUrl())) {
  console.error('[smoke] never saw the `dsh web:` URL line')
  child.kill()
  process.exit(1)
}

// The URL line prints before every sibling route (the /api owner) has mounted,
// so probe for the index page with retries.
//
// Auth dance (token-bearing bundles):
//   GET /?token=<launchToken> -> 303 Location: / + Set-Cookie: dsh-auth-...
//   GET / with the valid cookie -> 200 index.html
//   anything else -> 401
// A direct 200 + shell marker on the tokenized fetch is the legacy path
// (bundle without token auth) and is accepted as-is.
let lastError
for (let attempt = 0; attempt < 40; attempt += 1) {
  try {
    const body = await fetchIndex(url)
    if (body !== null) {
      console.log('[smoke] index served: shell HTML present')
      clearTimeout(timer)
      child.kill()
      await new Promise((resolve) => setTimeout(resolve, 500))
      console.log('[smoke] OK')
      process.exit(0)
    }
    lastError = 'unexpected body'
  } catch (error) {
    lastError = error instanceof Error ? error.message : String(error)
  }
  await new Promise((resolve) => setTimeout(resolve, 1000))
}
console.error(`[smoke] index probe failed: ${lastError}`)
child.kill()
process.exit(1)

// Fetch `location` (a tokenized URL if present) without following redirects, and
// if the host answers with a 303 auth redirect, replay the Set-Cookie onto the
// origin root. Returns the index body if the shell marker is served, else null.
async function fetchIndex(tokenizedUrl) {
  // The caller only checks whether the shell index was served, so any
  // non-null value means success.
  const looksLikeShell = (ok, body) => (ok && body.includes('<div id="root">') ? body : null)
  const res = await fetch(tokenizedUrl, { redirect: 'manual' })
  if (res.status === 303) {
    const cookie = getFirstCookie(res)
    if (cookie === null) throw new Error('303 without a Set-Cookie header')
    const origin = new URL(tokenizedUrl).origin
    const index = await fetch(`${origin}/`, {
      headers: { Cookie: cookie },
      redirect: 'manual',
    })
    return looksLikeShell(index.ok, await index.text())
  }
  return looksLikeShell(res.ok, await res.text())
}

// Mask a launch token in a URL for console output; the unredacted value is
// still used for the actual network requests.
function redactUrlForLog(value) {
  return value.replace(/token=[^&\s]*/, 'token=[redacted]')
}

// Forward every complete line of `full` past offset `forwarded`, token-
// redacted. Returns the new offset; a trailing partial line stays buffered.
function forwardRedacted(full, forwarded, stream) {
  const pending = full.slice(forwarded)
  const cut = pending.lastIndexOf('\n')
  if (cut === -1) return forwarded
  stream.write(redactTokenInLine(pending.slice(0, cut + 1)))
  return forwarded + cut + 1
}

// Mirror `redact_token` in src-tauri/src/host.rs: mask every `token=` value
// up to the next query delimiter, whitespace, or end of line.
function redactTokenInLine(text) {
  return text.replace(/token=[^&\s]*/g, 'token=[redacted]')
}

function getFirstCookie(res) {
  const setCookie = res.headers.getSetCookie
  if (typeof setCookie === 'function') {
    const list = setCookie.call(res.headers)
    return list.length > 0 ? list[0] : null
  }
  // Fallback for environments without getSetCookie().
  for (const [name, value] of res.headers) {
    if (name.toLowerCase() === 'set-cookie') return value
  }
  return null
}
