# Batch L - agent-bundle-authoring.md, skills.md

You are writing documentation for the **hya** repository at
`/chivier-disk/yanweiye/Projects/yaca`. This is a Rust workspace for a
terminal-first coding agent with a Bun/OpenTUI frontend.

## Your batch

You own exactly 2 file(s). Do not create or edit any other file.

- `docs/agent-bundle-authoring.md`
- `docs/skills.md`  **(new file)**

You have **16 gap entries** and **1 stale claims** to resolve.

docs/skills.md is NEW and is the canonical home for Skills authoring. Paired because bundle `resources.skills` overlaps skill discovery. Do not edit docs/README.md, docs/configuration.md, docs/cli.md, or docs/self-update.md that some entries mention -- other batches own those.

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

### `docs/agent-bundle-authoring.md`

**1. [config-key] harness_access (agent-level tool exposure)** — `thin` · severity medium

- Source: `crates/hya-bundle/src/model.rs:40-44, crates/hya-core/src/runtime_registry.rs:1500-1541`
- Evidence: docs/agent-bundle-authoring.md:112 is the only description: "`harness_access: none | basic | full` chooses the Harness-owned candidate set." It never says what each value actually exposes, so an author cannot pick between `basic` and `full`.
- Write: Expand line 112 into a table of the three values: `none` exposes no harness tools at all (the agent sees only Bundle-local tools); `basic` exposes only the ORIGINAL builtin tool snapshot — nothing contributed by MCP servers or plugins; `full` exposes builtins plus MCP- and plugin-contributed tools. Add the non-obvious rule that MCP exports are excluded from the `tool` resource kind because MCP is its own resource kind, so an MCP export is selected as an mcp reference rather than as a tool reference even under `full`.

**2. [config-key] resource_view allow/deny/aliases/namespace** — `thin` · severity medium

- Source: `crates/hya-bundle/src/model.rs:69-75, crates/hya-core/src/runtime_registry.rs:769-900`
- Evidence: docs/agent-bundle-authoring.md:112 says only "`resource_view.allow`, `deny`, `aliases`, and `namespace` deterministically narrow and name it." The collision rule and the skill-facade requirement — both hard errors an author will hit — are undocumented.
- Write: Give `resource_view` its own subsection with the four keys: `allow` and `deny` are reference lists selecting which tool/skill/mcp candidates enter the view; `aliases` renames selected entries; `namespace` prefixes the public names. Document the two hard failures: (1) tool and mcp resources SHARE one provider-facing namespace, so a tool name colliding with an MCP export's public name is a `NamespaceCollision` error that rejects the whole view — aliases or a namespace must disambiguate; (2) selecting any harness skill also requires selecting the `skill` tool facade, otherwise the view is rejected, because the skill body is only reachable through that tool.

**3. [behavior] can_spawn agent allowlist** — `thin` · severity medium

- Source: `crates/hya-app/src/runtime.rs:1907-1934`
- Evidence: docs/agent-bundle-authoring.md:106 and docs/architecture/agent-tool-surface.md:465,479 state that the roster and spawn derive from `can_spawn` reachability, but neither documents the two typed failures. Grep for `UNKNOWN_AGENT_ID` and `AGENT_SPAWN_NOT_ALLOWED` across all in-scope docs returns nothing.
- Write: Where `can_spawn` is described, add the enforcement outcome: a `task` call resolves `subagent_type` against the turn's bound agent definitions. An id that does not exist fails with the typed tool error `UNKNOWN_AGENT_ID: <id>`; an id that exists but is outside the caller's `can_spawn` reachability fails with `AGENT_SPAWN_NOT_ALLOWED: <caller> cannot spawn <id>`. Both are surfaced to the model as tool errors (types `unknown_agent_id` / `agent_spawn_not_allowed`), not as permission prompts — so widening `can_spawn` is the only fix; a permission rule cannot grant the spawn.

**4. bundle source manifest: bundle.yaml alternative form and the agent keys model_policy / workdir / resource_profile / description / color, plus resources.skills and resources.mcp** — `thin` · severity medium

