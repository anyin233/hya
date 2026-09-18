# Context maturation: compaction ladder, handle URLs, trustworthy token counts

## Goal

Bring hya's context management up to the standard set by the coding agents it is
measured against (opencode, pi-coding-agent, oh-my-pi). Three graded tiers, in
dependency order:

1. **Compaction** — a fixed escalation ladder of reduction methods driven by a
   structured, anchored summarization prompt, rather than one all-or-nothing
   summarizer call.
2. **Tool output persistence** — large tool output moves to durable storage and
   leaves a retrievable handle in the transcript, addressed by an internal URL
   family (`artifact://`, `skill://`, `local://`) with a user post-processing
   hook on the artifact path.
3. **Token accounting** — believe a provider's reported usage only while it
   stays believable; otherwise count locally with a calibrated tokenizer. The
   behaviour is selectable.

The tiers are ordered by dependency, not by value: the ladder's cheapest rung is
only non-destructive because tier 2 gives it somewhere to put the bytes, and
every threshold in tier 1 is only as good as the count tier 3 supplies.

## Problem

### P1 — Compaction was one lossy step, and it silently did nothing

The turn loop had two reduction paths and no ladder between them. Tool-output
eviction ran first and **destroyed** the bodies: the transcript kept
`[tool output evicted … re-run the tool if you still need it]` and the only
recovery was to pay for the tool again. If eviction was not enough, one
summarizer call folded the prefix into prose.

Worse, a native provider compact that *succeeded* but left the transcript still
over threshold ended the sequence, so the request went out over the window it
was trying to fit. An `else` branch below the threshold check called
`compact_with` on every under-threshold turn, whose result was never persisted —
work that could only cost latency, never help.

### P2 — Summaries were built from the wrong input and cut off mid-structure

`render_for_summary` rendered `Part::Text` only. In a coding session that is the
narration and none of the work: every command, path, diff and result was hidden
from the summarizer, which is why compacted sessions read as descriptions of
intent rather than resumable state.

The summarizer's output was capped at a hard-coded 1024 tokens, and the prompt
asked for no particular structure. `builtin_agents/prompts/compaction.md` already
told the model to "follow the exact output structure requested by the user
prompt" — but no user prompt ever requested one. Compaction was also memoryless:
each pass re-summarized the previous pass's prose, so detail decayed
geometrically across a long session.

### P3 — Summaries were dropped entirely on Anthropic routes

Summaries are persisted as `Message::System` behind a marker. The Anthropic
encoder discarded every `Message::System` while building its request. On any
Anthropic route, compaction therefore cost a summarizer call and then threw the
result away — the transcript was cut at the marker and the summary that was
supposed to replace it never arrived.

### P4 — `/compact` grew the context it was asked to shrink

`compact_context` wrote the marker; `summarize_session`, the actual backend for
the TUI's `/compact`, injected `"Summary of earlier conversation:\n…"` instead.
Without the marker `compacted_messages` finds no cut point, so the full history
stayed *and* the summary was appended to it.

### P5 — The token count was a character heuristic, believed unconditionally

Occupancy came from `chars / 4` when the provider reported nothing, and from the
provider's figure whenever one was present — with no check that the figure
described the prompt. Routes that report a delta, a post-cache number, or
nothing at all drove the thresholds directly. `chars / 4` under-counts CJK prose
by up to a third and base64-like tool payloads by more than half, which is
exactly the content that fills a window.

### P6 — Truncated tool output was unrecoverable

`cap_tool_output` cut results to 5000 chars and kept the tail. Whatever it
dropped was gone. `bash` alone spilled to disk, through a mechanism nothing else
could reach and the model could not address.

## Domain model

| Term | Definition |
| --- | --- |
| **Rung** | One reduction method on the escalation ladder. |
| **Ladder** | The fixed order rungs are attempted in, cheapest and most recoverable first. |
| **Spill** | Moving a payload to durable storage and leaving a handle in its place. |
| **Handle** | An internal URL naming an agent-owned resource: `scheme://path[?projection]`. |
| **Projection** | A slice of a resolved body, requested in the handle's query string. |
| **Hook** | User post-processing applied to an artifact body **on retrieval**. |
| **Anchored summary** | The summary a session already carries, which the next compaction updates rather than re-derives. |
| **Occupancy** | Tokens the transcript is believed to consume of the window. |
| **Provider-anchored count** | A reported prompt size plus a local estimate of what was appended after it. |

## Requirements

### Tier 1 — Compaction

#### R1 — Fixed escalation ladder

`CompactionRung::LADDER` is walked in order and stops at the first rung that
brings the transcript under the threshold:

1. `SpillToolOutputs` — move stale tool-output bodies to an artifact, leaving a
   handle. Every call, input, and reasoning step survives and the body stays
   retrievable.
