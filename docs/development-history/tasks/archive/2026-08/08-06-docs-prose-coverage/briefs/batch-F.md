# Batch F - storage.md, admission-and-governor.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 2 file(s). Do not create or edit any other file.

- `docs/architecture/storage.md`
- `docs/architecture/admission-and-governor.md`  **(new file)**

You have **13 gap entries** and **2 stale claims** to resolve.

admission-and-governor.md is NEW. These two are paired because the admission schema and the admission API must agree -- you own both so they cannot drift. This is a safety-critical spawn-budget state machine; prefer omitting a claim to guessing one.

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
   list does not count. 6 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/architecture/storage.md`

**1. [behavior] saved permissions (persistent allow-always)** — `thin` · severity medium

- Source: `crates/hya-store/src/permission.rs:14-59, crates/hya-server/src/pending/saved_permission.rs:36-52`
- Evidence: docs/architecture/server-client.md:92 says "SQLite-backed saved permissions" and docs/compat-parity.md:119 says an `always` reply "feed[s] a SQLite-backed Compat-shaped saved-permission list/removal API that survives server restart". docs/architecture/storage.md never mentions the table; grep for `saved_permission` across in-scope docs returns nothing.
- Write: Add a `saved_permission` table entry to the storage inventory: an `always` reply writes one row keyed `psv_<requestId>`, with `project_id` hardcoded to the literal `"global"` (saved permissions are NOT scoped per project or per session), the lowercase action name (see the Action list), and the remembered match pattern. Rows survive server restart and are listable via GET /api/permission/saved and removable via DELETE /api/permission/saved/:id. Note the global scoping explicitly, because it means an "allow always" granted in one workspace applies in all of them.

**2. SessionStore::list_sessions / SessionInfo and SessionStore::delete_session** — `thin` · severity medium

- Source: `crates/hya-store/src/lib.rs:157`
- Evidence: AGENTS.md:79 says hya-store 'lists/deletes sessions'. docs/FOLLOWUPS.md:26 names list_sessions as future work. docs/architecture/storage.md documents connect/append/replay/read_projection/record_usage but NOT list_sessions or delete_session. No rustdoc at lib.rs:157 or :138.
- Write: Add both to the store API section. list_sessions derives SessionInfo{session, started_millis, updated_millis, events} by GROUP BY session_id over event_log — session identity comes from the LOG, not the `session` table, so a session row is not required for a session to be listed. delete_session runs one transaction that deletes token_ledger rows then event_log rows for the session and returns whether any log rows were removed.

**3. decode_session_key (BLOB key → SessionId)** — `undocumented` · severity medium

- Source: `crates/hya-store/src/lib.rs:229`
- Evidence: No rustdoc; zero hits in in-scope docs. docs/architecture/storage.md:65-67 documents the WRITE side ('hysec_... ASCII bytes for new sessions; 16-byte UUID keys for legacy sessions') but not the read/decode rule.
- Write: Document the decode side next to the existing write-side note: decode_session_key first tries to interpret the BLOB as UTF-8 and parse it as a SessionId (hysec_, ses_<uuid>, or raw uuid text); only if that fails does it interpret the 16 bytes as a legacy raw UUID. This ordering is what lets both key encodings coexist in one column.

**4. Migration 0001 table columns — session, message, part, team_run, team_member, mail, task_board, goal** — `thin` · severity medium

- Source: `crates/hya-store/migrations/0001_init.sql:1`
- Evidence: docs/architecture/storage.md:30-40 lists the table GROUPS only ('sessions, messages, and parts', 'team runs and members', 'mail and task board state', 'goals', 'token ledger'). docs/project-structure.md:153-155 repeats the same grouping. No column, key, FK, or index is documented for any of them.
- Write: Add a schema reference table per 0001 table with columns, PKs, FKs and indexes: session(id BLOB PK, parent_id BLOB FK→session(id), agent, model, workdir, title, permission TEXT default '{}', created_at, updated_at; index session_parent); message(id BLOB PK, session_id FK ON DELETE CASCADE, role, agent, model, finish, cost_json, tokens_json, created_at; index message_session); part(id BLOB PK, message_id FK ON DELETE CASCADE, seq, kind, body_json, UNIQUE(message_id,seq)); team_run(id, lead_session FK, spec_json, state, created_at); team_member(id, team_id FK CASCADE, session_id FK, background_task_id, role, state, created_at); mail(id, team_id FK CASCADE, from_ep, to_ep, kind, body_json, delivered_at, acked_at, created_at); task_board(id, team_id FK CASCADE, title, body, status, assignee, created_at, updated_at); goal(id, session_id FK, condition, bound_json, state, turns_evaluated, last_reason, started_at, cleared_at). Mark `mail` and `task_board` explicitly as the pre-ADR-0001 relational mailbox, SUPERSEDED by event-sourced MailSent and not on any live read path.

**5. Table event_log — globally monotonic seq, no FK to session** — `thin` · severity medium

- Source: `crates/hya-store/migrations/0001_init.sql:37`
- Evidence: docs/architecture/storage.md:61-70 documents what append_event inserts and says 'SQLite assigns the monotonic seq', but does not say the seq is a single global AUTOINCREMENT across all sessions, nor that event_log has NO foreign key to `session` (so a log can exist with no session row).
- Write: In the Event Log section, add the schema and two consequences: seq INTEGER PRIMARY KEY AUTOINCREMENT is GLOBAL, not per-session — one session's envelopes have gaps; and there is deliberately no FK from event_log.session_id to session(id), so an event log can exist without a session row (which is why list_sessions derives identity from the log). Columns: seq, session_id BLOB, payload TEXT (serialized Event JSON), ts INTEGER millis; index event_log_session.

**6. Table sync_event (migration 0002)** — `undocumented` · severity medium

- Source: `crates/hya-store/migrations/0002_sync_event.sql:1`
- Evidence: grep for 'sync_event' across all in-scope docs returns ZERO hits. docs/architecture/storage.md:28-50 jumps from 0001 straight to 0005. docs/compat-parity.md:120 describes /sync/history and /sync/replay behaviour but never names the backing table.
- Write: Add migration 0002 to the Migrations section: sync_event(aggregate_id TEXT, seq INTEGER, payload TEXT) with a composite (aggregate_id, seq) primary key and index sync_event_seq. It backs the /sync/history and /sync/replay compat routes: replay_sync_events does INSERT OR IGNORE on the composite key and returns only newly-inserted events; sync_history returns every stored event whose seq exceeds the caller's per-aggregate `known` watermark.

**7. Table saved_permission (migration 0003) and the SavedPermission store API** — `undocumented` · severity medium

- Source: `crates/hya-store/migrations/0003_saved_permission.sql:1`
- Evidence: grep for 'saved_permission' across all in-scope docs returns ZERO hits. docs/architecture/tools-and-permissions.md describes ask flows but not the durable store; docs/compat-parity.md:105 mentions '/api/permission/saved' routes without naming the table. crates/hya-store/src/permission.rs has 69 lines and zero doc comments.
- Write: Add migration 0003: saved_permission(id TEXT PK, project_id, action, resource) with UNIQUE(project_id, action, resource) and index saved_permission_project — the durable 'always allow' store behind the TUI's remember-this-decision option. Document the three methods: save_permission (INSERT OR IGNORE, so re-saving is a no-op), list_saved_permissions(project_id: Option<..>), remove_saved_permission(id). Cross-link from docs/architecture/tools-and-permissions.md.

**8. admission_journal schema (0004 → 0006 rebuild → 0007 binding columns → 0008 fairness columns)** — `thin` · severity high

- Source: `crates/hya-store/migrations/0006_admission_queue_states.sql:1`
- Evidence: docs/architecture/storage.md:47-50 mentions admission_journal only as 'gains nullable actor_id/actor_epoch columns'. Migrations 0004, 0006, 0007, 0008 are named nowhere in scope. The composite PK, the state CHECK constraint, the fingerprint columns, the all-or-nothing binding CHECK, and the FIFO promotion indexes are entirely undocumented.
- Write: Add a full admission_journal subsection. Composite PK (operation_id, member_ordinal) with UNIQUE(source_tool_call_id, member_ordinal). Columns: root_session_id, request_fingerprint (32 bytes), state CHECK IN (queued, accepted, started, waiting, completed, cancelled, aborted), admission_units>0, logical_released 0/1, terminal_reason, created_at, updated_at, actor_id, actor_epoch>0, member_ordinal<batch_size, batch_size>0. From 0007: runtime_fingerprint_version + runtime_fingerprint(32B), admission_binding_fingerprint_version + fingerprint(32B), spawn_intent BLOB (1..=1 MiB) — with an all-or-nothing CHECK so those five binding columns are either all NULL or all present. From 0008: admission_sequence and promotion_sequence (both >0, backfilled from rowid), each with a partial UNIQUE index, plus partial indexes on (state='queued', admission_sequence) and (root_session_id, promotion_sequence) that make FIFO promotion an index scan.

**9. Bundle registry database (separate SQLite file, its schema, and its fail-fast PRAGMAs)** — `undocumented` · severity medium

- Source: `crates/hya-store/bundle_migrations/0001_init.sql:1`
- Evidence: docs/cli.md:102-103 documents the registry FILE PATH ($XDG_DATA_HOME/hya/bundles/registry.sqlite3) but no doc covers its schema or connection settings. docs/architecture/storage.md never mentions a second database. crates/hya-store/src/bundle_registry.rs has 423 lines and ZERO doc comments.
- Write: Add a 'Bundle registry database' section: it is a SEPARATE SQLite file from sessions.db, with its own migration set under bundle_migrations/. Schema: bundle_registry_generation(singleton=1, generation>=0) plus installed_bundle(bundle_id PK, version, publisher, source_digest BLOB(32), prepared_digest, prepared_bytes, installed_at). Connection PRAGMAs differ deliberately from the session store: WAL, synchronous=FULL, busy_timeout ZERO (so contention fails fast to StoreError::BundleRegistryBusy instead of blocking a turn), 8 pooled connections. Also document the API: generation(), snapshot() → BundleRegistrySnapshot{generation, bundles}, install_inspection, install → BundleInstallOutcome, uninstall → BundleUninstallOutcome.

**10. SessionStore::replay_sync_events / sync_history** — `thin` · severity low

- Source: `crates/hya-store/src/sync.rs:9`
- Evidence: crates/hya-store/src/sync.rs has 83 lines and zero doc comments. docs/compat-parity.md:120 describes the ROUTE behaviour ('merges persisted raw sync events with hya event-log events ordered by sequence and filters events at or below the caller's known aggregate sequence') but no doc names the store methods or states the INSERT OR IGNORE return contract.
- Write: Alongside the new sync_event table section, document replay_sync_events (INSERT OR IGNORE on (aggregate_id, seq); returns ONLY the newly-inserted events, so a re-replay of an overlapping history returns an empty set) and sync_history (returns every stored event whose seq exceeds the caller's per-aggregate `known` watermark).

**11. StoreError variants** — `undocumented` · severity medium

- Source: `crates/hya-store/src/error.rs:8`
- Evidence: crates/hya-store/src/error.rs has 79 lines and zero doc comments. docs/project-structure.md:144 says only 'Store error wrapper'. Only StaleActorClaim and MailboxRejected are named in prose (docs/adr/0002:52, and MailboxRejected nowhere at all).
- Write: Add an Errors section listing every StoreError variant and when it fires: Sqlite, Migrate, Json, Bundle, BundleRegistryData, BundleRegistryCorrupt, BundleRegistryBusy (fail-fast from the zero busy_timeout), BundleNotFound, BundleContentConflict, PrivateActivationUnsupported, BundleImmutable, OperationIdConflict, AdmissionNotFound, AdmissionTransitionConflict, AdmissionData, AdmissionCapacityExceeded, ActorAlreadyClaimed, StaleActorClaim, ActorClaimUnavailable, ActorClaimData, MailboxRejected.

**STALE 1.** The document claims: "Provider usage reporting is represented in the data model, but live HTTP routes currently declare `usage_reporting: false`."

- Reality: Every HTTP route sets `usage_reporting: true` in its default Capabilities (crates/hya-provider/src/http.rs:172), and all four protocol decoders extract real usage (openai/decoder.rs:197, openai/response_decoder.rs:295, anthropic/decoder.rs:261, google.rs:338). The claim is inverted.
- Action: correct or delete. Do not merely supplement.

**STALE 2.** The document claims: The Migrations section describes `0001_init.sql` and `0005_resident_actor_claim.sql`, presenting them as the schema story.

- Reality: Migrations 0002_sync_event.sql, 0003_saved_permission.sql, 0004 (admission journal), 0006_admission_queue_states.sql, 0007_admission_bindings.sql and 0008_admission_fairness.sql also exist, plus an entirely separate migration set under crates/hya-store/bundle_migrations/ for a second SQLite database (the bundle registry). admission_journal is mentioned only as 'gains nullable actor_id/actor_epoch columns', which understates a table with a composite PK, a 7-value state CHECK, five all-or-nothing binding columns, and two FIFO promotion index families.
- Action: correct or delete. Do not merely supplement.

### `docs/architecture/admission-and-governor.md`

**1. Spawn admission lifecycle — SpawnAdmissionOutcome, begin_spawn_admission, finalize_spawn_admission, finalize_root_spawn_admissions, abort_recovered_actor_operations** — `undocumented` · severity high

- Source: `crates/hya-core/src/engine/admission.rs:17`
- Evidence: crates/hya-core/src/engine/admission.rs is 736 lines with ZERO doc comments. grep across all in-scope docs for 'SpawnAdmissionOutcome', 'begin_spawn_admission', 'finalize_spawn_admission' returns zero hits. docs/architecture/storage.md:48-50 mentions admission_journal only as gaining nullable actor columns.
- Write: Create this new doc. Document SpawnAdmissionOutcome{Started, Existing(AdmissionState), Overloaded, MaxDepth, Cancelled}. Document begin_spawn_admission's fixed ordering — durable claim FIRST, then cancel check, then depth check, then governor reservation, then start_admission — and that each failure path terminalizes the journal row with a distinct reason string: 'cancelled before debit', 'maximum subagent depth exceeded', 'spawn admission overloaded', 'failed to persist started state'. Document finalize_spawn_admission: it terminalizes a row, and the governor budget is refunded ONLY when the store reports release_required, which is what makes refund exactly-once across concurrent finalizers. Document finalize_root_spawn_admissions (root-turn cleanup) and abort_recovered_actor_operations (post-takeover abort that refunds only logically-released rows).

**2. Admission store types and API — AdmissionState, capacity caps, MAX_ADMISSION_INTENT_BYTES, AdmissionClaim/Intent/Launch/Record/ActorBinding, outcome enums, and the 15 store methods** — `undocumented` · severity high

- Source: `crates/hya-store/src/admission.rs:16`
- Evidence: crates/hya-store/src/admission.rs is 1487 lines with 11 doc-comment lines total. grep across all in-scope docs for 'AdmissionState', 'MAX_ACTIVE_ADMISSIONS', 'claim_admission', 'promote_queued_admissions' returns ZERO hits.
- Write: In the new admission doc, add a store-API section. AdmissionState = Queued | Accepted | Started | Waiting | Completed | Cancelled | Aborted, with is_terminal() covering the last three. Capacity caps: MAX_ACTIVE_ADMISSIONS=100 (accepted+started) and MAX_NON_ACTIVE_ADMISSIONS=156 (queued+waiting); exceeding either returns StoreError::AdmissionCapacityExceeded. MAX_ADMISSION_INTENT_BYTES = 1_048_576 is the hard ceiling on the persisted spawn_intent blob and is mirrored by the SQL CHECK. Document the input/record types (AdmissionClaim{operation, tool call, root, 32B fingerprint, units, optional ActorClaim}, AdmissionIntent{two versioned 32B fingerprints + spawn_intent bytes}, AdmissionLaunch, AdmissionRecord incl. member_ordinal/batch_size/logical_released/terminal_reason, AdmissionActorBinding) and the outcome enums (AdmissionClaimOutcome{Claimed,Existing}, AdmissionBatchClaimOutcome{Claimed(Vec<AdmissionLaunch>),Existing}, AdmissionStartOutcome{Started,Existing}, AdmissionTerminal{Completed,Cancelled,Aborted}, AdmissionFinalizeOutcome{record, release_required}, AdmissionReleaseOutcome{finalized, promoted}). Then list the 15 methods: claim_admission, claim_admission_batch, suspend_parent_and_claim_admission_batch, queue_waiting_admission_member, admission, admissions, admission_counts, promote_queued_admissions, start_admission, start_admission_member, finalize_admission, finalize_admission_members, recover_nonterminal_admissions, abort_recovered_actor_admissions, nonterminal_admissions_for_root.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 13 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