- Source: `crates/hya-bundle/src/prepare.rs:474-517; crates/hya-bundle/src/source.rs:108-171`
- Evidence: docs/agent-bundle-authoring.md:22 says "Use one root `bundle.hya.md`" and never mentions that a `bundle.yaml` form exists, that a source dir must have EXACTLY ONE of the two (both present is an error, neither is UnsupportedSource, source.rs:108-171), or that the manifest is parsed with deny_unknown_fields. The example manifests cover api_version, kind, identity, resources.tools/hooks, extensions.js, and the agent keys local_id/stable_id/role/prompt/spawn_lifecycle/harness_access/resource_view/can_spawn/hook_refs, but `model_policy` (model, category, reasoning), `workdir`, `resource_profile`, `description`, `color`, `resources.skills`, and `resources.mcp` appear in no doc.
- Write: Add a `## Manifest reference` section that (a) states a bundle source directory must contain EXACTLY ONE of `bundle.yaml` or `bundle.hya.md` (YAML frontmatter + markdown prompt body); both present is a hard error and neither is UnsupportedSource (source.rs:108-171); (b) states the manifest is parsed with deny_unknown_fields, so an unrecognised key is a hard error, not a warning; and (c) gives the full key table: top level `api_version`, `kind`, `identity{id, version, publisher}`, `resources{tools, skills, mcp, hooks}`, `extensions{js, rust}`, `agents[]`; per-agent `local_id`, `stable_id`, `description`, `role` (main|subagent), `color`, `prompt`, `model_policy{model, category, reasoning}`, `workdir`, `spawn_lifecycle` (transient|resident, default transient), `resource_profile`, `harness_access` (none|basic|full), `resource_view{allow, deny, aliases, namespace}`, `can_spawn`, `hook_refs` (prepare.rs:474-517). Mark `extensions.rust` and `resources.mcp` as declared-but-unsupported in the current release, consistent with the existing Trust section.

**5. bundle.yaml directory-source manifest layout (item 80)** — `undocumented` · severity high

- Source: `crates/hya-bundle/src/prepare.rs:474`
- Evidence: grep 'bundle.yaml' across all in-scope docs: ZERO hits. docs/agent-bundle-authoring.md:22 instructs 'Use one root `bundle.hya.md`' and every example (docs/examples/bundle.hya.md, bun-transient, bun-resident, bun-disjoint) uses the markdown form only. Yet bundles/builtin/hya-core-agents/bundle.yaml and bundles/builtin/hya-development/bundle.yaml are real, shipped, prepared-at-build-time sources.
- Write: Document that a bundle source must contain EXACTLY ONE of `bundle.yaml` (directory form) or `bundle.hya.md` (single-agent markdown form); having both is an error. Explain bundle.hya.md's extra constraints: it requires YAML frontmatter fenced by ---, its markdown body becomes the prompt of its single agent, and that agent must NOT also name a prompt file. Explain that bundle.yaml is the plain-YAML manifest form used by the shipped built-in bundles and is required whenever the manifest is not carrying a body prompt. Point at bundles/builtin/hya-core-agents/bundle.yaml as a real example.

**6. bundle manifest `identity` constraints (item 81): id must contain '/' and match [A-Za-z0-9/-_.], version non-empty, deny_unknown_fields** — `thin` · severity medium

- Source: `crates/hya-bundle/src/model.rs:46`
- Evidence: docs/agent-bundle-authoring.md never states the identity rules; the block appears only in the examples (docs/examples/bun-disjoint/bundle.hya.md:4-7 shows id: hya/docs-bun-disjoint). docs/cli.md:95 mentions 'info reports identity, publisher, ...'. Nothing says an id without a slash is rejected.
- Write: Add an 'identity' field table: the block is REQUIRED and uses deny_unknown_fields (an extra key fails preparation). `id` must contain at least one '/' and may use only [A-Za-z0-9/-_.]; `version` must be non-empty; `publisher` is required. Give the canonical form <publisher>/<name> and note that structural checks never establish publisher authenticity.

**7. bundle manifest resources.tools[] aliases and resources.skills[] (items 82,83)** — `thin` · severity medium

