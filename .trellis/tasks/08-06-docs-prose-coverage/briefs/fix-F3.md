# Fix batch F3 - tools-and-permissions.md, event-model.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/architecture/tools-and-permissions.md`
- `docs/architecture/event-model.md`

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


### `docs/architecture/tools-and-permissions.md`

**CONTRADICTION 1**

- The doc claims: The Action table gives the lowercase wire value for `ExternalDirectory` as `external_directory` (with an underscore), presented as the string that appears in saved-permission rows and rules.
- Reality: `Action` carries `#[serde(rename_all = "lowercase")]` (permission.rs:26-27), which lowercases the variant name WITHOUT inserting separators, so `ExternalDirectory` serializes as `externaldirectory`. `crates/hya-server/src/pending/saved_permission.rs:77-82` (`action_name`) writes the DB `action` column straight from `serde_json::to_value(action)`, so a saved allow-always row for that action literally stores `externaldirectory`. A client filtering saved permissions on `external_directory` matches nothing. docs/architecture/storage.md:253 gets this RIGHT (`externaldirectory`), so the two rewritten docs now contradict each other.
- Source: `crates/hya-tool/src/permission.rs:24-56, crates/hya-server/src/pending/saved_permission.rs:77-82, docs/architecture/storage.md:253`

**CONTRADICTION 2**

- The doc claims: The ToolError->wire-`type` table is presented as the complete vocabulary, closed with 'Clients that switch on this string should treat all twelve values as first-class.'
- Reality: A thirteenth wire `type` exists and is not in either table: when a `tool.execute.before` hook vetoes a call, `crates/hya-core/src/engine/turn.rs:745` emits `tool_error_message_value("blocked", ...)`, producing `{"error":{"type":"blocked","message":"blocked by plugin: <reason>"}}`. `blocked` is not a `ToolError` variant so it is absent from `tool_error_type`, but it reaches clients through the same `Event::ToolError.value` field. A client that exhaustively switches on 'all twelve values' falls through on every plugin veto. Ironically docs/architecture/runtime.md:593-600 documents the veto message text but never says it uses a distinct type string.
- Source: `crates/hya-core/src/engine/turn.rs:736-750, crates/hya-core/src/engine/tool_error.rs:8-32`

**CONTRADICTION 3**

- The doc claims: `path` (for read/write) and `path`/`old`/`new`/`replace_all` (for edit) are described as 'runtime-only alias(es)' accepted 'during direct execution', with the preamble adding that 'provider-side schema validation requires the advertised names'.
- Reality: All of those short spellings ARE advertised properties in the model-facing JSON schema; they are merely absent from `required`. read.rs:38-47 lists both `filePath` and `path` under `properties`; write.rs:33-41 does the same; edit.rs:36-51 advertises `path`, `old`, `new`, AND `replace_all` alongside the camelCase names. Nothing about them is runtime-only, and provider-side schema validation accepts them. Note agent-tool-surface.md:294-300 (READ) and :436-438 (EDIT) describe this correctly ('it also lists `path` for compatibility'), so the WRITE section at :410-413 contradicts its own sibling sections two paragraphs apart by asserting both 'it also lists `path` for compatibility' and '(`path` is runtime-only)'.
- Source: `crates/hya-tool/src/read.rs:34-49, crates/hya-tool/src/write.rs:29-43, crates/hya-tool/src/edit.rs:36-51`

**CONTRADICTION 4**

- The doc claims: 'Clients that switch on this string should treat all twelve values as first-class' (line 285), after a twelve-row table of ToolError -> wire `type`.
- Reality: A thirteenth value exists. The plugin/sidecar veto path emits ToolError with `value: tool_error_message_value("blocked", &message_text)`, producing `{"error":{"type":"blocked",...}}`, which is not produced by tool_error_type and is absent from the table. A client switching on the documented twelve will fall through on every plugin veto.
- Source: `crates/hya-core/src/engine/turn.rs:731-741, crates/hya-core/src/engine/tool_error.rs:8-15`

**CRITIC 1 - Wire value of the `ExternalDirectory` permission action in saved-permission rows**

- Source: `crates/hya-tool/src/permission.rs:26-27 (`#[serde(rename_all = "lowercase")]` on `enum Action`) together with crates/hya-server/src/pending/saved_permission.rs:77-82 (`action_name` serializes the enum with serde, so `ExternalDirectory` → `"externaldirectory"`). The separate `"external_directory"` string in crates/hya-server/src/compat/agent_permission.rs:54 is a Compat-only agent-permission mapping, not the persisted/rule value.`
- Why it matters: docs/architecture/tools-and-permissions.md:122 lists the wire value as `external_directory` under a heading that explicitly says "Action serializes lowercase in saved-permission rows and rules (`#[serde(rename_all = \"lowercase\")]`)". docs/architecture/storage.md:253 lists the same field as `externaldirectory`. Only one can be right; `rename_all = "lowercase"` strips the underscore, so storage.md is correct. A reader writing a permission rule or querying the `saved_permission` table from tools-and-permissions.md will use a value that never matches.

**CRITIC 2 - Source line citation for `ToolRegistry::builtins()`**

- Source: `crates/hya-tool/src/tool.rs:311-347 is the actual body of `pub fn builtins()`. Lines 237-271 are inside unrelated registry accessor code. The alias registrations cited as tool.rs:265-269 are actually at tool.rs:341-345.`
- Why it matters: docs/architecture/tools-and-permissions.md:16-18 and docs/architecture/agent-tool-surface.md:24-26 both cite `crates/hya-tool/src/tool.rs:237-271` as the evidence for the 26-builtin inventory, and agent-tool-surface.md:213 cites `tool.rs:265-269` for the five hidden aliases. The stated fact (26 canonical names, five aliases) is correct, but every deep-link lands on the wrong code, so a reader verifying the claim finds nothing there. Both documents carry the same stale range, so they agree with each other and disagree with the source — the drift is invisible without opening the file.


### `docs/architecture/event-model.md`

**CONTRADICTION 1**

- The doc claims: Fan-out table row: '`reject` + legacy action scope | Cascades to other pending in the session matching that legacy filter path'.
- Reality: There is no filtering. `take_related_for_reply` maps `(Reject, RememberScope::LegacyAction)` to `scope = None` (permission.rs:291), and `take_related` then guards with `scope.is_none_or(...)`, which is unconditionally `true` when scope is `None` (permission.rs:305). So a reject on a legacy-scope request drains EVERY pending permission request in that session -- different actions, different remember scopes, unrelated tools all get auto-rejected. The doc's 'matching that legacy filter path' wording tells a reader the opposite of the widest-possible behavior, and this is the one fan-out case with real blast radius.
- Source: `crates/hya-server/src/pending/permission.rs:279-317`

**CONTRADICTION 2**

- The doc claims: Permission fan-out table row: 'reject + legacy action scope | Cascades to other pending in the session matching that legacy filter path' (line 237).
- Reality: For (Reject, RememberScope::LegacyAction) take_related_for_reply sets `scope = None`, and take_related's predicate is `entry.session == Some(session) && scope.is_none_or(...)`. With scope None the second conjunct is unconditionally true, so the reply drains EVERY pending permission in that session regardless of action or remember scope — there is no legacy filter applied.
- Source: `crates/hya-server/src/pending/permission.rs:286-317`

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
