# Modular harness, native swarm, AgentBundle, runtime refresh, and secure update design

## 1. Planning status and evidence anchor

This document combines future target design with explicitly marked delivered
contracts. Release `0.34.6` is committed/pushed at
`680f9fb535fc48f71f9aead64cc3d3d30161678a`; remote CI run `30634501761`
attempt 2 is green. The user has now authorized only the `0.34.7` resident
recovery, TTL-free actor claim/epoch, and minimal correctness-fencing slice.
Every later patch and every production/owner gate remains unauthorized until
its recorded entry conditions are met.

- Authoritative development workspace: the saved project on the
  `fuji1 remote worker` at
  `/chivier-disk/yanweiye/Projects/yaca`.
- Audited `main` HEAD:
  `267bfc3c6c66e46fe8514e2e70657489f853b7f0`.
- The `origin/main` tracking ref matched that commit with `0/0` ahead/behind
  during the audit. On 2026-07-31 a direct remote query and fetch observed
  `origin/main` at `156d0ad3c50aea67dfac0054485eb6991e77308b`; the sole intervening
  commit changes only the README icon reference. The isolated feature branch
  is rebased to that newer implementation base, while dirty `main` remains at
  the audit commit.
- `[workspace.package].version` at the audit anchor is `0.34.2`; delivered
  releases `0.34.3` through `0.34.6` are in the isolated branch, and `0.34.7` is
  the active release target.
- Before this task directory was created, `git status --porcelain` contained
  exactly 19 user-owned entries. They are enumerated in
  `research/fuji1-sync-preflight.md`. The untracked task directory is the
  twentieth status entry. Product and task changes for the active release are
  confined to the isolated worktree.
- Dirty `hya-sdk` and startup-benchmark files were excluded from HEAD behavior
  claims.

Every consequential statement uses one of these evidence levels:

| Level | Meaning |
| --- | --- |
| **Implementation** | Directly present in the anchored source. |
| **Accepted ADR/document** | An accepted or descriptive contract that may be ahead of implementation. |
| **Inference requiring test** | A plausible risk or absence that must be proved by a test, benchmark, or fault injection. |
| **Target contract** | Behavior proposed here for future implementation. |

The detailed source evidence lives in `research/head-architecture-audit.md`.
Future implementation must update that evidence if HEAD changes; it must not
promote summaries or the absence of a symbol into a fact.

### 1.1 Current-cycle owner supersession

This section overrides any broader exploratory target below:

- no sandbox, seccomp/container work, capability broker, escrow delegation, or
  independent `SecurityEpoch` is built in this patch sequence;
- explicitly installed JS/Rust AgentBundle and plugin code is trusted same-UID
  extension code; the Harness does not isolate malicious plugins/bundles;
- Harness config and the existing `PermissionPlane`/dispatch path are the only
  permission authority. Bundle/plugin declarations can narrow that policy but
  never expand it;
- admission, cancellation, binding consistency, crash containment, actor
  epochs, `OperationId`, and effect fencing remain correctness/performance
  mechanisms;
- the third-round Pro result fixes the ABI-neutral AgentBundle catalog/flat
  manifest and namespace/resource-view rules in section 5.2, but is advisory
  provenance. A/B/C execution, context transfer, and resident idle/turn
  semantics still require explicit owner selection;
- the active patch/release sequence is authoritative in
  `research/next-step-roadmap.md`; only `0.34.7` is active now.

### 1.2 Controlling native-only cutover ruling

The owner has now ordered **DROP ALL LEGACY AGENT SUPPORT**. Consequently:

- no old-agent adapter, synthetic representation, old-source CLI surface, or
  agent-file loader/runtime fallback may be designed or implemented;
- all built-in agents must become native AgentBundle definitions, and the old
  agent parser/discovery/execution branch is deleted in the same cutover;
- `research/agent-capability-parity-matrix.md` is a native-Bundle migration
  proof, not a compatibility mechanism;
- historical event/session agent IDs remain replayable without making old
  agent files runnable.

`role: main|subagent` controls visibility only.
`spawn_lifecycle: transient|resident` controls native-spawn behavior. Harness,
not the Bundle manifest, owns the lifecycle of a TUI-selected root Session.

The MacBook Air coordinator returned Pro round six on 2026-07-31 and adopted
the native-only result with two corrections: no premature `0.34.4` Bundle
cutover and no temporary old-file detector. Built-in packages are prepared
deterministically at build time, embedded read-only with a digest-bound index,
and merged with installed packages into one immutable generation. The
authoritative patch order is in section 19 and
`research/next-step-roadmap.md`. That ruling did not broaden the then-active
`0.34.3` slice and does not broaden the active `0.34.7` boundary.

## 2. Five target gaps

| Product target | Corrected HEAD gap | Target outcome |
| --- | --- | --- |
| Modular coding harness | **Implementation:** `hya-app` composes mutable/process-specific managers directly; discovery has startup-static, spawn-live, and round-live visibility regimes. | Deep authorities own generation, binding, admission, actors/effects, and update; existing `PermissionPlane` remains the permission boundary. Discovery/process managers become adapters. |
| Native 100+ subagent swarm | **Implementation:** 128 limits only depth-greater-than-zero provider streams; spawn intake and task-per-request fan-out are unbounded; a new background transient session is allocated before `run_team` reserve; resident spawn bypasses transient reserve/depth accounting; resident execution is not rehydrated after restart. | One durable bounded authority admits before every allocation and demonstrates 100 active subagent work items, 156 durably non-active items, typed overload at item 257, and restart/fault recovery. |
| Per-agent Markdown/JS/Rust `AgentBundle` | **Implementation:** `AgentSpec` lacks bundle/catalog identity; parsed agent permissions/options and skill `allowed-tools`/`model` do not reach an enforced runtime view; plugin subprocesses are ordinary same-UID children. | One flat Harness-owned catalog manifest defines identity/extensions/resources/agents; agent views and permission overlays only narrow current Harness policy; executable code is explicitly trusted and not malicious-code isolated. |
| Atomic runtime registry refresh | **Implementation through `0.34.5`:** `RuntimeRegistry` is the single effective snapshot owner; a turn retains one `TurnBinding` for prompt skills, schemas, resolution, and dispatch; deferred MCP publishes one complete candidate. | `0.34.6` adds dynamic MCP desired/observed/effective reconciliation and plugin startup/crash re-handshake consistency for tool exports plus RPC binding, without replacing the `0.34.5` publisher/binding authority or claiming plugin hot reload. |
| Secure Rust self-update | **Implementation:** `install.sh` already stages, smokes, backs up, and restores; release CI already emits hashes/provenance. | A separately protected verifier/activator adds canonical signatures, anti-replay/anti-rollback state, immutable runtime generations, crash-consistent activation, and forward-only rollback epochs while retaining the installer as break-glass recovery. |

## 3. Terms and non-negotiable invariants

### 3.1 Stable identities

Names are presentation and compatibility aliases. They are never sufficient
authority or persistence keys.

| Identity | Contract |
| --- | --- |
| `SourceId` | Stable namespace owner for a builtin set, config source, MCP server, plugin, skill source, or bundle publisher. It is explicit configuration/state, not a PID or filesystem scan order. |
| `ToolId` | Stable logical declaration key within a `SourceId`; independent of display name, alias, registry order, process incarnation, schema version, and artifact version. |
| `DeclarationId` | Content identity of one canonical declaration: schema, protocol, effect class, requested capability ceiling, and compatibility metadata. |
| `ArtifactId` | Digest identity for immutable Markdown data, JS packages, Rust source/binaries, or runtime artifacts. |
| `BundleId` / `BundleRevisionId` | Stable logical agent-bundle identity and the digest of one canonical manifest revision. |
| `BindingId` | Exact verified relationship among a logical declaration, immutable artifact, protocol, observed resource identity, and compatible executor. |
| `BindingSetId` | Content identity of the complete immutable structural view for a Turn attempt: tools, aliases, schemas, executors, model route, skills, instructions, bundle revision, and structural capability ceilings. |
| `TurnId` | Stable logical admitted user/team turn. |
| `TurnAttemptId` | Durable identity of one execution attempt. Transport/model retries and crash recovery of that attempt keep its binding; an explicitly restarted terminal attempt receives a new ID. |
| `OperationId` | For the `0.34.4` tool-call admission slice, derived deterministically and domain-separately by fixed-namespace UUIDv5 from the already-persisted UUID-backed `ToolCallId`. It has no independent random constructor and is not exposed through Event/HTTP/CLI surfaces. Later non-tool-call effects require a separate owner-approved identity rule. |
| `ActorId` | Stable logical root/resident/transient actor identity across process incarnations. |

