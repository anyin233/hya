# 0.37.0

## Configurable provider retry with a zero-event replay window

Route retry is now a budget you can tune instead of a hardcoded triple.
A top-level `provider_retry:` block (`max_attempts`, `backoff_base_ms`,
`backoff_max_ms`) sets the default, per-provider `retry:` blocks override
individual fields, and `HYA_PROVIDER_RETRY_*` environment variables win over
both — wired through `HttpProvider::with_retry` into the existing pre-stream
attempt loop (transport errors, 429, 5xx, one forced auth refresh).

The same budget now also covers the zero-event replay window: when an
established response dies before delivering any event to the consumer
(truncated body, connection reset, idle stall before the first frame), the
whole request is transparently re-issued while budget remains. Link-level
failures only — provider-decided errors (200-with-error-body frames,
malformed payloads, missing terminal frames) surface immediately even at
zero events, and the first delivered event closes the window permanently.
The strict no-replay contract is unchanged and the spec scenario is updated.

## `exec --json` streams the trajectory live

Headless `exec --json` now prints durable envelopes as the engine
broadcasts them instead of dumping a replay after the turn ends: an
initial catch-up pass covers anything persisted before the stream attached,
broadcast lag resyncs from the durable log, and a final tail flush makes
the printed set exactly what `tail-session` replays. An abnormally
terminated run therefore leaves a usable partial trajectory on stdout
before the nonzero exit surfaces. Concurrent writers (turn loop, resident
batches, mailbox commits) can publish out of seq order, so printing
deduplicates by seq rather than assuming ascending arrival.

## The usage ledger is always recorded

Every finished assistant message — root, subagent, or resident actor — now
writes a `token_ledger` row. Provider-reported usage is stored as reported
(`confidence: provider`; prompt side is `input + cache_read`). When the
provider reports nothing, the turn's texts are counted with the model
family's real `tokenizer.json` (GPT, Claude, DeepSeek, GLM, Kimi, and Qwen
initially adapted; resolved lazily from the hya cache, the local HF cache,
or a one-time download, then cached per process; `confidence: hf:<repo>`).
Unmatched models fall back to the structure-aware `CalibratedTokenizer`
estimate (`confidence: estimated`). Rows also carry the `provider` and
`model` columns.

## `^parent` resolves the registration DM channel

A subagent's `send ^parent` (or an omitted channel) now prefers the DM
channel minted with its direct parent at registration time instead of
rewriting to the parent handle. The handle rewrite bounced with
"`main` is not a teammate you can message" whenever the root registration
was still lazy, and at depth ≥ 2 it could never target the root; the
channel route works in both cases and falls back to the handle path for
older projections. The mailbox rejection hint now names `list_channel`
(the real tool surface) instead of the nonexistent `roster`.

## `--pure`: no external context, MCP, plugins, or skills

The previously inert `--pure` flag is now implemented across `exec`, `run`,
`rpc`, goal mode, `workflow`, and `serve`: no `AGENTS.md`/context-file
discovery (startup-baked in direct modes, per-turn guidance on `serve`),
no MCP servers, no plugins, and no external skill directories — the
embedded builtin skill catalog is the whole skill surface. Websearch keeps
its own configuration and builtin tools are unaffected, giving
reproducible runs whose prompt context is exactly what you passed.

## Team, mail, and session state materialize into SQLite

`session`, `team_run`, `team_member`, `mail`, and `task_board` are now
live write-through projections maintained inside the same transaction as
the event append: `session_created` writes the session row,
`agent_registered` ensures the team run and member rows, `mail_sent` writes
a mail row (`from_ep`/`to_ep`/`kind`/`body_json`), and
`member_spawned`/`subagent_reported` drive task-board status from
`pending` to `done`/`failed`. FK anchors self-heal with placeholder session
rows, and `event_log` remains the single source of truth — reads keep
folding the event log.
