# Remembered connection: one store, auto-apply, silent Managed remember

> **Superseded (2026-09-01):** the standalone redraw ([ADR-0003](0003-standalone-only-attach-retired.md)) retired Attach Mode together with the mode picker and the remembered connection.

The splash's connection choice (dsh-u3m.2) persists as a single **remembered connection** — the mode plus that mode's payload (Attach: the confirmed server URL; Managed: the chosen profile name) — in one versioned store record (v2, migrated from the v1 attach-only file), and the remembered mode **auto-applies at the next launch**: the mode picker shows only when no remembered path exists or it fails, never on every launch. Managed Mode remembers its profile as soon as chosen, with no opt-in checkbox, because a profile is a setting rather than a one-shot secret; Attach keeps its existing rule of remembering only after a successful auth handshake.

## Considered options

- **Always show the picker**, remembered choice pre-selected — rejected: it taxes every default launch; auto-apply simply extends the existing remembered-server semantics (dsh-df4) to Managed.
- **Separate stores per mode** — rejected: one record makes "reset the connection choice" and schema validation single-file concerns.

## Consequences

- The invariant "no payload, no remember": a one-off attach connect (no "remember" checkbox) writes nothing, so the next picker offers Managed again — there is no half-state where the mode is remembered without a payload.
- Migration v1 → v2 maps an existing remembered URL to `{mode: attach, attach: {url}}`.
- A remembered-but-broken payload degrades to the picker opened on that mode's face, payload ignored.