Collision or ambiguous ownership fails the whole candidate. Builtins, static
MCP, dynamic Compat MCP, and plugins do not win collisions by discovery order.
External tool names remain aliases during migration, but internal resolution
uses `ToolId` plus the pinned binding.

### 3.2 Forward-only epochs

- `activation_epoch` is process-wide and increments for every effective
  structural activation. Selecting older content during rollback still creates
  a new, strictly greater epoch.
- `actor_epoch` is durable and monotonic per `ActorId`. Only the current
  incarnation may consume leases, advance cursors, publish results, or commit
  effects.
- Epoch allocation and the journal record it guards are one transaction.
  Recovery never guesses or decrements an epoch.
- There is no independent `SecurityEpoch` in the current-cycle design.
  Permission decisions continue through Harness config and the existing
  `PermissionPlane`; future changes cannot infer a new security authority from
  `activation_epoch` or `actor_epoch`.

### 3.3 Turn and effect invariants

1. A `TurnAttemptId` pins exactly one `BindingSetId` and
   `activation_epoch` before its first provider round.
2. Every provider-visible schema and every later dispatch/alias/executor lookup
   comes from that same binding set.
3. A retry that remains the same attempt keeps the same binding. Rebinding
   requires a new explicit attempt after the old attempt is terminal or fenced.
4. Structural additions/removals become visible to newly admitted attempts.
   Existing permission evaluation remains authoritative at dispatch.
5. Immediately before an effect's linearization point, one `EffectGate`
   revalidates the current `actor_epoch`, binding, admission/lease state,
   `OperationId`, and the existing PermissionPlane result where the current
   dispatch path requires it.
6. Failure to validate a binding, reconcile a lease, prove the current actor
   epoch, or obtain the existing permission decision
   fails closed for new effects.
7. `OperationId`-journaled idempotent effects may be conclusively deduplicated by
   `OperationId`. An indeterminate non-idempotent external effect is not
   automatically retried unless the remote system honors the same idempotency
   key or a reconciler proves the outcome.

## 4. Authority and module boundaries

The six lanes are deep modules with one state-transition owner each. Supporting
ID/event types remain dependency-light in `hya-proto`; durable append/replay and
transactions remain in `hya-store`; `hya-app` wires authorities and adapters but
does not become a second authority.

| Lane and owner | Owns | Adapters / consumers |
| --- | --- | --- |
| `GenerationAuthority` | Canonical inputs, immutable `ConfigGeneration`, bundle manifests, source precedence, candidate validation | Filesystem/config/AGENTS/agent/skill loaders; plugin/MCP desired declarations |
| `BindingAuthority` | Desired/observed/effective reconciliation, immutable binding sets, activation lifecycle and Turn pinning | `hya-tool`, `hya-mcp`, `hya-plugin`, provider routing, skill/agent adapters |
| Existing `PermissionPlane` boundary | Harness-configured ask/allow/deny evaluation, plugin/bundle narrowing input, logging, and fail-closed dispatch errors; no independent security epoch or second framework | Existing permission/interaction adapters and effect dispatch |
| `AdmissionAuthority` | Durable acceptance, bounded queues, phase resource vectors, fairness, leases, overload and recovery | Root/control, foreground/background, transient/resident, provider/process/storage adapters |
| `ActorEffectAuthority` | Actor reconstruction/epochs, cursor and wake state, operation journal, effect classes and fences | Resident/transient execution, mailbox, tool/plugin/MCP effect adapters |
| Independent `UpdateAuthority` | Signed metadata, trust roots, staging, smoke, activation journal, selector, anti-rollback floor | Release artifacts and a minimal OS/package integration; never plugins/MCP/bundles/provider code |

No adapter may publish itself as effective. It can report desired configuration
or observed state only. No queued item owns a Tokio task; tasks exist only for
bounded active phases.

### 4.1 MacBook Air Browser / ChatGPT Pro advisory boundary

Browser/Pro is a task-coordination consultation channel outside the six runtime
authorities above. It receives no authority ID, epoch, binding, capability,
admission slot, revocation role, fencing role, event-writing role, or updater
privilege. The canonical role/host/session identities are defined once in
`research/browser-pro-escalation-protocol.md`.

- The `MacBook Air coordinator` source session is the sole controller of in-app
  `[@Browser](plugin://browser@openai-bundled)` and the sole participant allowed
  to select ChatGPT Pro Model and submit an uncertainty packet.
- The `fuji1 remote worker` canonical session remains authoritative for
  repository inspection, implementation, builds, tests, benchmarks, and
  reproducible evidence. It does not support Browser and must not invoke,
  imitate, simulate, proxy, substitute, or fall back to another
  browser/model, nor silently guess when the escalation predicate is met.
- Pro output is untrusted advisory evidence. The `MacBook Air coordinator`
  records a bounded MacBook Air ruling, but neither the answer nor the ruling
  can approve an architecture-invariant change, production activation,
  permission expansion, irreversible DB/event migration, rollback, owner
  gate, or user authorization.

A consultation is required only when a material complex problem both matches a
trigger and remains undecidable after authoritative current-source inspection
and any safe bounded experiment. The mandatory trigger classes are:

1. architecture-invariant conflicts;
2. irreversible DB/event migrations;
3. capability, revocation, actor-fencing, or effect-fencing security
   semantics;
4. disagreement about the 100-active/156-non-active/256-total/item-257
   admission contract;
5. plugin/MCP protocol compatibility not decided by an official source or
   conformance fixture;
6. self-update TCB, anti-rollback, recovery rollback, or activation semantics;
7. two or three viable consequential designs, for any other materially complex
   issue, without a source or experimental discriminator.

Ordinary implementation/test problems, complexity alone, time pressure, and
worker disagreement stay with the `fuji1 remote worker`. If a trigger has no
deterministic criterion, the package owning the decision and the smallest
downstream dependency closure that would encode, consume, certify, or activate
it pause; independent packages may continue.

The coordination flow is fail-closed:

```text
fuji1 remote worker review
  -> deterministic evidence: fuji1 remote worker continues
  -> unresolved trigger: affected dependency scope blocks
       -> fuji1 remote worker canonical session returns a minimal redacted
          uncertainty packet to the MacBook Air coordinator source session
       -> MacBook Air coordinator submits it through Browser/Pro
       -> MacBook Air coordinator records provenance and a ruling
       -> MacBook Air coordinator explicitly sends the result back to the same
          fuji1 remote worker canonical session
       -> fuji1 remote worker performs source/TDD/benchmark verification
            -> verified | rejected | stale | owner-gated
```

The packet records the exact question, uncertainty, authoritative HEAD and
relevant dirty-state qualification, affected files/symbols, verified facts,
pending inferences, two or three candidates and tradeoffs, failure impact, and
the determinations requested. It excludes secrets, credentials, user-private
binaries/data, unrelated source, and non-minimal unredacted snippets.

The consultation record includes a stable ID and packet digest, UTC date,
safe canonical session URL, exact displayed model label, question summary, Pro
conclusion/caveats, MacBook Air ruling, bounded next action, remaining gates,
and claim-level evidence status. Unsafe credential-bearing URLs are represented
by `withheld-sensitive` plus a protected audit reference rather than committed.
Results are explicitly sent back to the same `fuji1 remote worker` canonical
session and persisted in this Trellis task; they do not create reverse
filesystem synchronization from the MacBook Air coordinator to the
`fuji1 remote worker`.

Claim-level statuses are `Pro-advised`, `source-verified`,
`experimentally-verified`, and `rejected`. `Pro-advised` never means verified
and never closes TDD, compatibility, capacity, sandbox, security, updater,
activation, or owner gates. Reproducible source/experimental evidence wins over
Pro; conflicting advice is marked `rejected`, with at most one narrow
evidence-focused follow-up.

The normative state machine, packet/record schemas, disclosure rules,
staleness behavior, and phase checkpoints are in
`research/browser-pro-escalation-protocol.md`.

## 5. Lane A — immutable configuration generation

### 5.1 Inputs and canonicalization

A candidate generation includes:

