# Fix batch H3 - providers.md, plugin-protocol.md, self-update.md, tools-and-permissions.md, agent-tool-surface.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/architecture/providers.md`
- `docs/plugin-protocol.md`
- `docs/self-update.md`
- `docs/architecture/tools-and-permissions.md`
- `docs/architecture/agent-tool-surface.md`

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


### `docs/architecture/providers.md`

**CONTRADICTION 1**

- The doc claims: "**Catalog.** `ProviderRouter::catalog` flattens every provider's `catalog()`, sorts by `(provider_id, model_id)`, and dedups identical provider/model pairs. That ordering is what `hya-backend models` and `GET /api/model` expose."
- Reality: The `GET /api/model` half is correct (compat/catalog.rs `catalog_models` reads `st.engine.provider_catalog()`), but `hya-backend models` never touches `ProviderRouter::catalog()`. main.rs:369 passes `runtime.models`, which runtime.rs:1296 sets from `cfg.models` — i.e. `config::model_entries(&authorized)`, built directly from the parsed `providers:` block. models_cmd.rs `model_lines` then formats `"{provider}/{id}"` strings and calls `lines.sort()` on the joined strings, with no dedup. Same content in practice, but the stated mechanism and sort key are wrong, so a reader debugging catalog ordering will look in the wrong crate.
- Source: `crates/hya-backend/src/main.rs:369, crates/hya-app/src/runtime.rs:1296, crates/hya-app/src/config.rs:1343 (model_entries), crates/hya-backend/src/models_cmd.rs:31-56`

**CONTRADICTION 2**

- The doc claims: "`DevProvider` claims the same set **minus** `reasoning_request` (left false via `Capabilities::default()`), which is why it accepts any `ModelRef`."
- Reality: The capability flags have no bearing on which refs DevProvider accepts. `DevProvider::capabilities(&self, _model: &ModelRef)` returns `Some(dev_capabilities())` unconditionally — it ignores the model argument entirely. The flag content (including `reasoning_request: false`) is accurate; the causal "which is why" clause is not. The same file states it correctly at line 502 ("It claims every model (`capabilities` always `Some`)"), so the two passages disagree.
- Source: `crates/hya-provider/src/dev.rs:53-70`


### `docs/plugin-protocol.md`

**CONTRADICTION 1**

- The doc claims: `permission.ask` is labelled a **guard** with **Default posture: Safe**, and the posture table states Safe means 'Host treats the failure as a **veto** of the action'. A reader concludes that a Safe-posture `permission.ask` plugin that errors or times out denies the request.
- Reality: Posture has zero behavioral effect for `permission.ask`. In `PluginHost::permission_ask` the posture is read only as a registration test (`if conn.posture(HookName::PermissionAsk).is_none() { continue; }`); a serialize failure, an RPC error (`let Ok(reply) = conn.call_hook(...) else { continue; }`), or an undecodable reply all just skip to the next plugin, and all-skipped falls through to the interactive user ask. `GUARD_FAILED_SAFE` exists in dispatcher.rs only on the `tool.execute.before` arm — it is the sole hook where Safe posture converts a failure into a veto. docs/architecture/tools-and-permissions.md:206-209 states the correct behavior ('or every plugin errors ... falls through'), so the two rewritten docs disagree with each other on a security-relevant point.
- Source: `crates/hya-plugin/src/permission_bridge.rs:96-118 and crates/hya-plugin/src/dispatcher.rs:131-145`

**CONTRADICTION 2**

- The doc claims: '`plugin.kind` | Wire snake_case: `rust` (default), `compat`, `other`.' — the '(default)' reads as 'omissible from the initialize reply'.
- Reality: `PluginInfo.kind` carries no `#[serde(default)]`, so `kind` is a REQUIRED field of the initialize reply; omitting it fails deserialization and the handshake aborts. The `#[default] Rust` on `PluginKindWire` only takes effect where a `#[serde(default)]` exists — `PluginEntry.kind` (config.rs:18) and `Manifest.kind` (manifest.rs:17), i.e. YAML config and `plugin.toml`, not the wire. docs/configuration.md's two tables use '(default)' correctly for those; docs/plugin-protocol.md inherits the wording into a context where it is wrong.
- Source: `crates/hya-plugin/src/messages.rs:190-199 (PluginInfo) vs crates/hya-plugin/src/config.rs:17-18 and crates/hya-plugin/src/manifest.rs:16-17`


### `docs/self-update.md`

**CONTRADICTION 1**

- The doc claims: Each artifact `sha256_hex` — 'Exactly **64 lower-hex** characters. Uppercase hex is rejected.'
- Reality: `canonical_metadata_payload` checks `artifact.sha256_hex.len() != 64 || !artifact.sha256_hex.bytes().all(|b| b.is_ascii_hexdigit())`. `is_ascii_hexdigit()` accepts `A-F`, so an uppercase artifact digest passes this validation and signature verification. It only fails much later and with a different error (`ArtifactDigestMismatch` from `verify_artifact_bytes`, which formats the computed digest as `{b:02x}` and string-compares). The neighbouring `trust_roots.json` row IS correct — `trust.rs:81` has an explicit `is_ascii_uppercase()` rejection — which is likely where the claim was copied from. As written, an operator reading the table would expect a clear `InvalidMetadata` at signing/verify time and instead gets a confusing per-artifact digest mismatch during staging.
- Source: `crates/hya-updater/src/verify.rs:64-66 vs crates/hya-updater/src/trust.rs:81-85`


