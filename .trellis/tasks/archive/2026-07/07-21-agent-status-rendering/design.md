# Agent status rendering design

## Scope

Fix the three confirmed boundaries in place. Keep the existing event log, projection, transcript synchronization, reasoning renderer, and lifecycle types unchanged.

## 1. Legacy prompt variant boundary

`PromptPayload` in `crates/hya-server/src/compat/session_prompt_legacy.rs` gains `variant: Option<String>`. Before the existing `model_ref_from_value` call, a trimmed non-empty top-level variant is inserted into an object-form `model`, replacing any nested variant. Missing or empty top-level values leave the nested model untouched; string-form models retain their current behavior. Serde continues to reject non-string top-level variants.

The existing model decoder and switch path remain the sole owners of model parsing and session mutation. This restores the selected variant before provider execution, allowing the projected user message, effort control, and reasoning event stream to follow their existing paths.

## 2. Observation lifetime

Remove the terminal-driven pane disposal mechanism from `packages/hya-tui-ts/src/upstream/routes/session/subagent-workspace.ts` and its dispatch sites in `index.tsx`: the `closeOnBlur` field, `terminal` workspace action, reducer branch, and terminal-session ID derivation used only by those dispatches.

Terminal completion changes presentation state only. An observation remains open until its existing explicit `close` action or a successful `reconcileSessions` call removes a session that no longer exists. Transcript synchronization and rendering do not change.

## 3. Lifecycle presentation

Add one typed presentation resolver beside the existing run-tree types in `subagent-workspace.ts`. It selects `member.status` when a transient member exists and otherwise uses `roster.status`, returning a text label and whether the agent is working.

| Source status | Label | Working |
| --- | --- | --- |
| `spawning`, `running`, `busy` | `Working` | yes |
| `done` | `Finished` | no |
| `failed` | `Failed` | no |
| `cancelled` | `Cancelled` | no |
| `idle` or absent | `Idle` | no |

The observation header and `DialogSubagent` import this resolver. Working observations use the existing `Spinner`; working dialog rows place the same spinner in the existing option gutter. Text remains visible so status is not color-only.

## Compatibility

- Existing nested variants, string-form model references, and requests without variants keep their current behavior.
- Existing explicit observation close and stale-session reconciliation remain intact.
- No persisted event, projection, database, or API response migration is required.

## Testing

- Extend the existing legacy model-switch integration test first to prove explicit top-level variant precedence and projection round-trip.
- Replace the two terminal auto-close reducer tests with retention assertions covering terminal completion and later focus changes; preserve reconciliation coverage.
- Add focused resolver tests for member precedence and every lifecycle label/working result.
- Run the TypeScript package checks and the repository Rust gates after focused tests pass.

## Release And Rollback

Update the patch version and one-version changelog required by repository policy after behavior tests pass. Rollback is the inverse atomic diff: restore the old prompt payload, pane policy, and raw status presentation together with their prior tests and version metadata. No data rollback is needed.
