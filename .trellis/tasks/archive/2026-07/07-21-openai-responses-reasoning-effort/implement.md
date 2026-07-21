# Implement: OpenAI Responses API and startup reasoning defaults

Each behavior is implemented strict red -> green and committed as one atomic
change after its focused gate. No new dependencies.

## Task 1 - Persist opaque reasoning data [AC2]

Files: `hya-proto` reasoning event/part/projection, core message reconstruction
and fork copying, focused proto/core tests.

1. RED: add one replay test proving `ReasoningEnd.provider_data` survives event
   serde and projection, plus one core test proving reconstructed messages retain
   it.
2. GREEN: add optional `provider_data` to `ReasoningEnd`, `PartProjection`, and
   `Part`; store it in the reducer; map reasoning parts into provider messages;
   preserve it when forking. Add `..` only where existing exhaustive render
   patterns require it; never expose the opaque value as visible reasoning text.
3. Run `cargo test -p hya-proto && cargo test -p hya-core`.
4. Commit and push the atomic event/replay change.

## Task 2 - Add config kinds and model defaults [AC1, AC3, AC5]

Files: `hya-app/src/config.rs`, `hya-app/src/runtime.rs`, startup callers, focused
app/backend tests.

1. RED: parse a config with `kind: openai-response` and detailed
   `gpt-5.6-sol` metadata; assert all seven variants, default `Medium`, selected
   runtime reasoning, and first `AgentSpec.reasoning`. Add rejection cases for an
   unknown kind, effort, and unsupported explicit default. Preserve a legacy
   string-model/Chat alias case.
2. GREEN: add the untagged model config, normalize/validate at load, add
   `OpenAiResponse`, resolve each model default with the existing helper, and
   carry the selected startup default through `RuntimeConfig` into every base
   `AgentSpec` construction (server, exec, RPC, goal, TUI, team base).
3. Keep `openai`, `openai-compatible`, and `openai-completion` on Chat. Route
   `openai-response` to the Responses kind; its protocol is completed in Task 3.
4. Run `cargo test -p hya-app && cargo test -p hya-backend`.
5. Commit and push the atomic config/startup change together with Task 3 if the
   new provider kind cannot compile independently.

## Task 3 - Implement the Responses protocol [AC1, AC2, AC3, AC4]

Files: `hya-provider/src/openai/responses.rs`, OpenAI module exports,
`hya-provider/src/http.rs`, existing HTTP integration tests.

1. RED request-boundary test: `OpenAiResponse` posts to `/responses`, uses flat
   tools, `store:false`, and emits each of `none|minimal|low|medium|high|xhigh|max`
   unchanged under `reasoning.effort`. Keep the existing Chat test green at
   `/chat/completions` with `reasoning_effort` semantics unchanged.
2. GREEN: implement `OpenAiResponsesProtocol::encode`, select it in
   `HttpProvider::new`, and advertise all seven variants for this kind.
3. RED stream fixture: feed semantic SSE covering visible reasoning summary,
   output text, parallel function calls/argument deltas, completion, and usage;
   assert canonical ordered events and exactly one request per function call.
   Add a `response.failed` fixture asserting the nested provider error.
4. GREEN: implement the indexed Responses decoder behind the existing
   `Decoder` trait. Reuse the HTTP SSE pump unchanged.
5. RED continuation test: project a completed tool round, rebuild the next
   request, and assert input order is opaque reasoning item, synthetic
   `function_call`, then matching `function_call_output`.
6. GREEN: encode reasoning/tool history using existing `Part` and wire helpers.
7. Run `cargo test -p hya-provider && cargo test -p hya-core`.
8. Commit and push the atomic config/startup/protocol feature once both Tasks 2
   and 3 compile and their gates pass.

## Task 4 - Documentation and release metadata [AC5]

1. RED/non-code check: compare docs with parser tests and confirm current version
   is `0.33.13`.
2. Update `docs/configuration.md` with both OpenAI kinds and detailed model
   syntax. Archive the old root changelog as
   `docs/changes/CHANGELOG_0.33.13.md`, write only `0.33.14` notes to root
   `CHANGELOG.md`, and bump `[workspace.package].version` to `0.33.14`.
3. Run focused doc/config tests and inspect the diff.
4. Commit and push the atomic docs/version change after the full gate below.

## Task 5 - Final verification

1. Run `cargo fmt --all --check`.
2. Run `cargo clippy --workspace --all-targets -- -D warnings`.
3. Run `cargo test --workspace`.
4. Build local executables with `cargo build -p hya -p hya-backend`.
5. Run one real `gpt-5.6-sol` Responses smoke using configured credentials,
   without printing secrets: explicit `medium`, streamed reasoning/text or tool
   events, usage, and successful tool continuation.
6. Review `git diff`, `git status`, and commit contents; push only the atomic task
   files. Run Trellis quality/finish workflow and record evidence in
   `progress.md`.

## Test To Acceptance Map

| Proof | Acceptance |
| --- | --- |
| Config/runtime first-agent test | startup default before first request |
| Seven request-boundary cases | exact `gpt-5.6-sol` effort vocabulary |
| Existing Chat HTTP test | Chat endpoint and serialization unchanged |
| Responses SSE fixtures | text, reasoning, tools, completion, errors, usage |
| Event replay plus continuation test | stateless opaque reasoning round trip |
| Full workspace gate, build, live smoke | integration and release readiness |
