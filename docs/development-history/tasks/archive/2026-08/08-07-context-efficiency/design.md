# Design — Context efficiency

Builds on `08-07-context-observability` (branch `feat/context-observability`,
released 0.34.15). Branch: `feat/context-efficiency`, stacked on that.

## 0. Scope correction from research

**E4 as written in `prd.md` is wrong on its central claim.** It says "every
subagent re-walks and re-renders the same `AGENTS.md` chain into its own system
prompt". Subagents do **not** re-walk. `guidance_at`
(`crates/hya-server/src/compat/reference.rs:130`) renders the guidance once per
top-level turn into an `Arc<str>`, and that Arc is cloned into `MemberSpec.guidance`
and appended by `agent_with_guidance_layer`. No subagent touches the filesystem.

So E4's real cost is **one filesystem walk + read of every `AGENTS.md` up the
chain, per top-level turn** — an I/O and latency cost, not a token cost. The
token duplication that remains (each agent's system prompt contains the guidance)
is semantically required: removing it would change what each agent sees.

E4 is therefore rescoped to **caching the discovery**, and its value is honestly
smaller than the PRD implied. It is kept because it is cheap and correct, not
because it saves tokens.

## 1. E1 — Model-aware threshold

### Current

`needs_compaction` compares `estimate_tokens` against a flat
`CompactionConfig::token_threshold` (default `100_000`, `compaction.rs:34`).
`Capabilities::max_context` exists (`hya-provider/src/lib.rs:109`) but only ever
reaches the catalog API (`compat/catalog.rs:190`).

### Change

Add to `ProviderRouter`:

```rust
/// Capabilities of the first route that claims `model`.
pub fn capabilities(&self, model: &ModelRef) -> Option<Capabilities>;
```

Add to `CompactionConfig`:

```rust
/// Fraction of the model's advertised context window at which to compact.
/// Ignored when the route advertises no window.
pub context_fraction: f32,   // default 0.75
```

Add a pure resolver in `compaction.rs`:

```rust
/// Threshold in tokens for this turn: a fraction of the model's advertised
/// window when known, else the configured flat threshold.
pub fn resolved_threshold(cfg: &CompactionConfig, max_context: Option<u32>) -> usize;
```

Rules, each asserted by a test:

- `max_context` absent or `0` → return `cfg.token_threshold` (today's behaviour).
- Otherwise → `floor(max_context * context_fraction)`.
- Never return `0`; a zero threshold would compact every turn. Clamp to a floor.
- `context_fraction` outside `(0.0, 1.0]` → fall back to `token_threshold` rather
  than trusting a nonsense config.

`HYA_COMPACTION_CONTEXT_FRACTION` joins the existing env overrides in
`hya-app/src/runtime.rs:127`.

## 2. E2 — Real token counts

### Current

`estimate_tokens` is `chars / 4` (`compaction.rs:103`).

### Change

The provider already tells us the true prompt size: the last assistant message's
`TokenUsage`. Use it, and estimate only the delta since.

```rust
/// Measured prompt tokens from the most recent assistant message that reported
/// usage, plus an estimate of everything appended after it.
pub fn measured_tokens(messages: &[Message]) -> Option<usize>;
```

- Scan backwards for the last `Message::Assistant` with `tokens: Some(u)` where
  `u` is non-empty.
- Window occupancy is `u.input + u.cache_read`: cached prompt tokens still occupy
  the window, and providers differ on whether `input` already includes them. Using
  the sum can only over-count, which fails safe (compacts slightly early).
- Add `estimate_tokens` of every message *after* that assistant message.
- Return `None` when no usage was ever reported — caller falls back to
  `estimate_tokens` over the whole transcript.

`needs_compaction` keeps its signature for compatibility; the turn loop calls a
new `tokens_in_use(messages) -> usize` that prefers the measured path.

**Failure mode to guard:** a route with `usage_reporting: false` never reports,
so the fallback must stay exact. A test asserts identical behaviour to today when
no usage is present.

## 3. E3 — Selective eviction before summarizing

### Current

Compaction is all-or-nothing: split at `len - keep_recent`, summarize everything
before it. Tool output is the dominant bulk — `output_cap.rs` caps each result at
5000 chars, so a tool-heavy transcript is mostly tool payloads.

### Change

Before summarizing, try a cheaper, lossless-in-structure step: in messages older
than `keep_recent`, replace completed tool **output** with a short placeholder
while keeping the tool name, call id, and input.

```rust
/// Replace stale completed tool outputs with a size notice. Returns how many
/// parts were evicted. Request-local: never writes the store.
pub fn evict_stale_tool_outputs(messages: &mut [Message], keep_recent: usize) -> u32;
```

Order in the turn loop:

1. compute `tokens_in_use`
2. if under threshold → send as-is (unchanged from today)
3. else evict stale tool outputs; recompute
4. if now under threshold → **skip summarization entirely** and emit
   `ContextEvicted`
5. else → summarize as today, and emit `ContextCompacted` as today

### Observability

Consistent with child A, the decision must be visible. New additive event:

```rust
ContextEvicted {
    session: SessionId,
    evicted_parts: u32,
    tokens_before: u64,
    tokens_after: u64,
    threshold: u64,
}
```

Reducer no-op (a record, not a state transition), same as `ContextCompacted`.

**The eviction is request-local.** The event log keeps every tool output in full,
so an offline viewer still reconstructs the true trajectory. This preserves child
A's guarantee that the log is a sufficient statistic.

## 4. E4 — Cache `AGENTS.md` discovery

Cache `discover_context_files` results per canonical workdir, validated by the
`(path, mtime, len)` of every file the previous walk returned. An edit to any
`AGENTS.md` invalidates the entry, so behaviour is unchanged — only the repeated
walk is skipped.

Correctness requirement: a *new* `AGENTS.md` appearing in the chain must also
invalidate. Validation therefore re-runs the cheap `is_file()` chain walk and
compares the resulting path list before trusting cached contents; only the file
reads are skipped. That keeps the win (reads, which dominate) without a staleness
bug.

## 5. Risks

1. **E1 changes when every session compacts (high).** A 200k-window model moves
   from a 100k threshold to 150k. Intended, but it is the largest behavioural
   change in this task. Mitigated by the resolver being pure and table-tested.
2. **E2 over-counting via `cache_read` (medium).** Fails safe (early compaction),
   never late. Documented at the call site.
3. **E3 changes what the model can see (high).** An agent can no longer re-read an
   old tool result from context. This is the point — it must re-run the tool — but
   the placeholder must say so explicitly so the model understands the absence.
4. **E4 staleness (medium).** A cached chain that misses a newly added `AGENTS.md`
   would silently change agent behaviour. Mitigated by re-walking paths and only
   caching reads.

## 6. Test plan

| Item | Test |
| --- | --- |
| E1 | Table test over `resolved_threshold`: absent window, zero window, normal, fraction out of range, clamp floor |
| E1 | Turn-level: a route advertising 200k compacts at the fraction, not at 100k |
| E2 | `measured_tokens` prefers reported usage + delta estimate |
| E2 | **Regression:** no usage reported → byte-identical decision to today |
| E3 | `evict_stale_tool_outputs` keeps name/input, drops output, respects `keep_recent` |
| E3 | Eviction alone drops under threshold → summarizer never invoked, `ContextEvicted` emitted |
| E3 | Eviction insufficient → summarizer still runs, `ContextCompacted` emitted |
| E3 | The log still contains the full tool output after an evicted turn |
| E4 | Second call returns cached content; touching an `AGENTS.md` invalidates; adding a new one invalidates |
