# Batch P - 0001-event-sourced-mailbox-and-channels.md, 0002-resident-actor-model-and-autonomous-main-agent.md, 0003-tmux-tui-single-input-readonly-panes.md, 0006-tui-session-reset-and-subagent-visibility.md, CONTEXT.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 5 file(s). Do not create or edit any other file.

- `docs/adr/0001-event-sourced-mailbox-and-channels.md`
- `docs/adr/0002-resident-actor-model-and-autonomous-main-agent.md`
- `docs/adr/0003-tmux-tui-single-input-readonly-panes.md`
- `docs/adr/0006-tui-session-reset-and-subagent-visibility.md`
- `CONTEXT.md`

You have **5 gap entries** and **3 stale claims** to resolve.

ADRs record decisions as they were made. Correct statements of FACT about current behaviour, but do not rewrite the historical decision or its context. If a decision was later reversed, note that rather than deleting it.

## Non-negotiable rules

1. **Confirm every claim against the source before you write it.** Every entry
   below carries a `source` reference. Open it. If the source contradicts the
   entry, the SOURCE WINS -- write what the code does and report the discrepancy.
2. **If you cannot confirm a claim from source, do not write it.** Say you could
   not confirm it. Plausible prose that is wrong is worse than an admitted gap,
   because a reader trusts the document.
3. **Stale and contradicted entries are corrected or deleted, never merely
   supplemented.** A document that contradicts the code is a defect.
4. **Do not edit any file outside your batch.** Other writers are working in
   parallel. In particular never touch `docs/README.md`, `README.md`, `AGENTS.md`,
   `DESIGN.md`, or `docs/project-structure.md` -- a later reconciliation pass owns
   all cross-links and the docs map. Some entries below suggest edits to other
   files; ignore that part and write only your own.
5. **Match the existing documentation style.** Read the file you are editing
   before writing. Use the project's vocabulary as defined in `CONTEXT.md`.
