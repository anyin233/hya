# Fix batch H2 - server-client.md, storage.md, runtime.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/architecture/server-client.md`
- `docs/architecture/storage.md`
- `docs/architecture/runtime.md`

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


### `docs/architecture/server-client.md`

**CONTRADICTION 1**

- The doc claims: 500 Internal Server Error covers 'engine Invalid("session not found") on native admit/turn when the session is missing' (and :49 claims native turn paths do not map that error to 404).
- Reality: Native admit/turn never produces Invalid("session not found") — admit_user_prompt has no existence check. Only summarize_session/summary_messages raise it, and the Compat routes map it to 404 (session_summarize.rs:98) / a session_not_found response (session_wait.rs:40), never 500.
- Source: `crates/hya-core/src/engine/summary.rs:29, crates/hya-server/src/compat/session_summarize.rs:93-101, crates/hya-server/src/compat/session_wait.rs:35-43, crates/hya-core/src/engine/admission.rs:372-448`

**CONTRADICTION 2**

- The doc claims: PromptResponse is shown as `"message": "<user MessageId>"` and is labelled '(also returned by command and shell)'.
- Reality: True for /prompt and /command (both return admit_user_prompt / admit_command_prompt's USER message id), but /sessions/:id/shell returns run_shell's ASSISTANT message id. One shape, two different message identities — the '<user MessageId>' annotation is wrong for the shell route. (The rustdoc on api.rs:61 is wrong in the opposite direction, calling it the assistant id for all three.)
- Source: `crates/hya-server/src/lib.rs:196-211, crates/hya-core/src/engine/shell.rs:30-113, crates/hya-proto/src/api.rs:58-65`

**STILL OPEN 1 - ApiError status mapping (404 not_found and 503 service_unavailable)** (`contradicted`)

- Source: `crates/hya-server/src/lib.rs:54`
- Why it is still open: The 404/503 rows themselves are correct, but the status table now carries a fabricated claim about the one error it singles out. Row 49 says 'Native turn paths do not map engine "session not found" here' and row 52 says 500 covers 'engine Invalid("session not found") on native admit/turn when the session is missing'. Neither is true. `Invalid("session not found")` is produced ONLY by crates/hya-core/src/engine/summary.rs:29 and :54 (summarize_session / summary_messages). The native prompt/command/shell handlers (crates/hya-server/src/lib.rs:160-210) call admit_user_prompt -> run_turn, and admit_user_prompt (crates/hya-core/src/engine/admission.rs:372-448) has no session-existence check at all — it just emits into the log — so no such error can reach a native turn route. Where the error IS raised, Compat translates it to 404, not 500 (crates/hya-server/src/compat/session_summarize.rs:98 -> ApiError::not_found; crates/hya-server/src/compat/session_wait.rs:40 -> session_not_found). A reader using this table to predict status codes gets the opposite answer.

**CRITIC 1 - SSE `permission.asked` / `permission.replied` / `question.asked` / `question.replied` / `question.rejected` frames on `/event`, `/api/event`, and `/global/event`**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-server/src/pending/permission.rs:448-480 (`permission_asked_event`, `permission_replied_event`); /chivier-disk/yanweiye/Projects/yaca/crates/hya-server/src/pending/question.rs:204-226,264-275 (`publish_replied`, `publish_rejected`, `question_asked_event`); merged into all three SSE routes at /chivier-disk/yanweiye/Projects/yaca/crates/hya-server/src/compat/event.rs:53-68, 111-133, 141-217`
- Why it matters: These are the only signal an HTTP integrator gets that the agent is blocked waiting for a permission decision or a `question`/`ask_user` answer. A client that does not handle them will hang forever mid-turn with no visible cause. The frame shapes are also non-obvious: `permission.asked` carries a `LegacyPermissionRequestView`, `permission.replied` carries `{sessionID, requestID, reply}` where `reply` is `once|always|reject`, `question.asked` carries `{id, sessionID, questions[]}` with per-question `{question, header, options[], multiple, custom}`, and `question.replied` carries `answers: string[][]` (one array per question). Nothing in docs/** states any of this. `docs/compat-parity.md:108` enumerates the session/message/part frames and omits the permission/question family entirely; `permission.asked` and `question.asked` appear in docs/** only in the attention-sound table at docs/tui-reference.md:499-500, which is about TUI notification sounds, not the wire contract; `permission.replied`, `question.replied`, and `question.rejected` appear nowhere in docs/**.

**CRITIC 2 - `session.next.step.started` / `session.next.step.ended` frames emitted on `/api/event` and `/global/event`**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-server/src/compat/event.rs:485-495 (`api_envelope_payload` maps `Event::StepStarted`/`StepFinished`) and 542-579 (`step_started_event_payload`, `step_ended_event_payload`); wired at event.rs:101 (`/api/event`) and event.rs:174 (`/global/event`)`
- Why it matters: The same engine event is rendered differently on the two SSE surfaces: legacy `/event` turns `StepStarted`/`StepFinished` into `message.part.updated` step parts (event.rs:315-333), while `/api/event` and `/global/event` turn them into `session.next.step.started` / `session.next.step.ended`. An integrator that ports a client from one stream to the other silently loses turn boundaries. Worse, the only doc sentence touching this asserts the opposite: docs/compat-parity.md:108 ends with "...before hya implemented any durable `session.next.*` stream", which reads as "hya emits no `session.next.*` frames". The payloads are also undocumented: `started` carries `{timestamp, sessionID, assistantMessageID, agent, model}`, `ended` carries `{timestamp, sessionID, assistantMessageID, finish, cost, tokens}`. `docs/project-structure.md:265` names the `session.next.*` family only as a label for hya-sdk's reducer module, and `hya_sdk::reducer::V2Event` is a public `#[non_exhaustive]` enum with ~29 variants of which hya's server emits exactly these two — an integrator has no way to learn which are live.

**CRITIC 3 - Pending permission/question queues are replayed as a snapshot on `/global/event` connect, and re-snapshotted on broadcast lag — but are silently dropped on legacy `/event`**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-server/src/compat/event.rs:143-164 (`snapshot_asked()` chained after the `server.connected` frame) and 182-215 with `recover_pending` at 220-232; contrast /chivier-disk/yanweiye/Projects/yaca/crates/hya-server/src/compat/event.rs:53-65 where legacy `/event` has no connect snapshot and maps `Err(_lagged) => None``
- Why it matters: This decides whether a client can safely reconnect. On `/global/event` a late or reconnecting client receives every currently-pending `permission.asked`/`question.asked` immediately after `server.connected`, and a lagged consumer gets the full pending set re-emitted instead of a gap — so reconnect is lossless and the client must expect duplicate `asked` frames and dedupe by `requestID`. On legacy `/event` neither happens: a client that connects after a permission was raised, or that falls behind the broadcast buffer, never sees the prompt and the session appears to hang with no recovery path other than polling `GET /permission`. Nothing in docs/** describes either behavior or the divergence.

**CRITIC 4 - `resync` SSE frame is also emitted by the Compat streams `/event`, `/api/event`, and `/global/event`**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-server/src/compat/event.rs:49, 105, 178 — each live-envelope stream maps `Err(_lagged)` to `SseEvent::default().event("resync")``
- Why it matters: `resync` is documented only for the native route: docs/architecture/server-client.md:143-146 scopes it to `GET /sessions/:id/stream`, and docs/troubleshooting.md:175-177 repeats that scope. An integrator building on the Compat streams (which is what hya-sdk and the TUI use) will receive an SSE event with no `data` field and an unrecognized event name, and — per the documented native contract — has no equivalent catch-up endpoint, since `?since_seq=` exists only on `/sessions/:id/events`. The doc needs to say `resync` appears on the Compat streams too and state how to recover there.


### `docs/architecture/storage.md`

**CONTRADICTION 1**

- The doc claims: decode_session_key falls back to the 16-byte legacy UUID interpretation whenever the UTF-8 SessionId parse fails; 'that ordering is what lets both encodings coexist'.
- Reality: The fallback is gated on UTF-8 decoding failing, not on parsing failing. Valid-UTF-8 bytes that do not parse as a SessionId return None with no UUID attempt.
- Source: `crates/hya-store/src/lib.rs:258-263`

**STILL OPEN 1 - decode_session_key (BLOB key -> SessionId)** (`contradicted`)

- Source: `crates/hya-store/src/lib.rs:229`
- Why it is still open: storage.md:163 states the read rule as 'First try UTF-8 parse as SessionId (hysec_, ses_<uuid>, or raw uuid text); **only if that fails**, interpret the 16 bytes as a legacy raw UUID', then asserts at :165 'That ordering is what lets both encodings coexist in one session_id column.' The source (crates/hya-store/src/lib.rs:258-263) returns EARLY on any successful UTF-8 decode: `if let Ok(raw) = std::str::from_utf8(key) { return raw.parse().ok(); }` — the UUID fallback runs only when the bytes are NOT valid UTF-8, never when the parse fails. A 16-byte legacy UUID whose bytes happen to be valid UTF-8 (all bytes < 0x80) decodes to None instead of a SessionId. Anyone implementing a compatible decoder from this prose writes the wrong control flow.


### `docs/architecture/runtime.md`

**CONTRADICTION 1**

- The doc claims: 'Contract for clients: a UI that has seen MessageStarted is guaranteed to eventually see MessageFinished, so it never spins forever waiting for a finish event.'
- Reality: The force-emit is gated on `outcome.is_err() && !matches!(&outcome, Err(CoreError::Cancelled))`, so a turn that ends with CoreError::Cancelled and no sidecar-loss token fire emits nothing. Three such paths exist: the round-top activation-hook health gate (turn.rs:547), the post-before-hook health gate (turn.rs:~712), and the resident tool-cancel path (turn.rs:~805, actor_claim.is_some() && ToolError::Cancelled && cancel.is_cancelled()). In each the assistant message stays open. The guarantee holds for the three rows in the table above it, not universally.
- Source: `crates/hya-core/src/engine/turn.rs:455-490 (force-emit guards), :546-548, :711-716, :805-810`

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