- builtin declarations and aliases;
- deterministic embedded repo-native built-in Bundle packages and the
  installed Bundle registry;
- ancestor instructions, skills, model routing, and provider capability data;
- static/deferred MCP and dynamic Compat MCP desired configuration;
- native and Compat plugin manifests;
- `AgentBundle` manifests and requested capability/resource ceilings;
- compatibility versions and the generation compiler version.

The compiler first inventories and tests current precedence. Any precedence
change is an explicit migration, not an accidental side effect of sorting.
Canonicalization uses stable ordering and normalized encodings; identical
effective input produces identical bytes and IDs.

Secret values are not copied into manifests, content digests exposed to models,
or event logs. Manifests hold scoped references; Harness-owned existing config
resolution supplies values only through the current authorized dispatch path.

### 5.1.1 Active `0.34.5` minimal generation boundary

`0.34.5` implements the narrow prerequisite, not the later reconciliation
model:

- `SessionEngine` owns exactly one `RuntimeRegistry`. `ToolRegistry` remains a
  mutable offline candidate builder, but an engine freezes it immediately and
  never retains it as an effective authority.
- `RuntimeSnapshot` contains a `ConfigGeneration`, one immutable lock-free tool
  view (including MCP-backed tools), and immutable skill catalogs keyed by
  workdir. `TurnBinding` retains the snapshot `Arc` and selected workdir.
- Candidate construction and publication are serialized by one publisher
  critical section. Candidate validation completes before the next monotonic
  generation is allocated; publication is one replacement of the active
  snapshot `Arc`.
- A failed or logically unchanged candidate leaves the active generation and
  exact effective view unchanged. Concurrent successful publications receive
  unique increasing generations and can expose only a whole candidate.
- After admission, `run_turn` and direct shell bind once before prompt,
  provider, schema, skill, or tool behavior. Every provider round and dispatch
  uses the retained binding. Newly admitted turns see a later successful
  publication; in-flight turns do not.
- `TurnBindingRecorded` stores only session routing, message identity, and
  generation identity. The shared projection records that optional generation
  on the assistant message; registry contents remain outside events and no
  parallel read model is introduced.
- Startup plugins and synchronous MCP tools enter the complete initial
  candidate. Deferred MCP connection publishes its full tool set through the
  same owner; it cannot mutate an engine-visible builder.

No public configuration, desired/observed/effective resource state, plugin
respawn declaration, namespace migration, Bundle behavior, or permission
reinterpretation is introduced in this boundary.

### 5.1.2 Delivered `0.34.6` reconciliation boundary

`0.34.6` consumes the `0.34.5` publisher without creating another effective
authority:

```text
validated desired MCP/plugin set
  -> app RuntimeReconciler { revision, per-source ticket, observed outcome }
  -> I/O outside reconciler state lock
  -> complete PreparedSource set for the current revision
  -> RuntimeRegistry current-snapshot candidate + atomic publication
  -> RuntimeSnapshot { source ID, declaration digest, exports/resources, owner }
  -> TurnBinding resolve/dispatch
```

- `SourceId = (mcp|plugin, configured_id)` is canonical and independent of PID,
  task completion order, or generation. A source export retains its declared ID
  beside the compatible external canonical name and aliases.
- Desired is the latest validated declarative set. Observed records the desired
  revision/ticket and `connecting|ready|failed|removed`, optional declaration
  digest, and typed error. Effective is composed only from the active
  `RuntimeRegistry` source manifest/generation; the reconciler caches no
  effective tool set.
- A stale success never publishes and its unpublished owner is released after
  the state lock. A stale failure is discarded. A current failure marks the
  atomic attempt failed, releases every staged owner, and keeps the previous
  generation and view.
- Removal/disable is safety-priority: it publishes a drop-only complete
  candidate before unrelated additions are prepared. Old bindings retain their
  old source owner; new bindings immediately lack the removed source.
- Candidate publication always begins from the registry's current snapshot in
  its single publication critical section. This acts as the required
  current-base closure and cannot overwrite an intervening skill refresh.
- Candidate validation rejects duplicate sources, same-source declared export
  IDs, configured/handshake plugin ID mismatch, canonical collisions, alias
  collisions, and alias/canonical cross-collisions before generation
  allocation. External names remain unchanged and insertion order never wins.
- Plugin consistency covers tool exports plus their existing RPC binding.
  Respawn canonicalizes and compares the complete initialize declaration,
  including plugin metadata, tools, command/permission hooks, and workspace
  adapters. Drift closes the replacement and subsequent calls fail closed.
- Existing hooks/commands/permission callbacks remain on `PluginHost` and the
  existing `PermissionPlane`. There is no dynamic hook plane, plugin watcher,
  plugin hot-reload claim, new permission framework, or sandbox.
- Compat MCP routes receive only an app-supplied `McpControl` trait. The server
  owns no desired/status/effective map; its status is composed on demand from
  reconciler state and the registry manifest.
- MCP/plugin startup uses existing configuration. No user-facing field or
  command is introduced, so updated configuration documentation is the runnable
  example and no new skill is warranted.

In the later target architecture, filesystem changes, process handshakes, and
package downloads create candidates rather than mutating an effective
generation in place; a later owning stage may debounce bursts and durably
journal activation. `0.34.6` adds no watcher, debounce layer, or activation
journal.

### 5.2 `AgentBundle` graph

The accepted principle is:

> Bundle provides definitions; Harness executes them. An agent is a catalog
> entry, not a tool. Native Harness spawn is the only execution entry.

The sole authority flow is:

```text
repo-native built-in sources + installed package artifacts
  -> one deterministic package preparer
  -> immutable prepared package bytes + digest-bound index
  -> AgentBundleIR
  -> one immutable Generation/catalog snapshot
  -> TurnBinding
  -> AgentSpec execution projection
  -> SessionEngine
```

The prepared package bytes are authoritative; the generated index is derived in
the same build/preparation action and must match their digest. Catalog/TUI
metadata remains in the same immutable generation from which `AgentSpec` is
projected, so neither UI nor spawn resolution can become a second source.

The manifest is flat rather than an inheritance/runtime tree:

| Level | Required shape |
| --- | --- |
| Bundle | `identity`, `resources`, `extensions`, `agents[]` |
| `identity` | namespaced ID, version, publisher |
| `resources` | canonical static skill payloads in `0.34.8`; executable tool/MCP/hook declarations are typed unsupported until their consumers ship |
| `extensions` | JS and Rust references are typed unsupported in `0.34.8`; the runner is deferred to `0.34.10` |
| Agent entry | explicit stable agent ID, local ID, `role: main|subagent`, `spawn_lifecycle: transient|resident`, prompt, model policy, workdir, `harness_access`, narrowing `resource_view`, and `can_spawn` |

Invariants:

- `role` controls TUI/catalog visibility only; it does not encode root Session
  lifetime or grant spawn authority. It is the sole selector-visibility field;
  the prepared IR has no second `hidden` flag;
- `spawn_lifecycle` controls transient/resident behavior only when Harness
  native spawn invokes the definition;
- a bundle defines resources once and agents reference them; no inheritance or
  nested overlay exists;
- a main may be TUI-selectable only when its complete resource view is
  executable; subagents are excluded from the TUI selector. Internal and
  agent-facing rosters use the caller's `can_spawn` graph, not `role`;
- `can_spawn` is a catalog reachability allow-list with default deny, not a new
  permission system. `compaction`, `title`, and `summary` are reserved Harness
  system-operation agents and have no ordinary inbound `can_spawn` edge;
- `harness_access` selects `none | basic | full` Harness-owned candidates;
  `resource_view` then narrows and aliases that candidate set. Neither can
  expand the existing `PermissionPlane`;
- a source `resource_profile` or permission overlay is rejected in `0.34.8`
  until a complete typed consumer exists; no unconstrained string or silently
  ignored policy metadata enters prepared IR;
- the only system bypass is the existing fixed compaction/title/summary call
  sites, which exact-lookup their compile-time stable IDs in the current
  `TurnBinding` catalog. There is no general unchecked spawn API;
- explicitly installed JS/Rust code is trusted same-UID extension code. This
  design does not isolate malicious plugins or bundles.

#### Stable namespace and resolution

- Harness resource ID: `harness:<kind>/<id>`.
- Bundle resource ID: `bundle:<bundle-id>/<kind>/<id>`.
- Inside its own manifest, `bundle:<kind>/<id>` expands to the current bundle
  ID at load time.
