# Batch E - event-model.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 1 file(s). Do not create or edit any other file.

- `docs/architecture/event-model.md`

You have **20 gap entries** and **3 stale claims** to resolve.

This file documents a WIRE CONTRACT. A wrong claim here misleads integrators building against the event stream. Verify each Event variant against the source enum before writing it.

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
   list does not count. 11 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/architecture/event-model.md`

**1. [behavior] pending permission requests + related-request fan-out** — `thin` · severity medium

- Source: `crates/hya-server/src/pending/permission.rs:79-101,277-292`
- Evidence: docs/architecture/tools-and-permissions.md:92-94 documents remember-scope coalescing of pending asks, but the SSE payload names `permission.asked` / `permission.replied` appear in no in-scope doc, and the asymmetry (an `always` or an exact-scope `reject` cascades, a plain `once` does not) is not stated.
- Write: Add the permission ask lifecycle to the event/SSE inventory: AskRequests are parked in a per-server pending map and published to clients as `permission.asked` and `permission.replied` SSE payloads. Document the fan-out rule precisely: answering one request also resolves every OTHER pending request in the same session that shares its RememberScope — an `always` reply and an exact-scope `reject` both cascade, while a plain `once` reply resolves only the request it answered.

**2. Event enum — full variant catalog (45 variants)** — `stale` · severity high

- Source: `crates/hya-proto/src/event.rs:20`
- Evidence: docs/architecture/event-model.md:29-45 lists 'major event groups' only. The list omits member lifecycle (MemberSpawned/MemberStatusChanged/MemberFinished), team registration/roster (AgentRegistered/AgentActivityChanged), and mail/channel (MailSent/ChannelJoined/ChannelLeft) entirely, and omits CommandExecuted. No per-variant payload is given anywhere. crates/hya-proto/src/event.rs has module-level //! docs but NO per-variant /// docs for variants 1-38.
- Write: Replace the bullet-list of 'major event groups' with a complete table of all 45 Event variants, grouped by family: session lifecycle, message lifecycle, step markers, text streaming, reasoning streaming, tool lifecycle, member lifecycle, team/roster/mail, error, Unknown. For each row give: snake_case wire `type` tag, payload fields with Rust types, and the one-line reducer effect (fold / no-op / compat-bridge-only). Note the enum is `#[serde(tag="type", rename_all="snake_case")]`. Read crates/hya-proto/src/event.rs:20-330 for the exact field lists.

**3. Session lifecycle event payloads — SessionCreated, SessionMoved, SessionTitled, SessionMetadataSet, SessionPermissionSet, SessionArchived, SessionShareSet, SessionShareCleared, AgentSwitched, ModelSwitched** — `thin` · severity medium

- Source: `crates/hya-proto/src/event.rs:22-67`
- Evidence: docs/architecture/event-model.md:33-34 names the group ('session metadata, title, archive/share, agent/model switch, and status') and :82-83 says these 'update session state'. No field lists, no types, no example JSON. Zero rustdoc on these ten variants.
- Write: In the new event table, document each with fields: SessionCreated{session, parent: Option<SessionId>, agent, model, workdir} (parent is the link session_lineage walks to find the team root); SessionMoved{session, workdir}; SessionTitled{session, title}; SessionMetadataSet{session, metadata: serde_json::Value}; SessionPermissionSet{session, permission: Vec<serde_json::Value>} (REPLACES the rule list, not merges); SessionArchived{session, archived: serde_json::Number}; SessionShareSet{session, url}; SessionShareCleared{session} (reducer sets share back to None); AgentSwitched{session, message: Option<MessageId>, agent} and ModelSwitched{session, message: Option<MessageId>, model} where the optional message anchors the switch to a transcript position.

**4. Event::SessionStatus and Event::CommandExecuted (reducer-ignored, compat-bridge-only events)** — `thin` · severity medium

