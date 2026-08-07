# Fix batch F5 - runtime.md, storage.md, 0001-event-sourced-mailbox-and-channels.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/architecture/runtime.md`
- `docs/architecture/storage.md`
- `docs/adr/0001-event-sourced-mailbox-and-channels.md`

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

**CONTRADICTION 1**

- The doc claims: publish_live is 'Used for high-frequency streaming text/reasoning deltas' and at round end 'those parts are re-emitted durably as a TextStart / TextReplace / TextEnd triple (or the corresponding reasoning events)' (lines 30-34); the per-round list repeats 'live text/reasoning via publish_live' (line 167-168).
- Reality: publish_live has exactly two call sites, both guarded by `matches!(&event, Event::TextStart{..} | Event::TextDelta{..} | Event::TextEnd{..})`. Every reasoning event goes to `self.emit_for_actor(actor_claim, session, event)` and is durable immediately; only `durable_text_parts` is re-emitted as the triple. No reasoning re-emission exists.
- Source: `crates/hya-core/src/engine/stream_round.rs:62-124, crates/hya-core/src/engine.rs:641`

**CONTRADICTION 2**

- The doc claims: Tier 2 (fallback) 'falls back to the local ModelSummarizer via compact_with, which writes a system message carrying the HYA_COMPACTED_CONTEXT marker as a plain local summary (no Responses item array).'
- Reality: compact_with builds `Message::System { content: format!("Summary of {older_count} earlier messages:\n{summary}") }` in a returned Vec and nothing else — no marker string, no emit, no store write. The turn loop only assigns `messages = compacted` for that round's request. Only engine/summary.rs::compact_context prefixes HYA_COMPACTED_CONTEXT, and it is the explicit /compact path, not the fallback.
- Source: `crates/hya-core/src/compaction.rs:135-153, crates/hya-core/src/engine/turn.rs:603-621, crates/hya-core/src/engine/summary.rs:20`

**STILL OPEN 1 - SessionEngine::publish_live and SessionEngine::publish_envelope** (`contradicted`)

- Source: `crates/hya-core/src/engine.rs:557`
- Why it is still open: The original wrong claim was rewritten but replaced with a NEW wrong claim. runtime.md:30-34 says publish_live is 'Used for high-frequency streaming text/reasoning deltas. At round end those parts are re-emitted durably as a TextStart / TextReplace / TextEnd triple (or the corresponding reasoning events)', and runtime.md:167-168 repeats 'live text/reasoning via publish_live'. grep shows publish_live has exactly two call sites (stream_round.rs:75 and :89), both inside the `matches!(event, TextStart|TextDelta|TextEnd)` branch. Reasoning events fall through to `self.emit_for_actor(...)` (stream_round.rs:92) and are durable on first emit; there is no reasoning re-emission loop (only durable_text_parts is re-emitted). event-model.md:134-135 states the correct behaviour, so the two documents now contradict each other on the same seam.

**STILL OPEN 2 - Compaction fallback chain and the HYA_COMPACTED_CONTEXT marker** (`contradicted`)

- Source: `crates/hya-core/src/engine/turn.rs:569`
- Why it is still open: runtime.md:490-493 claims Tier 2 'falls back to the local ModelSummarizer via compact_with, which writes a system message carrying the HYA_COMPACTED_CONTEXT marker as a plain local summary'. compaction.rs:135-153 shows compact_with returns an in-memory Vec<Message> whose injected system message content is `format!("Summary of {older_count} earlier messages:\n{summary}")` — no marker, and nothing is emitted or persisted (turn.rs:616 just does `messages = compacted`). Only compact_context (engine/summary.rs:20, the explicit /compact path) writes the marker. Because compacted_messages (engine/turn/messages.rs:85-95) selects on `starts_with(COMPACT_CONTEXT_MARKER)`, the Tier-2 fallback leaves no durable marker at all and is redone from scratch every round — the opposite of what runtime.md:495-497 tells the reader.


### `docs/architecture/storage.md`

**STILL OPEN 1 - SessionStore::replay_sync_events / sync_history** (`thin`)

- Source: `crates/hya-store/src/sync.rs:9`
- Why it is still open: storage.md:279-290 documents the dedup and watermark semantics but not the returned JSON shape, which differs between the two calls. replay_sync_events returns clones of the CALLER's original events (`inserted.push(event.clone())`, camelCase `aggregateID`), whereas sync_history returns the STORED payload built by history_event (sync.rs:61-86), which is reshaped to snake_case keys `{id, aggregate_id, seq, type, data}`. A client written from the doc would read `aggregateID` off sync_history rows and get null.


### `docs/adr/0001-event-sourced-mailbox-and-channels.md`

**CONTRADICTION 1**

- The doc claims: append_direct_mail 'Reject with StoreError::MailboxRejected when ... the target is a stopped/terminal resident (see eligibility below)' (line 41), and the eligibility table row 'Resident with RosterStatus::Done or Failed | No'.
- Reality: The resident eligibility branch is gated on `entry.session != root` — a resident whose roster entry is the team root itself is never subjected to resident_member_is_eligible and is never rejected, no matter its status or claim state. The adjacent transient bullet does carry the `session != root` qualifier, so the omission reads as deliberate rather than elided.
- Source: `crates/hya-store/src/mailbox.rs:57-69, :502-510`

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
