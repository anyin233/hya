# Modular harness, native swarm, AgentBundle, runtime refresh, and secure update design

## 1. Planning status and evidence anchor

This document is a **target design**, not an implementation claim. Release
`0.34.3` is committed/pushed at
`b8c21deeb5004e1f703b199a40de196902fadf35` with green remote CI. The user has
now authorized only the `0.34.4` OperationId/minimal durable-admission slice.
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
  release `0.34.3` is the current isolated HEAD and `0.34.4` is the active
  release target.
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
  `research/next-step-roadmap.md`; only `0.34.4` is active now.

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
`research/next-step-roadmap.md`. It does not broaden `0.34.3`.

## 2. Five target gaps

| Product target | Corrected HEAD gap | Target outcome |
| --- | --- | --- |
| Modular coding harness | **Implementation:** `hya-app` composes mutable/process-specific managers directly; discovery has startup-static, spawn-live, and round-live visibility regimes. | Deep authorities own generation, binding, admission, actors/effects, and update; existing `PermissionPlane` remains the permission boundary. Discovery/process managers become adapters. |
| Native 100+ subagent swarm | **Implementation:** 128 limits only depth-greater-than-zero provider streams; spawn intake and task-per-request fan-out are unbounded; a new background transient session is allocated before `run_team` reserve; resident spawn bypasses transient reserve/depth accounting; resident execution is not rehydrated after restart. | One durable bounded authority admits before every allocation and demonstrates 100 active subagent work items, 156 durably non-active items, typed overload at item 257, and restart/fault recovery. |
| Per-agent Markdown/JS/Rust `AgentBundle` | **Implementation:** `AgentSpec` lacks bundle/catalog identity; parsed agent permissions/options and skill `allowed-tools`/`model` do not reach an enforced runtime view; plugin subprocesses are ordinary same-UID children. | One flat Harness-owned catalog manifest defines identity/extensions/resources/agents; agent views and permission overlays only narrow current Harness policy; executable code is explicitly trusted and not malicious-code isolated. |
| Atomic runtime registry refresh | **Implementation:** `ToolRegistry` mutates per name; schemas and execution resolution are independently live; static/deferred and Compat dynamic MCP use separate control paths; plugin restart discards new initialization declarations. **Accepted ADR:** 0007/0008 require next-Turn visibility. | Desired/observed/effective reconciliation produces one verified binding set, atomically visible to newly admitted Turn attempts and pinned through every round. |
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

Filesystem changes, process handshakes, and package downloads create
candidates. They never mutate an effective generation in place. Change bursts
are debounced/coalesced into one candidate, but every accepted activation is
durably journaled.

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
| Bundle | `identity`, `extensions`, `resources`, `agents[]` |
| `identity` | namespaced ID, version, publisher |
| `extensions` | JS references and Rust-sidecar references |
| `resources` | bundle-defined tools, skills, MCP, and hooks |
| Agent entry | explicit stable agent ID, `id`, `role: main|subagent`, `spawn_lifecycle: transient|resident`, prompt, model policy, `resource_view`, declarative/narrowing `permission_overlay`, `resource_profile`, optional `can_spawn`, optional hooks |

Invariants:

- `role` controls TUI/catalog visibility only; it does not encode root Session
  lifetime or grant spawn authority;
- `spawn_lifecycle` controls transient/resident behavior only when Harness
  native spawn invokes the definition;
- a bundle defines resources once and agents reference them; no inheritance or
  nested overlay exists;
- a main may be TUI-selectable only when its complete resource view is
  executable; subagents stay hidden and are reached only through native spawn;
- `can_spawn` is a catalog reachability allow-list with default deny, not a new
  permission system;
- bundle declarations, `resource_view`, allow/deny, and
  `permission_overlay` can only narrow Harness config/current
  `PermissionPlane`; they never expand it;
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

#### Harness resource views

- `none`: bundle-local tool/skill/MCP only.
- `basic`: bundle-local plus the Harness-defined builtin basic set.
- `full`: bundle-local plus all tool/skill/MCP resources loaded in the current
  active binding.

The effective view is the narrowing intersection of requested
view/allow-deny/overlay and Harness policy. A bundle never expands that policy.

#### Illustrative target YAML (design-only, not implemented in `0.34.3`)

