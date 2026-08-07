# Fix batch G4 - runtime.md, storage.md, event-model.md, 0003-tmux-tui-single-input-readonly-panes.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/architecture/runtime.md`
- `docs/architecture/storage.md`
- `docs/architecture/event-model.md`
- `docs/adr/0003-tmux-tui-single-input-readonly-panes.md`

Do not create or edit any other file.

## What the three kinds of finding mean

- **CONTRADICTION** - the document says something the source does not support. The
  new writing introduced it. This is the worst kind: a reader trusts it today.
  Fix by correcting or DELETING the claim. Never leave the wrong text alongside a
  correction.
- **STILL OPEN** - an original gap the previous writer did not really close.
  Usually "thin": the feature is named but a reader still could not use it.
- **CRITIC** - something no gap entry covered, found by a fresh reader.

## Non-negotiable rules

1. **Open the cited source before you change anything.** Every finding names a
   `file:line`. The auditor may itself be wrong - if the source supports the
   current documentation, KEEP it and say so in your report. Do not "fix" correct
   text because a report told you to.
2. Deleting an unsupported claim is a valid and often correct fix. Do not invent
   replacement behaviour to fill the space.
3. Do not weaken precise contract wording into vague prose. Some sentences in
   these documents are asserted verbatim by tests in `crates/hya-bundle/tests/`;
   if you rewrite a sentence that reads like a contract, keep its exact terms.
4. Edit only your files. Other writers are working in parallel.
5. Do not run `git commit`.

## Findings


### `docs/architecture/runtime.md`

**STILL OPEN 1 - RuntimeCatalogRefresh trait** (`thin`)

- Source: `crates/hya-core/src/engine.rs:149`
- Why it is still open: runtime.md:55-61 is a 5-line prose paragraph that says what the trait is for and when it fires, but never gives the one thing an implementor needs: the method. `async fn refresh_if_changed(&self, runtime: &RuntimeRegistry) -> Result<bool, CoreError>` is absent, as is its return contract (Ok(true) = a new generation was published, Ok(false) = nothing changed, Err aborts the bind entirely — see the rustdoc at engine.rs:157-171 and the call site `bind_root_runtime` at engine.rs:492-500). A reader cannot implement the trait from the doc; they can only learn the trait exists. That is a name mention plus context, not a usable contract.

**STILL OPEN 2 - AgentSpec field types** (`thin`)

- Source: `crates/hya-core/src/engine.rs:95`
- Why it is still open: runtime.md:48-53 names the five fields (`name`, `model`, `system_prompt`, `workdir`, `reasoning`) but gives no types, unlike every comparable struct table elsewhere in the doc set. The omission is load-bearing for `workdir`: on `AgentSpec` it is a `PathBuf`, while the `workdir` on `Event::SessionCreated` / `SessionMoved` that the same doc set documents is a `String`. A reader reconciling the two surfaces gets no signal that they differ. Borderline call — the field names and purpose are there — but it falls short of "parameters and their semantics".


### `docs/architecture/storage.md`

**STILL OPEN 1 - [behavior] saved permissions (persistent allow-always)** (`thin`)

- Source: `crates/hya-tool/src/permission.rs:780-794, crates/hya-server/src/pending/saved_permission.rs:36-52`
- Why it is still open: The `## Saved permissions` section documents the row shape (`psv_<requestId>`, `project_id: "global"`, lowercase action, resource pattern), the global-scoping consequence, the store API and the two Compat routes — all correct. What is missing is the only semantic a reader actually needs: whether a saved row grants anything. Nothing outside `hya-server/src/pending/` ever reads these rows — `grep -rn 'list_saved_permissions|SavedPermission|save_permission' crates/` outside `hya-store/src/permission.rs` hits only `hya-server/src/pending/{permission,saved_permission}.rs` and a store test; `hya-app` and `hya-core` have zero references. `Decision::AllowAlways` is remembered only in the in-process `PermissionPlane` (`self.persistent` rules / `native_grants`), which is rebuilt empty at startup. So an `always` answer does NOT survive restart as a grant, even though the row does. The doc's closing line "Rows survive server restart because they live in the session SQLite file" is literally true but leads a reader to the opposite conclusion, and neither this section nor the `## Ask Flow` step 1 in tools-and-permissions.md states that the persisted rows are never re-loaded into the plane.


### `docs/architecture/event-model.md`

**CONTRADICTION 1**

- The doc claims: Line 89-90: the `agent_switched` / `model_switched` rows describe the payload's `message: Option<MessageId>` as an "optional message anchors transcript position; not stored on the session row".
- Reality: The engine never anchors these to a transcript position. `SessionEngine::switch_agent` and `switch_model` (crates/hya-core/src/engine/session_state.rs:8-34) both hardcode `message: Some(MessageId::new())` — a freshly minted id that matches no existing message in the projection. Downstream, `compat/session_context_messages.rs:41-50` uses that id as the identity of a *synthetic* switch pseudo-message it injects into the compat message list. So the field is the id of a synthetic switch row, not a pointer to a real transcript position. A client that tries to look the id up among `SessionProjection.messages` will always miss.
- Source: `crates/hya-core/src/engine/session_state.rs:8-34, crates/hya-server/src/compat/session_context_messages.rs:41-50`

**CONTRADICTION 2**

- The doc claims: Line 174: the `mail_sent` reducer row states "channel -> channel log + fan-out to current subscribers".
- Reality: The reducer does not fan out to every current subscriber. `Projection::apply_event`'s `MailSent` arm (crates/hya-proto/src/projection.rs:683-704) walks the channel member set and `continue`s past any member whose roster entry is `mode.is_resident()` AND whose status is `Done` or `Failed`. ADR-0001 (docs/adr/0001-...md:21-26) states this correctly and explicitly warns the fan-out "reaches every current **eligible** subscriber, not literally every subscriber" — so the rewritten event-model.md row now contradicts the ADR it sits alongside, and understates a stopped actor's inbox behavior.
- Source: `crates/hya-proto/src/projection.rs:683-704`


### `docs/adr/0003-tmux-tui-single-input-readonly-panes.md`

**STILL OPEN 1 - Subagent roster dialog and lifecycle labels (gap #11)** (`contradicted`)

- Source: `packages/hya-tui-ts/src/upstream/routes/session/subagent-workspace.ts:151, routes/session/dialog-subagent.tsx:47-83`
- Why it is still open: docs/tui-reference.md now documents the roster correctly (title, v/s/r keys, lifecycle map, selectable Main row). But the entry's original wrong claim lives in ADR-0003 and was NOT corrected: the ADR still says "`main` may appear as a non-selectable root row" while `flattenRunTree` sets `selectable: node.session !== undefined` at depth 0 with the comment "Depth-0 Main is selectable so the roster can return focus to Main after a split". The ADR also still calls the surface the "Subagent manager" (shipped title is `Subagent roster - Tab|Vertical|Horizontal`), never mentions the `r` retry-on-error binding the gap explicitly asked to add, and still promises "a compact status/count indicator ... showing total live subagents plus attention counts such as blocked/permission/question states" that the session route does not render (it renders only the `pane.roster` + optional `session.background` hint line at index.tsx:1938-1958). ADR-0003 got historical-note corrections for the two navigation/new-output items but these were left stale.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
