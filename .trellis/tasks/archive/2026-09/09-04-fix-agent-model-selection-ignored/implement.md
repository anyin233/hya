# Implementation Plan: Fix Ignored Agent Model Selections

## Rules

- The user's installed symptom is ground truth.
- Build the provider-request feedback loop before reading implementation details
  or choosing a cause.
- Use fake providers only; redact any captured headers or configuration.
- Follow one RED → GREEN slice at a time.
- Do not modify unrelated `.agents/**`, `.codex/**`, `.omp/**`, or Trellis
  runtime changes.

## Slice 1 — RED provider-request loop

1. Add one focused public-boundary test with `fake/default` and `fake/selected`.
2. Open a Session on the default, apply the normal selection, issue the next
   prompt, and assert the provider receives `fake/selected`.
3. Apply a targeted preference for a second Agent, execute it, and assert its
   request uses `fake/selected` without changing the first Agent.
4. Run only this test and record the exact default-model failure.

## Slice 2 — Minimize and diagnose

1. Remove non-load-bearing UI, restart, and catalog setup until the smallest red
   scenario remains.
2. Trace the selected identity across frontend, HTTP, app, Session, core, and
   provider boundaries with LSP references and focused source reads.
3. Write 3–5 ranked falsifiable hypotheses in the planning findings.
4. Use a debugger or one tagged probe per hypothesis; change one variable at a
   time.

## Slice 3 — GREEN fix

1. Fix the first boundary that replaces or ignores the selected identity.
2. Update every affected root, subagent, Workflow, and fixed-Agent caller in a
   clean cutover.
3. Preserve explicit direct/category and request/spawn/Stage precedence.
4. Run the minimized regression and the original public-boundary loop green.

## Slice 4 — Adjacent regression coverage

1. Cover set, clear, stale fallback, existing Session, new Session, restart, and
   per-Agent isolation at observable seams.
2. Cover frontend success-before-update and failure rollback if the defect is in
   TUI synchronization.
3. Cover provider-local model IDs containing slashes if identity conversion is
   involved.
4. Remove every temporary `[DEBUG-agent-model-ignored]` probe.

## Slice 5 — Verification and installation

Run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --jobs 1 --exclude hya-e2e
cargo build -p hya-backend --bin hya-backend -p hya-ts --bin hya-ts
cargo test -p hya-e2e -- --test-threads=1
cd packages/hya-tui-ts
bun run typecheck
bun test
```

Then install and smoke:

```sh
./install.sh --prefix "$HOME/.local"
"$HOME/.local/bin/hya" --version
```

Drive the actual installed TUI against an isolated fake provider and record the
selected model from the provider request. Picker/API state alone is not proof.

## Slice 6 — Review and release

1. Run `trellis-check` and apply only source-verified findings.
2. Update executable specs with the root cause and prevention contract.
3. Bump workspace and TUI version to `0.36.11`; archive `CHANGELOG_0.36.10.md`
   and write the newest-only root changelog.
4. Commit one atomic fix, push, archive the Trellis task, record the journal, and
   push the maintenance commits.

## Rollback

Revert the atomic fix commit. Retain existing preference rows; the previous
runtime will ignore or fall back from them safely.
