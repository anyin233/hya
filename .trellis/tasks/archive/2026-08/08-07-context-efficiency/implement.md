# Implementation plan — Context efficiency

TDD gate per `AGENTS.md`: one atomic failing test, verify it fails for the right
reason, implement the smallest change, run the step's validation.

Ordering rationale: E4 and E2 are self-contained and low risk, so they land first
and keep the risky steps small. E1 and E3 both change compaction behaviour and
are isolated so either can be reverted alone.

---

## S1 — E4: cache `AGENTS.md` discovery · low risk

`crates/hya-core/src/prompt.rs`. Cache keyed by canonical workdir; re-walk the
`is_file()` chain to validate the path list, skip only the reads, and invalidate
on `(path, mtime, len)` change.

- **Tests:** repeat call returns same content; touching a file invalidates;
  adding a new `AGENTS.md` to the chain invalidates.
- **Validate:** `cargo test -p hya-core --lib prompt`

## S2 — E2: real token counts · medium risk

`crates/hya-core/src/compaction.rs`. Add `measured_tokens` and `tokens_in_use`;
turn loop uses `tokens_in_use`.

- **Tests:** prefers reported usage plus delta; **regression** — with no usage
  reported the decision is identical to today.
- **Validate:** `cargo test -p hya-core`

## S3 — E1: model-aware threshold · high risk

`ProviderRouter::capabilities`, `CompactionConfig::context_fraction`,
`resolved_threshold`, env override, turn-loop wiring.

- **Tests:** table over the resolver (absent / zero / normal / out-of-range /
  floor clamp); turn-level test that a 200k route compacts at the fraction.
- **Validate:** `cargo test -p hya-core -p hya-provider`
- **Gate:** revert point. Do not start S4 until green.

## S4 — E3: selective eviction + `ContextEvicted` · high risk

Wire-format addition plus a model-input change.

1. `Event::ContextEvicted` in `hya-proto`; extend `Event::session()`; add to the
   reducer no-op arm and every exhaustive match the build enumerates (see the
   spec scenario "Adding an Additive `Event` Variant").
2. `evict_stale_tool_outputs` in `compaction.rs`.
3. Turn-loop order: measure → evict → re-measure → summarize only if still over.

- **Tests:** eviction shape; eviction-alone path never invokes the summarizer and
  emits `ContextEvicted`; insufficient eviction still summarizes; the log retains
  full tool output afterwards.
- **Validate:** `cargo test -p hya-proto -p hya-core -p hya-server`

## S5 — Full gate

```sh
cargo fmt -p hya-core -p hya-proto -p hya-provider -p hya-server -p hya-app
cargo clippy -p hya-core -p hya-proto -p hya-provider -p hya-server -p hya-app --all-targets -- -D warnings
cargo test --workspace --exclude hya-e2e
cargo build -p hya-backend --bin hya-backend && cargo test -p hya-e2e -- --test-threads=1
```

Then the version-bump checklist from the spec scenario "Workspace Version Bump" —
all seven files, or `-p hya`'s `version_metadata` test fails.

**Known pre-existing:** `cargo fmt --all --check` and workspace-wide clippy fail
inside `crates/hya-sdk`, untouched here and already failing on `main`. Do not fix.

## Commit plan

| Commit | Covers |
| --- | --- |
| `perf(core): cache AGENTS.md discovery per workdir` | S1 |
| `feat(core): drive compaction from reported token usage` | S2 |
| `feat(core): scale compaction threshold to the model context window` | S3 |
| `feat(core): evict stale tool outputs before summarizing` | S4 |
| `chore(release): X.Y.Z` | S5 |

## Rollback points

- After S3 — reverts the threshold change, keeps E2/E4.
- After S4 — reverts eviction alone; the wire addition is additive and harmless.
