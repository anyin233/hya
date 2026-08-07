# Fix batch J4 - compat-plugins.md, agent-bundle-authoring.md, self-update.md, compat-parity.md, agent-matrix.md, process-e2e.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/compat-plugins.md`
- `docs/agent-bundle-authoring.md`
- `docs/self-update.md`
- `docs/compat-parity.md`
- `docs/testing/agent-matrix.md`
- `docs/testing/process-e2e.md`

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


### `docs/compat-plugins.md`

**CONTRADICTION 1**

- The doc claims: Under "Local path resolution": "A directory resolves to itself if it contains `package.json`, otherwise to `index.ts` / `index.tsx` / `index.js` / `index.mjs` / `index.cjs`, otherwise `PluginPathResolutionError`."
- Reality: `resolvePathPluginTarget` does throw `PluginPathResolutionError`, but its ONLY caller, `resolveLocalPluginSpec`, wraps the call in `.catch((error) => { if (error instanceof Error) return target; throw error })` — every `PluginPathResolutionError` is swallowed and the spec silently falls back to the raw directory `file://` URL. The author never sees `PluginPathResolutionError`; they see a later import failure printed as `compat plugin <spec>: <message>` on stderr while the adapter keeps running. Grep confirms `PluginPathResolutionError` has no other construction or propagation site in src/ or test/.
- Source: `crates/hya-plugin-compat/adapter/src/loader/shape.ts:39-54 (resolveLocalPluginSpec catch), :56-83 (resolvePathPluginTarget throw site)`

**CONTRADICTION 2**

- The doc claims: The five-step discovery list attaches "(unless `COMPAT_DISABLE_PROJECT_CONFIG` is set)" only to step 3 ("Project ancestor configs"). Step 4 ("Compat config dirs") is listed as unconditionally including "project `.opencode` dirs (cwd → worktree)".
- Reality: `compatConfigDirs` guards the project dirs with the same flag: `if (context.disableProjectConfig !== true) { dirs.push(...projectConfigDirs(context.directory, context.worktree)) }`. Setting `COMPAT_DISABLE_PROJECT_CONFIG=1` therefore also removes every `<ancestor>/.opencode` directory from step 4, so `<project>/.opencode/opencode.json` and every `.js`/`.ts` under `<project>/.opencode/plugin/` and `/plugins/` stop loading too. A reader following the doc would expect project-directory plugin files to still be scanned.
- Source: `crates/hya-plugin-compat/adapter/src/loader/config_dirs.ts:5-23 (compatConfigDirs), :25-38 (projectConfigFiles)`

**CONTRADICTION 3**

- The doc claims: The "OpenCode → hya hook translation" table is introduced by "duplicate hya names collapse to one registration (first match wins in adapter registration order)", implying the table rows are in adapter registration order; it places `tool.definition` 9th, between `experimental.chat.system.transform` and `permission.ask`.
- Reality: `HOOK_MAPPINGS` puts `tool.definition` LAST (13th), after `tool.execute.after`. Because `hookRegistrationsFrom` iterates `HOOK_MAPPINGS` in array order and dedups on the hya name, the emitted `hooks[]` array order differs from the doc whenever a plugin declares `tool.definition` plus a later-mapped OpenCode hook: e.g. a plugin exporting only `tool.definition` and `permission.ask` registers `permission.ask` first and `chat.params` second, the reverse of what the table implies. Low blast radius (the host does not depend on hooks[] order) but the table is not the registration order it claims to be.
- Source: `crates/hya-plugin-compat/adapter/src/registration.ts:7-21 (HOOK_MAPPINGS), :23-38 (hookRegistrationsFrom)`


### `docs/agent-bundle-authoring.md`

**STILL OPEN 1 - [config-key] resource_view allow/deny/aliases/namespace** (`thin`)

- Source: `?`
- Why it is still open: allow/deny/aliases and both hard failures (tool-vs-mcp NamespaceCollision, skill-facade requirement) are now correct and usable, but the `namespace` key is documented as "Optional prefix for public names; default is the bundle id" (line 227), which does not match the code. In runtime_registry.rs `assign_public_names`/`reserve_stable_names` (lines 1994-2100), `namespace` is substituted ONLY into the bundle-local qualified spelling `bundle:<namespace>/<kind>/<short>`; harness candidates keep `harness:tool/<name>` untouched, and no short public name is ever prefixed (test at runtime_registry.rs:3518 asserts exactly this). The doc's own worked example (lines 271-286) sets `namespace: explore` over an allow list containing only `harness:tool/*` and `harness:skill/*` entries, where the key provably has zero effect - so a reader following the section cannot predict what namespace does.


### `docs/self-update.md`

**CONTRADICTION 1**

