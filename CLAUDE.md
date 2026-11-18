# CLAUDE.md — hya

> Project overview, component map, change guidance, and verification live in
> `AGENTS.md`. Claude Code does not load that file by itself: read it before
> changing code. Its Verification section sets which checks to run for the
> components a change touches, and its documentation, TUI, and release rules
> apply to feature work.

## Commit and push

- Commit only when the user explicitly asks: one commit per atomic change,
  staging only that change's files.
- Push only when the user authorizes it.
- Use one-line semantic commit messages with no agent or AI attribution.
- Report failed or unavailable checks; they do not block a commit or push the
  user asked for.

## Agent skills

### Issue tracker

Issues are tracked in GitHub Issues for `chrono-foundry/hya`; external PRs are not a triage surface. See `docs/agents/issue-tracker.md`.

### Triage labels

Triage uses the canonical label vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, and `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Domain docs use a single-context layout: root `CONTEXT.md` plus root `docs/adr/`. See `docs/agents/domain.md`.
