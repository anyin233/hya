<!--
  Built-in skill. Name and description are registered in
  skill_catalog.rs. The body below becomes the skill content.
-->

# AgentBundle authoring

Author and install a 0.34.11 public `AgentBundle`. Static-only Bundles remain process-free. A public executable Bundle may add one activation-scoped Bun Compat sidecar for Bundle-local tools, hooks, and event handlers; Harness remains the agent runtime.

Repository references:

- Authoring guide: `docs/agent-bundle-authoring.md`
- Static example: `docs/examples/bundle.hya.md`
- Transient Bun example: `docs/examples/bun-transient/`
- Resident Bun example: `docs/examples/bun-resident/`
- Working split-entrypoint example: [`docs/examples/bun-disjoint`](docs/examples/bun-disjoint/) (`bun-disjoint`)

## Runtime boundary

Harness owns the agent prompt/model loop, task and mailbox input, tool permission checks, events, projection, MemberOutcome, admission, cancellation, and recovery. There is one sidecar per activation. The sidecar never runs the agent/model loop; there is no `agent/invoke`, no sidecar send/wait, and no terminal/artifact result. It never receives the task, prompt, transcript, model state, grants, or runtime snapshot.

Harness's `SessionEngine` remains the only agent runtime. The sidecar never receives task/prompt/transcript or returns `MemberOutcome`; each executable activation owns one per-activation process.

Static-only Bundles remain process-free. Bun Compat is the only executable sidecar implementation supported in 0.34.11.

## Source and exact package closure

Use one root `bundle.hya.md` with both v1 markers:

```yaml
kind: AgentBundle
```

For the Markdown form, a nonempty Markdown body remains the prompt for exactly one agent and needs no prompt field; omit frontmatter `prompt:`. An empty Markdown body plus explicit per-agent `prompt:` paths enables multiple agents; every agent in that form needs an explicit prompt path. No second manifest/schema/loader is introduced. A public archive contains the root `bundle.hya.md` plus exactly the normalized paths represented by the existing v1 agent prompt/resource/Extension contract, and nothing else. The archive rules are: undeclared directory files are ignored; unreferenced archive files are rejected. Missing declared files, wrapper directories, duplicate normalized paths, traversal, absolute paths, and non-regular files fail closed. Directory and archive forms of the same declared closure prepare to the same canonical identity and digest.

A minimal executable source tree is:

```text
bundle.hya.md
extensions/runtime.js
```

The manifest's Tool selector and JavaScript Extension entrypoint both reference the same `extensions/runtime.js` source, archived once under the controlled cross-kind exact-path rule. The 0.34.11 public JS profile admits only self-contained selected Extension entrypoint files; no separate Bundle-local helper file kind or transitive JS source closure exists. Use external single-file bundling before packaging; activation never executes the authoring tree. Only selected captured PreparedResource bytes are rematerialized for activation. A missing relative helper import fails before ACK, with existing cleanup handling the failure before model or dispatch. There is no import scanner, dependency installer, or new dependency guarantee; do not add files not represented by the v1 source contract.

## Selected Tool/Hook resources and the disjoint example

The working split-entrypoint example is [`docs/examples/bun-disjoint`](docs/examples/bun-disjoint/) (`bun-disjoint`) and has this six-file layout:

```text
docs/examples/bun-disjoint/
├── bundle.hya.md
├── prompts/
│   ├── alpha.md
│   ├── beta.md
│   └── static.md
└── extensions/
    ├── alpha.js
    └── beta.js
```

From `docs/examples/bun-disjoint/`, archive exactly the root, three prompts, and two extensions:

```sh
7z a -t7z -mx=0 -ms=off bun-disjoint.hyabundle bundle.hya.md prompts/alpha.md prompts/beta.md prompts/static.md extensions/alpha.js extensions/beta.js
```

`hook_refs` select Bundle-local Hook resources only; all `harness:hook/*` spellings reject. Harness host hooks stay outside AgentBundle metadata. They use the existing local/alias/canonical resource-reference grammar; prepare canonicalizes them to stable Hook resource IDs. The supported hook IDs are exactly `event`, `tool.execute.before`, and `tool.execute.after`; aliases do not rename hooks.

Each selected Tool or Hook source path must exact-path match exactly one JavaScript Extension resource in the referenced resource's owning bundle; cross-bundle selection joins in the owner. Never basename/prefix/digest/alias inference. The activation rule is: only selected Tool/Hook resources determine a deduplicated deterministic entrypoint list; staged does not mean activated.

