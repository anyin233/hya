# Fix batch G2 - cli.md, process-e2e.md, troubleshooting.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/cli.md`
- `docs/testing/process-e2e.md`
- `docs/troubleshooting.md`

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


### `docs/cli.md`

**CONTRADICTION 1**

- The doc claims: "Filtering with `models <provider>` when that provider has no configured models exits with `Provider not found: <id>`."
- Reality: `model_lines` synthesizes the offline fallback when `provider.is_none_or(|provider| provider == "hya")`, so on an empty catalog `hya-backend models hya` prints `hya/<fallback_model>` and succeeds; the `Provider not found` error is only reached for a filter value other than `hya`.
- Source: `crates/hya-backend/src/models_cmd.rs:31-57`

**STILL OPEN 1 - TUI slash-command reference (/sessions … /diff) — covers features 100-122; specifically the `/undo` row** (`thin`)

- Source: `?`
- Why it is still open: The 24-row table in docs/cli.md:124-148 and its mirror in docs/tui-keybindings.md:520-544 and docs/tui-reference.md are accurate for 23 of 24 commands, but `/undo` is described only as "Undo the previous user message". The real handler (packages/hya-tui-ts/src/upstream/routes/session/index.tsx:747-775) does three things a user must know: (1) it aborts an in-flight turn first when `session_status` is not `idle`; (2) it calls `session.revert` at the last user message *before the current revert point*, so repeated `/undo` walks backwards; (3) it OVERWRITES the prompt buffer with that message's text parts and re-attaches its file parts. Point (3) is a destructive side effect on whatever the user had typed and appears in no doc (grepped `undo` across docs/**). A reader cannot predict what `/undo` will do to their draft from what is written.

**STILL OPEN 2 - `hya-backend models` unknown-provider error and synthesized fallback entry** (`contradicted`)

- Source: `crates/hya-backend/src/models_cmd.rs:51`
- Why it is still open: docs/cli.md:451-454 correctly documents the offline synthesis for the unfiltered case, then states flatly that "Filtering with `models <provider>` when that provider has no configured models exits with `Provider not found: <id>`". `model_lines` (models_cmd.rs:36) guards the synthesis with `provider.is_none_or(|provider| provider == "hya")`, so with an empty catalog `hya-backend models hya` prints `hya/<fallback_model>` and exits 0 -- it does not produce `Provider not found: hya`. The documented rule is only true for provider ids other than `hya`.


### `docs/testing/process-e2e.md`

**STILL OPEN 1 - env HYA_ROUTE (and HYA_FAST_BOOT)** (`thin`)

- Source: `?`
- Why it is still open: docs/testing/process-e2e.md:165 documents HYA_ROUTE as "JSON value parsed at TUI startup that overrides the initial route. Malformed JSON throws during boot" — it never gives the accepted JSON shape, so a harness author cannot construct a value from this doc. The real contract (packages/hya-tui-ts/src/upstream/context/route.tsx:6-23,43-53) is exactly `{"type":"home"}`, `{"type":"session","sessionID":"<id>"}`, or `{"type":"plugin","id":"<id>"}`. Worse for a test harness: `initialRoute()` SILENTLY returns undefined and falls back to `{type:"home"}` for well-formed JSON of an unrecognised shape (e.g. `{"type":"session"}` with no sessionID) — a silent wrong-route pass that the doc's "malformed JSON throws" sentence actively leads readers to believe cannot happen. The HYA_FAST_BOOT row on line 166 is complete and correct. (docs/configuration.md:719 and docs/architecture/tui.md:142 hint at the shape, but process-e2e.md does not link to either for this.)


### `docs/troubleshooting.md`

**CRITIC 1 - Headless permission behaviour (exec / run / goal / rpc / serve)**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-app/src/permission.rs:11-14 (`spawn_reject_responder` replies `Decision::Reject { feedback: None }` to every `AskRequest`), wired at /chivier-disk/yanweiye/Projects/yaca/crates/hya-backend/src/main.rs:96,159,233`
- Why it matters: `docs/troubleshooting.md:143-149` says "Headless `exec`, `run`, goal mode, `rpc`, and `serve` install an automatic permission responder. By default it allows reads, globs, grep, shell, MCP, and edits that stay inside the active workdir after symlink-aware resolution." `docs/configuration.md:666-667` and `docs/architecture/tools-and-permissions.md:198-199` say the opposite: "Headless `exec`, RPC, and goal modes reject unresolved asks" / "answer residual asks with `Reject`". The source installs a pure reject responder — nothing is auto-allowed. troubleshooting.md also claims "symlink-aware resolution", while tools-and-permissions.md:220-224 documents the path boundary as purely lexical with symlinks deliberately NOT canonicalized. It also lists `serve` as headless, while configuration.md:666 says server mode forwards asks to its endpoint.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