2. `ProviderCompact` — ask the route to fold its own window.
3. `Summarize` — fold the transcript prefix into a structured summary.

The order encodes **how much the model loses**, not a preference, so it is fixed
in code rather than exposed as configuration. Escalation is symmetric: a rung
that ran and left the transcript still over threshold must fall through to the
next one.

#### R2 — Spilling is a move, not a drop

`evict_stale_tool_outputs` takes an optional `EvictionSink`. With a sink the body
is written to durable storage and the transcript keeps
`[tool output moved to artifact://… ; read that handle to retrieve it]`. Without
one, the old lossy notice stands. A sink that fails on one body must not fail the
compaction it is serving.

Both notice shapes are recognized on re-entry, so a repeat pass is idempotent and
the reported eviction count reflects real work.

#### R3 — Summaries see the work, not just the narration

`render_for_summary` renders every part: text, reasoning, media descriptors, and
for tools the name, input, and output or error. Any single rendered payload is
capped at `SUMMARY_PART_BUDGET` (2000 bytes) with the dropped byte count stated,
so one 50 KB result cannot crowd fifty turns out of the summarizer's own window.

#### R4 — Structured, anchored summarization

The summarizer prompt carries `SUMMARY_TEMPLATE`: eight named sections (primary
request, key concepts, files touched, errors and fixes, problem solving, pending
tasks, current work, next step), every heading retained even when empty, exact
paths and identifiers preserved.

`SummarizeOptions` gains `previous_summary` and `max_output_tokens`. When the
session already carries a summary, it is passed as the anchor and the model
updates it. `previous_summary` returns `None` for a provider-folded window, whose
body is serialized response items rather than prose.

The output cap is `CompactionConfig::summary_max_tokens` (default 4096), not the
former hard-coded 1024.

#### R5 — Trigger accounts for the reply

`resolved_threshold` returns the tighter of `window × context_fraction` and
`window − reserve_tokens` (default 16384), floored at `MIN_RESOLVED_THRESHOLD`.
A generous fraction on a large window can still leave less headroom than the
reply needs.

#### R6 — Mid-conversation system messages survive every encoder

The Anthropic encoder must carry `Message::System` that appears after the first
user turn. Dropping it silently discards compaction output (**P3**).

#### R7 — `/compact` writes the marker

`summarize_session` injects behind `COMPACT_CONTEXT_MARKER`, the same marker the
automatic path uses. The marker constant is defined once in `hya-provider` and
imported, rather than re-declared per module.

### Tier 2 — Handle URLs

#### R8 — A closed scheme set

`artifact://` (spilled tool output), `skill://` (skill bodies), `local://` (agent
scratch). An unknown scheme is an **error**, not a pass-through, so a typo
surfaces immediately and a foreign URL (`https://`, `file://`) can never be
mistaken for an agent-owned resource.

Only `local://` is writable. `artifact://` is an immutable capture — spilling is
safe precisely *because* the stored bytes stay authoritative — and `skill://` is
a catalog view rather than a file the router owns. Writing either reports
`HandleError::NotWritable`, deliberately distinct from `UnknownScheme`:
reporting a real family as unknown sends the caller looking for a typo.

#### R9 — Ordinary paths are untouched

`read`, `write`, `grep`, and `bash` treat every filesystem path exactly as
before. A tool opts into handles, and `HandleError::NotAHandle` — returned for
anything with no `scheme://` prefix — is the signal to take the original path
route. This is the omp separation: handles index *agent* resources, not the
workspace.

#### R10 — Traversal is rejected at parse time

Empty, absolute, and `..`-bearing paths are refused by `HandleRef` parsing, so no
resolver can forget the check. `ArtifactId` is a character allowlist
(`[A-Za-z0-9_-]`), which makes a separator or encoding trick unable to address
anything but an artifact. `local://` additionally canonicalizes and re-checks
containment, covering the case parsing cannot see: a symlink inside the root
pointing out of it.

#### R11 — Projections

`?lines=N` / `?lines=N-M`, `?head=N`, `?tail=N`, `?grep=pattern`, `?q=.dotted.path`
for JSON bodies. `?head=0` is rejected: it asks for nothing, which is a mistake
rather than an empty result.

#### R12 — Retrieval hooks, never write hooks

`ArtifactHook` runs when an artifact is **retrieved**. The stored bytes stay the
authoritative capture, so a hook that is wrong — or that someone changes their
mind about — cannot have already destroyed the original output. Applicable hooks
chain in registration order, each transforming the previous result, which is what
lets "strip build noise" and "extract the failing assertion" compose. Registered
through `SessionEngine::with_artifact_hooks`.

Order of operations on resolve is: load body → hook chain → projection. A hook
therefore always sees the whole body, and `?head=40` always means the first forty
lines of what the caller actually receives.

#### R13 — Handles are read by the reader that reads files

