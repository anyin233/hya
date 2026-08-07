# Fix batch F6 - skills.md, troubleshooting.md, self-update.md, overview.md, project-structure.md

You are correcting documentation in the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`.

These documents were rewritten in a large coverage pass. An INDEPENDENT audit then
re-read them against the source and found the problems below. Your job is to fix
exactly these problems.

## Your files

- `docs/skills.md`
- `docs/troubleshooting.md`
- `docs/self-update.md`
- `docs/architecture/overview.md`
- `docs/project-structure.md`

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


### `docs/skills.md`

**CONTRADICTION 1**

- The doc claims: `allowed-tools` | no | Per-skill tool allowlist (list of strings). **Empty or absent means unrestricted.** -- and, in 'How skills surface to the model': the body is loaded '...subject to any `allowed-tools` policy on that skill.'
- Reality: allowed_tools is parsed into SkillCatalogEntry and then used ONLY as input bytes to the skill-view semantic-identity digest. No code path filters, gates, or restricts tools based on it. There is no 'allowed-tools policy' at runtime; a skill declaring `allowed-tools: [read]` still leaves every tool available.
- Source: `crates/hya-tool/src/skill_catalog.rs:47-48,120 ; crates/hya-core/src/runtime_registry.rs:1451-1456 (only consumer)`

**CONTRADICTION 2**

- The doc claims: `model` | no | Optional per-skill model override.
- Reality: skill.model is parsed and then only hashed into the semantic identity. Nothing in the model-routing path (hya-core/src/category.rs, the turn/completion path) reads a skill's model, so declaring `model:` in SKILL.md does not override the model for that skill.
- Source: `crates/hya-tool/src/skill_catalog.rs:166 ; crates/hya-core/src/runtime_registry.rs:1458-1465 (only consumer)`

**STILL OPEN 1 - SKILL.md frontmatter fields (name, description, allowed-tools, model, disable, license)** (`contradicted`)

- Source: `?`
- Why it is still open: The frontmatter table now exists and is complete, but two of the six keys carry wrong semantics. `allowed-tools` is described as a "Per-skill tool allowlist" and `model` as an "Optional per-skill model override", and the table singles out only `license` as "Parsed but currently unused" -- which tells a reader the other two ARE enforced. In the source neither is enforced anywhere: `SkillCatalogEntry.allowed_tools` and `.model` are read in crates/hya-tool/src/skill_catalog.rs and then consumed only by append_skill_view_identity (crates/hya-core/src/runtime_registry.rs:1451-1465) to feed the semantic-identity hash. There is no tool-filtering site and no model-routing site (grep for allowed_tools / skill.model across crates/ and packages/ finds no other consumer). A skill author who writes `allowed-tools: [read, grep]` gets no restriction, and `model:` changes nothing. The doc must say both are parsed-but-not-yet-enforced, the same way it does for `license`.


### `docs/troubleshooting.md`

**CONTRADICTION 1**

- The doc claims: "The response body is truncated to the **first 500 characters**, so a long HTML error page is cut off." (section `Provider Call Fails with http: <status>: ...`)
- Reality: The code is `text.get(..500).unwrap_or(text.as_str())`, which indexes by BYTE, not character, and falls back to the ENTIRE untruncated body when byte 500 is not a UTF-8 char boundary. A non-ASCII upstream error body (e.g. a localized JSON error) is therefore either cut at a byte offset below 500 characters or not truncated at all — the stated guarantee does not hold. Same pattern applies to the compact path (http.rs:578) and the OAuth catalog errors (models_catalog.rs:67, :109) which use 400 bytes.
- Source: `crates/hya-provider/src/http.rs:519-521`


### `docs/self-update.md`

**CRITIC 1 - `hya-updater apply --trust-roots <PATH>`**

- Source: `/chivier-disk/yanweiye/Projects/yaca/crates/hya-updater/src/bin/hya-updater.rs lines 63-65 — `/// Override trust roots path (default: <root>/trust_roots.json)` on the `Apply` subcommand`
- Why it matters: This is an operator-facing flag on the security TCB binary: it selects which ed25519 verifying keys are used to validate a release signature. Every other flag on `apply` (`--root`, `--metadata`, `--package`, `--platform`, `--smoke`, `--owner-authorized-activation`) is documented, and `docs/self-update.md` documents the `trust_roots.json` file format and the `init-roots` command that writes it — but the flag that points `apply` at a trust-roots file somewhere other than `<root>/trust_roots.json` is absent from all of docs/. A grep for `--trust` across docs/ returns nothing. Operators keeping trust roots on separate/read-only media, or verifying a candidate against a staged key set during rotation, cannot discover the flag short of running `--help` or reading the source. docs/cli.md's `hya-updater` example block (lines 499-512) also omits it.


### `docs/architecture/overview.md`

**CRITIC 1 - Whether the assistant turn's provider/tool round loop has a hard iteration cap**

- Source: `crates/hya-core/src/engine/turn.rs:546 opens `loop {` in `run_turn_rounds`; `rounds` is declared at :533, used only as the `StepStarted`/`StepFinished` step index at :663, and incremented at :871. There is no comparison of `rounds` against any limit anywhere in the function — the only exits are cancellation, error, and a round that produced no tool calls. The `--max-iterations` cap (default 6) in crates/hya-backend/src/cli_args.rs:19-20 applies to `-p` goal mode, not to this loop.`
- Why it matters: docs/architecture/overview.md:53 states "A hard cap stops runaway tool loops." docs/architecture/runtime.md:174-176 states "The turn continues until the provider finishes, cancellation is observed, or execution returns an error" — i.e. no cap. runtime.md matches the source; overview.md asserts a safety property the runtime does not have, which is exactly the kind of claim an operator would rely on when deciding they need no external guard.


### `docs/project-structure.md`

**CRITIC 1 - Whether `bash` is a hidden alias of `shell` or a second advertised canonical tool**

- Source: `crates/hya-tool/src/tool.rs:340 registers `bash` via `insert_named_builtin`, whose body (tool.rs:507-519) inserts into `inner.tools` — the canonical map that feeds `ToolRegistry::schemas()`. The hidden-alias map `inner.aliases` is populated only by `insert_aliased_builtin` at tool.rs:341-345, giving exactly five aliases: `patch`, `fetch`, `search`, `todo`, `plan`. `bash` is not among them.`
- Why it matters: docs/project-structure.md:108 describes `shell.rs` as the "Shell execution tool and `bash` alias". docs/architecture/agent-tool-surface.md:32 says `shell`, `bash` are "Two advertised names backed by the same shell implementation", and :199-208 enumerates the aliases as exactly five, excluding `bash`; docs/architecture/tools-and-permissions.md:52-53 lists the same five. Calling `bash` an alias implies it is hidden from the model's tool list, when in fact both names are advertised — which changes the expected schema count (26 canonical names, not 25) and what a permission `^bash$` tool selector will match.

## When you are done

Report:

1. Each file changed and what you changed in it.
2. Any finding where the SOURCE supported the existing documentation, so you kept
   it. Name the finding and the `file:line` you checked.
3. Any finding you could not resolve, and why.
