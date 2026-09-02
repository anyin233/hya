# Implementation Plan

## Preconditions

Implementation begins only after the user approves the current PRD/design/plan
summary and `task.py start` changes this task from `planning` to `in_progress`.
Before product code:

1. Load `trellis-before-dev` for backend, provider, and frontend guidance.
2. Re-read this plan and the task manifests.
3. Use LSP references for every exported symbol or trait method changed,
   especially `Tool`, `GrepTool`, `ToolRegistry::builtins`, `ShellTool` aliases,
   and provider result replay.
4. Preserve the existing uncommitted `crates/hya-tool/tests/read.rs` change,
   `.argus_subagents/`, and the untracked `\` path until the owning change
   deliberately supersedes or excludes them.
5. Do not query or mutate the user's live Session database. Do not stop a running
   backend until every verification gate passes and its exact stale PID/command
   has been verified for the requested local rollout.

## 1. Fix The Task/Subagent Schema Mismatch

### Red

- Add a `hya-tool` schema contract proving `inline_agent.description` is absent
  from both the single and member schemas.
- Add a direct Task-tool test using the captured all-default inline object and
  asserting that the spawn request carries no description.
- Extend `crates/hya-e2e/tests/p08_subagent_task.rs` with the exact captured empty
  nested description. Assert a distinct child, the expected subagent type, and
  parent resumption.
- Run the focused tests and record that current code fails because the field is
  advertised/retained and hya-app returns
  `UNSUPPORTED_INLINE_AGENT_FIELD: `description``.

```bash
cargo test -p hya-tool --test task task_schema_hides_unsupported_inline_description -- --exact
cargo test -p hya-tool --test task task_normalizes_empty_inline_description -- --exact
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e --test p08_subagent_task -- --test-threads=1
```

### Green

- Remove only the two nested schema properties in `task.rs`.
- Convert an empty/whitespace-only parsed description to `None`; preserve a
  non-empty `Some` so hya-app retains its typed no-side-effect rejection.
- Keep and run the existing admission rejection tests.

```bash
cargo test -p hya-tool --test task
cargo test -p hya-app --test spawn_admission inline_description_is_unsupported_before_admission_without_side_effects -- --exact
cargo test -p hya-e2e --test p08_subagent_task -- --test-threads=1
```

Rollback point: Task schema/parser/tests only; no child/session data migration.

## 2. Establish Native Hashline Dependencies And Provenance

- Add minimal Rust-only dependencies at workspace and `hya-tool` scope:
  native XXH32, gitignore-aware traversal/glob matching, and a cross-platform
  PTY only if the existing standard library/libc surface cannot meet the tested
  PTY contract. Disable unused default features.
- Add `crates/hya-tool/NOTICE` with the complete applicable
  `pi-hashline-edit`/Oh My Pi MIT notices and source pins. Add concise provenance
  headers to source-derived modules and test fixtures.
- Create the private `hashline` module boundary and registry-owned runtime; do
  not publish hashline types.

No production behavior changes in this step. Dependency selection is accepted
only if `cargo tree -p hya-tool` shows no JS runtime or avoidable heavy feature.

## 3. Port Hashing, Formatting, And Parsing With Golden TDD

### Red

Create pure tests before each implementation slice:

- `alpha/beta/gamma/delta -> KT/JB/KJ/PX` and 2/3/4-character vectors;
- previous/current/next boundaries, CR removal, ECMAScript trailing whitespace,
  leading/internal whitespace, Unicode, and NUL separators;
- line-number padding, full-file neighbor use for slices, terminal-newline
  sentinel exclusion, empty file, raw output, and first-line oversize behavior;
- accepted `>>>`/`+`/`-` anchors and every bad-reference/config-width error.

```bash
cargo test -p hya-tool hashline::hash::tests
cargo test -p hya-tool hashline::apply::parser_tests
```

Each command must fail for the expected missing behavior before its implementation
is added.

### Green

- Implement incremental seed-0 XXH32 and exact nibble encoding without a joined
  context allocation.
- Implement line indexing over one normalized buffer and strict anchor parsing.
- Keep error strings/codes compatible with the pinned fixture corpus.

Run the same focused commands until green. Use property/table tests for width and
round-trip invariants, not source-text assertions.

## 4. Port The Bounded Runtime And Atomic Filesystem Core

### Red

Add tests for:

- newest-first snapshots, identical-version fusion, 8-path/4-version/32-MiB
  eviction, and Session/workdir isolation;
- no-op calls one/two soft and three hard, applied-payload rejection, external
  content change, and non-raw Read reset;
- same-target serialization and unrelated lock-shard safety;
- mode-0600 create, existing mode preservation, hard-link inode preservation,
  relative/absolute/dangling symlinks, cycle/40-hop failure, restrictive umask,
  fsync/error cleanup, BOM, CRLF, mixed endings, and invalid UTF-8 warning;
- exact context-three merge success, shifted/conflicting live content,
  no-history, and older-version recovery.

```bash
cargo test -p hya-tool hashline::state::tests
cargo test -p hya-tool hashline::fs::tests
cargo test -p hya-tool hashline::merge::tests
```

### Green

- Implement `Arc<str>` LRU accounting, digest-based payload guards, and fixed
  lock shards.
- Implement symlink/hard-link-aware atomic mutation and explicit cleanup.
- Implement exact stale replay/merge. Never add fuzzy relocation.

Run each focused module after its smallest production slice. Roll back the whole
private module together if an invariant cannot be met; do not expose a partial
hashline engine.

## 5. Replace Read End To End

### Red

- Replace the existing uncommitted schema expectation with the approved
  canonical `path` contract; preserve the user's test intent but not the
  superseded `filePath` decision.
- Add the full dual-key/empty/conflict/offset-zero matrix, raw and hashline text
  results, invalid UTF-8 warning, snapshot reset, result-shape, directory,
  attachment, truncation, and permission-order cases.
- Change `hya-core/tests/turn_loop.rs::text_tool_result_text_round_trip` and
  `hya-e2e/tests/p02_tool_loop_fs.rs` to use the exact captured
  `{filePath,path,offset,limit}` input and assert a correlated `ToolResult`.
- Confirm the focused current-code failure is the known duplicate-field or
  missing hashline behavior, not an unrelated fixture failure.

```bash
cargo test -p hya-tool --test read
cargo test -p hya-tool --test read_limits
cargo test -p hya-tool --test read_missing
cargo test -p hya-core --test turn_loop text_tool_result_text_round_trip -- --exact
cargo build -p hya-backend --bin hya-backend
cargo test -p hya-e2e --test p02_tool_loop_fs -- --test-threads=1
```

### Green

- Publish only `path/offset/limit/raw` with positive bounds.
- Deserialize canonical/legacy paths separately and apply the exact resolution
  matrix before permissions or filesystem work.
- Route text through the hashline runtime; keep directory/media extensions and
  existing policy.
- Return bounded title/output/display/truncation metadata.

Rerun all focused commands. Inspect event assertions, not the user's live DB.

## 6. Replace Edit With Strict Hashline Operations

Work operation by operation; each bullet is one red-green cycle.

1. Schema and package `prepareArguments` compatibility/rejections.
2. Single/range replace and deletion.
3. Anchored/implicit append and prepend with terminal newline preservation.
4. Exact `replace_text` zero/multiple/overlapping occurrence behavior.
5. Compound descending application, deduplication, overlap/boundary conflicts,
   invalid rendered rows, and would-empty guard.
6. Text hints, collision veto, ellipsis rules, stale suggestions/budgets.
7. No-op/duplicate guards, Read-to-Edit fresh chaining, and stale recovery
   success/conflict/no-history.
8. Atomic filesystem matrix, formatter/BOM/final-anchor behavior, LSP
   diagnostics, bounded diff/classification/warnings, and session-diff metadata.
9. Cancellation and failure-after-mutation context.

Migrate every repository caller from `filePath/oldString/newString` to
`path/edits`, including core/server/provider/process fixtures and docs. Delete
`edit_replace.rs`, `edit_replace/replacers.rs`, and fuzzy tests in the same
cutover; replace them with observable hashline contracts.

```bash
cargo test -p hya-tool --test edit
cargo test -p hya-tool --test edit_hashline
cargo test -p hya-tool --test formatter_bom
cargo test -p hya-tool --test lsp_write_edit
cargo test -p hya-core --test hooks_seam
cargo test -p hya-e2e --test p15_todo_and_edit -- --test-threads=1
```

The exact new test filename may follow the crate's integration-test convention,
but it must remain one focused hashline contract suite.

Rollback point: new private runtime plus Read/Edit adapters and migrated callers
must revert together; fuzzy and anchored Edit must never coexist publicly.

## 7. Replace Grep With Native Hashline Search

### Red

Add schema and behavior tests for regex/literal, case, glob, gitignore, file and
directory targets, binary/unreadable files, empty results, context merge and
separator rules, deterministic order, exact limit versus limit+1, cancellation,
hashline rows, display metadata, and Grep-to-Edit stale recovery.

```bash
cargo test -p hya-tool --test glob_grep grep_
cargo test -p hya-tool --test grep_hashline
```

### Green

- Move Grep to `grep.rs`, inject the shared runtime, and retain a documented
  constructor/default if the exported unit type changes.
- Traverse/search in a cancellable blocking Rust worker with gitignore/glob
  semantics; never spawn `rg`.
- Reload/result-format through the shared text/hashline path and record bounded
  snapshots/display groups.
- Remove `include` from the public contract and migrate all callers.

Run focused tests plus Read/Edit chaining tests after the change.

## 8. Align Write And The Shared Result Envelope

### Red

Add tests for the closed `{path,content}` schema, rejection of old advertised
keys, exact whole-file writes, unambiguous hashline-prefix stripping, ambiguous
content preservation, parent creation, atomic/mode/BOM/formatter/LSP behavior,
shebang executable success/failure warning, final preview truth, snapshot update,
and structured result-cap retention.

Add provider tests proving object results replay their string `output` field,
while objects without it retain JSON fallback. Add output-cap tests for default
5,000-character behavior and declared bounded coding-tool envelopes.

```bash
cargo test -p hya-tool --test write
cargo test -p hya-tool output_cap::tests
cargo test -p hya-provider --test conformance tool_result
cargo test -p hya-server --test compat_session_legacy_tool_state_api
```

### Green

- Publish only `path + content` and reuse the native atomic/snapshot core.
- Make tool-result caps shape-aware and defaulted on `Tool`; retain the unrelated
  5,000-character policy.
- Prefer `output` for provider replay and preserve title/metadata in durable
  results.

Rerun all touched Write, formatter, LSP, provider, core, and Compat tests.

## 9. Align Canonical Bash And Implement PTY/Bounded Capture

Use separate red-green cycles for schema/name, timing, capture, and PTY.

- Assert that schemas advertise `bash` only, `shell` resolves as a hidden alias,
  input uses `cwd`/seconds/`pty`, and resource views migrate to `bash`.
- Assert default 300 seconds, disabled zero, finite validation, 1..=3600 clamp
  notices, cwd checks, command permission ordering, nonzero exit metadata,
  timeout warning, cancellation, and process-group cleanup.
- Assert interleaved output ordering, 50-KiB inline cap, complete artifact,
  bounded memory behavior, UTF-8 loss handling, and no `env` disclosure.
- Assert a real PTY changes TTY-observable behavior and obeys the same timeout,
  cancellation, output, and cleanup contracts.

```bash
cargo test -p hya-tool --test shell
cargo test -p hya-tool --test tool bash_
cargo test -p hya-core --test shell_direct
cargo test -p hya-e2e --test p03_permissions -- --test-threads=1
```

Implement the base OMP contract in Rust. Do not add async/background jobs,
direnv, internal URL expansion, or silent PTY fallback.

Rollback point: schema/name alias plus all migrated resource lists must revert as
one unit; never advertise both `shell` and `bash`.

## 10. Add Hya-Owned OpenTUI Coding-Tool Blocks

### Red: normalization and actual renderer

Create:

- `test/coding-tool-presentation.test.ts` for allowlisted view normalization,
  all tool states, canonical/legacy paths, directories/attachments, malformed
  metadata, truncation, diagnostics, both Bash names, ANSI stripping, and proof
  that `env`/unknown keys never render.
- `test/coding-tool-render.test.tsx` with OpenTUI `testRender` at 80 and 140
  columns. Assert semantic titles/content/line offsets/diff mode and use
  `captureSpans()` to prove syntax-token colors without a brittle full-screen
  snapshot.
- `test/coding-tool-sync.test.tsx` for initial SDK hydration followed by one
  `message.part.updated` replacement, with no presentation-specific network
  call or timer.

```bash
cd packages/hya-tui-ts
bun test test/coding-tool-presentation.test.ts
bun test test/coding-tool-render.test.tsx
bun test test/coding-tool-sync.test.tsx
```

Each test must fail on the current inline/generic presentation for the expected
missing block behavior.

### Green

- Add `src/hya/coding-tool-presentation.tsx` with the pure normalizer and one
  renderer.
- Make one narrow retained-route dispatch change and remove superseded completed
  Read/Write/Edit/Grep/Bash branches. Keep pending/error/permission fallback.
- Use current theme, parser, line-number, diff, path, width, and collapse
  primitives. Add no dependency or raw color.

Run focused tests, then:

```bash
cd packages/hya-tui-ts
bun run typecheck
bun test
```

### Real surface

Extend the existing serialized PTY harness with a deterministic real-backend
scenario that produces Read, Edit, Write, Grep, and Bash results. Verify the
live 140-column frame, reopen the same Session and verify replay, then verify an
80-column frame with unified diff and readable prompt/footer.

```bash
cargo build -p hya-backend --bin hya-backend
cargo build -p hya-ts --bin hya-ts
cd packages/hya-tui-ts
bun test test/pty-smoke.test.ts --test-name-pattern "coding tool blocks"
bun test test/pty-smoke.test.ts
```

The PTY run is the required actual-surface proof. Renderer spans are the syntax
highlighting proof.

## 11. Update Specifications, Documentation, And 0.36.9 Metadata

- Update `.trellis/spec/backend/quality-guidelines.md` and error-handling rules
  for canonical schemas, hashline anchors/state, typed conflicts, result caps,
  and Task schema/executable coherence.
- Update frontend component/quality guidance for the coding-tool presentation
  seam, semantic metadata, 80/wide rendering, and replay.
- Update `docs/architecture/tools-and-permissions.md`,
  `docs/architecture/agent-tool-surface.md`,
  `docs/architecture/event-model.md`, `docs/tui-reference.md`, and parity docs.
- Archive root `CHANGELOG.md` as
  `docs/changes/CHANGELOG_0.36.8.md`, write only 0.36.9 notes at root, and update
  every established current-version location:
  `Cargo.toml`, `Cargo.lock`, `README.md`,
  `crates/hya/tests/version_metadata.rs`,
  `crates/xtask/tests/release_rehearsal.rs`,
  `packages/hya-tui-ts/package.json`, and
  `packages/hya-tui-ts/UPSTREAM.md`.
- Do not create a release tag or GitHub Release.

Run version/rehearsal and doc-linked focused tests before the full gate.

## Review Hardening Remediation

The post-implementation review produced independent security, correctness,
resource-safety, TUI, and release findings. Implement them as parallel
non-overlapping source slices after these focused regressions have failed on the
reviewed behavior:

1. Read/Edit/hashline: lexical authorization before metadata, `[E_BAD_READ]`,
   bounded diagnostics, prepared-inode revalidation, and committed-cancellation
   reconciliation.
2. Grep/Glob: inner-loop cancellation, one-MiB logical-line discard, bounded
   caller/ignore globs, both negated-class spellings, and kind-blind external
   authorization.
3. Bash/output/core: armed private artifact ownership, PTY descendant cleanup,
   notices before spill selection, one-pass structural budgets, and final
   post-hook capping.
4. OpenTUI: one SyncProvider owner, complete backend truncation/termination
   facts, positioned error diagnostics, narrow diff rows, and command-only
   highlighting.
5. Release/install: recursive TUI source copy, both byte-identical notices,
   immutable workflow/archive/smoke checks, and required `node_modules`.

Rerun every new focused regression after the source wave. A harness failure is
not green; correct the fixture until it fails only on the reviewed production
gap. Then run the complete gates below.

## 12. Run Final Gates

Run in this order from the repository root unless noted:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --jobs 1 --exclude hya-e2e
cargo build -p hya-backend --bin hya-backend
cargo build -p hya-ts --bin hya-ts
cargo test -p hya-e2e -- --test-threads=1
cd packages/hya-tui-ts && bun run typecheck && bun test
cd packages/hya-tui-ts && bun test test/real-backend.test.ts test/task-presentation.test.ts test/real-backend-agents.test.ts
python3 ./.trellis/scripts/task.py validate 09-02-fix-read-tool-session-errors
```