- Source: `crates/hya-proto/src/event.rs:68-75`
- Evidence: docs/architecture/event-model.md:34 says 'and status' inside a group name; CommandExecuted appears nowhere in in-scope docs except docs/compat-parity.md:108 as the compat frame `command.executed`. Neither is listed in the event-model.md Projection section, and readers cannot learn that the reducer ignores both.
- Write: Document SessionStatus{session, status: serde_json::Value} as a free-form status ping that the reducer does NOT fold — it exists only to be bridged to the compat SSE `session.status` frame. Document CommandExecuted{session, command, arguments, message: MessageId} as the record that a /slash command produced a specific user message; also reducer-ignored, bridged to compat `command.executed`. Add an explicit 'events the reducer does not fold' subsection listing SessionStatus, CommandExecuted, StepStarted, ToolInputDelta, TextEnd.

**5. Message lifecycle events — MessageStarted, MessageFinished, MessageDeleted, PartDeleted** — `thin` · severity medium

- Source: `crates/hya-proto/src/event.rs:78-110`
- Evidence: docs/architecture/event-model.md:84 ('MessageStarted creates a message row in memory'), :93-94 ('delete events remove messages or parts', 'MessageFinished records finish reason'). No payload fields, no FinishReason enumeration, no mention that the engine force-emits MessageFinished on error/cancel. No rustdoc.
- Write: Document MessageStarted{session, message: MessageId, role: Role} as the event that creates the MessageProjection row; MessageFinished{session, message, role, finish: FinishReason, tokens: Option<TokenUsage>} and state that the engine force-emits it with Error or Cancelled on turn error / sidecar loss so clients never wait forever for a finish (see crates/hya-core/src/engine/turn.rs:459); MessageDeleted{session, message} (retain-by-id removal of the whole message); PartDeleted{session, message, part: PartId}.

**6. Event::TurnBindingRecorded and Event::UserPromptContextRecorded** — `thin` · severity medium

