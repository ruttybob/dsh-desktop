# Issue tracker: bd (beads)

Issues and specs for this repo live in a local [bd (beads)](https://github.com/gastownhall/beads) database — a dependency-aware issue tracker backed by an embedded Dolt SQL store (no server, no external service). Use the `bd` CLI for all operations; it emits JSON via `--json`, so an agent session running in this repo drives it directly (treat it like `gh`). `bd prime` is the single source of truth for operational commands — run it for the full, up-to-date reference; this doc records only this repo's conventions and the mappings the engineering skills read.

> **Recorded values** (set at setup):
>
> - Prefix: `dsh` — issue identifiers look like `dsh-<hash>` (`dsh-a3f2dd`).
> - Visibility: `stealth` — `.beads/` is kept out of git via `.git/info/exclude`; local-only.
> - Sync remote: _none_ (no cross-machine sync).

## Model

One layer: the repo's `.beads/` directory holds an embedded Dolt database. There is **no workspace/project split** and **no per-feature project** — every issue in this repo shares one flat space, scoped only by labels, parent-child links, and epics. Each issue gets a prefix-wide identifier `dsh-<hash>` and carries a **status** (lifecycle), **priority** (0–4), **assignee**, **labels**, and **dependency edges** (bd's first-class feature).

## Init (run once at setup)

```bash
bd init --prefix dsh --role maintainer --stealth --skip-agents --skip-hooks
```

- `--prefix dsh` — short lowercase key prefixing every issue id.
- `--role maintainer` — you own this tracker (use `contributor` for an OSS fork).
- `--stealth` — keep `.beads/` out of git via `.git/info/exclude` (local-only; no cross-machine sync). This repo uses stealth.
- `--skip-agents` — the agent-skills setup writes its own lean AGENTS.md pointer (see the `## Agent skills` block); skip bd's built-in AGENTS.md/Claude/Codex generation to avoid a duplicate source of truth.
- `--skip-hooks` — no git hooks (the flow does not depend on them).

Confirm: `bd where` (active workspace) and `bd context` (backend identity). Idempotent re-runs: add `--init-if-missing`.

## Memory (persistent, prime-injected)

bd holds persistent project memory in the same database — insights that survive across sessions and **auto-inject at `bd prime` time**, so every session has them without manual loading. This is the replacement for ad-hoc `NOTES.md` / memory files.

- **Store**: `bd remember "<insight>"` (key auto-generated from content), or `bd remember "<insight>" --key <slug>` for a stable, recallable key. Re-`remember` with the same `--key` updates in place.
- **Recall**: `bd recall <key>` (a bare existing key passed to `bd remember` also recalls it).
- **List / search**: `bd memories` / `bd memories "<phrase>"`.
- **Forget**: `bd forget <key>`.

Use it for the unwritten conventions, gotchas, and reasons-behind-choices a new session needs but no config confesses. Do **not** use it for issue state (issues are for that) or per-session scratch (`bd note` on an issue is).

## Labels (implicit — no `create` step)

bd labels spring into existence on first use (`bd update --add-label X` / `bd create --labels X`) — there is no `label create` command, so nothing needs pre-creating. This repo defines **no fixed triage vocabulary**; any labels in use are ad hoc.

The `wayfinder:*` labels (`wayfinder:map`, `wayfinder:research`, `wayfinder:prototype`, `wayfinder:grilling`, `wayfinder:task`) **are** used here — wayfinder runs natively on bd (see *Wayfinding operations*), so they spring into existence like any other label, with no creation step.

## Conventions

- **Create**: `bd create "Title" -d "desc" -t task -p 2 [--assignee X] [--labels a,b] [--deps type:id,…]`. Types: `bug|feature|task|epic|chore|decision`. Priorities: 0 (critical) – 4 (backlog), default 2. `--deps discovered-from:<id>` links auto-discovered follow-up work.
- **Read**: `bd show <id>` (or `bd show <id> --json`, `--include-comments`, `--children`). `bd show --current` shows the active issue.
- **List**: `bd list --status open --json` / `--label X` / `--priority 0` / `--assignee X` / `--all` (incl. closed).
- **Comment** (conversation): `bd comment <id> "text"`. **Note** (persistent field): `bd note <id> "text"`.
- **Labels**: `bd update <id> --add-label X` / `--remove-label X`.
- **Claim**: `bd update <id> --claim` (sets assignee + `in_progress`; idempotent if already yours) — or `bd ready --claim --json` (atomic claim of the next ready issue).
- **Close**: `bd close <id> --reason "…"`. **Reopen**: `bd reopen <id>`.