```yaml
identity:
  id: acme/research-suite
  version: 1.0.0
  publisher: acme

extensions:
  js:
    - id: fact-tools
      ref: ./extensions/fact-tools.js
  rust_sidecars:
    - id: audit-hook
      ref: ./extensions/audit-hook

resources:
  tools:
    - id: fact_lookup
      extension: bundle:tool/fact-tools
  skills:
    - id: evidence
      markdown: ./skills/evidence.md
  mcp:
    - id: sources
      config: ./mcp/sources.yaml
  hooks:
    - id: audit
      extension: bundle:hook/audit-hook

agents:
  - id: lead
    role: main
    spawn_lifecycle: transient
    prompt: ./agents/lead.md
    model_policy: inherit
    resource_view: full
    permission_overlay:
      deny: []
    resource_profile: interactive
    can_spawn:
      - bundle:agent/fact-checker
      - bundle:agent/monitor
    hooks:
      - bundle:acme/research-suite/hook/audit

  - id: fact-checker
    role: subagent
    spawn_lifecycle: transient
    prompt: ./agents/fact-checker.md
    model_policy: inherit
    resource_view: basic
    permission_overlay:
      allow:
        - bundle:tool/fact_lookup
        - bundle:skill/evidence
    resource_profile: short

  - id: monitor
    role: subagent
    spawn_lifecycle: resident
    prompt: ./agents/monitor.md
    model_policy: inherit
    resource_view: none
    permission_overlay:
      allow:
        - bundle:mcp/sources
    resource_profile: resident
```

The YAML illustrates required content and flat ownership; exact serialization
must remain compatible with the parser tests. It does not make these agents
executable now.

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

The dependency-ordered delivery is:

- `0.34.8`: one atomic built-in cutover. Freeze the capability/replay fixtures,
  add the minimal Bundle IR/catalog/namespace/resource-view compiler, prepare
  and embed all repo-native built-ins, switch startup/TUI/spawn resolution to
  the one immutable generation, and delete every old agent-file
  loader/parser/discovery/runtime path in the same release. It must boot
  without `hya bundle install`.
- `0.34.9`: add `.hyabundle` public/private inspection, the four-command CLI,
  package registry, immutable built-in list/info semantics, and atomic
  generation activation. It does not execute external bundles.
- `0.34.10`: after the owner gate, extend the existing out-of-process plugin
  JSON-RPC/stdio transport for installed public/private main/transient
  execution, plus runnable Markdown/JS/Rust examples and the authoring skill.
- `0.34.11`: add resident Bundle integration through the same Harness actor,
  admission, mailbox, event, and fencing path; no second actor runtime.

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
∩ AgentBundle requested none|basic|full view
∩ agent allow/deny aliases and permission_overlay
```

- bundle/plugin policy declarations are input to Harness evaluation and cannot
  grant beyond Harness policy;
- ask/allow/deny interaction, logging, minimum protocol validation, and
  fail-closed dispatch errors reuse current planes;
- model/resource constraints are resolved in the binding or rejected as
  unsupported; ignored metadata never silently grants;
- actor/binding/OperationId checks at an effect boundary are correctness
  fencing, not a replacement permission system;
- ordinary same-UID extension code retains ambient user authority outside
  brokered tool calls, so malicious-code isolation is explicitly not claimed.

## 8. Lane D — durable multi-resource admission

### 8.0 Active `0.34.4` minimal journal boundary

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

### 9.1 Durable actor state

For every logical actor persist:

- `ActorId`, current `actor_epoch`, root/session/handle and agent identity;
- desired mode, pinned bundle/binding, admission class, and current lease;
- inbox/mail range, durable cursor/acknowledgement, pending wake reason;
- parent/child wait dependencies, quiescence sequence, budgets, cancellation,
  and terminal reason.

Startup sequence:

1. replay authoritative actor/team/admission state;
2. reconcile uncertain leases and operations before admitting new work;
3. allocate and commit a higher `actor_epoch` for each resumed incarnation;
4. rebuild in-memory caches/tasks only for admitted active phases;
5. schedule pending mail/wakes through `AdmissionAuthority`;
6. enable notifications after durable state and fences agree.

Broadcast/notify and in-memory maps are wake optimizations only. Lag recovery
replays durable state; it does not assume an actor already exists in memory.

### 9.2 Operation journal

An operation record contains its stable ID, actor and actor epoch, Turn attempt,
binding/declaration, effect class, input digest, the applicable existing
PermissionPlane decision evidence, and one of:

```text
planned -> authorized -> started
  -> committed | failed | indeterminate
```

Adapters declare effects as pure, idempotent, reversible, reconcilable, or
non-idempotent. Recovery rules are class-specific:

- pure/idempotent journaled work may replay under the same `OperationId`;
- reversible work records and tests its compensation;
- reconcilable work queries the authoritative external state before retry;
- an indeterminate non-idempotent effect stops for explicit reconciliation.

All actor writes, mail acknowledgements, results, and effect completions carry
the current `actor_epoch`. Stale incarnations cannot advance durable state.

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
| Refresh churn | Change skills/agents/AGENTS, static/deferred and Compat MCP, plugin restart/add/remove, invalid/partial candidates during provider rounds and dispatch | One attempt keeps one binding; next attempt sees only a complete verified generation |
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