- The doc claims: In the "Metadata field validation" table, attributed to `canonical_metadata_payload` in verify.rs: "Each artifact `sha256_hex` | Exactly **64 lower-hex** characters. Uppercase hex is rejected."
- Reality: `canonical_metadata_payload` checks only `artifact.sha256_hex.len() != 64 || !artifact.sha256_hex.bytes().all(|b| b.is_ascii_hexdigit())`. `is_ascii_hexdigit()` accepts A-F, so an uppercase digest PASSES this gate and can be signed and verified. It only fails much later in `verify_artifact_bytes`, which formats the computed digest with `{b:02x}` and does a plain string compare, yielding `ArtifactDigestMismatch` (a "wrong bytes" error) rather than a metadata-format rejection. The code's own error string says "lower-hex" but the predicate does not enforce it. Contrast `trust.rs::decode_hex_key`, which DOES have an explicit `is_ascii_uppercase()` rejection — the doc's identical claim at line 105 for `verifying_key_hex` is correct, which makes the line-129 row look verified when it is not.
- Source: `crates/hya-updater/src/verify.rs:64-70 (canonical_metadata_payload), crates/hya-updater/src/verify.rs:213-227 (verify_artifact_bytes), crates/hya-updater/src/trust.rs:77-93 (decode_hex_key, for contrast)`


### `docs/compat-parity.md`

**CRITIC 1 - GET /experimental/capabilities — client feature-detection endpoint**

- Source: `crates/hya-server/src/compat/experimental.rs:12 (route), :58-60 (handler)`
- Why it matters: Returns a hardcoded `{"backgroundSubagents": false}`. A Compat/OpenCode-compatible client that probes capabilities before enabling a feature path needs to know (a) that the endpoint exists and (b) that the value is a static constant rather than a live reflection of runtime state — so gating on it will never flip to true, no matter how the server is configured. The same constant is also embedded in the `/tui/bootstrap` payload under `capabilities`. docs/compat-parity.md line 105 covers its neighbours with the phrase "safe-default experimental console/workspace/resource/sync routes", but `capabilities` is not in that list, and neither the path nor the `backgroundSubagents` key appears anywhere in docs/**. This is a smaller gap than /tui/bootstrap — it is a stub, not a feature — but it is a routed, integrator-visible endpoint with zero documentation.

**CRITIC 2 - Enumeration of the built-in tool registry (how many canonical tools `ToolRegistry::builtins()` installs)**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-tool/src/tool.rs:313-346 — 19 tools in the loop, plus `shell`, the separately named `bash`, and 5 aliased canonicals = 26 canonical names; the loop includes `ask_user`, `list_agents`, `send`, `roster`, `channels`, `join`, `leave`.`
- Why it matters: docs/compat-parity.md:84 enumerates the registry as "Builtins include `invalid`, `read`, `write`, `edit`, `ls`, `glob`, `find`, `grep`, `shell`, `bash`, `question`, `lsp`, `skill`, `task`, plus the **advertised** Compat-facing names `apply_patch`, `webfetch`, `websearch`, `todowrite`, and `plan_exit`" — 19 names, and then closes the set with "The five **hidden** aliases are `patch`, `fetch`, `search`, `todo`, and `plan`", which reads as exhaustive. docs/architecture/tools-and-permissions.md:16 and docs/architecture/agent-tool-surface.md:24 both say `ToolRegistry::builtins()` installs **26** canonical schema names and list all of them. compat-parity.md silently drops the seven interaction/agent/mailbox tools (`ask_user`, `list_agents`, `send`, `roster`, `channels`, `join`, `leave`).


### `docs/testing/agent-matrix.md`

**CRITIC 1 - How to invoke the xtask dev tooling (`matrix-check`)**

- Source: `/chivier-disk/yanweiye/Projects/yaca/.github/workflows/ci.yml:72 — the CI gate runs `cargo run -p xtask -- matrix-check`; there is no `.cargo/config.toml` in the repo, so no `xtask` cargo alias exists. `docs/development.md:104-109` states this explicitly.`
- Why it matters: docs/testing/agent-matrix.md:165 says "`crates/hya-e2e/matrix.toml` is validated by `cargo xtask matrix-check`", while docs/development.md:104-105 says "There is **no** Cargo alias named `xtask` in this workspace: invoke it as `cargo run -p xtask -- <task> …`" and repeats at :109 that "the working invocation is still `cargo run -p xtask -- …`". A reader following agent-matrix.md gets `error: no such command: xtask`. agent-matrix.md is wrong.


### `docs/testing/process-e2e.md`

**CRITIC 1 - Range of Track P process-E2E test files in `crates/hya-e2e/tests/`**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-e2e/matrix.toml:143 (registers `crates/hya-e2e/tests/p16_swarm_mailbox.rs`); the directory holds p01…p16, including p12_context_api.rs, p13_project_agents_context.rs, p14_compact_summarize.rs, p15_todo_and_edit.rs, p16_swarm_mailbox.rs.`
- Why it matters: docs/testing/process-e2e.md:14 describes the layout as "`tests/p01_*.rs` … `p11_*.rs` | One scenario family per file; run alone with `cargo test -p hya-e2e --test p0N_…`", but docs/testing/agent-matrix.md:41-61 documents scenarios T1.12–T2.11 living in `tests/p12_context_api.rs` through `tests/p16_swarm_mailbox.rs`. process-e2e.md is stale — it understates the suite by five files and its `p0N_…` invocation template does not even cover p10/p11.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