`HandleRouter::resolve_to_path` returns a path, so `read` presents a handle with
exactly the code that presents a file — permission checks, line numbering,
paging, truncation notices — instead of growing a second implementation that can
drift. An untransformed body resolves to the stored file itself; a hooked or
projected body is materialized once into a content-addressed view keyed by both
handle text and body digest, and reused.

`write` takes the same route through `HandleRouter::write_target`, so a
`local://` payload is created by the code that creates a file, and a scheme with
no writer is not left resolving nothing.

Both tools advertise the namespace in their schema description. A handle the
model is never told about is a handle it will not use, which would leave every
spill pointer in the transcript unreachable and the whole retrieval path dead
weight.

#### R14 — Every capped tool spills

`cap_tool_output_spilling` preserves what the cap drops as an artifact before
truncating, for every tool — builtin, MCP, and plugin — not `bash` alone.

#### R15 — Writes are atomic

Artifacts are staged under a private `0600` temporary name and published by
rename, so a reader never observes a half-written artifact and an interrupted
call leaves no partial file.

### Tier 3 — Token accounting

#### R16 — Calibrated estimation

`CalibratedTokenizer` classifies text into run categories (alphanumeric, numeric,
punctuation, newline/indentation, CJK) with integer weights in sixteenths, fitted
against `o200k_base`. Runs longer than `LONG_RUN` (12) are charged near half a
token per character, the regime of base64 attachments and digests that the prose
rate under-counts fourfold.

Measured against held-out repository corpora: within 15% of true count for 98% of
files, against 58–84% for the `bytes / 4` heuristic it replaces. Refit with
`scripts/tokenizer-calibration.py fit`.

#### R17 — Plausibility gate

In `Auto`, a reported figure is believed only when the route advertises usage
support, actually reported non-zero, **and** the figure is plausible against the
local estimate of the same prefix — ratio within `[0.5, 2.0]`. Outside that band
the provider is reporting a different quantity (a delta, a post-cache figure, a
different unit) and the transcript is estimated outright.

The comparison is like with like: the reported figure describes the prompt up to
and including the message that reported it, not the whole transcript.

#### R18 — Provider-anchored counting

When a reported figure is believed, occupancy is that figure plus a local
estimate of everything appended after it, rather than a re-estimate of the whole
transcript. Occupancy counts `input + cache_read`: cached prompt tokens still
occupy the window and providers disagree on whether `input` already includes
them, so summing can only over-count, which fails safe.

#### R19 — Selectable, and off is a real option

`TokenAccountingMode` is `auto` (default), `provider` (always trust reported
usage, even absent or implausible), or `estimate` (always count locally). Set in
`config.yaml` under `compaction.token_accounting` or via `HYA_TOKEN_ACCOUNTING`;
env wins. An unrecognized value is ignored rather than silently adopting a mode
the user did not ask for.

#### R20 — Post-compaction counts are re-estimated, never re-anchored

A provider's reported usage describes a prompt that no longer exists once a rung
has rewritten the transcript. Trusting the stale anchor would report the
pre-compaction size and escalate the ladder straight past the rung that had just
worked, so `reload_after_compaction` re-estimates.

### Configuration

`compaction:` block in `~/.config/hya/config.yaml`; absent fields keep engine
defaults; per-field env overrides win.

| Field | Env | Default |
| --- | --- | --- |
| `token_threshold` | `HYA_COMPACTION_THRESHOLD` | `100000` |
| `keep_recent` | `HYA_COMPACTION_KEEP_RECENT` | `6` |
| `context_fraction` | `HYA_COMPACTION_CONTEXT_FRACTION` | `0.75` |
| `reserve_tokens` | `HYA_COMPACTION_RESERVE_TOKENS` | `16384` |
| `summary_max_tokens` | `HYA_COMPACTION_SUMMARY_MAX_TOKENS` | `4096` |
| `token_accounting` | `HYA_TOKEN_ACCOUNTING` | `auto` |

## Constraints

- **`hya-tool` may not depend on `hya-core`.** The handle module lives in
  `hya-tool` and reaches session-owned state through planes
  (`ArtifactPlane`, `SkillPlane`), the same indirection `LspPlane` and
  `WebSearchPlane` already use.
- **`HandleRouter::resolve` is synchronous.** Every shipped scheme resolves from
  a file or an in-memory snapshot. A scheme whose data lives behind the async
  store does not fit without either a snapshot plane or an async refactor — see
  *Deferred*.
- **Workspace lints hold**: `missing_docs`, `clippy::unwrap_used`, and
  `clippy::expect_used` are `deny` for library crates.
- **The ladder order is not configuration.** It encodes loss, and a user who
  reorders it gets a worse outcome with no signal.
