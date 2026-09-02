# AGENTS.md

## Agent skills

### Issue tracker

Issues live in a local bd (beads) database in this repo; drive it with the `bd` CLI (`bd prime` is the command reference). See `docs/agents/issue-tracker.md`.

### Persistent memory

In bd, via `bd remember` / `bd recall` — auto-injected at `bd prime` time, so present in every session. Reach for `bd remember "<insight>"` for anything worth keeping across sessions. See *Memory* in `docs/agents/issue-tracker.md`.

### Domain docs

Single-context layout (root `CONTEXT.md` + `docs/adr/`). See `docs/agents/domain.md`.