Tool and Hook initialize declarations independently equal the selected expected sets regardless of order; missing, extra, duplicate, or unselected declarations reject. The contract is: tool-only reports zero hooks and hook-only reports zero tools. When authoring, generic superset modules are rejected and must be split; authors may instead select the complete set.

The alpha and beta executable agents have disjoint selected closures: alpha selects `echo` and `event` from `extensions/alpha.js`, while beta selects `beta` and `tool.execute.before` from `extensions/beta.js`. The `docs-bun-static` agent selects neither and remains process-free. Do not add a second manifest/DTO/loader/import graph/provenance, schema, free Hook mapping, generic extension auto-load, or claim sandboxing.

## Sidecar ABI

The sidecar wire is newline-delimited JSON-RPC 2.0 using hya plugin protocol version 1. Initialize remains request/reply: initialize retains existing `protocol_version` and `host` fields, and the only activation-specific metadata is `{ activation_id, lifecycle }`. Activation begins only after initialize succeeds and declarations match the prepared Bundle. `tool/call` and `hook/*` are request/reply. `event` is one-way and has no id or result.

stdout is protocol-only; bare or malformed stdout is a protocol failure. stderr is diagnostic-only and bounded. The sidecar cannot issue inbound Harness requests. Never invent a second transport or task/result protocol.

## Lifecycle and recovery

- `spawn_lifecycle: transient` starts one sidecar for the whole Harness activation and shuts it down/reaps it when the activation ends.
- `spawn_lifecycle: resident` reuses one healthy sidecar across mailbox messages. Its in-process state is volatile. Process loss never replays completed messages or effects.
- explicit stop is final and idempotent: Harness rejects new sends, fences running work, cancels queued work, shuts down or terminates/reaps the child, removes the resident, and releases its claim.
- There is no TTL, heartbeat, idle reclaim, process adoption, or persisted PID/stdio state.

## Stable identity, role, and spawn

`stable_id` is the public `AgentName`; preserve its bytes for events, projection, replay, fork, and resume. `local_id` is not a replacement public identity.

- `role: main` is selectable in the TUI direct selector.
- `role: subagent` is hidden from direct TUI selection.
- `role` controls selector visibility only. Agent-facing roster and ordinary spawn derive from the caller's `can_spawn` reachability, never from `role`.
- `spawn_lifecycle` is orthogonal to `role`.
- Explicit unknown or denied targets fail closed: catalog lookup does not rewrite an unknown `subagent_type` to `general`. Empty or omitted `subagent_type` on the `task` tool still normalizes to `general` before authorization.

## Resources and permissions

`harness_access: none | basic | full` chooses the Harness-owned candidate set. `resource_view.allow`, `deny`, `aliases`, and `namespace` deterministically narrow and name it. Bundle-local names keep Bundle precedence and canonical qualified names remain exact.

Bundle-local tool calls resolve against the activation's captured catalog binding. Existing `PermissionPlane` and plugin policy run before `tool/call`; denial prevents RPC. `PermissionPlane`/plugin policy remain the final gate before sidecar RPC. A Bundle adds no sandbox and causes no permission expansion. Host tools, static skills, and host-managed MCP remain governed by the Harness view.

## Package workflow

Create an exact lowercase `.hyabundle` with an external `7z`; runtime inspection never shells to system `7z`:

```sh
7z a -t7z -mx=0 -ms=off example.hyabundle bundle.hya.md extensions/runtime.js
hya bundle info -f example.hyabundle
hya bundle install example.hyabundle
hya bundle list
hya bundle info <bundle-id>
hya bundle uninstall <bundle-id>
```

`hya bundle info -f` does not mutate registry or publication. Content magic, not the suffix, selects public/private parsing after the exact lowercase command suffix check.

## Trust and unsupported combinations

Only public Bundles are supported for activation in 0.34.11. Private inspection reports `authentication=unverified` and `payload=opaque`; private activation unsupported and generation-preserving. Raw Rust extensions, Bundle-declared MCP, and resource profiles without an enforceable current host mapping are unsupported. Structural and declared-digest checks do not establish publisher authenticity. There is no sandbox and no permission expansion. Do not add decryption, signatures, a marketplace, compilation on activation, native commands, arbitrary environment access, a second permission plane, or legacy agent-file discovery.