- **Hooks never run on write.** Capture fidelity is not negotiable.
- **Artifact roots are derived from the call's own working directory** at the
  moment they are needed, so a plane can never address a different session than
  the `ToolCtx` it travelled in.

## Acceptance criteria

- [x] **AC1** Ladder walks in order and stops at the first sufficient rung;
      a rung that runs but leaves the transcript over threshold falls through.
      (`crates/hya-core/tests/token_accounting.rs`, `tests/turn_loop.rs`)
- [x] **AC2** Eviction with a sink leaves a resolvable `artifact://` handle;
      without a sink leaves the lossy notice; a repeat pass is idempotent under
      both shapes. (`compaction.rs` unit tests)
- [x] **AC3** `render_for_summary` includes tool calls, inputs, outputs, errors,
      and reasoning, each capped with the dropped byte count stated.
- [x] **AC4** `previous_summary` returns the anchored prose summary and `None`
      for a provider-folded window.
- [x] **AC5** `resolved_threshold` returns the tighter of the fraction and the
      reserve bound.
- [x] **AC6** Mid-conversation system messages survive the Anthropic encoder.
      (commit `2b742b97`)
- [x] **AC7** `/compact` injects behind `COMPACT_CONTEXT_MARKER`.
      (`crates/hya-server/tests/compat_session_summarize_api.rs`)
- [x] **AC8** Unknown scheme errors; no-scheme text returns `NotAHandle`.
      (`crates/hya-tool/tests/handle_ref.rs`)
- [x] **AC9** Absolute, empty, and `..` paths are refused at parse time;
      `local://` refuses a symlink escaping its root.
      (`crates/hya-tool/tests/handle_router.rs`)
- [x] **AC10** Every projection slices as specified; `?head=0` is refused.
- [x] **AC11** Hooks run on retrieval in registration order, before projection;
      stored bytes are unchanged by a hook.
- [x] **AC12** `read` resolves a handle through the same presentation path as a
      file, and an ordinary path is unaffected.
      (`crates/hya-tool/tests/handle_read.rs`)
- [x] **AC13** A capped tool result spills what the cap drops.
      (`crates/hya-tool/tests/output_spill.rs`)
- [x] **AC14** `CalibratedTokenizer` beats `bytes / 4` on CJK and base64 corpora.
- [x] **AC15** An implausible reported figure falls back to estimation; a
      plausible one is anchored and extended.
- [x] **AC16** `provider` and `estimate` modes bypass the gate in both
      directions; config parses and env wins.
      (`crates/hya-app/src/config.rs` unit tests)
- [x] **AC17** `write` creates a `local://` payload under the scratch root and
      the same handle reads it back; `artifact://` and `skill://` targets are
      refused as read-only and the capture is left intact; a projection on a
      write target is refused; a symlink escaping the scratch root is refused.
      (`crates/hya-tool/tests/handle_router.rs`, `tests/handle_read.rs`)
- [x] **AC18** `read` and `write` schema descriptions name the handle namespace
      and the available projections.

## Out of scope

- **Prompt-cache accounting.** pi tracks cache-miss cost; hya has no cost ledger
  to hang it on, and adding one is a billing feature, not a context feature.
- **Context epochs.** opencode persists system context as a versioned baseline
  row. That is a store-schema change whose payoff is cache stability, not
  window headroom.
- **Speculative / idle compaction.** Worth doing, but it changes when the engine
  spends money without being asked, which needs its own decision.
- **Mechanical elision (`shake`).** A fourth, zero-cost rung below spilling.
  Deferred only because the ladder had to exist first.
- **Subagent context isolation and `workpool` scheduling.** Orchestration, not
  context representation.

## Deferred

### `xd://`

Requested by name alongside `skill://`. In oh-my-pi `xd://` is the control plane
for `ast_edit`'s **staged** rewrites: a match set is proposed and then finalized
by writing a reason to `xd://resolve` or `xd://reject`.

hya has no staged-mutation tool. `edit`, `apply_patch`, and `hashline` all apply
in one phase, so there is nothing for `xd://` to resolve or reject. Shipping the
scheme now would mean shipping a URL with no referent.

Landing it means first landing a two-phase edit tool, which is an editing
feature rather than a context-management one and carries its own design
questions (what a proposal is, where it is stored, what happens to one that is
never finalized, how it interacts with concurrent edits). Tracked separately.

### `agent://` and `history://`

The natural next schemes, and the two that would most extend tier 2's value:
passing a large subagent result or an earlier transcript slice **by reference**
is the same win as spilling tool output, applied to the payloads that are
currently inlined whole.

Both are blocked on the synchronous-resolve constraint above: subagent results
and session transcripts live behind the async store, while every shipped scheme
resolves from a file or a snapshot. Landing them requires choosing between a
snapshot plane (cheap, but stale within a turn) and making resolution async
(clean, but touches every call site). That choice is deliberately not being made
under this task.