After those gates pass, install the dev-profile 0.36.9 closure with
`./install.sh --prefix "$HOME/.local" --profile dev`, verify all installed
entrypoints/runtime files, stop only the verified stale hya process group, and
exercise a Read call through the installed 0.36.9 backend/TUI surface. Preserve
the append-only Session database and historical errors.

If any command fails, fix the source cause, rerun its narrow red/green command,
then rerun the complete gate. A failed gate blocks every commit and push.

After the smoke test succeeds, perform required cleanup:

- remove temporary instrumentation/prototypes and search for the chosen debug
  prefix;
- verify every old schema key/tool name/fuzzy caller is migrated or intentionally
  retained as the documented hidden Read/Task/Bash compatibility boundary;
- verify no unbounded state/output path, secret-bearing fixture, second read
  model, JS backend dependency, or copied code without notice remains.

## 13. Review, Commit, Push, And Finish

Review task-owned changes only. Do not stage `.argus_subagents/` or the untracked
`\` path. Create semantic commits in dependency order after the complete gate:

1. `fix(agent): align inline task agent contract`
2. `feat(tool): port native hashline coding tools`
3. `feat(tool): align write and bash contracts`
4. `feat(tui): render coding tool blocks`
5. `chore(release): prepare 0.36.9`

Use the Commit skill, stage exact files for each commit, and push the current
branch once all five commits exist. Then run `trellis-check` against the complete
change and `trellis-finish-work` to archive the task and update the developer
journal. If review fixes alter behavior, rerun the affected focused tests and the
complete final gate before amending/new commits.

The final report must include:

- root causes and why the new boundaries cannot generate the two observed
  schema/execution contradictions;
- files/modules changed and source/license pins;
- exact focused, full, process, typecheck, renderer, and PTY results;
- commit hashes and push result;
- the operational note that historical errors remain in the append-only log and
  an already running 0.36.8 backend must restart before future calls use 0.36.9.
