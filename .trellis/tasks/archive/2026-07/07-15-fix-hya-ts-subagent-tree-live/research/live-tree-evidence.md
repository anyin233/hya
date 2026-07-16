# Live Tree Evidence

## Confirmed Runtime

- Installed `hya`, `hya-backend`, and `hya-ts` report version `0.33.7` after
  reinstall.
- `hya --server <url>` attaches to an existing compatible backend.
- `hya-ts --server <url> --session <id>` observes an existing session.
- `hya-backend serve --db <path> --bind 127.0.0.1:0 --model <model>` owns one
  explicit persistent loopback backend.
- The installed agent catalog currently exposes `build`, whose declared allow
  rules are read, glob, and grep.

## Empty-Root Checks

One real backend and native-created `hysec_*` root returned a tree with zero
children, and the production TypeScript `parseRunTree` accepted it.

The installed `hya-ts` binary was then driven through `/usr/bin/script` with an
xterm transcript. The driver waited for the live `Hya-Main` Session marker,
sent `Ctrl+X O`, and observed `Subagent roster` with no `Subagent tree
unavailable` text.

These checks rule out an unconditional route, parser, launcher, or roster-open
failure. Child-bearing live state remains the discriminating case.

## Existing Executable Contracts

- `crates/hya-app/tests/nested_spawn_tree.rs` creates root, child, and grandchild
  sessions through the production `SpawnerPlane`, then checks recursive tree
  ancestry and roster metadata.
- `packages/hya-tui-ts/test/subagent-workspace.test.ts` covers tree parsing,
  loader state, retained data, error state, and retry transitions.
- `packages/hya-tui-ts/test/pty-smoke.test.ts` covers the visible roster,
  retained fetch failure, exact retry request count, recursive members, and
  focused read-only child observation.
- The task tool supports a `members` array, so one foreground call can launch
  exactly two sibling sessions and return both outcomes to the root.

## First Edit Gate

The approved bounded live run reached two sequential `task` / `build`
permission requests, replied `once` to each validated request, and received a
successful JSON tree response after spawn began. Production `parseRunTree`
rejected that child-bearing response before the roster could open. The backend,
listener, private database, and transcripts were then removed.

The owning wire-contract mismatch is source-backed: Rust
`MemberProjection.summary` uses `skip_serializing_if = "String::is_empty"`, so
active members omit `summary`; the TypeScript parser currently requires a
string. Empty roots and completed fixtures therefore pass while a live running
member fails. The next edit is one parser-owned RED covering an omitted active
summary, followed by normalization to the projection's semantic empty string.

The sanitized failure path did not retain session IDs before cleanup. No raw
prompt, provider payload, authorization material, response body, database, or
transcript remains.

## Check-Agent Verification

- Restoring the old strict parser made only the new omitted-summary regression
  fail (`15` passed, `1` failed); restoring the fix passed all `16` focused
  tests. The full TypeScript suite passed `27` tests and `2,191` assertions,
  followed by typecheck and build.
- The recursive Rust tree test passed `3/3`, and `hya`, `hya-backend`, and
  `hya-ts` built at `0.33.8`. Cargo metadata, the TypeScript package, and the
  current and archived changelogs agree on their respective versions.
- `git diff --check` and Trellis task validation passed. Workspace formatting,
  clippy, and a later workspace test were blocked only by concurrent
  permission-policy edits outside this task; no such file was changed here.
- No post-fix provider workload was run because the single authorization had
  already been consumed.

## Post-Fix Replay Attempt

- The checkout's `0.33.8` binaries and prepared TypeScript runtime were
  installed atomically under `~/.local` after all offline checks passed.
- A newly authorized bounded replay passed its no-prompt installed-TUI
  preflight, then submitted one root prompt. The temporary acceptance driver
  stopped before any permission approval or admitted child because it required
  raw `children` on the initial empty tree.
- This was a harness defect: Rust intentionally omits an empty `children` array,
  while production `parseRunTree` correctly normalizes the omission to `[]`.
  The temporary driver now applies the same empty-array rule.
- Cleanup removed the second private database and transcripts. Because provider
  traffic may have started after prompt admission, this authorization is treated
  as consumed even though zero permissions were approved and zero children were
  admitted.

## Current Permission Contract

- Concurrent change `d7116136` landed while this task was active and is present
  in the installed `0.33.8` binaries. Its documented and executable default
  policy allows the canonical `task` tool without prompting; the focused
  `invocation_policy_evaluates_models_rules_and_fallbacks` test passes.
- A final authorized attempt delivered the exact prompt but the temporary
  driver waited for the two `0.33.7` permission replies before advancing to UI
  observation. Current default mode emits zero such prompts, so the driver hit
  its wall cap and cleaned the database without retaining child evidence.
- The temporary driver now accepts zero default-mode task prompts or exactly two
  validated strict/configured-mode prompts. Every other permission remains a
  hard stop. An offline dev-provider replay also proved the long root prompt is
  delivered byte-for-byte, ruling out PTY input loss.

## Live Parser Verification

- A subsequent corrected run used the current default policy, emitted zero
  permission prompts, admitted exactly two child sessions, and returned roster
  metadata for both.
- While at least one child summary was omitted, the installed `0.33.8`
  production parser accepted the live tree and normalized the missing summary.
  This directly verifies the fixed wire boundary that failed on `0.33.7`.
- The acceptance driver then stopped before launching `hya-ts` because it
  over-constrained child `description` values to the requested assignment labels;
  the model had preserved distinct assignments but paraphrased those display
  descriptions. Cleanup removed the private database and transcripts.
- The temporary check now validates the two task prompts semantically and keeps
  exact `RUST-RUNTIME` and `TS-ROSTER` literals only where required: the final
  root synthesis.

## Final Live Acceptance

- The final explicitly authorized `0.33.8` run passed with the current default
  permission policy: zero permission prompts, exactly two `build` children, and
  no questions, mutation paths, nested delegation, or third child.
- The live raw tree omitted at least one active member summary. The installed
  production parser normalized it to `""`; root and child routes resolved the
  same top ancestor and both children exposed matching roster metadata.
- Installed `hya-ts` opened `Ctrl+X O` without a retry error, displayed both live
  children, and opened one focused read-only child. A typed sentinel was absent
  from both root and child event logs.
- Both children finished and the root's final assistant text contained the exact
  `RUST-RUNTIME` and `TS-ROSTER` labels. Cleanup stopped every owned process,
  closed the listener, removed the private database/transcripts, and deleted the
  temporary driver.

## Final Gates

- `cargo fmt --all --check`, workspace/all-target clippy with warnings denied,
  `cargo test --workspace`, and the local `hya`/`hya-backend`/`hya-ts` build pass.
- The focused parser suite passes `16/16`; TypeScript typecheck and production
  build pass.
- After explicit authorization to align the stale permission integration tests,
  the complete Bun suite passes `27/27` with `2,194` assertions. Exact command
  remember scope and the question tool-permission handshake now match
  permission-policy commit `d7116136`.
- The Trellis check agent confirmed the parser still rejects a present
  non-string summary, and the run-tree wire contract is recorded in the
  frontend quality spec.
- Commits `87bc1bea` and `3c815452` were pushed to `origin/main`. Release
  `0.33.8` was built from detached commit `3c815452`, installed atomically under
  `~/.local`, and passed binary/version plus installed-parser smoke checks.
