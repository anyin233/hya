# yaca — Follow-ups & Deferred Work

Reference for work intentionally left for a future pass. The pi-parity waves
(1–7) and their follow-ups are merged into `main`.

## Deferred (not yet implemented)

- **OAuth interactive login (device code / PKCE browser flow).** The auth
  substrate exists: a token store + `yaca login <provider> <token>` + router
  preference for stored tokens. The remaining piece is the full interactive flow —
  device-authorization request, opening the browser, polling the token endpoint,
  PKCE code exchange, and refresh-token handling — per provider (Anthropic,
  OpenAI-class, Google). Keep it as its own task (it is large on its own). Until
  then, paste a token via `yaca login`.

## Implemented (merged)

- Wave 1 — permission responder (Scoped / ReadOnly / Yolo) + interactive TUI
  approval; `ls` / `find` tools; `edit` ambiguity guard.
- Wave 2 — system-prompt builder + AGENTS.md / context-file discovery.
- Wave 3 — slash commands (`/help` `/model` `/clear` `/new` `/exit` `/sessions`)
  + prompt templates.
- Wave 4 — context compaction (`ModelSummarizer` auto-trigger, env-tunable
  threshold) + SKILL.md skills.
- Wave 5 — native Google (Gemini) provider + auth token store + `yaca login`.
- Wave 6 — session list / branch / resume (`list_sessions`, `yaca sessions`,
  `--db` / `--resume`, TUI session picker).
- Wave 7 — `exec --json` and `yaca rpc` (stdin/stdout JSONL) integration modes.
- Hardening — path-containment resolves symlinks on existing ancestors.
- TUI typed-deny feedback — the permission overlay captures optional rejection
  text and sends it through `Decision::Reject { feedback }`.

## Notes

- This work was developed on `feat/yaca-w1-agent-can-code` (branched from the
  pre-permission baseline) and merged with the concurrent `tui-opencode-parity`
  permission commit; on overlap the broader implementation won, while that
  commit's `Decision::Reject { feedback }` plane and tool-output truncation were
  preserved.