### `docs/architecture/tools-and-permissions.md`

**CONTRADICTION 1**

- The doc claims: Under "Plugin permission bridge", bullet 2 "**Cache invalidation**": "remembered plugin-mediated decisions are keyed by a domain-separated SHA-256 digest over the literal domain string `b"hya.plugin.permission-bridge.semantic-identity/v1"` plus, per participating plugin, its id, its canonical initialize declaration, and its effective `permission.ask` posture. Adding, removing, or changing any permission plugin automatically invalidates previously cached decisions."
- Reality: There is no cache of plugin-mediated permission decisions, and nothing is keyed by that digest. `PermissionPlane::apply_decision` (crates/hya-tool/src/permission.rs) stores an allow-always as either `Rule(action, "*", Allow)` pushed onto the `persistent` rule list (legacy scope) or an `ExactSubject` inserted into the `native_grants` HashSet (native scope) — neither is keyed by, or consulted against, any digest, and neither is ever invalidated when the plugin set changes. The bridge digest (crates/hya-plugin/src/permission_bridge.rs:54-84) is only returned from `PermissionInterceptor::semantic_identity_v1`, which is mixed into `PermissionPlane::semantic_identity_v1` (permission.rs:596-620) and from there into `TurnBinding::semantic_fingerprint_v1` (crates/hya-core/src/runtime_registry.rs:596-635). That is a runtime-view fingerprint used to detect policy change across refreshes — it is not a decision-cache key, and changing a plugin does not clear any remembered grant. This text was introduced by the doc pass (commit 4ba8d5a5), so it is a newly-added false claim. It also duplicates and misstates the correct description that already exists in docs/architecture/runtime.md:309-320.
- Source: `crates/hya-tool/src/permission.rs:596-620, crates/hya-tool/src/permission.rs (apply_decision, ~line 760-790), crates/hya-plugin/src/permission_bridge.rs:54-84, crates/hya-core/src/runtime_registry.rs:596-635`


### `docs/architecture/agent-tool-surface.md`

**CONTRADICTION 1**

- The doc claims: Under `## WRITE` → "Schema and validation": "Runtime deserialization stores the path in one optional field and accepts `filePath` as an alias (`path` is runtime-only)."
- Reality: `path` is NOT runtime-only: `WriteTool::schema` advertises it in the model-facing input schema under `properties` alongside `filePath` (only `required` is limited to `filePath` and `content`). The parenthetical also contradicts the sentence immediately before it in the same paragraph ("it also lists `path` for compatibility") and contradicts docs/architecture/tools-and-permissions.md:20-23 and :29, which correctly state that short spellings such as `path` appear under schema `properties`. Note the field direction is also inverted relative to READ's identical wording: in write.rs the serde field is `path` with `#[serde(alias = "filePath")]`, i.e. `filePath` is the alias of `path`, not the other way round.
- Source: `crates/hya-tool/src/write.rs:15-19 (serde field `path`, alias `filePath`), crates/hya-tool/src/write.rs:33-41 (input_schema properties list `filePath` AND `path`; required = ["filePath", "content"])`

**STILL OPEN 1 - [tool] apply_patch (alias patch) — patch envelope format** (`thin`)

- Source: `crates/hya-tool/src/apply_patch/parse.rs:40-152 (and apply_patch/mod.rs:16-46)`
- Why it is still open: The new `## APPLY_PATCH` section (agent-tool-surface.md:501-522) names the parameter `patchText`, the alias `patch`, the four hunk kinds, the relative-path rule, the all-paths-checked-before-write ordering, and the post-edit formatter/BOM/LSP step — but it describes the payload only as "a Codex/Compat-style patch envelope". That is a bare mention of the format's name. The envelope grammar exists nowhere in docs/: `*** Begin Patch` / `*** End Patch` sentinels (both required, Begin must precede End), `*** Add File: <path>` whose body lines must every one start with `+` (any other prefix is the input error `add file lines must start with '+'`), `*** Delete File: <path>`, `*** Update File: <path>` optionally followed on the very next line by `*** Move to: <path>`, `@@` chunk headers with optional trailing context text, chunk body lines prefixed with a space (context), `-` (removed) or `+` (added), and the `*** End of File` chunk terminator. CRLF is normalised to LF before parsing, and a `patchText` containing no recognised header parses to zero hunks, which the tool rejects as `patch rejected: empty patch`. A reader cannot construct a valid `patchText` from what is written — the tool's single parameter is effectively undocumented. `apply_patch` is also the ONLY file-mutation tool advertised to gpt-* models (edit/write are filtered out by `include_tool`), so this is the primary write path for that model family.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
