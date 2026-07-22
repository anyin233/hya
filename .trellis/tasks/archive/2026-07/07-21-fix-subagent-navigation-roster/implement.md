# Implementation Plan: Subagent Navigation and Roster Dock

## Gate

- [ ] Obtain user review of `prd.md`, `design.md`, and this plan.
- [ ] Activate the task with
  `python3 .trellis/scripts/task.py start 07-21-fix-subagent-navigation-roster`.
- [ ] Re-read `git status` and preserve all unrelated work, especially the active
  web-search task and the other untracked Trellis tasks.

## 1. Resolve Parentless Team Ownership

- [ ] Add one focused `hya-sdk` test where a session has no `parent_id`, exactly
  one different Team roster contains it, and `team_root_for` is expected to
  return that Team root.
- [ ] Run
  `cargo test -p hya-sdk team_root_for_resolves_unique_roster_owner`
  and record the expected RED result: the child ID is returned instead.
- [ ] Extend `MessageStore::team_root_for` with the minimum unique-roster fallback
  after explicit ancestry and own-Team authority.
- [ ] Re-run the focused test GREEN.
- [ ] Add passing assertions for own-Team authority and ambiguous roster
  ownership, then run `cargo test -p hya-sdk team_root_for`.

Rollback point: only `crates/hya-sdk/src/store.rs` is changed.

## 2. Classify Roster-Only Children

- [ ] Add one `hya-tui` unit test proving the existing parentless failed-start
  fixture is `SubagentStatus::Child` after root resolution.
- [ ] Run the exact test filter and record RED: current status is not `Child`.
- [ ] Update `session::subagent_status` to derive child index/total from the
  uniquely owning Team roster while preserving explicit-parent precedence.
- [ ] Re-run the focused test GREEN and run existing `subagent_status` tests.

Rollback point: add only the classification branch in
`crates/hya-tui/src/screens/session.rs`.

## 3. Return Direct Child Routes with Esc

- [ ] Add one harness regression using `seed_main_and_child_roster`: load the
  parentless child route, press one bare `Esc`, and expect
  `main_route_session() == Some("ses_main")`.
- [ ] Run the exact harness test and record RED: the route remains `ses_child`.
- [ ] Add private `Runtime::return_to_team_root` using the existing spawned store
  read and `AppEvent::LoadSession` pattern.
- [ ] Intercept unmodified direct-child `Esc` in `handle_prompt_key`; do not gate
  it on an empty prompt.
- [ ] Re-run the focused test GREEN and retain root-session interrupt behavior.

Rollback point: only the private runtime helper and one prompt-key branch are
added.

## 4. Recover from Child-Backed Splits

- [ ] Extend the harness fixture with one parentless sibling roster entry.
- [ ] From the original child route, invoke leader+`V`, select the sibling, and
  assert a vertical auxiliary split exists before pressing `Esc`.
- [ ] Press one bare `Esc`; expect main focus and root route restoration. The
  normal `LoadSession` transition may clear the split.
- [ ] Run the exact harness test and record RED: focus returns but the route
  remains the child.
- [ ] Invoke `return_to_team_root` from the existing auxiliary bare-`Esc` branch
  after focusing main.
- [ ] Re-run the focused test GREEN plus existing tab, vertical, horizontal,
  close, cycle, focus-main, and lifecycle pane tests.

Rollback point: one call is added to the existing auxiliary `Esc` branch.

## 5. Render the Shared Dock

- [ ] Add a semantic session-screen test at 80 and 120 columns expecting the
  full roster and all 13 selected default binding texts directly before the
  prompt, with every rendered line within the requested width.
- [ ] Run the test and record RED: only the current one-line status exists.
- [ ] Build one app-specific dock text helper in `screens/session.rs` from the
  Team projection, existing status colors, `default_binding_specs()`, and
  `Text::wrap`.
- [ ] Replace the fixed one-row session reservation with the wrapped dock height
  while preserving prompt priority and saturating geometry.
- [ ] Re-run the main render test GREEN.
- [ ] Add an auxiliary-pane render test expecting the same roster/shortcut text
  at the pane bottom and no prompt composer; run it RED.
- [ ] Reserve the wrapped dock at the bottom of `screens/pane.rs` after the
  header, reduce the transcript viewport, and render the shared text.
- [ ] Re-run the auxiliary test GREEN and run the focused `hya-tui` suite.

Rollback point: the helper stays in `hya-tui`; no new module, dependency, public
API, or `hya-tui-lib` change is introduced.

## 6. Release Metadata

- [ ] Immediately before editing release files, re-read `Cargo.toml`,
  `Cargo.lock`, root `CHANGELOG.md`, `docs/changes/`, and `git status`.
- [ ] If another task still owns dirty release-file changes, stop this step and
  coordinate rather than overwriting or combining them.
- [ ] Bump `[workspace.package].version` to the next patch after the then-current
  version, update matching workspace entries in `Cargo.lock`, archive the prior
  root changelog as `docs/changes/CHANGELOG_<prior>.md`, and write only this
  fix's notes to root `CHANGELOG.md`.
- [ ] Verify the workspace version, changelog heading, and archive filename
  agree.

## 7. Verification and Delivery

- [ ] Run focused checks:
  `cargo test -p hya-sdk team_root_for` and the new exact `hya-tui` tests.
- [ ] Run `cargo fmt --all --check`.
- [ ] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] Run `cargo test --workspace`.
- [ ] Build the local executable with `cargo build -p hya`.
- [ ] Run the Trellis quality review and resolve only verified findings within
  task scope.
- [ ] Review `git diff` and `git status`; stage only this task's files and
  preserve all unrelated changes.
- [ ] Commit the verified atomic feature with a one-line semantic message and
  push it. Do not commit or push if any required gate fails.
- [ ] Record session progress and finish/archive the Trellis task.

## Completion Criteria

- One `Esc` restores the Team root from direct, failed-start, and child-backed
  auxiliary observation states.
- Existing root interrupt and valid split behavior remains intact.
- Main and auxiliary views share one complete, width-safe roster/shortcut dock.
- Release metadata is aligned without overwriting concurrent work.
- All required checks and the local `hya` build pass before commit and push.