- Source: `crates/hya-bundle/src/source.rs:149,158`
- Evidence: docs/examples/bun-disjoint/bundle.hya.md:8-14 shows resources.tools with id+path only. grep 'resources.tools', 'resources.skills' in scope: zero prose hits. No doc mentions the `aliases` key or skill resources at all, nor the stable_id scheme.
- Write: Add a resources reference: `resources.tools[]` and `resources.skills[]` both take {id, path, aliases[]}, where path names a file inside the bundle. Each gets a SHA-256 content digest and a stable id — `bundle:<bundle_id>/tool/<id>` for tools and `bundle:<bundle_id>/skill/<id>` for skills. Explain that `aliases` provide additional bundle-local names usable in an agent's resource_view, and that an alias colliding with an existing tool/skill name is an AliasCollision error. Add a skills example, since no current example ships one.

**8. bundle agent fields description, color, model_policy, workdir (items 90,92,94,95)** — `undocumented` · severity high

- Source: `crates/hya-bundle/src/source.rs:131,133,136; crates/hya-bundle/src/model.rs:63`
- Evidence: grep 'model_policy' across all in-scope docs: ZERO hits. The agent `description`, `color`, and `workdir` fields likewise never appear in prose or in any of the four shipped bundle examples. docs/agent-bundle-authoring.md's 'Stable identity, role, and spawn' section covers only stable_id, local_id, role, and spawn_lifecycle.
- Write: Add a full per-agent field table covering the four undocumented optional fields alongside the documented ones. `description` — optional human/model-facing text used in agent selectors and spawn menus (so omitting it makes the agent unlabeled in the picker). `color` — optional display color carried into the prepared agent. `model_policy` — optional {model, category, reasoning}, all three sub-fields optional, deny_unknown_fields; this is the per-agent model preference block. `workdir` — optional working-directory hint carried into the prepared agent. Show at least one example using model_policy.

**9. resource_view reference grammar and the bundle stable-id / ExportKind grammar (items 98,116)** — `thin` · severity high

- Source: `crates/hya-bundle/src/model.rs:71; crates/hya-bundle/src/catalog.rs:11`
- Evidence: docs/agent-bundle-authoring.md:112 is one sentence: 'resource_view.allow, deny, aliases, and namespace deterministically narrow and name it.' No doc gives an accepted reference string, and no example uses deny, aliases, or namespace (the examples only use allow with a bare local name). grep 'harness:tool', 'ExportKind', 'bundle:<' in scope: zero hits; only 'harness:hook/*' appears at line 78 as a REJECTED spelling.
- Write: Spell out the reference grammar authors must type. resource_view takes allow[], deny[], aliases{}, and namespace; allow/deny entries are sorted and deduped and each entry is resolved to a stable id. Accepted reference forms: `harness:tool/<name>`, `harness:skill/<name>`, `harness:mcp/<name>`, a fully-qualified `bundle:<bundle_id>/<kind>/<local_id>` stable id, or a bare bundle-local name or alias. The five ExportKind namespaces are tool, skill, mcp, hook, extension. An ambiguous bare name is an AliasCollision error, as is an alias key that collides with an existing tool or skill name. Add an example that actually uses deny, aliases, and namespace.

**10. Prepared catalog format (items 103,104,117): PREPARED_FORMAT_VERSION=1, canonical ordering, full digest re-verification on decode, and catalog semantic identity v1** — `undocumented` · severity medium