6. **A feature counts as documented only if a reader can use it** from what you
   write: what it does, its parameters or keys, and its semantics. A name in a
   list does not count. 2 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/adr/0001-event-sourced-mailbox-and-channels.md`

**1. Channel fan-out skips terminal residents** — `undocumented` · severity medium

- Source: `crates/hya-proto/src/projection.rs:630`
- Evidence: Not mentioned in docs/adr/0001-event-sourced-mailbox-and-channels.md (which only says channel sends fan out to 'every current subscriber'), nor in CONTEXT.md:184-188 (same wording), nor in docs/architecture/event-model.md. No rustdoc at the fan-out site.
- Write: Add a Consequences bullet: when the reducer folds a MailSent addressed to a channel, subscribers whose RosterEntry is resident AND whose RosterStatus is Done or Failed are SKIPPED, so a stopped actor's inbox stops growing. Correct the 'every current subscriber' phrasing in CONTEXT.md:184-188 and in this ADR to 'every current eligible subscriber' and define eligible.

**2. SessionStore::append_direct_mail / append_channel_mail — rejection and eligibility rules** — `thin` · severity high

- Source: `crates/hya-store/src/mailbox.rs:38`
- Evidence: docs/adr/0001-event-sourced-mailbox-and-channels.md describes the event-sourced design but not the writer-transaction rejection rules. crates/hya-core/src/engine/mailbox.rs:127-131 has good rustdoc for the ENGINE side ('returns a receipt with the resolved sender handle and the recipient count') but the store-side rejection set (unknown handle, transient non-root target, stopped/terminal resident) and resident_member_is_eligible (crates/hya-store/src/mailbox.rs:500) are documented nowhere.
- Write: Add a 'Delivery rules' section. append_direct_mail runs BEGIN IMMEDIATE → optional claim fence → replay the root projection → REJECT with MailboxRejected for an unknown handle, a transient non-root target, or a stopped/terminal resident → append MailSent → commit; the caller publishes the returned Envelope only AFTER commit. append_channel_mail uses the same writer-lock discipline but COUNTS eligible subscribers instead of rejecting. Define eligibility precisely (resident_member_is_eligible): a non-resident member always counts; a resident counts only if its RosterStatus is neither Done nor Failed AND it currently holds an `active` row in resident_actor_claim — the durable liveness check behind mail rejection.

### `docs/adr/0002-resident-actor-model-and-autonomous-main-agent.md`

**1. Explicit-stop cursor advance (AgentActivityChanged{Failed, "resident stopped"})** — `undocumented` · severity medium

- Source: `crates/hya-proto/src/projection.rs:543`
- Evidence: Not mentioned anywhere in scope. docs/adr/0002-resident-actor-model-and-autonomous-main-agent.md:41-46 describes the inbox cursor only in terms of a completed turn advancing it. The magic string "resident stopped" and its cursor-jump effect appear in no doc.
- Write: Add to the crash-recovery/fencing section: an AgentActivityChanged with status=Failed AND current_task == "resident stopped" is treated by the shared reducer as an EXPLICIT stop, not a failure — it jumps resident_cursor to the full inbox length so a later restart of that handle does not replay mail the stopped actor never needed to see. Name the exact literal, because it is a load-bearing sentinel that crates/hya-store/src/mailbox.rs:225 (finalize_resident_stop) writes and the reducer reads.

**2. resident_effect_terminal_events — the terminal cleanup event set for a lost actor** — `undocumented` · severity medium

- Source: `crates/hya-store/src/mailbox.rs:342`
- Evidence: docs/adr/0002:33-46 says startup 'aborts old-epoch running work' and docs/architecture/runtime.md:170-171 says it 'terminalizes each old epoch's actor-bound admissions and running work', but neither names the events written. The literal error code STALE_ACTOR_CLAIM appears in no in-scope doc.
- Write: Enumerate exactly what recovery appends for a lost actor: MemberFinished{status: Cancelled} for every member still Spawning or Running; a ToolError carrying value {"code":"STALE_ACTOR_CLAIM"} for every tool part still Pending or Running; and MessageFinished{Cancelled} for every unfinished assistant message. State that clients can key off the STALE_ACTOR_CLAIM code to distinguish takeover cleanup from a genuine tool failure.

**3. SessionStore::finalize_resident_stop / finalize_resident_failure and RecoveredResidentWork** — `thin` · severity medium

- Source: `crates/hya-store/src/mailbox.rs:225`
- Evidence: docs/adr/0002:53-55 covers release-aborts-admissions generally. Neither function name appears in scope, nor does the 'resident stopped' reason literal, nor the idempotence rule, nor the RecoveredResidentWork/RecoveredResidentOutcome shapes returned to the caller.
- Write: Document finalize_resident_stop / finalize_resident_failure: they terminalize an actor with a reason ('resident stopped' for an explicit stop — the same literal the reducer keys on for the cursor jump), abort its accepted/started admissions marking the started ones logical_released, append AgentActivityChanged{Failed}, and flip the claim to released. State they are IDEMPOTENT when a matching released claim already reached the same state. Document the returned RecoveredResidentWork / RecoveredResidentOutcome = Idle | Queued{inbox_cursor} | AbortedRunning{inbox_cursor, queued_after}, returned together with the envelopes to publish and the admissions that were aborted.

### `docs/adr/0003-tmux-tui-single-input-readonly-panes.md`

**STALE 1.** The document claims: "Navigation initially reuses existing observation controls: Ctrl+X . cycles focus, Ctrl+X W closes the focused observation view, and Escape returns to the main view. No dedicated tab-next/tab-prev bindings are introduced for this redesign." and "manual scroll pins that view and surfaces a new-output indicator until the user returns to bottom."

- Reality: The shipped keymap adds `<leader>right` / `<leader>left` pane cycling (config/keybind.ts:101) and unmodified digit 1-9 pane jumping while an observation is focused (routes/session/index.tsx:571). The observation pane header renders handle/agent_type/lifecycle/task/placement/focus/read-only plus a working spinner (index.tsx:1343); no new-output indicator is implemented.
- Action: correct or delete. Do not merely supplement.

### `docs/adr/0006-tui-session-reset-and-subagent-visibility.md`

**STALE 1.** The document claims: "The TUI treats `/new` as a clean Session-screen reset: it asynchronously aborts the old active Turn, clears local prompt bookkeeping, navigates immediately, and lazily creates the next persisted Session only when the next prompt is submitted."

- Reality: `session.new` in packages/hya-tui-ts/src/upstream/app.tsx:539-551 only calls `route.navigate({type: "home"})` and `dialog.clear()`. It issues no abort and touches no prompt bookkeeping; the old session is left running untouched on the server.
- Action: correct or delete. Do not merely supplement.

**STALE 2.** The document claims: "The live TUI timeline retains compact subagent activity rows only for failed or cancelled terminal outcomes; successful lifecycle events remain represented by the existing tool-call row without a duplicate activity row." and "The sidebar shows busy or attention-needed named Roster entries."

- Reality: `TaskMemberRow` (routes/session/index.tsx:2710) renders a row per delegated member in every state, with ✓/✗/│ icons and `Working...`/summary/duration detail lines, and members present only in the run tree are synthesized as extra rows on the last assistant message (index.tsx:1878). The sidebar has no roster section at all — its sections are Context, MCP, LSP, Todo, Modified Files and a footer (feature-plugins/sidebar/*).
- Action: correct or delete. Do not merely supplement.

### `CONTEXT.md`

_No entries. Verify against source and report if it is already complete._

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 5 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
