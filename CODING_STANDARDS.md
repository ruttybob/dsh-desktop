# Coding standards

Rules the reviewer enforces on top of tests and CI. Small by design: every rule here exists because a bug of this exact class slipped past green tests.

## Transport and auth fixes need live evidence

A change to the shell's network path — the cookie proxy, request-head rewriting, ready-line handling, navigation, host process lifecycle — is accepted only with evidence from a running app: the relevant `[host]` / `[proxy]` log lines from a launch, showing the fix's effect (response statuses, established connections, the `auth path verified` line).

Unit tests on head rewriting or parsing validate strings, not integration: the launch-401 bugs all passed green tests while the live flow returned 400/401/403. `npm run smoke` validates the server side only; it bypasses the WebView. The greppable proof to look for in a launch log:

- `[host] auth path verified through proxy (200)` — the whole chain works;
- `[proxy]` request failures, `[host]` warnings — the chain is broken, do not merge.