- Version is pinned by the active binding/generation, never embedded in the
  logical stable/short ID.
- Every built-in agent's current public ID is an explicit stable manifest
  field. It is never derived from a bundle path or version and is preserved
  unchanged in event/session data.
- Built-ins are `origin=builtin, immutable=true`. An installed package that
  collides with a built-in bundle ID or stable agent ID rejects the candidate
  before activation.
- Resolution order is:
  1. explicit qualified ID;
  2. per-agent alias, whose target must be qualified and whose alias cannot
     occupy a bundle-local short name;
  3. bundle-local short name;
  4. a unique Harness short name.
- Missing, ambiguous, or conflicting resolution fails closed. Tool, skill, and
  MCP use the same algorithm. Hooks require explicit bundle-qualified
  references.
- Bundle-local wins a short-name collision with Harness, but never overwrites or
  aliases over either stable ID.

#### Harness access and resource views

- `harness_access: none`: no Harness-owned resources enter the candidate set.
- `harness_access: basic`: only the Harness-defined builtin basic set enters.
- `harness_access: full`: every tool/skill/MCP resource in the current bound
  RuntimeSnapshot enters.
- Bundle-local prepared resources are catalog candidates independently of this
  Harness-access level. `resource_view.allow/deny/aliases/namespace` narrows
  and resolves the combined candidate set; a bundle-local short name wins over
  a Harness short name, while every qualified ID remains exact.

The effective view is the narrowing intersection of access, view, and Harness
policy. A Bundle never expands the final `PermissionPlane` decision.

#### Illustrative prepare-valid `0.34.8` YAML

```yaml
api_version: hya.agent-bundle/v1
kind: AgentBundle
identity:
  id: acme/research-suite
  version: 1.0.0
  publisher: acme

agents:
  - local_id: lead
    stable_id: acme-lead
    role: main
    spawn_lifecycle: transient
    prompt: ./agents/lead.md
    harness_access: full
    resource_view:
      deny: []
    can_spawn:
      - acme-fact-checker

  - local_id: fact-checker
    stable_id: acme-fact-checker
    role: subagent
    spawn_lifecycle: transient
    prompt: ./agents/fact-checker.md
    harness_access: basic
```

This is build-time input only. It does not make an agent executable until the
`0.34.8` cutover consumes the prepared catalog. Static skills may carry their
canonical content in prepared bytes. Tool/MCP/hook/JS/Rust declarations and
`resource_profile` are `UNSUPPORTED_BUNDLE_FEATURE` in this release rather than
path-only or parse-and-ignore entries.

#### Owner-gated external execution ABI

- A: transient-only `spawn(agent_id,input) -> handle; wait(handle)`.
- B: resident-only `spawn; send; wait`, dependent on durable mailbox/recovery.
- C: Pro/coordinator-recommended hybrid: one native `spawn`; catalog lifecycle
  selects transient/resident; only resident handles accept `send`; both share
  one handle/admission/event path.

The owner has not selected final external execution semantics. The current
recommendation is C, but the owner must also choose context transfer
(`input only`, `input + summary`, or `full context`) and resident idle/turn
lifecycle. No implementation may guess.

The authoritative shipped package split is now:

- `0.34.8`: one atomic built-in cutover. Freeze the capability/replay fixtures,
  add the minimal Bundle IR/catalog/namespace/resource-view compiler, prepare
  and embed all repo-native built-ins, switch startup/TUI/spawn resolution to
  the one immutable generation, and delete every old agent-file
  loader/parser/discovery/runtime path in the same release. It must boot
  without `hya bundle install`.
- `0.34.9`: deliver public/private inspection, the strict in-process public
  reader, and the authoritative registry core. It does not execute external
  bundles.
- `0.34.10`: deliver the four bundle CLI commands, lazy atomic publication of
  installed public-static catalogs, and the documentation/example/authoring
  skill.

This split supersedes every older placement of an external runner in `0.34.10`
later in this design. External runner work is deferred beyond this package
release, with no replacement version assigned. The older runner and resident
sections remain future architecture and historical planning evidence, not a
claim about the shipped `0.34.10` scope.

## 6. Lane B — versioned binding and atomic refresh

### 6.1 Desired, observed, and effective

Each managed resource has three deliberately separate records:

- **Desired:** source/config identity, requested artifact/protocol,
  declarations, capabilities, and enabled state.
- **Observed:** actual process/artifact identity, handshake result, protocol,
  declaration/schema digest, health, resource usage, and observation time.
- **Effective:** a verified, healthy, policy-compatible observed resource
  incorporated into a specific `BindingSetId` under an activation epoch.

MCP/plugin protocol output is untrusted observed input. A partial handshake,
schema collision, protocol mismatch, unexpected declaration broadening, or
unhealthy process never enters effective state.

Static/deferred MCP, dynamic Compat MCP, native/Compat plugins, skills, agents,
and AGENTS context all feed this pipeline. This removes the current duplicate
MCP authorities and round-specific discovery regimes.

### 6.2 Lifecycle

```text
discover/declare
  -> prepare immutable candidate
  -> observe resources
  -> verify identity, protocol, schema, policy, and health
  -> build complete binding set
  -> atomically activate at activation_epoch N
  -> quiesce previous generation
  -> drain pinned attempts
  -> retire resources and retained content after the rollback window
```

Activation is one durable prepare/commit publication, never a series of
per-tool mutations. Failed preparation leaves the prior effective generation
untouched.

An in-flight attempt keeps its binding even after a new activation. If its
bound process dies, calls return typed `binding_unavailable`; they do not
silently resolve by name into a replacement. A restarted MCP/plugin process may
reattach to an existing binding only when artifact, protocol, stable source and
declaration identities, and declaration digest all match. Otherwise it is
quarantined while a new generation is prepared.

Declaration removal or process compromise may cause immediate security
subtraction. Declaration additions and ordinary structural changes wait for a
new activation and the next admitted Turn attempt.

## 7. Lane C — existing permission boundary and narrowing views

Harness configuration and the existing `PermissionPlane`/dispatch path remain
the sole permission authority. This design adds no capability broker, escrow
delegation, independent security authority, or `SecurityEpoch`.

The effective resource view is a narrowing intersection:

```text
Harness policy/current PermissionPlane
∩ active binding resources
∩ AgentBundle `harness_access: none|basic|full`
∩ agent `resource_view` allow/deny/aliases/namespace
```

- Bundle resource declarations cannot grant beyond Harness policy. Unsupported
  policy overlays fail preparation until an effective typed consumer exists;
- ask/allow/deny interaction, logging, minimum protocol validation, and
  fail-closed dispatch errors reuse current planes;
- model/resource constraints are resolved in the binding or rejected as
  unsupported; ignored metadata never silently grants;
- actor/binding/OperationId checks at an effect boundary are correctness
  fencing, not a replacement permission system;
- ordinary same-UID extension code retains ambient user authority outside
  brokered tool calls, so malicious-code isolation is explicitly not claimed.

## 8. Lane D — durable multi-resource admission

### 8.0 Delivered `0.34.4` minimal journal boundary

The active patch is deliberately smaller than the target scheduler in the
rest of section 8:

```text
persist immutable accepted claim
  -> acquire current in-memory governor debit
  -> persist started
  -> create/dispatch through the existing spawn path
  -> completed | cancelled | aborted
```

One `SessionStore`-owned additive journal contains only:

- `operation_id` primary key and unique source `tool_call_id`;
- source/root session storage key needed for cancellation/root cleanup;
- a versioned SHA-256 request fingerprint;
- admission units;
- state, creation/update timestamps, terminal reason, and a logical
  released marker.

The fingerprint covers parent identity, background mode, ordered normalized
members, and every dispatch-affecting member/inline/model/category/resident/
task-id field. It excludes cancellation tokens, reply channels, resolved
provider/agent objects, and presentation-only output.

State invariants:

1. `accepted` is the durable claim before in-memory debit.
2. `started` means the current process acquired the governor debit; no child or
   effect may be created before this transition commits.
3. `completed`, `cancelled`, and `aborted` are irreversible terminals.
4. Identical claim replay returns the existing state and cannot debit or
   dispatch. Any immutable-field mismatch is typed
   `OPERATION_ID_CONFLICT` without mutation.