- Source: `crates/hya-bundle/src/prepare.rs:18,75; crates/hya-bundle/src/catalog.rs:8`
- Evidence: grep 'format_version' in scope: zero hits (the only 'format_version'-adjacent hit, docs/self-update.md:80, is the unrelated updater protocol_version). docs/cli.md:95 mentions info reporting 'digests' but nothing describes what is verified or the canonical ordering contract.
- Write: Document the prepared artifact: PREPARED_FORMAT_VERSION = 1, and the document is {format_version, bundles[], index[]} with a strict canonical ordering — bundles sorted by id, agents by stable_id, resources by local_id, and allow/deny/can_spawn/hook_refs each strictly sorted. Then document what PreparedCatalog::decode verifies before anything loads: the catalog SHA-256 against the expected digest, rejection of any non-canonical ordering, recomputation of every per-resource and per-prompt content digest, recomputation of each bundle digest (the bundle JSON minus its own digest field), re-validation of all references, and a rebuild-and-compare of the index. Also document catalog semantic identity v1: a domain-separated encoding over b"hya.bundle-catalog.semantic-identity/v1" plus sorted per-catalog records of {catalog digest, and each bundle's id/version/publisher/origin/immutable/digest}, so installing or removing any bundle changes the catalog identity.

**11. The two built-in bundles and how they are embedded (items 106,107,108,109): hya/core-agents, hya/development, build-time preparation, fail-closed builtin_catalog()** — `undocumented` · severity high

- Source: `bundles/builtin/hya-core-agents/bundle.yaml:4; bundles/builtin/hya-development/bundle.yaml:1; crates/hya-app/build.rs:17; crates/hya-app/src/runtime.rs:78`
- Evidence: grep 'core-agents', 'hya/development', 'builtin_catalog' across ALL in-scope docs: ZERO hits. docs/cli.md:96 says only 'Built-ins are merged read-only and cannot be replaced or uninstalled'. docs/configuration.md:90 mentions 'Falls back to the built-in `build` agent' without saying where `build` comes from. A user has no way to learn which agents ship by default.
- Write: Name and enumerate both shipped bundles. `hya/core-agents` ships build (main), plan (main), explore, general, compaction, summary, and title, with prompts under prompts/ and a shared can_spawn anchor listing all ordinary agents. `hya/development` ships hya-main, hya-planner, hya-implementer, hya-reviewer, hya-tester, hya-docs, hya-explorer, and hya-release, with prompts under prompts/. Explain the embedding mechanism: crates/hya-app/build.rs prepares bundles/builtin/hya-core-agents and bundles/builtin/hya-development into OUT_DIR/builtin-bundles.json plus a .sha256 at compile time, with cargo:rerun-if-changed on both source dirs — so editing a builtin bundle requires a rebuild, not a restart. And state the fail-closed rule: builtin_catalog() decodes and validates the embedded artifact exactly ONCE in a OnceLock and caches BOTH success and failure, so a tampered or invalid artifact keeps the process without any builtin agents for its whole lifetime.

**12. Public bundle package resource limits (item 112): 128 MiB archive, 64 MiB per-entry, 256 MiB expanded, 1000:1 ratio, 1024-byte path, 32-segment depth** — `undocumented` · severity medium

- Source: `crates/hya-bundle/src/package.rs:27`
- Evidence: docs/agent-bundle-authoring.md:29 covers the closure rules and failure modes (missing declared files, wrapper directories, duplicate normalized paths, traversal, absolute paths, non-regular files) but states no size or depth limits anywhere. grep '128 MiB', '256 MiB', 'expansion ratio' in scope: zero hits.
- Write: Add a 'Package limits' table so an author knows why a large bundle is rejected: max archive size 128 MiB; max per-entry manifest size 64 MiB; max total expanded size 256 MiB; max expansion ratio 1000:1, enforced STREAMING per chunk (so a zip-bomb aborts mid-read rather than after full extraction); max path length 1024 bytes; max path depth 32 segments.

**13. Bundle sidecar activation mechanics (items 126,127,128): resource materialization with owner-0000 slots, the `-- --bundle-extension <abs path>` launch contract, and activation_id path-safety validation** — `thin` · severity medium

- Source: `crates/hya-app/src/runtime.rs:134,717,1131`
- Evidence: docs/agent-bundle-authoring.md:88-90 and docs/architecture/runtime.md:117-154 describe the sidecar ABI and lifecycle at a conceptual level, but grep 'bundle-extension' returns ZERO hits, 'owner-0000' zero hits, and nothing states where the activation directory lives or how activation_id is validated.
- Write: Make the launch concrete. The host creates <bundle-registry-parent>/activations/<activation_id>, materializes each selected bundle tool/hook resource plus its unique exact-path-matching JS extension with create_new (multi-owner bundles get owner-0000/ style slots), then spawns the Bun compat adapter in that directory with env_clear(), appending `-- --bundle-extension <absolute path>` once per resolved entrypoint. The initialize reply must report protocol_version 1 and kind `compat` or the child is terminated. Also document activation_id validation: an empty id, or one containing '/', '\\', ':', or NUL, is rejected, so an activation id can never escape the staging root.

**STALE 1.** The document claims: 'Use one root `bundle.hya.md` with both v1 markers' — presented as the single source layout, and all four shipped examples use the markdown form.

- Reality: crates/hya-bundle/src/prepare.rs:474 accepts EXACTLY ONE of bundle.yaml (directory form) or bundle.hya.md. bundle.yaml is the form the shipped built-in bundles use (bundles/builtin/hya-core-agents/bundle.yaml, bundles/builtin/hya-development/bundle.yaml) and is never mentioned in any in-scope doc.
- Action: correct or delete. Do not merely supplement.

### `docs/skills.md`

**1. SKILL.md format and frontmatter fields (items 129,130): name, description, allowed-tools, model, disable, license** — `thin` · severity high

- Source: `crates/hya-tool/src/skill_catalog.rs:31,136`
- Evidence: grep 'allowed-tools' across all in-scope docs: ZERO hits. 'SKILL.md' appears 5 times (docs/project-structure.md:125 'Load and expose local SKILL.md content', docs/compat-parity.md:87, docs/hya-pi-compat-comparison.md:102, docs/testing/process-e2e.md:24, docs/FOLLOWUPS.md:24) and NONE show the file format, the frontmatter fence, or any field. There is no docs/skills.md and no skills entry in the docs/README.md Docs Map. A user cannot author a skill from the docs.
- Write: A complete authoring guide with a copy-pasteable example. Format: <dir>/SKILL.md, which MUST begin with a leading `---` fence; the frontmatter is parsed with serde_norway (YAML) and everything after it is the skill body. Field table: `name` (REQUIRED — a skill missing it is silently skipped), `description` (REQUIRED — same), `allowed-tools` (a per-skill tool allowlist; empty means unrestricted), `model` (a per-skill model override), `disable: true` (skips the skill entirely), and `license` (parsed but currently unused). Stress that missing name or description causes a SILENT skip with no error, which is the most common authoring mistake.

**2. Skill discovery search path — 11 directories, first-name-wins (item 131)** — `thin` · severity high

- Source: `crates/hya-tool/src/skill_catalog.rs:46`
- Evidence: docs/compat-parity.md:87 lists only '.hya/skills and ~/.config/hya/skills'; docs/hya-pi-compat-comparison.md:80 says 'hya skill locations such as .hya/skills and user config skill directories'. Neither enumerates the other nine directories nor states the precedence rule. Code scans eleven paths in a fixed order.
- Write: Give the ordered search path verbatim, because order determines which of two same-named skills wins: ./.hya/skills, ~/.config/hya/skills, ~/.claude/skills, ~/.config/opencode/skills, ~/.config/opencode/skill, ./.opencode/skills, ./.opencode/skill, ./.agents/skills, ~/.codex/skills, ~/.agents/skills. State that each IMMEDIATE subdirectory must contain a SKILL.md, and that the FIRST occurrence of a given skill name wins — later directories cannot override an earlier one. Correct docs/compat-parity.md:87's two-directory claim to point here.

**3. Built-in fallback skills customize-compat, agent-bundle-authoring, secure-self-update (item 136)** — `thin` · severity medium

- Source: `crates/hya-server/src/compat/skill_catalog.rs:22`
- Evidence: Only `secure-self-update` is mentioned, at docs/self-update.md:95 ('Built-in skill secure-self-update summarizes this workflow for agents'). grep 'customize-compat' in scope: zero hits; 'agent-bundle-authoring' hits are all links to the doc page, never the skill. Nothing describes the fallback mechanism or the '<built-in>' location marker.
- Write: Document the three shipped fallback skills — customize-compat, agent-bundle-authoring, and secure-self-update — and the mechanism: the server's skill listing APPENDS these include_str! templates with location "<built-in>" only when no discovered skill of the same name exists, so a user-authored skill with a matching name shadows the built-in entirely.

## When you are done

Report, in this order:

1. Each file you wrote and its approximate line count.
2. How many of the 16 gap entries you resolved. If any remain, name them.
3. Any entry where the source CONTRADICTED the work list, with the `file:line`
   you checked and what the code actually does.
4. Any claim you could NOT confirm from source and therefore omitted.
5. Any code defect you noticed. Do not fix it; just name it.