- Source: `crates/hya-proto/src/event.rs:85-95`
- Evidence: docs/architecture/event-model.md:85-87 mentions both in the Projection fold list ('stores the assistant message's lightweight ConfigGeneration', 'preserves prompt attachment metadata') but gives no payload fields and no emission rule.
- Write: Document TurnBindingRecorded{session, message, generation: ConfigGeneration} — emitted immediately after MessageStarted{Assistant} on every turn so the immutable runtime snapshot that ran the turn is durable before any provider call (crates/hya-core/src/engine/turn.rs:405). Document UserPromptContextRecorded{session, message, files: Vec<Value>, agents: Vec<Value>} — the @file / @agent mention context attached to a user prompt; note the engine short-circuits and emits nothing when both vectors are empty (crates/hya-core/src/engine/admission.rs:493).

**7. Event::StepStarted / Event::StepFinished (provider round markers)** — `thin` · severity low

- Source: `crates/hya-proto/src/event.rs:115-125`
- Evidence: docs/architecture/event-model.md:38 says 'step markers' in the group list and nothing else. The Projection fold list never mentions them, so a reader cannot learn they are reducer no-ops.
- Write: Document StepStarted{session, message, step: u32} and StepFinished{session, message, step, finish: FinishReason} as the boundary markers for one provider round inside an assistant message. State that both are reducer no-ops used by the UI and compat bridge only, and that StepFinished.finish defaults to Stop when replaying older logs that lacked the field.

**8. Text streaming events — TextStart, TextDelta, TextReplace, TextEnd** — `thin` · severity medium

- Source: `crates/hya-proto/src/event.rs:127-147`
- Evidence: docs/architecture/event-model.md:39,41 name 'text streaming' and 'text/reasoning replacement' as groups; :88-90 describes the fold generically. No fields, and no statement that TextEnd is a reducer no-op or that TextReplace is what the text_complete plugin hook produces.
- Write: Document TextStart{session, message, part: PartId} (reducer pushes an empty PartProjection::Text), TextDelta{session, message, part, text} (appends), TextReplace{session, message, part, text} (wholesale overwrite; this is the shape the `text_complete` plugin hook emits), TextEnd{session, message, part} (reducer no-op — state is already accumulated). Cross-link to the live-vs-durable text streaming rule described for collect_stream_round.

**9. Reasoning streaming events — ReasoningStart, ReasoningDelta, ReasoningEnd, ReasoningReplace** — `thin` · severity medium

- Source: `crates/hya-proto/src/event.rs:149-172`
- Evidence: docs/architecture/event-model.md:40-41 names the group; no fields anywhere. The `provider_data` payload on ReasoningEnd (encrypted thinking blocks) is documented nowhere in scope.
- Write: Document ReasoningStart{session, message, part}, ReasoningDelta{session, message, part, text}, ReasoningEnd{session, message, part, provider_data: Option<Value>} — call out that provider_data carries opaque provider state such as Anthropic encrypted thinking blocks and must be round-tripped back to the provider verbatim — and ReasoningReplace{session, message, part, text}.

**10. Tool lifecycle events — ToolInputStart, ToolInputDelta, ToolCallRequested, ToolResult, ToolError, ToolPartUpdated** — `thin` · severity high

- Source: `crates/hya-proto/src/event.rs:175-222`
- Evidence: docs/architecture/event-model.md:42-43 names 'tool input and tool result lifecycle' and 'tool-part state updates'; :91-92 describes the fold in one line each. No payload fields, no call_id/name/input/output/time_ms, no error `value` shape. Zero rustdoc on these six variants.
- Write: Document each with its fields and its ToolPartState transition: ToolInputStart{session, message, part, call: ToolCallId, name} → Pending; ToolInputDelta{session, message, part, text} streams raw argument JSON and is a reducer no-op (the compat bridge forwards it as pending `raw`); ToolCallRequested{session, message, part, call, name, input: Value} → Running, and this is the event the turn loop collects into its tool_calls list for the round; ToolResult{session, message, part, call, output: Value, time_ms} → Completed; ToolError{session, message, part, call, message_text, value: Value} → Error, where `value` is the structured {error:{type,message}} object; ToolPartUpdated{session, message, part, state: ToolPartState} directly overwrites the state and is used by fork/copy and out-of-band progress updates.

**11. Member lifecycle events — MemberSpawned, MemberStatusChanged, MemberFinished** — `undocumented` · severity high

- Source: `crates/hya-proto/src/event.rs:226-250`
- Evidence: grep for MemberSpawned / MemberStatusChanged / MemberFinished across README.md, CONTEXT.md, DESIGN.md, AGENTS.md, CHANGELOG.md and docs/**/*.md (excluding changes/ and superpowers/) returns ZERO hits. docs/architecture/event-model.md:29-45 does not list a member-lifecycle group. The variants have no rustdoc (unlike their team-comms neighbours at event.rs:254+).
- Write: Add a 'Member lifecycle' section. MemberSpawned{session, member: MemberId, child: Option<SessionId>, subagent_type, description, depth} is appended to the PARENT's log — this is what makes the agent tree observable without leaking child transcripts; it sets MemberProjection.status to Spawning. MemberStatusChanged{session, member, status: MemberRunStatus} updates the member row on the parent's log. MemberFinished{session, member, status, summary} carries a BOUNDED summary string and never the child transcript. State clearly that these live on the parent log while AgentRegistered/AgentActivityChanged/MailSent live on the TEAM-ROOT log — the two are different logs whenever the parent is not the root.

**12. Event::Error** — `thin` · severity low

- Source: `crates/hya-proto/src/event.rs:314`
- Evidence: docs/architecture/event-model.md:44 says 'runtime errors' in the group list. No payload. docs/compat-parity.md:108 mentions the compat `session.error` frame but not the source event shape.
- Write: Document Event::Error{session: Option<SessionId>, code, message} — note that `session` is optional so a global (session-less) error is expressible, that Event::session() returns None for that case, and that it is bridged to the compat `session.error` frame.

**13. FinishReason (stop | tool_calls | length | cancelled | error)** — `undocumented` · severity medium

- Source: `crates/hya-proto/src/message.rs:19`
- Evidence: No rustdoc on the enum. docs/architecture/event-model.md:63 mentions 'finish reason' as a Message::Assistant field and :94 says MessageFinished 'records finish reason', but the five variants are never enumerated in any in-scope doc.
- Write: Add FinishReason to the Messages and Parts section with its five snake_case wire values — stop, tool_calls, length, cancelled, error — and note it is the terminal state of BOTH a message (MessageFinished) and a provider round (StepFinished), and that StepFinished defaults to Stop when absent from old logs.

**14. TokenUsage** — `undocumented` · severity medium

- Source: `crates/hya-proto/src/message.rs:94`
- Evidence: No rustdoc. grep for 'TokenUsage' across in-scope docs returns zero hits. docs/architecture/event-model.md:63 only says 'optional usage'. docs/architecture/storage.md:100-109 documents the token_ledger row fields, which are a DIFFERENT shape.
- Write: Document TokenUsage with its five counters (input, output, reasoning, cache_read, cache_write), the `prompt`/`completion` serde aliases accepted on decode, and the two aggregation rules that differ: TokenUsage::merge() takes the MAX per field (provider re-reports cumulative totals), while the turn loop SUMS across provider rounds. Contrast it explicitly with the token_ledger row shape documented in storage.md so readers do not conflate the two.

**15. CostBreakdown** — `undocumented` · severity low

- Source: `crates/hya-proto/src/message.rs:123`
- Evidence: No rustdoc; zero hits across all in-scope docs. docs/architecture/storage.md:110-111 mentions usage reporting is off but never names CostBreakdown, and the `cost_json` column on the `message` table is undocumented too.
- Write: Document CostBreakdown{input_usd, output_usd} as the per-message cost pair, and note it is the value persisted into the `message.cost_json` column. State that live HTTP routes currently report usage_reporting: false, so this is populated only where a provider supplies pricing.

**16. EventSeq semantics — globally monotonic AUTOINCREMENT rowid, and seq 0 reserved for un-persisted live publishes** — `contradicted` · severity high

- Source: `crates/hya-proto/src/ids.rs:460`
- Evidence: The rustdoc reads 'Monotonic per-session event sequence (the `event_log.seq` rowid)'. The schema at crates/hya-store/migrations/0001_init.sql:38 is `seq INTEGER PRIMARY KEY AUTOINCREMENT` on a single shared event_log table, so seq is GLOBALLY monotonic, not per-session — gaps between consecutive events of one session are normal. Neither the rustdoc nor docs/architecture/storage.md:70 mentions that seq 0 is reserved for live-only (never persisted) publishes emitted by SessionEngine::publish_live.
- Write: State that EventSeq is the globally monotonic event_log AUTOINCREMENT rowid shared by every session — so a single session's envelope sequence has gaps, and clients must treat it as strictly-increasing-but-not-contiguous. Then state that seq 0 is a reserved sentinel meaning 'live-only, not persisted': the engine publishes high-frequency text/reasoning deltas at seq 0 and Projection::apply folds them WITHOUT advancing last_seq. Also fix the rustdoc at ids.rs:460 which currently says 'per-session'.

**17. Projection::apply — the seq==0 live-only branch** — `contradicted` · severity high

- Source: `crates/hya-proto/src/projection.rs:212`
- Evidence: docs/architecture/event-model.md:99-108 presents the idempotence rule as exactly `if env.seq.0 <= self.last_seq { return; }` and concludes 'a caller can safely ignore duplicate older envelopes'. docs/project-structure.md:69-71 repeats the same claim. The real code has a PRECEDING branch: `if env.seq.0 == 0 { self.apply_event(&env.event); return; }` — seq-0 envelopes are applied unconditionally and never advance last_seq, so they are NOT idempotent on redelivery.
- Write: Correct the code excerpt to show both branches in order. Explain: seq==0 means a live-only publish (SessionEngine::publish_live) — it is applied but does not advance last_seq, so it is deliberately outside the idempotence guarantee and must never be replayed; seq<=last_seq is a no-op, which is what makes SSE reconnect and duplicate delivery safe. Apply the same correction to docs/project-structure.md:69-71.

**18. MessageProjection fields (config_generation, files, agents)** — `thin` · severity medium

- Source: `crates/hya-proto/src/projection.rs:58`
- Evidence: No rustdoc on the struct. docs/architecture/event-model.md:84-87 says MessageStarted 'creates a message row in memory' and that TurnBindingRecorded/UserPromptContextRecorded store the generation and prompt attachments, but never lists the projected row's fields.
- Write: Add a table for MessageProjection: id, role, config_generation (from TurnBindingRecorded), finish, tokens, files and agents (from UserPromptContextRecorded), parts. Pair it with SessionProjection's field list (id, parent, agent, model, workdir, title, metadata, permission, archived, share, messages, members) so a reader can see exactly what a replay reconstructs.

**19. PartProjection — media parts have NO projection variant** — `contradicted` · severity high

- Source: `crates/hya-proto/src/projection.rs:75`
- Evidence: docs/architecture/event-model.md:67 lists `Part::Media` in the Messages and Parts table as a first-class part type, and the adjacent Projection section (:88-92) describes text/reasoning/tool folding without ever saying media is dropped. PartProjection is tagged on `kind` with exactly three variants: text | reasoning | tool — there is no media arm, so media attachments do not survive the fold.
- Write: Add an explicit note under the Projection section: PartProjection has exactly three variants (text, reasoning, tool{call, name, state}) tagged on `kind`. Part::Media exists on the model-facing Message/Part value type but has NO PartProjection counterpart, so media attachments are not reconstructed by replay. Anyone relying on media surviving a projection read must go to the raw event log.

**20. collect_stream_round — live-only deltas re-emitted as a durable TextStart/TextReplace/TextEnd triple** — `undocumented` · severity high

- Source: `crates/hya-core/src/engine/stream_round.rs:24`
- Evidence: crates/hya-core/src/engine/stream_round.rs has 142 lines and ZERO doc comments. docs/architecture/runtime.md:63-65 says only 'Streams provider events. Appends text, reasoning, and tool-input events.' Nothing in scope explains that Text* events are published live at seq 0 and only persisted after the stream ends — which is why replaying an event log yields a different (smaller) event sequence than an SSE subscriber saw.
- Write: Add a section 'Live-only vs durable streaming'. During a provider round, Text events are published live at seq 0 and NOT persisted; after the stream ends the accumulated text is re-emitted durably as a TextStart → TextReplace → TextEnd triple carrying the final content. Consequences readers must know: (1) an SSE subscriber sees many TextDelta frames that GET /sessions/:id/events will never return; (2) a replay of the log produces the final text in one TextReplace instead; (3) Projection state is identical either way, which is what makes the two paths interchangeable. Also state that within collect_stream_round, ToolCallRequested events are collected into the round's tool_calls list and MessageFinished from the provider is swallowed (its finish + tokens are returned as the StreamRound result rather than re-emitted).

**STALE 1.** The document claims: Presents the reducer's idempotence rule as exactly `if env.seq.0 <= self.last_seq { return; }` and concludes "a caller can safely ignore duplicate older envelopes during replay or after an SSE reconnect."

- Reality: Projection::apply (crates/hya-proto/src/projection.rs:212) has a PRECEDING branch: `if env.seq.0 == 0 { self.apply_event(&env.event); return; }`. Seq-0 (live-only) envelopes are applied unconditionally and never advance last_seq, so they are explicitly outside the idempotence guarantee.
- Action: correct or delete. Do not merely supplement.

**STALE 2.** The document claims: The "Major event groups" list enumerates session, message, step, text, reasoning, tool, error and resident-work groups.

- Reality: The list omits three whole families that exist in crates/hya-proto/src/event.rs: member lifecycle (MemberSpawned/MemberStatusChanged/MemberFinished, event.rs:226-250), team registration and roster (AgentRegistered/AgentActivityChanged, event.rs:257-278), and mail/channels (MailSent/ChannelJoined/ChannelLeft, event.rs:292-312). It also omits CommandExecuted and Event::Unknown.
- Action: correct or delete. Do not merely supplement.

**STALE 3.** The document claims: Lists `Part::Media` — "Canonical media attachment with MIME type, data, and optional filename" — in the Messages and Parts table immediately above the Projection section.

- Reality: PartProjection (crates/hya-proto/src/projection.rs:75) is tagged on `kind` with exactly three variants: text | reasoning | tool. There is no media arm, so media parts do NOT survive the fold and cannot be recovered from read_projection.
- Action: correct or delete. Do not merely supplement.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 20 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