5. An accepted overload becomes terminal without a governor release.
6. A store compare-and-set chooses the one winning terminalizer. Only a winner
   whose prior state was `started` may remove the operation-keyed governor
   debit. Same-terminal replay is a no-op; a different terminal is a typed
   transition conflict.
7. Completion, explicit cancellation, child/create infrastructure failure,
   and root cleanup call the same finalizer. The process-local governor stores
   debit units by `OperationId`; callers never supply a refund amount.
8. Startup atomically marks all `accepted`/`started` rows `aborted` before
   constructing or starting resident/team spawn supervisors. Old process-local
   debits disappeared with that process, so recovery records logical release
   only and does not credit the fresh governor.

No `operation_child`, durable runnable queue, scheduler, member/effect journal,
public Event variant, lease/epoch/resource map, or result cache is part of
`0.34.4`. Session events/projection remain canonical for session behavior and
replay independently of this control-plane journal.

### 8.1 Work state machine

```text
requested (not acknowledged)
  -> queued (durably acknowledged)
  -> reserved (transactional phase lease)
  -> active
  -> waiting/suspended -> queued
  -> completed | failed | cancelled | indeterminate
```

`recovering` and `quiescing` are explicit durable annotations during startup
and cutover. Acknowledgement occurs only after the queued/admitted record and
capacity claim commit. Storage failure returns a typed failure rather than a
false acceptance.

Parents awaiting children release their active execution vector and become
durable `waiting`; otherwise 100 parents could consume every active slot and
deadlock their children. Waiting, suspended, and recovering swarm work still
occupies one of the 156 non-active envelope positions.

An idle resident is durable actor state, not an active work item. Each mail
wake or autonomous Turn creates/promotes a work item through the same admission
authority as transient work. Resident metadata has its own measured
memory/storage budget and cannot bypass global accounting.

### 8.2 Initial capacity convention

The falsifiable phase-0 convention is:

- the **swarm envelope** is exactly 256 subagent work items: 100 active phase
  leases plus 156 durable queued/waiting/recovering positions;
- request 257 receives typed `overloaded` or `deferred` without creating a
  session, Tokio task, process, provider stream, or effect;
- root/control/recovery traffic is owned by the same authority but uses a
  separate, hard-capped reserved class;
- provider streaming remains a distinct 128-slot resource: at most 100 general
  swarm streams and 28 non-borrowing root/control/recovery streams in the
  initial model.

This preserves the product claim of 100 active subagents without treating 128
as agent capacity. P0 must ratify this counting convention; if the intended
claim is process-wide 100 including the lead, the test becomes one lead plus 99
workers and the public wording must change.

### 8.3 Resource vectors and fairness

A phase lease atomically reserves the resources required by that phase:

- active worker position;
- provider-stream class;
- bundle/plugin/MCP process and file descriptors;
- memory and temporary storage budgets;
- event-log/write and bounded-bus budget;
- effect concurrency;
- team Turn/message budget;
- queue/storage position.

Queued work reserves only its durable queue/storage budget. Promotion acquires
the complete active vector in one transaction; there is no hold-and-wait.

Initial scheduling is deterministic and starvation-bounded: reserved
root/control/recovery service, per-root fair sharing for swarm work, oldest
eligible item within a root, and bounded eligible-item bypass when the oldest
item lacks a resource. Aging eventually prevents younger conflicting items
from bypassing indefinitely. Every skip/promotion reason is observable and
replayable.

## 9. Lane E — actor and effect fencing

### 9.1 Delivered `0.34.7` resident claim

`ActorId` is the resident's already-persisted agent-session `SessionId`, not a
PID, `MemberId`, or runtime generation. One narrow SQLite table stores:

```text
resident_actor_claim(
  actor_id PRIMARY KEY,
  epoch INTEGER NOT NULL,
  owner_run_id NOT NULL,
  state active|released
)
```

The in-memory `ActorClaim { actor_id, epoch, owner_run_id }` is an internal
execution capability. `OwnerRunId` is generated once per harness process and
is persisted only in claim rows/diagnostics. `try_claim_new` succeeds only for
a never-claimed or released actor; simultaneous ordinary claims have exactly
one winner. `recover_claim` is a startup-only takeover that transactionally
increments epoch. `release_claim` requires the full tuple and is idempotent.

This is an incarnation fence, not a time lease. There is no TTL, heartbeat,
wall-clock expiry, background lease supervisor, distributed lock, consensus,
HA, or active-active behavior.

### 9.2 Delivered recovery and mutation boundary

Startup remains closed to spawn/send/wait while it:

1. advances all active actor epochs, invalidating every old capability;
2. fail-closed aborts old nonterminal `admission_journal` rows without
   crediting the fresh process governor;
3. replays the canonical roster, inbox, actor-session, and member projection;
4. terminalizes running tool parts, assistant messages, children, and the
   root resident-work marker without retry;
5. recreates the existing `ResidentSupervisor` task owner and notifies only
   committed queued-not-started work.

`ResidentWorkStarted` is committed before a resident may dispatch a provider,
tool, or child. Its inbox boundary lets recovery consume the aborted running
batch while preserving mail committed after that boundary. Existing
`ToolError`, `MessageFinished`, `MemberFinished`, and activity events record
the terminal state; no second resident read model or effect event family is
introduced.

Resident canonical event/mailbox/child commits use one store fence-and-append
transaction. Resident-originated entries in the existing `admission_journal`
carry nullable actor ID/epoch and its existing transitions perform the same
claim check. Tool dispatch checks the claim at the actual dispatch boundary;
the result is checked again and can enter canonical state only through the
transactional fence. Old `TurnBinding` snapshots remain alive independently of
actor takeover.

The guarantee ends at canonical local state. External filesystem/network/API
effects completed before takeover cannot be reversed or made exactly once.
Running/in-flight work is aborted and never automatically retried. A future
effect taxonomy, remote reconciliation framework, durable multi-resource
scheduler, and 100/256 certification remain outside `0.34.7`.

## 10. Lane F — independent update TCB

The existing installer/release pipeline is retained as bootstrap and manual
break-glass recovery. The new updater is a minimal process/package with no
plugin, MCP, provider, bundle, session database, or runtime-registry dependency.

Signed canonical metadata covers:

- release/update sequence and activation intent;
- artifact digests, sizes, target/platform, protocol/schema compatibility;
- minimum updater/runtime compatibility;
- freshness/expiry and anti-replay metadata;
- authorized recovery/rollback intent.

The updater verifies metadata and artifacts without loading candidate runtime
logic. It stages a complete immutable runtime directory, fsyncs data and parent
directories as required by the supported filesystem contract, smokes the
candidate in a dedicated smoke subprocess, journals prepare, quiesces/drains/fences the
old runtime, atomically switches one selector, and journals commit.

Rollback selects retained older bits only through a newly authorized,
higher-sequence activation. It never lowers activation, actor, or
trusted update epochs.

A separate process is not automatically an independent TCB. Production
activation is blocked until OS ownership/permissions or an equivalent
package-manager boundary proves that the candidate runtime and same-UID
extensions cannot modify verifier code, trust roots, accepted floors, the
activation journal, or selector. Signing-key custody, rotation/revocation, and
break-glass ownership are explicit owner gates.

Updater implementation may proceed independently after the P1 identity/journal
contracts exist. Its production activation remains gated on generation,
quiescence, effect fencing, trust-root custody, and crash-recovery proof.

## 11. Cross-lane flows

### 11.1 Generation refresh

```text
desired inputs
  -> canonical ConfigGeneration candidate
  -> observe MCP/plugin/bundle resources
  -> verify declarations, identity, policy, compatibility, and health
  -> compile BindingSet
  -> journal prepare
  -> atomic activation at higher activation_epoch
  -> new Turn attempts pin it
  -> old attempts drain on their old binding
  -> retire after rollback window
```

### 11.2 Turn and effect

```text
durable admission
  -> TurnAttemptId + actor_epoch
  -> pin BindingSetId + activation_epoch
  -> derive structural capability ceiling
  -> provider rounds use pinned schemas/model/instructions
  -> plan OperationId
  -> existing PermissionPlane/dispatch authorization
  -> EffectGate checks actor_epoch + binding + admission/lease + OperationId
  -> adapter linearizes
  -> durable operation outcome
```

### 11.3 Restart

```text
replay control journals and session projections
  -> recover effective generation and monotonic floors
  -> quarantine/re-observe external processes
  -> reconcile admission leases and uncertain effects
  -> increment actor epochs
  -> rebuild only bounded active tasks
  -> enqueue pending wakes/mail
  -> resume admission
```