## Dependencies / blocking (bd's first-class feature)

```bash
bd dep add <issue> <depends-on>         # <depends-on> blocks <issue>
bd dep <blocker> --blocks <blocked>     # same thing, other direction
bd dep list <id>                        # edges of one issue
bd dep tree <id>                        # visualize
bd dep cycles                           # detect circular deps
```

Dependency types: `blocks` (hard gate), `related` (soft link), `parent-child` (epic / subtask), `discovered-from` (auto-created for AI-discovered follow-ups).

The **frontier** is built in: `bd ready` lists open issues with **no active blockers** — claimable now.

## Resolving an issue

bd has a native terminal state:

```bash
bd close <id> --reason "Landed in PR #42, commit abc1234"
```

`closed` means *landed / worked*, recorded in the Dolt history. The close reason is the link to where the work shipped (PR / commit / branch) — bd has no auto-linking to git, so the reason is the record. Closed issues fall out of every active scan (`bd ready`, `bd list --status open`) automatically.

Lifecycle status (`open` → `in_progress` → `closed`, plus operational `blocked` / `deferred`) is independent of labels; this repo applies no standing triage-label scheme on top of it.

## Wayfinding operations

`/wayfinder` consults this section. **Wayfinder runs natively on bd** — bd's first-class dependencies (`bd dep`) and built-in scoped frontier (`bd ready --parent`) are a cleaner substrate than body conventions. A **map** is one bd issue; its **decision tickets** are child issues.

- **Create the map**: `bd create "<Destination>" -t epic -d "<map body>" --labels wayfinder:map`. Body uses the map template (`## Destination`, `## Notes`, `## Not yet specified`, `## Out of scope`). The map is an index: a decision lives in its ticket; the map only gists + links. `## Decisions so far` is **not** in the body — it is the map's **notes** field, appended one line per resolution (below).
- **Create a ticket**: `bd create "<Question title>" --parent <map-id> --labels wayfinder:<type> -d "<question body>"` (multiline body via `--stdin` / heredoc). Types: `research` / `prototype` / `grilling` / `task`. The `--parent` edge is the containment; the bd issue id is the ticket's identity.
- **Wire blocking** (second pass, after tickets have ids): `bd dep add <child> <blocker>` (type `blocks`, the default). A ticket is unblocked when every blocker is `closed`.
- **Frontier query**: `bd ready --parent <map-id>` — open, unblocked descendants of the map. Claimed tickets fall out automatically (`in_progress`). Pick the first by priority, or the one the user named.
- **Claim**: `bd update <ticket-id> --claim` (sets assignee + `in_progress`; idempotent if already yours) — the session's first write, before any work. Do not bare-`--assignee` (that leaves status `open` and the ticket still in the frontier).
- **Resolve**: `bd comment <ticket-id> "<answer>"` (the resolution), then `bd close <ticket-id>`, then append the index line to the map's Decisions-so-far: `bd note <map-id> "- [<ticket title>](link) — <one-line gist>"`. A **research** ticket is resolved by a `/research` subagent whose findings land on a throwaway branch — link the branch from the ticket as its asset (`bd note <ticket-id> "branch: …"`).
- **Refer by name**: cite tickets by **title**, never a bare id — the id rides inside the name.
- **Concurrency**: bd is a single database with atomic `--claim`; other sessions may edit concurrently. Re-`bd show` the map / re-run the frontier before acting.

## Pull requests as a request surface

No. bd is an internal tracker with no PR object.

## When a skill says "publish to the issue tracker"

```bash
bd create "Title" -d "…" -t task -p 2
```

Add `--deps` / `--assignee` as needed.

## When a skill says "fetch the relevant ticket"

`bd show <id>` (or `bd show <id> --json --include-comments` for full context).
