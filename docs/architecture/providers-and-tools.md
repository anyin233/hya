# Providers, Tool Plane & Scoping

This page covers how Hya talks to different LLM APIs (`crates/hya-provider`), loads and sandboxes tool paths (`crates/hya-tool`, `crates/hya-tool-loader`), and scopes bundles/plugins per Project (`crates/hya-app`).

---

## 1. Provider Routing & Stream Normalization (`hya-provider`)

Models vary wildly on the wire—OpenAI Chat Completions, OpenAI Responses (`/responses`), ChatGPT Codex OAuth, xAI Grok Build, Anthropic Messages, and Google Gemini each encode tool calls, reasoning blocks, prompt caching, and token counts differently.

`hya-provider` hides all of those quirks behind `ProviderRouter`:

- **One canonical event stream:** Every upstream SSE stream decodes into the exact same `hya_proto::Event` sequence (`StepStarted`, `ReasoningDelta`, `TextDelta`, `ToolCallRequested`, `UsageRecorded`, `MessageFinished`).
- **Normalized token accounting:** Across every vendor, `TokenUsage.input` excludes cached tokens (`prompt_total = input + cache_read + cache_write`) and `output` includes `reasoning` (`thinking + visible + unknown == output`).
- **Reasoning effort & model overrides:** `HttpProvider::with_model_reasoning_defaults` installs per-model defaults from `config.yaml` overrides ([ADR-0029](../adr/0029-config-model-entries-override-the-model-cache.md)). `Provider::reasoning_default` and `Provider::supports_reasoning_effort` resolve metadata from the **first match** in the router. Non-Workflow model refs accept `#variant` suffixes (stripped before upstream dispatch), while `Workflow` stage routes validate explicit `reasoning:` fields per candidate during admission.
- **Tool-result images:** Images read by tools (`png`, `jpeg`, `gif`, `webp`) are attached only on the round immediately following the `read` call (up to 2 MiB per round) and replaced by a compact text note on subsequent rounds so history size doesn't explode.

---

## 2. Native Tool Families & Lockstep ABI (`hya-tool`, `hya-tool-loader`)

Hya's 25 full-mode built-in tools live outside the core crate in four native dynamic-library bundles under `bundles/builtin/{base,extended,network,channel}-tools/native`:

- **Why split them out?** Keeps `hya-core` tiny while letting each family (`hya-base-tools`, `hya-extended-tools`, `hya-network-tools`, `hya-channel-tools`) compile cleanly and declare its visibility, default permission posture (`read_only`, `task`, `tool`, `command`, `mcp`), aliases, and `overrides:` (such as `channel-tools`'s mail-aware `wait` overriding `extended-tools`'s `wait`) in `exposure.yaml`.
- **Lockstep ABI guard:** `hya-tool-loader` verifies a 32-byte `abi_digest_v1` hash covering the Hya version, `rustc -V`, target OS/arch, struct layouts, and `tool.rs`/`deadline.rs` source bytes before calling `hya_tool_bundle_register_v1`, preventing mismatched `.so`/`.dylib` libraries from ever loading.
- **Output caps:** Every tool output passes through host envelope bounds (50 KiB coding text caps, 5,000-char default plugin/bundle caps with automatic spill to `.hya/tool-output/` or `artifact://`) so one noisy command can never blow out a model's context window.

---

## 3. Multi-Root `ProjectScope` & `PermissionPlane`

File tools (`read`, `write`, `edit`, `ls`, `glob`, `find`, `grep`, `apply_patch`, `lsp`) enforce workspace containment through `hya_tool::ProjectScope` ([ADR-0026](../adr/0026-multi-root-path-permissions.md)):

1. Every target path—and its nearest existing ancestor for not-yet-created files—is canonicalized on disk **resolving all symlinks**. A symlink inside your repo pointing to `/etc/passwd` resolves to `/etc/passwd` (outside!).
2. If the canonical path sits inside **any** ordered root of the session's Project, no outside-directory prompt is needed.
3. Otherwise, `ProjectScope` raises an `ExternalDirectory` permission check for `<canonical-parent-dir>/*` *before* running the tool's normal read/edit check. Saving *Always allow* scopes that grant strictly to the active `project_id`.

---

## 4. Lazy Per-Project `CatalogScope` (`hya-app` & `hya-engine`)

Because one persistent daemon serves many Projects on your machine ([ADR-0027](../adr/0027-per-project-catalog-tier.md)), Hya never loads `.hya/` folders from the directory `hya serve` happened to start in.

Instead, `RuntimeRegistry` maintains an LRU + idle-TTL cache (`catalog_scopes: { max: 32, idle_ttl_secs: 1800 }`) keyed by `CatalogScope`:
- **`Global`:** Built-in presets + user-installed bundles + global `config.yaml` MCP/plugins + user skills/commands.
- **`Directory(path)`:** Adds disk skills/commands/`AGENTS.md` for an unregistered folder (never spawns untrusted project processes).
- **`Project { id, roots }`:** Layers `<root>/.hya/bundles` and `<root>/.hya/plugins` across all roots of the registered Project (first root wins on ID collisions) on top of the user and built-in tiers. When a Project sits idle past `idle_ttl_secs` with no active turns, its project-scoped sidecar processes are cleanly stopped and reclaimed.