## 12. Persistence, replay, and native cutover

- The event-sourced architecture remains authoritative. Cross-session runtime
  control state uses append-only durable journals with deterministic reducers;
  mutable tables are indexes/checkpoints derivable from those journals, not a
  second source of truth.
- New IDs/epochs/journals are additive. Do not fabricate binding, operation, or
  actor IDs for historical events.
- Historical event/session agent IDs stay byte-for-byte unchanged and replay
  without consulting any removed agent-file loader. No old source is imported,
  translated, or rewritten into a Bundle.
- Until old-reader tolerance is proved, new global journals remain outside
  shared session event variants or use optional/versioned wire fields.
- External tool names remain aliases; internally every new attempt resolves by
  stable ID.
- A pre-cutover active Turn drains or is explicitly interrupted; it is never
  rebound in place.
- Continuing a replayed session whose agent definition is unavailable returns
  a typed unavailable-definition error; it never silently executes a different
  prompt/model definition. The source-tested unknown-new-spawn fallback remains
  an explicit `0.34.8` owner gate until resolved.
- Existing resident teams without durable incarnation state drain or are
  deliberately restarted at a higher actor epoch after cutover.
- No rollback down-migrates or deletes additive journal state. Security and
  monotonic floors survive rollback.
- Projection snapshots, batching, or SQLite replacement are allowed only after
  the frozen capacity matrix demonstrates the current replay/write path is a
  failing constraint.

## 13. Workload, capacity, and fault matrix

All scale tests use deterministic fake provider/tool/MCP/plugin/bundle fixtures
unless the test is specifically about an external adapter. External provider
limits must not contaminate the internal harness result.

| Dimension | Required cases | Gate |
| --- | --- | --- |
| Steady swarm | 100 active subagent Turn attempts for at least 30 minutes and 10,000 completions, whichever is longer; resident/transient mixes `0/100`, `50/50`, `100/0` | No lost acknowledgement, unbounded task/process/FD growth, stale commit, or silent error |
| Burst | Barrier holds exactly 100 active; exactly 156 more are durably queued; request 257 | Counts stay exact under races; item 257 is typed overload/defer with no downstream allocation |
| Parent/child | Active parents spawn/wait for children at full envelope | Waiting parents release active leases; no all-parent deadlock; budgets/depth still apply |
| Provider classes | 100 general streams plus pressure against 28 root/control/recovery reservations | Never exceeds 128; general work cannot consume reserved slots; control traffic remains bounded |
| Histories/storage | 1k, 10k, and 100k event histories; SQLite contention, busy timeout, bounded disk-full, checkpoint off/on only if introduced | Report measured replay/write constraint; no speculative storage replacement |
| Restart | Kill at exact 100-active/156-queued state and at every admission/activation/operation transition | Same accepted set converges; no duplicate promotion, actor, lease, cursor advance, or committed journaled effect |
| Refresh churn | Change skills/agents/AGENTS, static/deferred and Compat MCP, plugin crash/re-handshake, invalid/partial candidates during provider rounds and dispatch | One attempt keeps one binding; next attempt sees only a complete verified generation |
| Permission dispatch | Harness allow/ask/deny, bundle narrowing overlays, direct dispatch, unavailable policy, and concurrent binding change | Bundle/plugin input never expands Harness policy; current `PermissionPlane` failure propagates closed |
| Actor/effect | Delayed messages/results from old epochs; idempotent/reversible/reconcilable/non-idempotent outcomes | Stale writes rejected; uncertain remote outcome is visible and not blindly retried |
| Bundles | Build-time preparation, unknown-field/reference rejection, stable-ID/bundle-ID collision, namespace/alias conflict, missing/ambiguous resources, `none/basic/full`, `can_spawn`, hook qualification, package/runner crash/timeout/cancellation/resource pressure | `0.34.8` atomically cuts built-ins to one native catalog and removes old loaders; `0.34.9` packages without external execution; `0.34.10`/`0.34.11` reuse SessionEngine/current PermissionPlane for external transient/resident execution; trusted same-UID code is never described as malicious-code isolated |
| Updater | Bad signature/hash/platform, replay/freeze/downgrade, key rotation/revocation fixtures, disk-full, failed smoke, kill before/after every fsync/rename/journal step | Always bootable into one verified complete generation; floors never decrease |
| Bus/projection | Broadcast lag and receiver restart while durable state advances | Reconciliation from journal/projection restores state; notification loss is not state loss |

For every resource `i`:

```text
required_i = active * per_active_i
           + non_active * per_non_active_i
           + resident_metadata_i
           + fixed_i
           + reserved_control_i
```

Phase 0 freezes CPU/RSS, queue-age, storage, recovery-time, and p50/p95/p99
baselines on the `fuji1 remote worker`. The provisional PRD gates remain:
admission
acknowledgement p99 <= 500 ms; append p99 <= 50 ms steady and <= 250 ms burst;
burst drain <= 2x the service-time model; no p95 regression above 20% without a
reviewed budget change; measured peak within 20% of the resource model and
memory within 10% of pre-burst baseline within five minutes. A threshold change
requires a recorded capacity decision.

## 14. Observability

Without secrets, expose and correlate:

- config/generation/bundle/declaration/binding IDs;
- Turn, attempt, actor, operation, admission, and source instance IDs;
- activation and actor epochs plus the applicable existing permission decision;
- desired/observed/effective state and quarantine reason;
- admission class, resource vector, queue age, promotion/skip reason, lease and
  recovery state;
- operation effect class and planned/authorized/started/outcome state;
- requested, acknowledged, queued, waiting, reserved, active, quiescing,
  drained, rejected, recovered, and indeterminate counters;
- refresh, security-fence, stale-actor, reconciliation, and update failures.

Metrics and APIs must use shared typed DTOs/projections. The TUI/server/client
do not independently derive state machines from raw fields.

## 15. Cutover, rollback, and obsolete-path removal

Each lane starts in record/shadow mode, compares decisions with HEAD behavior,
then becomes authoritative behind an explicit activation gate. Shadow mode may
observe but never grants authority.

Rollback rules:

1. stop new admission/activation;
2. commit a subtractive security fence if required;
3. quiesce and drain or explicitly fence old attempts/actors/effects;
4. reconcile leases and indeterminate operations;
5. activate retained verified content under a **higher** epoch;
6. keep additive journals for forward recovery.

Every obsolete bypass has a named removal gate:

- unbounded spawn intake and task-per-request queued work;
- independent resident/transient admission authorities;
- mutable per-name effective registry mutation;
- duplicate static/Compat MCP effectiveness paths;
- in-memory cursor/quiescence authority;
- name-only internal dispatch and ignored policy metadata.

The old agent loader/parser/discovery/runtime branch is special: it is removed
exactly once in the atomic `0.34.8` native built-in cutover, after frozen
capability/replay fixtures pass and before that release can be called complete.
It is never retained as a rollback selector. Other obsolete bypasses are
removed in their owning patch after the corresponding rollback/fault gate and
call-path inspection prove no bypass remains.

## 16. Planning conflict resolutions

The four read-only planning views disagreed in a few useful places:

1. **What counts as 100:** one view counted the root inside 100; another treated
   100 as subagent capacity. The product title and PRD require 100 active
   subagents, so this design uses a 100-subagent envelope and a separately hard
   capped, globally accounted root/control/recovery class. P0 must ratify the
   public wording and test fixture.
2. **Updater sequencing:** a serial plan placed updater work after bundle
   execution; the architectural view noted its independence. Design and
   fixtures may proceed after P1, but production activation waits for runtime
   generation, drain, fencing, trust-root, and crash-consistency gates.
3. **Native built-in cutover:** earlier drafts retained an agent adapter or
   delayed removal. The owner rejected both. `0.34.8` freezes fixtures,
   prepares embedded built-ins, switches startup/TUI/spawn to one catalog, and
   removes all old agent-file code in one atomic release.
4. **Catalog sequencing:** `0.34.6` lays only generic stable-ID/namespace seams.
   `0.34.8` introduces the Bundle IR/catalog and built-in execution together;
   `0.34.9` adds package distribution without external execution;
   `0.34.10`/`0.34.11` add owner-gated external transient/resident execution.
