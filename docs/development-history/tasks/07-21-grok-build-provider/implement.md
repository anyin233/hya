# Grok Build Provider Implementation Plan

## 1. Activate

- [x] Validate the task artifacts and context manifests.
- [x] Start `.trellis/tasks/07-21-grok-build-provider` so implementation runs in `in_progress` state.
- [x] Snapshot the dirty worktree and preserve unrelated concurrent edits, especially in `crates/hya-app/src/config.rs`.

## 2. Configuration RED/GREEN

- [x] Add one focused `hya-app` config test that parses native YAML with `kind: grok-build` and asserts `low`, `medium`, `high`, defaulting to `high`.
- [x] Run only that test and confirm RED is the unknown provider kind.
- [x] Add the YAML config variant, provider conversion, `ProviderKind::GrokBuild`, fallback reasoning variants, and initial `/responses` Bearer routing.
- [x] Re-run the focused test GREEN.

## 3. Request RED/GREEN

- [x] Extend the local `hya-provider/tests/http_headers.rs` harness with one Grok Build request test using a fake token.
- [x] Assert `/responses`, Bearer auth, `stream: true`, `store: false`, `include: ["reasoning.encrypted_content"]`, and nested `low`, `medium`, and `high` efforts.
- [x] Run the focused test and confirm RED is the missing encrypted-content request field.
- [x] Add a crate-private Grok Responses protocol adapter that delegates to the existing encoder and appends only the required `include` field.
- [x] Re-run the focused test GREEN and retain an assertion that regular `openai-response` bodies do not gain the field.

## 4. Stream RED/GREEN

- [x] Add focused local SSE coverage for `response.reasoning_text.delta` and a typed terminal event.
- [x] Confirm RED is the missing normalized reasoning event, then route it through the existing reasoning-delta decoder path and rerun GREEN.
- [x] Add Grok-only cases for bare `[DONE]` and EOF without a typed terminal event.
- [x] Confirm RED is permissive success, then add the smallest decoder mode/state needed to reject both for Grok while retaining existing OpenAI Responses behavior.
- [x] Re-run the focused Grok and existing Responses HTTP/SSE tests GREEN.

## 5. Documentation And Release Metadata

- [x] Add `grok-build` and its Responses behavior to `docs/configuration.md`.
- [x] Capture the implemented protocol contract in `.trellis/spec/backend/quality-guidelines.md`.
- [x] Bump `[workspace.package].version` from `0.33.16` to `0.33.17` and refresh `Cargo.lock` through Cargo.
- [x] Move the current root changelog to `docs/changes/CHANGELOG_0.33.16.md` and write a newest-only `CHANGELOG.md` for `0.33.17`.

## 6. Verification

- [x] Run focused `hya-app` and `hya-provider` tests after each TDD slice.
- [x] Run `cargo fmt --all --check`.
- [x] Run `cargo clippy --workspace --all-targets -- -D warnings`.
- [x] Run `cargo test --workspace`.
- [x] Run `cargo build -p hya --bin hya`.
- [x] Review `git diff` and `git status`; include only task-owned changes and preserve unrelated work.

## 7. Live Gate And Delivery

- [ ] Retry sanitized `grok-4.5` Responses probes for `low`, `medium`, and `high` without printing or persisting the credential or response secrets.
- [ ] Record HTTP status and typed terminal event/error class in research.
- [ ] If all required gates pass, review spec-update needs, commit the atomic feature, and push it as required by the repository.
- [ ] If inference remains unavailable, report the external blocker and do not claim or commit an unverified feature.
