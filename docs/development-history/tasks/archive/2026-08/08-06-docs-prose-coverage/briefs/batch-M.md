# Batch M - self-update.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 1 file(s). Do not create or edit any other file.

- `docs/self-update.md`

You have **5 gap entries** and **0 stale claims** to resolve.

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
   list does not count. 5 of your entries are status `thin`, meaning the
   feature IS already mentioned but unusably so -- those need real content, not a
   second mention.
7. Do not run `git commit`. Writing the files is enough.

## Work list

Each entry was produced by an agent that read the source. Treat it as a work list
and a starting point, not as verified truth -- rule 1 still applies.

### `docs/self-update.md`

**1. Updater metadata validation rules and the verification gate chain (items 142,143)** — `thin` · severity medium

- Source: `crates/hya-updater/src/verify.rs:33,135`
- Evidence: docs/self-update.md:79-85 lists the required metadata FIELDS and the anti-rollback rule, but does not state the validation constraints or the ordered gate chain. min_updater_version is named as a field at line 81 but never described as a gate.
- Write: Add the validation rules: platform, key_id, and min_updater_version must be non-empty; not_after must be >= not_before (both inclusive Unix seconds); artifacts must be non-empty, each with a non-empty name and a digest of exactly 64 LOWER-hex characters. Then add the ordered gate chain apply runs, since the first failing gate is what an operator will see: (1) protocol_version, (2) min_updater_version compared against the crate version by dotted-numeric compare, (3) sequence strictly greater than the accepted floor, (4) platform equality, (5) not_before/not_after window, (6) trust-root lookup by key_id and Ed25519 signature verification.

**2. trust_roots.json file format (item 144)** — `thin` · severity medium

- Source: `crates/hya-updater/src/trust.rs:11`
- Evidence: docs/self-update.md:15-22 shows trust_roots.json in the layout with the comment 'ed25519 verifying keys (TCB)', and lines 68-72 show `init-roots --root ci-root-1=<64-lower-hex-verifying-key>`. The actual JSON shape is never shown, so an operator editing or auditing the file by hand has nothing to check against.
- Write: Show the exact file: {"roots":[{"key_id":"...","verifying_key_hex":"..."}]}. State the constraints: at least one root is required, key_id must be non-empty, and verifying_key_hex must be exactly 64 LOWER-hex characters — uppercase hex is REJECTED, which is an easy hand-editing trap.

**3. Staging and smoke-test constraints (items 149,150)** — `thin` · severity medium

- Source: `crates/hya-updater/src/stage.rs:27; crates/hya-updater/src/smoke.rs:16`
- Evidence: docs/self-update.md:56 shows `--smoke smoke.sh` in the apply example but never states the path constraints on that argument, and nothing describes what staging does or when it refuses.
- Write: Document the --smoke contract: the command path must be RELATIVE and must not contain `..`; it is run as a child process from inside the staged release directory, never loaded into the updater's address space, and a non-zero exit is reported as SmokeFailed. Also document staging: it creates root/releases/<sequence> and ERRORS if that directory already exists (so re-applying the same sequence fails rather than overwriting), re-verifies each artifact's size and SHA-256 before writing, fsyncs each file, chmods 0o755 on Unix, and confirms every declared artifact landed.

**4. Crash recovery decision rules (item 153)** — `thin` · severity low

- Source: `crates/hya-updater/src/journal.rs:126`
- Evidence: docs/self-update.md:42-43 documents the `recover` command ('Recover interrupted prepare/commit') and line 21 labels activation.journal as 'prepare/commit/abort records', but the three recovery outcomes are never stated, so an operator cannot predict what recover will do.
- Write: State the three cases explicitly: (a) no journal, or last phase is committed or aborted -> keep the current selector unchanged; (b) last phase is `prepare` and the selector still points at the PREVIOUS generation -> write an `aborted` record and keep the old generation; (c) last phase is `prepare` and the selector ALREADY points at the candidate -> finish the activation by writing the accepted floor and a `committed` record.

**5. discard_staged_release guardrails (item 156)** — `thin` · severity low

- Source: `crates/hya-updater/src/pipeline.rs:108`
- Evidence: docs/self-update.md:62-63 shows `discard --root ... --sequence 42` described as 'Discard a staged-but-not-accepted candidate' but never lists the four refusal conditions.
- Write: List when discard refuses: sequence 0, the currently selected sequence, any sequence at or below the accepted floor, and a sequence whose staged directory is absent. Frame it as the safety property — discard can only ever remove bits that were never accepted.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 5 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