5. **Context manifests:** some planner drafts proposed source files and task
   documents. Trellis injects task artifacts automatically and explicitly
   forbids code paths in `implement.jsonl`/`check.jsonl`; the manifests therefore
   contain only relevant specs, ADRs, and research.
6. **Consultation scope:** one view suggested package-only blocking and another
   a broad task hold. The design blocks the smallest affected dependency
   closure. It also reconciles “any non-deterministic complex problem” with
   normal `fuji1 remote worker` work by requiring both material impact and
   absence of a source/experiment discriminator. Session URLs are canonicalized
   or replaced with a protected reference; consultation provenance never
   justifies leaking access material.

## 17. Owner decisions that gate activation

These may remain open while the task stays in progress, but the owning phase
cannot activate until recorded. Pro advice may inform a proposal but cannot
resolve or approve any item in this list:

1. ratify the 100-subagent versus process-wide active-count definition and
   public wording;
2. ratify queue fairness/aging bounds and root/control reservation policy;
3. choose final external AgentBundle execution semantics (current
   recommendation: one Harness spawn API with catalog-selected
   transient/resident behavior), context transfer, and resident idle/turn
   lifecycle; same-UID trusted-only/no-malicious-isolation is already fixed;
4. choose private Bundle key ownership between offline OS/device-bound custody
   and online publisher/license unwrap before private decrypt/activation;
5. define update signing-key custody, signing authority, trust-root
   rotation/revocation, and break-glass ownership;
6. prove the updater/verifier ownership boundary against runtime write access;
7. decide additive wire/event compatibility after old-reader tests;
8. freeze baseline-derived performance and recovery SLOs;
9. authorize any threshold/authorized-recovery signing policy beyond a single
   rotatable root.

## 18. Non-goals

- Distributed or multi-host scheduling.
- Provider concurrency above 128 without new evidence.
- Replacing SQLite/full replay before a measured gate fails.
- Exactly-once claims for arbitrary remote effects.
- In-process Rust dynamic-library bundles.
- Calling ordinary child processes untrusted sandboxes.
- Native built-in cutover before `0.34.8` prerequisites and fixtures pass, or
  installed-bundle execution before the `0.34.10`/`0.34.11` owner and runner
  gates pass.
- Development in the dirty `main` checkout, builds on the MacBook Air
  coordinator, bidirectional synchronization, deletion propagation, or a sync
  daemon. The single isolated fuji1 task worktree is required.

## 19. Traceability

`research/defect-register.md` is the stable-ID source for current defects,
target gaps, evidence levels, and closure gates.
`research/next-step-roadmap.md` is the dependency-order source for immediate
containment, the first minimal tracer, conditional storage work, certification,
native built-in cutover, external Bundle delivery, and updater activation. This
design remains the normative target contract; neither research document is an
implementation claim.

| Goal | Primary lanes | Proof phase |
| --- | --- | --- |
| Modular harness | All six authorities and adapter boundaries | `0.34.3`–`0.34.7`, then per-patch bypass audits |
| 100+ native swarm | Admission + actor/effect, supported by binding/current PermissionPlane | `0.34.3` tracer, `0.34.4`/`0.34.7`, `0.34.12` matrix |
| Markdown/JS/Rust `AgentBundle` | Generation/catalog + binding + existing PermissionPlane + admission/effects | `0.34.8` native built-ins, `0.34.9` packaging, `0.34.10` external main/transient, `0.34.11` resident |
| Atomic runtime refresh | Generation + binding + existing PermissionPlane | `0.34.5`–`0.34.6`, package generation in `0.34.8`–`0.34.9`, churn proof in `0.34.12` |
| Secure self-update | Independent update TCB plus generation/drain/fencing contracts | `0.34.13` implementation/activation gate after `0.34.12` certification |

## 20. Consult26 authoritative `0.34.11` sidecar architecture

This section supersedes the unresolved external-execution owner gate and every
older 0.34.10/0.34.11 runner placement in sections 5.2, 16, 17, 18, and 19.
Release 0.34.10 is closed as CLI plus lazy public-static catalog publication.
Release 0.34.11 owns public transient and resident activation together.

The architecture is one Harness agent runtime with one optional extension
sidecar per executable activation:

```text
captured TurnBinding + Bundle AgentSpec
  -> SessionEngine/Harness model + mailbox + tool loop
  -> app-owned validated closure materialization
  -> hya-plugin-owned Bun Compat sidecar
       initialize { activation_id, lifecycle } -> ACK + declaration validation
       tool/call and hook/* -> request/reply
       event -> one-way EventNotificationParams { envelope }
  -> existing Harness result/event/MemberOutcome/recovery path
```

The sidecar never receives the agent task or context and never returns an agent
result. Static-only Bundles remain process-free. Harness remains authoritative
for spawn/send/wait, admission, cancellation, permission, provider/model work,
events, projection, resident mailbox, epoch fencing, refund/finalization, and
recovery. `hya-plugin` owns process/stdio/shutdown/kill/reap; `hya-app` owns
captured-resource resolution/materialization and construction; core sees only a
preconfigured factory plus activation identity/lifecycle and an opaque handle.
There is no second actor, runtime, catalog, loader, DTO, or transport.

One transient activation owns one sidecar through all provider rounds and
Bundle tool/hook/event activity, then shuts it down. A healthy resident reuses
one sidecar across mailbox messages. Idle loss parks without a process and the
next durable message lazily creates/ACKs a replacement. Running loss aborts the
whole item through the existing actor-epoch fence without replay; queued-after
work remains durable and ordered under the original pinned binding. Explicit
stop is final/idempotent and cancels queued work; host shutdown preserves queued
resident work but never PID/stdio state. There is no TTL, heartbeat, idle
reclaim, watcher, or restart loop.

Public package closure is root bundle.hya.md plus exactly the paths represented by the existing v1 agent/resource/Extension contract. Directory/archive preparation must be identity/digest equivalent for that declared closure. The 0.34.11 public JS profile admits only self-contained selected Extension entrypoint files; no separate Bundle-local helper file kind or transitive JS source closure exists. Undeclared directory files are ignored, unreferenced archive files reject, activation never executes the authoring tree, and no source/IR/runtime expansion or import scanner is authorized. Private activation, raw Rust, Bundle MCP, unmapped resource profiles, arbitrary commands/env, compile-on-activation, dylibs, terminal/artifact results, child send/wait, and sandbox claims remain unsupported. Existing PermissionPlane/plugin policy remains the final gate before sidecar RPC.

Retain the shipped `hya-core -> hya-bundle` dependency and
`RuntimeSnapshot`-owned `Arc<BundleCatalog>`; the 0.34.11 sidecar lifecycle/
start seam must introduce no new Bundle, package, `PreparedResource`, source,
path, digest, or `hya-plugin` types.
`hya-app` resolves and materializes from the captured snapshot/
binding catalog, while core start carries only `activation_id` and lifecycle.
The existing `hya-plugin -> hya-core` and `hya-app ->` both directions remain;
`hya-bundle` stays independent of core/app/server; no `hya-core -> hya-plugin`
edge or cycle is introduced.

A follow-up item-14 source audit found that selected executable main agents were exact-bound but never reached `BundleSidecarEnvironment::factory_for`; root compilation therefore received an empty sidecar binding. The controlling option-A correction reuses that one semantic interface: `hya-app` injects it once into `SessionEngine`, and each real root turn calls it exactly once after capturing its sole binding and exact stable `AgentName`. No second resolver/cache/replacement API or server callback is allowed. Static effective views return `None`; executable views return one opaque bound factory and start one transient sidecar before model polling. Actor synthesis remains process-free, while executable root and resident-child activations own distinct processes.

Root-main acceptance is ordered as: resolver/ACK gate; capability and ownership matrix; generation pinning; pre-model failure/cleanup table; then selected-main Bun E2E. All existing Consult26 method-role, package, permission, recovery, private/Rust/MCP, event-notification, catalog-authority, and dependency invariants remain unchanged.

The item-14 explicit-stop audit adds one controlling linearization invariant. Resident MailSent acceptance and stop finalization are ordered by SQLite writer transactions, not TeamState: send commits first and is canceled by stop, or stop atomically terminalizes/releases first and send rejects. One private stop-finalization command fences the claim, accounts for earlier accepted work/mail, applies existing cancel/abort/refund/finalize records once, appends existing Failed activity, releases the claim last, and commits atomically. Shared in-memory completion only makes duplicate callers await the same durable-plus-cleanup Result. Ordinary mail stays epoch-independent, channel bytes/replay stay stable, and the two-phase accepting_mail fallback is forbidden unless the one-transaction feasibility proof fails.

The controlling hook/resource-isolation invariant is capability-selected activation closure. Existing v1 `hook_refs` resolve to canonical stable Hook resource IDs, whose exact supported `local_id` is also the protocol name: `event`, `tool.execute.before`, or `tool.execute.after`. Selected Tool and Hook resources exact-path join only to an `extensions.js` resource in the referenced resource's owning bundle. The captured compiled view retains canonical selected Tool/Hook identities; app deduplicates and canonically sorts only their matching Extension entrypoints. Staging does not activate an Extension, aliases do not rename hooks, and all-Bundle Extension/tool loading is forbidden.

AgentBundle v1 hook_refs select Bundle-local Hook resources only. Every accepted ref resolves and canonicalizes through the existing Bundle resource resolver; every harness:hook/* spelling rejects before publication. Harness-owned host hooks remain in existing Harness/plugin ownership. Core and app gain no prefix branch, translation, compatibility plane, or fallback.

Initialize must report tool and hook declarations that independently equal those captured expected sets, ignoring order but rejecting duplicates, missing, extra, unsupported, and unselected names before model polling. The same immutable view drives schema, dispatch, hook/event routing, and entrypoint selection, while existing `PermissionPlane` remains the final tool-call gate. Controlled cross-kind same-path reuse represents one physical package file and one digest authority; no second schema, DTO, protocol, provenance map, import scan, catalog, or resolver is introduced.

That reuse is one selected self-contained entrypoint, not a helper carrier. Activation rematerializes only the selected captured-generation Extension bytes into fresh staging; unselected Extensions and authoring-tree-only files remain unavailable. Missing relative imports fail before ACK/model/dispatch through existing sidecar cleanup, and no dependency installation, network, portability, or sandbox guarantee is implied.

The strict 0.34.11 order is method-role lock, ACK gate, process ownership,
schema opening, archive closure, app materialization, atomic publication,
namespace/permission routing, hook/event fan-out, transient E2E, resident E2E,
loss/cancellation recovery, stop/drift behavior, then invariant/full gates.
0.34.12 owns only the missing minimal durable 100/156/256 admission authority plus integrated R10 certification; 0.34.13 remains updater only.

## 21. Consult27 durable capacity authority

`admission_journal` is the sole durable queue and active-lease authority. Its fixed state-derived resource vector is `Accepted|Started = one active`, `Queued|Waiting = one non-active`, terminal states = zero, with hard bounds 100 active, 156 non-active, and 256 total. Root/lead work is outside that vector. Writer-ordered bounded transactions atomically admit whole batches, suspend waiting parents, promote at most one eligible row per released active lease, terminalize/refund once, and reconcile restart. No task or execution resource exists for `Queued`; `Accepted` yields one launch instruction behind a barrier and becomes `Started` only after task installation.

Promotion is deterministic without clocks: oldest eligible original sequence within each root, one successful promotion per least-recently-promoted eligible root per round, stable root/admission ties, and ineligible-row skipping. Waiting preserves original order and re-enters through `Queued`. The provider plane is separately partitioned into non-borrowing 100 subagent-general and 28 root/control/recovery permits; only a durable active lease authorizes a general permit. These semaphores are reconstructed caches, not acceptance truth.

Restart requeues stale pre-start `Accepted`, aborts stale `Started` under the existing epoch/effect fence, and retains ordered `Queued`/`Waiting` work. The current single projection, stable Events, RuntimeSnapshot catalog Arc, captured binding, PermissionPlane, resident stop order, and sidecar lifetime remain unchanged. Exact R10 certification is permitted only after this authority and the complete 100/156/257, fairness, parent-wait, cancellation, provider-reserve, restart, regression, resource, and SLO matrix are green at the release SHA.

## 22. Consult28 runtime semantic fingerprint

Cross-process binding identity is a private derived value, not a publication ordinal. `RuntimeSemanticFingerprintV1` hashes deterministic, length-prefixed, sorted sections for the full BundleCatalog, all none/basic/full effective resource and immutable permission identities, all effective skills including workdir precedence/content, all built-in/plugin/MCP declarations/resources/collision order, and remaining reachable agent/source declarations. It is computed only after normal refresh/reconciliation/discovery converges. Unstable process state and secrets are excluded; an unidentifiable behavior source makes the fingerprint unavailable.

The versioned spawn intent stores the fingerprint version/value and diagnostic generation under its integrity hash for every nonterminal row eligible for restart reconstruction, including pre-`Started` `Accepted` work. Recovery captures exactly one current snapshot, matches the fingerprint before exact target resolution, and pins the reconstructed binding and descendants to that Arc. Mismatch/unavailability fails closed before allocation; no generation comparison, latest-catalog rebind, historical catalog retention, second authority, or public Event/wire change is permitted. Previously started `Waiting` parents remain non-replayable and abort under stale-running fencing.

## 23. Consult29 app-owned admission binding identity

`RuntimeSemanticFingerprintV1` remains the core-owned `TurnBinding` runtime-view identity. App-owned base/category/provider inputs also affect accepted child resolution, so `hya-app` composes it into a private `AdmissionBindingFingerprintV1` from one immutable `AdmissionResolutionContext`. The context holds the exact base `AgentSpec`, category registry, and deterministic configured-routing view used by the same resolution attempt; it is not stored and does not widen `RuntimeSnapshot` or create another catalog/runtime authority.

The composite hashes only behavior consumed by the current resolver: inheritable base model/prompt/reasoning; canonical ordered category candidates; deterministic ordered provider implementation/configuration/capability identity; and `ResolverSemanticsV1`. Overwritten base name/workdir, unused category prompt/token fields, secrets, and transient provider state are excluded. Missing deterministic provider identity makes restart-reconstructable admission unavailable. Store/core see only opaque versioned 32-byte identities and bounded raw intent bytes.

One canonical `SpawnIntentV1` row is capped at 1,048,576 bytes and contains raw accepted post-hook intent plus both fingerprints, resolver version, diagnostic generation, and integrity metadata—never a resolved/effective `AgentSpec`, selected model/prompt/reasoning, provider object, or runtime handle. Encode and bound every member before the writer transaction; one failure rolls back the whole batch. Recovery compares both identities before resolution/allocation and never rebinds to newer semantics.

## 24. Consult30 process-local reply ownership over durable admission

The existing `admission_journal` row transition is the only lifecycle authority; caller replies are process-local consequences, never durable acceptance state. `Queued` is therefore silent. A foreground request retains exactly one optional reply sender until all of its member rows are terminal and then sends the existing final vector once. Promotion, `Accepted`, `Started`, or partial completion cannot consume that sender. Mixed cancellation likewise waits for already accepted/started members to finish their established terminalization paths before the whole request completes.

Background transient and initial resident requests each have one stricter early-return gate: a real member must be promoted to `Accepted`, cross its exact durable member `Started` compare-and-set, and successfully register its real child session or resident handle before the sender may return the existing `running` outcome. No failure before registration may fabricate a session or handle. After the sender is consumed, execution/finalization remains durable and detached under the existing lifecycle.

Queued cancellation races promotion through the current serialized SQLite writer transaction and state predicate on the same row. `Queued -> Cancelled` winning first terminalizes with zero allocation, prevents later promotion, and takes/sends `Err(SpawnError::Cancelled)` once for queued-only background/resident work. `Queued -> Accepted` winning first makes queued cancellation a no-op; later cancellation uses existing post-promotion behavior. The sender remains an `Option` only in the operation's existing process-local request/launch owner, and send failure from a dropped receiver is ignored without retry or implicit cancellation. Restart retains durable work but no sender and offers no reply replay/reconnect protocol.

At exact fuji1 source, `SpawnRequest.reply` is already Result-bearing and `SpawnerPlane::spawn_inner` already flattens channel disappearance to `SpawnError::Unavailable`; the earlier Mac sync-area shape was stale. Do not change that type or flattening. Add only unit `SpawnError::Cancelled` (`spawn cancelled before activation`). No queued/deferred `MemberOutcome`, public queued status, DTO/Event/wire change, reply registry, second queue authority, task/session/actor/sidecar/provider/`Started` allocation while queued, or immediate background/resident start exception is permitted.
