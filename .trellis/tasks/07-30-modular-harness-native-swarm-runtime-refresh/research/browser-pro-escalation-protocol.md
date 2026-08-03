# MacBook Air Browser / ChatGPT Pro escalation protocol

## Status and scope

This is a task-scoped execution protocol for
`modular-harness-native-swarm-runtime-refresh`. It governs uncertainty handling
through planning and implementation. A recorded consultation may authorize
bounded verification only through the MacBook Air ruling and the user's
explicit phase scope; it never authorizes a later release, owner gate, or
production activation by itself.

The protocol is a consultation seam outside the runtime architecture. It does
not create a seventh runtime authority, event source, worker tool, admission
class, capability, epoch, binding, fencing role, or updater privilege.

## 1. Identity and authority boundary

The host/session identities are fixed for this task:

- **`MacBook Air coordinator`:** coordination host
  `remote-control:env_e_6a44ea9f84a8832b81c410436e6866fa`; source
  coordination session `019fb513-c030-7073-b073-0ad5442b7db4`.
- **`fuji1 remote worker`:** execution host
  `remote-control:env_e_6a43bba242cc832bb5d0a4ef1a4323ff`; canonical yaca
  session `019fb544-1308-7fa1-9db3-f329b1c0af17`.

The rest of this task uses those role names and never relies on ambiguous
machine-relative or unqualified current-session labels.

| Participant | Exclusive responsibility | Explicitly forbidden |
| --- | --- | --- |
| `MacBook Air coordinator` | Operate in-app `[@Browser](plugin://browser@openai-bundled)`, select ChatGPT Pro Model, submit an approved packet, record consultation provenance, and issue the MacBook Air ruling | Delegate Browser control to the `fuji1 remote worker`; treat Pro as an approver or executor |
| `fuji1 remote worker` | Inspect authoritative source, create and return the uncertainty packet, implement, build, test, benchmark, and verify every adopted conclusion | Invoke, imitate, simulate, proxy, or fall back to Browser/Pro or another browser/model; silently guess after the escalation predicate is met |
| ChatGPT Pro Model | Supply advisory analysis for the exact packet | Approve production activation, privileges, migrations, rollback, security claims, owner gates, or user authorization |
| User/owner | Retain every existing approval and activation gate | None of those gates may be inferred from a Pro answer or a MacBook Air ruling |

Only the `MacBook Air coordinator` source coordination session may perform the
Browser transition. The `fuji1 remote worker` does not support Browser. If
Browser is unavailable on the MacBook Air, the required model cannot be
selected, or the displayed model label is absent or different, there is no
fallback browser/model and no simulated consultation. The affected dependency
closure remains blocked.

The `fuji1 remote worker` remains the authority for repository facts even after
consultation. Pro output is untrusted, prompt-injection-capable advisory
material. It must not cause automatic command execution, dependency
introduction, disclosure expansion, permission expansion, or any other
external action.

### Current-cycle owner boundary

For the currently approved patch sequence:

- no sandbox, seccomp/container study, capability broker, escrow delegation, or
  independent `SecurityEpoch` is in scope;
- explicitly installed JS/Rust AgentBundle and plugin code runs as trusted
  same-UID extension code; the harness does not isolate a malicious plugin;
- final authorization remains in harness configuration and the existing
  `PermissionPlane`/dispatch path, which supplies minimum protocol validation,
  ask/allow/deny interaction, logging, and fail-closed error propagation;
- admission, cancellation propagation, resource bounds, binding consistency,
  crash containment, actor epochs, and effect fencing remain correctness and
  performance concerns, not a new permission framework.

## 2. Escalation predicate and mandatory triggers

A **non-deterministic complex problem** is a material, high-impact design or
control decision for which the authoritative design/current source and a safe,
bounded, reproducible experiment provide no present discriminator between
viable outcomes.

Before escalating, the `fuji1 remote worker` must inspect the applicable
design/ADR, current HEAD source, contracts, tests, protocol fixtures, and any
safe reversible experiment capable of deciding the question. Complexity, time
pressure, worker disagreement, an ordinary implementation choice, a failing
test, or a flaky environment is not by itself an escalation condition.

Escalation is mandatory when both are true:

1. a material problem matches one of the following triggers, including the
   catch-all final trigger; and
2. no authoritative source, authority-ordering rule, protocol fixture, or
   bounded experiment can deterministically decide it.

Mandatory triggers:

1. a conflict between architecture invariants or between their owning
   authorities;
2. an irreversible database or event migration;
3. capability, live-revocation, actor-fencing, or effect-fencing security
   semantics;
4. disagreement about the `100 active + 156 non-active = 256 admitted; item
   257 overloads` workload/capacity interpretation;
5. plugin/MCP protocol compatibility where official protocol evidence or a
   conformance fixture does not decide the behavior;
6. self-update TCB, anti-rollback, recovery rollback, or activation semantics;
7. two or three viable, consequential designs for any other materially
   complex problem when source or experimental evidence supplies no
   discriminator.

If a deterministic criterion exists, the `fuji1 remote worker` applies it and
continues without Browser. Official plugin/MCP specifications and conformance
fixtures take precedence over a consultation; Pro is never their substitute.

## 3. State machine and blocking

```text
FUJI_REVIEW
  -> RESOLVED_DETERMINISTICALLY
  -> ESCALATION_BLOCKED
       -> PACKET_READY
       -> PRO_CONSULTED
       -> MACBOOK_AIR_RULING_RECORDED
            -> REJECTED
            -> FOLLOW_UP_BLOCKED
            -> VERIFICATION_PENDING
                 -> VERIFIED
                 -> REJECTED
                 -> STALE
       -> OWNER_GATE_BLOCKED
```

- The `fuji1 remote worker` owns `FUJI_REVIEW`, packet preparation, and all
  verification.
- Only the `MacBook Air coordinator` owns
  `PACKET_READY -> PRO_CONSULTED -> MACBOOK_AIR_RULING_RECORDED`.
- A trigger without a missing deterministic criterion does not block.
- A trigger with no deterministic criterion blocks the smallest affected
  dependency closure: the package owning the decision plus every shared
  contract, generated artifact, consumer, certification, or activation path
  whose correctness assumes its outcome.
- Independent packages may continue only if they neither modify nor consume
  the disputed contract/artifact and do not cross a gate that assumes the
  unresolved result. Read-only source work and bounded non-production
  experiments remain permitted.
- A MacBook Air ruling may allow bounded, reversible work whose purpose is to
  verify the ruling. It cannot allow certification, production activation,
  privilege expansion, irreversible migration, or bypass of an owner gate.
- A relevant HEAD, dirty delta, source/spec/protocol, authority, assumption, or
  protocol-version change makes the packet/ruling `STALE` unless the record
  proves the change immaterial.
- At most one narrow follow-up may address one identified ambiguity without
  widening disclosure. If certainty is still absent, remain blocked and route
  the decision to the responsible owner/user.

Ordinary implementation, build, test, and benchmark problems remain with
the `fuji1 remote worker` and do not stop unrelated work.

## 4. Minimal uncertainty packet

The `fuji1 remote worker` creates a packet with the following fields:

```text
schema_version
packet_id
packet_revision
packet_digest
task_id
created_at_utc
prepared_by_fuji1_remote_worker
trigger_class
blocked_gate
affected_package_and_dependency_scope
exact_question
determinations_requested
reason_uncertain
authoritative_head_sha
relevant_worktree_state_or_delta_digest
authoritative_sources_and_symbols_checked
safe_experiments_considered_and_why_non_discriminating
verified_facts_with_evidence
pending_inferences
candidates[2..3]_and_tradeoffs
failure_security_or_data_integrity_impact
constraints_and_non_goals
conservative_default
data_classification
redaction_attestation
```

The packet must distinguish verified facts from inferences and must qualify
HEAD with the relevant dirty state when uncommitted source affects the
question. Unrelated dirty files are not included.

Outbound disclosure is data-minimized. The packet must not contain secrets,
credentials, tokens, signed URLs, user-private data, private binaries, raw
repositories, or unrelated source. Include only the smallest necessary code
fragments, redact identifiers or values where needed, and stop submission if
safe redaction cannot preserve the question.

## 5. MacBook Air submission and consultation record

The exact cross-host flow is:

1. the `fuji1 remote worker` canonical session creates and returns the minimal
   redacted packet to the `MacBook Air coordinator` source session;
2. the `MacBook Air coordinator` verifies packet completeness, digest, current
   HEAD qualification, and redaction attestation;
3. the `MacBook Air coordinator` opens in-app
   `[@Browser](plugin://browser@openai-bundled)`, explicitly selects ChatGPT
   Pro Model, and submits only the approved packet;
4. the `MacBook Air coordinator` records the exact displayed model label
   rather than inferring it from the requested selection;
5. the `MacBook Air coordinator` paraphrases the conclusion and records the
   MacBook Air ruling;
6. the `MacBook Air coordinator` explicitly sends the record and bounded
   ruling back to the same `fuji1 remote worker` canonical session; an
   authorized task writer there persists it in this Trellis task;
7. the `fuji1 remote worker` performs source/TDD/experiment/benchmark
   verification.

This coordination exchange is not a reverse filesystem synchronization path
from the `MacBook Air coordinator` to the `fuji1 remote worker`.

Each consultation record contains:

```text
consultation_id
packet_id
packet_revision
packet_digest
submitted_at_utc
returned_at_utc
safe_canonical_session_url
url_access_classification
requested_selection = ChatGPT Pro Model
displayed_model_label_exact
question_summary
pro_conclusion
pro_assumptions_caveats_or_unsupported_claims
macbook_air_ruling = adopt-for-verification | modify-for-verification | reject | defer
ruling_rationale
ruling_scope
permitted_next_action
forbidden_next_action
required_verification
remaining_owner_gates
head_sha_and_relevant_delta_at_ruling
evidence_status_by_determination
verification_evidence
follow_up_of
supersedes
final_disposition_and_resumption_decision
```

The session URL is stored only as a safe canonical URL with authentication
material, query, and fragment removed. If the URL itself grants access or is
otherwise unsafe to commit, record `withheld-sensitive` plus a digest or
protected audit reference; never leak access material merely to satisfy
provenance. The displayed label and URL prove provenance only, not correctness
or authority. Raw transcripts are not stored by default.

Consultation records are append-only in meaning. Corrections create a
superseding record rather than silently rewriting an earlier conclusion.

## 6. Evidence statuses and conflict rule

Apply statuses to each material determination, not to an entire consultation:

| Status | Meaning |
| --- | --- |
| `Pro-advised` | The determination came from the recorded Pro consultation. It is advisory and unverified. |
| `source-verified` | The `fuji1 remote worker` independently reproduced the determination from cited authoritative source at the recorded HEAD/relevant delta. |
| `experimentally-verified` | The `fuji1 remote worker` independently reproduced the determination with a cited deterministic test, experiment, or benchmark. |
| `rejected` | The advice was not adopted or was contradicted; the record states why and what remains blocked or replaces it. |

`Pro-advised` may later coexist with `source-verified` or
`experimentally-verified`, but never substitutes for either. When Pro conflicts
with reproducible source or experimental evidence, that evidence wins and the
affected advice is marked `rejected` (or `STALE` if its premise changed). One
narrow, evidence-focused follow-up is allowed; broad opinion shopping is not.

## 7. Required phase and verification checkpoints

The escalation scan is required before:

| Decision/gate | Minimum post-consultation proof |
| --- | --- |
| Stable IDs, epochs, generation publication, and Turn-pinned binding invariants | Current-source validation plus deterministic identity/atomicity tests |
| Irreversible DB/event migration | Compatibility/replay/crash evidence plus the existing owner authorization |
| Capability, live revocation, actor/effect fencing | Source verification and adversarial race/fault tests |
| Markdown-to-JS/Rust `AgentBundle` activation contract | Owner-selected execution/manifest contract, locked dependency/provenance checks, and existing `PermissionPlane` integration; document trusted same-UID execution and do not claim malicious-code isolation |
| Plugin/MCP desired-observed-effective compatibility | Official protocol evidence and conformance fixtures |
| `100/256` certification | Deterministic evidence for exactly 100 active, 156 durable non-active, 256 admitted total, and atomic typed overload at item 257 |
| Self-update TCB, rollback, and production activation | Signature, authenticity, anti-rollback, ownership, crash-recovery, and rollback evidence plus explicit owner authorization |

A phase/gate records one of:

```text
not-applicable-with-reason
resolved-deterministically
escalation-pending
Pro-advised-pending-verification
verified
rejected
```

Neither `escalation-pending` nor `Pro-advised-pending-verification` can close a
certification or activation gate. A `rejected` advice record closes only that
advice branch; the underlying gate still needs a deterministic verified
resolution or an explicit owner decision where the existing design requires
one.

## 8. Non-authority and owner gates

Pro and the `MacBook Air coordinator` cannot waive or replace:

- authoritative source validation by the `fuji1 remote worker`;
- RED/GREEN TDD and the touched/full verification commands;
- workload, capacity, benchmark, fault, protocol, or current PermissionPlane
  evidence;
- protocol specifications and compatibility fixtures;
- database/event compatibility and migration review;
- update signature, rollback, and recovery proof;
- any user/owner approval, production activation, permission expansion,
  irreversible migration, trust-root, release, or break-glass gate.

## 9. Consultation ledger

### `CONSULT-2026-07-31-PATCH-PLAN-01`

```text
consultation_id: CONSULT-2026-07-31-PATCH-PLAN-01
packet_id: not supplied to fuji1
packet_revision: not supplied
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
url_access_classification: canonical URL supplied by the MacBook Air coordinator; no query or fragment recorded
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: Pro
question_summary: sequence the audited architecture work into patch releases and identify a smallest admission tracer
pro_conclusion: advisory patch sequencing; the original answer placed AgentBundle foundations in 0.34.6 and combined updater delivery with legacy deletion
pro_assumptions_caveats_or_unsupported_claims: advisory did not establish owner approval, source correctness, TDD, benchmark, security, activation, or release evidence
macbook_air_ruling: modify-for-verification
ruling_rationale: keep 0.34.3 as the smallest pre-create admission slice; defer AgentBundle execution until an owner-selected contract; separate updater delivery from legacy deletion to reduce ABI and rollback coupling
ruling_scope: planning map and release 0.34.3 only
permitted_next_action: create an isolated fuji1 worktree, start this same Trellis task, and implement/test/release only 0.34.3
forbidden_next_action: implement 0.34.4+, finalize AgentBundle callable ABI or manifest shape, claim malicious-plugin isolation, bypass owner gates, tag, merge, or activate production
required_verification: source inspection at the recorded HEAD; atomic RED/GREEN tests; focused and full workspace gates; exact release metadata; pushed remote CI green
remaining_owner_gates: AgentBundle third-round ruling and owner selection; all later patch releases; updater activation; legacy deletion; release publication
head_sha_and_relevant_delta_at_ruling: 267bfc3c6c66e46fe8514e2e70657489f853b7f0; 19 protected user-owned dirty paths plus the task directory, with implementation isolated from that dirty checkout
follow_up_of: none
supersedes: none
final_disposition_and_resumption_decision: 0.34.3 source work may proceed on fuji1; later phases remain deferred
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| Use patch-sized release gates and begin with pre-create admission | `Pro-advised`, `source-verified` | Current HEAD confirms an unbounded `SpawnerPlane`, request-owned Tokio task creation, background child creation before `run_team` reserve, and resident bypass in `crates/hya-tool/src/spawn.rs::{SpawnerPlane,spawn_inner}`, `crates/hya-app/src/runtime.rs::spawn_team_supervisor`, and `crates/hya-core/src/{subagent.rs::run_team,resident.rs::ResidentSupervisor::spawn_resident}`. |
| Put AgentBundle foundations in 0.34.6 | `rejected` | MacBook Air ruling and owner scope defer all execution ABI/manifest semantics until the third-round ruling and owner decision. |
| Combine updater delivery and legacy deletion | `rejected` | MacBook Air ruling separates updater `0.34.10` from owner/compat-gated deletion `0.34.11`. |
| Treat the consultation as approval | `rejected` | Pro remains advisory and cannot replace source/TDD/benchmark/CI or owner authorization. |

The consultation date/time and packet digest were not supplied to the
`fuji1 remote worker`; this record does not invent them. A later provenance
correction must append a superseding entry.

### `CONSULT-2026-07-31-AGENTBUNDLE-02`

This is the third-round AgentBundle ruling from the same safe canonical
session URL and displayed `Pro` model label as
`CONSULT-2026-07-31-PATCH-PLAN-01`. Exact submission/return timestamps and a
packet digest were not supplied; receipt by the `fuji1 remote worker` is
recorded as `2026-07-31` UTC without inventing a time.

```text
consultation_id: CONSULT-2026-07-31-AGENTBUNDLE-02
packet_id: not supplied to fuji1
packet_revision: third-round AgentBundle discussion
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
url_access_classification: canonical URL supplied by the MacBook Air coordinator; no query or fragment recorded
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: Pro
question_summary: define the minimum AgentBundle catalog/manifest, namespace, resource-view, and execution-decision boundary without creating a second runtime
pro_conclusion: Bundle supplies definitions; Harness executes them; an agent is a catalog entry rather than a tool; native spawn is the only execution entry
macbook_air_ruling: adopt-for-design-and-source-verification
ruling_scope: task design, 0.34.6 ABI-neutral parser/catalog/resolver boundary, and 0.34.8 owner-gated execution
permitted_next_action: document the flat schema, routing rules, resource views, release split, and a non-executed YAML example
forbidden_next_action: implement any AgentBundle code in 0.34.3; expose a non-executable main agent in the TUI; spawn a bundle subagent before 0.34.8; select A/B/C, context transfer, or resident idle/turn semantics without the owner
required_verification: source/TDD/compatibility validation in the mapped future patch; existing PermissionPlane remains the only authorization authority
remaining_owner_gates: A transient-only vs B resident-only vs C hybrid; input-only vs input+summary vs full-context transfer; resident idle/turn lifecycle
head_sha_and_relevant_delta_at_ruling: 267bfc3c6c66e46fe8514e2e70657489f853b7f0 plus task-document updates only in the isolated worktree
follow_up_of: CONSULT-2026-07-31-PATCH-PLAN-01
supersedes: the earlier temporary hold on finalizing AgentBundle manifest/catalog shape, but not its execution ABI owner gates
final_disposition_and_resumption_decision: document now; implement only in 0.34.6/0.34.8 at their gates; 0.34.3 continues unchanged
```

Determination-level evidence:

| Determination | Status | MacBook Air ruling / verification requirement |
| --- | --- | --- |
| Bundle definitions, Harness execution, catalog entries, native spawn-only entry | `Pro-advised` | Adopted as target design; must be source/TDD verified in future patches. |
| Flat top-level `identity/extensions/resources/agents[]` and per-agent role/lifecycle/resource fields | `Pro-advised` | Adopted as the minimal ABI-neutral manifest shape for `0.34.6`; parser/catalog remains non-executable. |
| Stable namespace and fail-closed resolver | `Pro-advised` | Adopted for `0.34.6`; deterministic conflict/ambiguity tests required. |
| `none/basic/full` resource views narrowed by harness policy | `Pro-advised` | Adopted; bundle declarations can only narrow and never expand current harness policy/`PermissionPlane`. |
| Hybrid A/B behavior through one spawn path | `Pro-advised` | Recommended by Pro and coordinator but not selected by the owner; `0.34.8` remains blocked. |
| Permission or sandbox authority inside a bundle | `rejected` | Existing harness config/`PermissionPlane` is the sole authority; same-UID trusted-code/no-malicious-isolation boundary remains. |

### `CONSULT-2026-07-31-BUNDLE-DISTRIBUTION-03`

This records the fourth-round distribution/CLI review in the same safe
canonical Pro conversation. Exact timestamps and packet digest were not
provided to the `fuji1 remote worker` and are not invented.

```text
consultation_id: CONSULT-2026-07-31-BUNDLE-DISTRIBUTION-03
packet_id: not supplied to fuji1
packet_revision: fourth-round AgentBundle distribution and fixed-runtime-ABI review
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
url_access_classification: canonical URL supplied by the MacBook Air coordinator; no query or fragment recorded
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: Pro
question_summary: review minimal .hyabundle public/private formats, CLI/registry/install state, realistic secrecy limits, key ownership, and reuse of the existing plugin transport
pro_conclusion: detect formats by magic/version; safely stage public archives; retain encrypted private artifacts; use one fixed out-of-process JSON-RPC/stdio runtime protocol; make registry generations authoritative
macbook_air_ruling: adopt-for-future-design-and-verification
ruling_scope: future bundle packaging, inspection, installation, and runtime transport only
permitted_next_action: retain the requirements as advisory design evidence while implementing only 0.34.3
forbidden_next_action: implement packaging, private decryption/activation, bundle CLI, runner ABI, or future patch sequencing in 0.34.3
required_verification: archive traversal/size/symlink tests; protocol conformance; initialize handshake; atomic registry activation; realistic threat-model documentation; source reuse of existing plugin transport
remaining_owner_gates: private key model; Agent execution model; active-version policy; all activation/release gates
head_sha_and_relevant_delta_at_ruling: 267bfc3c6c66e46fe8514e2e70657489f853b7f0; advisory documentation only
follow_up_of: CONSULT-2026-07-31-AGENTBUNDLE-02
supersedes: none
final_disposition_and_resumption_decision: retain as future advisory evidence; 0.34.3 continues unchanged
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| Reuse out-of-process plugin JSON-RPC/stdio instead of a Rust dylib or second transport | `Pro-advised`, `source-verified` | Anchored source has one versioned plugin stdio protocol and initialization descriptor path; future Bundle work must extend equivalent methods rather than duplicate transport. |
| Public archive safe staging and atomic activation | `Pro-advised` | Future fault/compatibility tests required; no implementation authorization exists now. |
| Private envelope provides at-rest ciphertext, not DRM/root/debugger secrecy | `Pro-advised` | Adopted threat-model boundary; key model and activation remain owner-blocked. |
| Fourth-round patch numbering | `rejected` | The owner has placed all future sequencing on the sixth-round bootstrap hold. |

### `CONSULT-2026-07-31-AGENT-PARITY-04`

This records the fifth-round capability-parity review and its later explicit
owner supersession.

```text
consultation_id: CONSULT-2026-07-31-AGENT-PARITY-04
packet_id: not supplied to fuji1
packet_revision: fifth-round current-agent capability and migration review
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
url_access_classification: canonical URL supplied by the MacBook Air coordinator; no query or fragment recorded
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: Pro
question_summary: classify current agent capabilities as effective, parsed-but-ignored, or documentation-only and define a migration/parity gate for AgentBundle
pro_conclusion: build a source-anchored field matrix; require differential parity for effective behavior; implement or typed-reject silent fields; initially proposed a compatibility adapter and synthetic legacy representation
macbook_air_ruling: modify-for-verification, subsequently superseded by explicit owner override
ruling_scope: capability evidence and future built-in native Bundle migration
permitted_next_action: maintain research/agent-capability-parity-matrix.md as native Bundle migration evidence
forbidden_next_action: implement LegacyAgentAdapter, synthetic legacy bundles, legacy bundle list/info, old agent-file loading, or any future Bundle work in 0.34.3
required_verification: source-symbol matrix; current behavior characterization; native Bundle differential coverage for effective behavior; typed rejection or implementation for silent fields; event/replay fixtures
remaining_owner_gates: sixth-round bootstrap authority and phase ordering; Agent execution/context/resident choices; historical unavailable-definition resume semantics
head_sha_and_relevant_delta_at_ruling: 267bfc3c6c66e46fe8514e2e70657489f853b7f0; matrix research and owner supersession only
follow_up_of: CONSULT-2026-07-31-BUNDLE-DISTRIBUTION-03
supersedes: the compatibility-adapter portion is superseded by the owner override; the capability evidence taxonomy remains
final_disposition_and_resumption_decision: native-only parity research may continue; future bootstrap sequencing stays blocked; 0.34.3 continues unchanged
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| `A_EFFECTIVE / B_PARSED_IGNORED / C_DOC_ONLY` evidence taxonomy | `Pro-advised`, `source-verified` | `research/agent-capability-parity-matrix.md` cites current parsers, `AgentSpec`, prompt/model request, TUI, resolver, policy/resource view, and replay paths field by field. |
| Effective capabilities require native Bundle parity; silent fields require implementation or typed rejection | `Pro-advised` | Adopted as a future GA gate; no Bundle implementation is authorized now. |
| `LegacyAgentAdapter`, synthetic legacy bundle, or legacy Bundle CLI compatibility | `rejected` | The user's emergency owner override drops all legacy agent-file support. |
| Preserve historical event/session agent IDs for replay | `source-verified` target constraint | Current events/projection/fork retain agent identifiers; future cutover must keep protocol replay without old-file execution. |
| Fifth-round future patch numbering | `rejected` | Bootstrap authority and all later phase numbers await the sixth-round coordinator ruling. |

### `CONSULT-2026-07-31-NATIVE-BUNDLE-BOOTSTRAP-05`

This records the sixth-round native-only bootstrap/phase-order review in the
same safe canonical Pro conversation. Exact timestamps and packet digest were
not supplied to the `fuji1 remote worker` and are not invented.

```text
consultation_id: CONSULT-2026-07-31-NATIVE-BUNDLE-BOOTSTRAP-05
packet_id: UQ-AGENTBUNDLE-BOOTSTRAP-AND-UNKNOWN-ID-2026-07-31
packet_revision: sixth-round native-only bootstrap and patch-order review
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
url_access_classification: canonical URL supplied by the MacBook Air coordinator; no query or fragment recorded
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: Pro
question_summary: resolve native built-in Bundle bootstrap, immutable origin/stable IDs/replay fixtures, and the dependency-ordered cutover after all old agent-file support was dropped
pro_conclusion: use native Bundles only, prepare built-ins at build time, mark their origin immutable, preserve stable IDs and replay fixtures; Pro also proposed an early 0.34.4 cutover and a temporary old-file detector
macbook_air_ruling: adopt-native-only-with-sequencing-corrections
ruling_scope: controlling future task plan; no change to active 0.34.3 implementation scope
permitted_next_action: finish only 0.34.3 through local gates, commit/push, and remote CI; persist the corrected 0.34.4-0.34.13 planning map
forbidden_next_action: preimplement any future patch; add LegacyAgentAdapter, synthetic bundle, old-file conversion/migration/scanner/fallback, a second catalog/runtime, or treat Pro as owner/security/release approval
required_verification: current-source capability matrix; deterministic package preparation; frozen built-in prompt/model/resource/event fixtures; stable-ID/replay corpus; per-patch TDD/full gates/commit/push/remote CI
remaining_owner_gates: final external AgentBundle execution/context/resident semantics; private Bundle key ownership; unresolved unknown-new-spawn fallback versus fail-closed resolution; updater trust-root/activation; production or irreversible changes
head_sha_and_relevant_delta_at_ruling: 267bfc3c6c66e46fe8514e2e70657489f853b7f0; active isolated 0.34.3 worktree changes plus task documentation, with dirty main protected
follow_up_of: CONSULT-2026-07-31-AGENT-PARITY-04
supersedes: every prior future patch map and every proposal for an agent compatibility adapter, synthetic representation, later agent-format deletion, or temporary old-file detector
final_disposition_and_resumption_decision: native-only map is controlling planning; 0.34.3 continues unchanged and later work waits for its remote CI gate
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| Repo-native built-ins are deterministically prepared at build time and embedded read-only | `Pro-advised`, coordinator-adopted | Future `0.34.8` build/preparation, digest, boot-without-install, and reproducibility tests required. |
| Built-ins have immutable origin and explicit stable agent IDs; event/session IDs are not rewritten | `Pro-advised`, coordinator-adopted, `source-verified` target need | Current source/event matrix identifies the seven built-in IDs and persisted fields; future fixture and collision tests must close the gate. |
| One `AgentBundleIR -> immutable Generation/catalog -> TurnBinding -> AgentSpec -> SessionEngine` path serves built-ins and installed packages | `Pro-advised`, coordinator-adopted | Future source/TDD/no-second-authority audit required. |
| Cut Bundle over in `0.34.4` | `rejected` | Coordinator preserves audited prerequisites: `0.34.4` OperationId/durable admission, `0.34.5` generation/TurnBinding, `0.34.6` reconciliation, and `0.34.7` recovery/fencing before the atomic `0.34.8` cutover. |
| Add a temporary detector for old agent files | `rejected` | Old files are outside discovery; an explicitly supplied unsupported source receives a normal typed error at a Bundle-only boundary. |
| Keep or later delete an agent compatibility path | `rejected` | No adapter/migration/fallback is introduced. Old agent-file production code is removed exactly once in `0.34.8`. |
| Preserve historical event/session replay | `Pro-advised`, `source-verified` target constraint | Replay fixtures must decode unchanged IDs without an old loader or event rewrite. |
| Preserve current unknown-new-spawn fallback to `general` | `escalation-pending` | Round-six material supplied to fuji1 did not state a coordinator ruling for this source-confirmed A-class behavior. It blocks only the affected `0.34.8` resolver/cutover gate; it does not block `0.34.3`–`0.34.7`. |

The sixth-round bootstrap and phase-order hold is resolved. Only the narrowly
scoped unknown-new-spawn fallback decision remains open for `0.34.8`; the
`fuji1 remote worker` must not silently choose parity or a breaking fail-closed
change.

Post-ledger source accounting on 2026-07-31 is `source-verified`:
`origin/main` advanced from the consultation/audit HEAD
`267bfc3c6c66e46fe8514e2e70657489f853b7f0` to
`156d0ad3c50aea67dfac0054485eb6991e77308b` through a README icon-reference
change only. The isolated branch was rebased to the newer commit; this neither
changes the Pro ruling nor promotes any advisory conclusion.

### `CONSULT-2026-07-31-OPERATION-ADMISSION-06`

This records the MacBook Air coordinator's completed `0.34.4`
OperationId/durable-admission consultation and the controlling owner
corrections delivered to the canonical `fuji1 remote worker` session. The
delivery did not restate a safe session URL, exact displayed model label,
timestamps, or packet digest; this ledger does not infer them from an earlier
round.

```text
consultation_id: CONSULT-2026-07-31-OPERATION-ADMISSION-06
packet_id: not supplied to fuji1
packet_revision: 0.34.4 OperationId and fail-closed recovery ruling
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: not restated in coordinator delivery
url_access_classification: no URL recorded or inferred
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: not restated in coordinator delivery
question_summary: choose the minimum durable identity, admission journal, recovery, idempotency, and exact-release contract for 0.34.4
pro_conclusion: use a deterministic operation identity, a narrow additive journal, fail-closed startup recovery, immutable request identity, and one terminal/finalize path
macbook_air_ruling: modify-for-verification
ruling_scope: release 0.34.4 only
permitted_next_action: derive OperationId from persisted ToolCallId, add the narrow SessionStore journal, implement the specified RED/GREEN matrix, bump to 0.34.4, and deliver through the existing branch and draft PR after full local and remote gates
forbidden_next_action: add a random operation UUID, public OperationId DTO/event/CLI field, operation_child/member/effect journal, durable runnable queue or scheduler, Bundle/generation/reconciliation/fencing work, merge, or start 0.34.5 before remote CI is green
required_verification: pinned deterministic identity tests; serial/concurrent idempotency and fingerprint-conflict tests; no-debit overload; exact-once finalization; terminal immutability; startup abort recovery; replay independence; full workspace gates; pushed remote CI green
remaining_owner_gates: every 0.34.5+ stage and all existing Bundle/key/updater/activation decisions
head_sha_and_relevant_delta_at_ruling: b8c21deeb5004e1f703b199a40de196902fadf35; clean isolated worktree, with dirty main and its 19 user-owned paths untouched
follow_up_of: CONSULT-2026-07-31-NATIVE-BUNDLE-BOOTSTRAP-05
supersedes: the generic earlier P1 operation/effect-journal proposal only for the bounded 0.34.4 spawn-admission slice
final_disposition_and_resumption_decision: 0.34.4 may proceed in the existing isolated worktree/session/task/branch/PR; no later stage may begin before its remote CI gate
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| Derive one strong `OperationId` deterministically and domain-separately from the persisted UUID-backed `ToolCallId` | `Pro-advised`, coordinator-adopted, `source-verified` seam | `ToolCallId` is persisted on tool-call events and reaches both normal and direct-shell execution; fixed-vector TDD must verify the derivation and mandatory `ToolCtx` propagation. |
| Use `accepted -> started -> completed|cancelled|aborted` with immutable fingerprint/units and irreversible terminals | `Pro-advised`, coordinator-adopted | Additive SQLite migration and serial/concurrent transition tests are required; no public `Event` is added. |
| Abort every nonterminal admission before spawn readiness and never resume or redispatch | `Pro-advised`, coordinator-adopted | `build_session_engine` currently starts the spawner/resident supervisors; recovery must become a fallible prerequisite before those constructors. |
| Add `operation_child` and preallocate child IDs | `rejected` | Current fail-closed recovery never resumes an operation. The started/terminal gate prevents redispatch without a child table; `SessionEngine::create_with_id` is not expanded for this slice. |
| Treat restart recovery as governor refund | `rejected` | Old process-local debits disappear with the process. Recovery records only logical release state and never credits the fresh governor. |
| Treat Pro as migration, release, or security approval | `rejected` | The coordinator ruling plus explicit user authorization bounds implementation; source/TDD/replay/CI and all owner gates remain mandatory. |

### `CONSULT-2026-07-31-PTY-HARNESS-07`

This records the Browser/Pro diagnosis and source-evidence follow-up for the
three consecutive draft-PR PTY child-observation failures. The MacBook Air
coordinator identified this as a test-harness delivery/state-observation issue,
not authorization to alter TUI product behavior. The exact conversation URL,
displayed model label, timestamps, and packet digest were not restated in the
delivery to the `fuji1 remote worker` and are not inferred here.

```text
consultation_id: CONSULT-2026-07-31-PTY-HARNESS-07
packet_id: not supplied to fuji1
packet_revision: PTY FileSink delivery and causal observation ruling
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: not restated in coordinator delivery
url_access_classification: existing Browser/Pro conversation identified; no URL copied or inferred
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: not restated in coordinator delivery
question_summary: distinguish sequential PTY harness delivery/state races from concurrency, define deterministic FileSink TDD, and require causal diagnostics without product hooks
pro_conclusion: serialize each semantic input at the FileSink delivery boundary, prove it with a test-local spy, replace causal sleeps with existing request/render observations, and capture bounded callsite-specific phase evidence
macbook_air_ruling: adopt-with-source-evidence-corrections
ruling_scope: 0.34.4 draft-PR PTY release-gate repair only
permitted_next_action: preserve the focused PTY test, add one adjacent helper regression, instrument only its existing proxy/request and transcript seams, run all local release gates, then commit/push and await the same draft PR
forbidden_next_action: change test concurrency, product focus behavior, crates, public events/protocols/APIs, workflow timeouts, dependencies, log levels, child-pipe draining, retries/repeated chords, skipped widths, or begin 0.34.5
required_verification: deterministic RED then GREEN; unique worker callsites; bounded frame/transcript/process/phase diagnostics; TUI typecheck; PTY 3/3; complete TUI suite; full Rust fmt/clippy/test/build; remote CI green
remaining_owner_gates: all 0.34.5+ work and the existing Bundle/key/updater/activation decisions
head_sha_and_relevant_delta_at_ruling: d4825a8c35d86c37c19f87800c70a7eebd93a6b7; one uncommitted PTY test candidate in the isolated worktree, dirty main untouched
follow_up_of: CONSULT-2026-07-31-OPERATION-ADMISSION-06
supersedes: the partial stdin-flush/fixed-wait PTY repair only; no 0.34.4 product semantics
final_disposition_and_resumption_decision: implement and verify the bounded causal harness repair, then update the existing branch and draft PR; 0.34.5 remains blocked on remote green
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| 80/140 execution is sequential and mutable resources are isolated | `Pro-advised`, `source-verified` | The tests are ordinary non-concurrent cases; logs show serial execution, while each run owns a unique temp HOME/XDG/project/SQLite database, proxy/backend port, transcript, and session set. No concurrency change is permitted. |
| A semantic write must await both `FileSink.write` and the immediately following `FileSink.flush` | `Pro-advised`, `source-verified`, `experimentally-verified` | Bun's local type contract permits pending writes and defines flush as committing the internal pipe buffer. `semantic_input_flushes_before_next_action` deterministically RED-produced `["", "chord-a"]` instead of `["chord-a", "chord-b"]`; awaiting write before flush made it GREEN. |
| The two worker timeouts must be independently diagnosable | `Pro-advised`, `experimentally-verified` | The callsites are now `open-by-handle/worker-1-focused-header` and `ctrl-x-dot/worker-1-focused-header`; failure output includes bounded frame/raw tail, backend/PTY PID and status, and a 64-entry monotonic phase trace. |
| The observed worker failure was missing delivery | `rejected` | Local causal trace showed the open-by-handle request/list/open/focus/render path completed and the `Ctrl+X .` bytes flushed, but the stable render focused `scroll-1`: the old test had not established `researcher-1` as worker's predecessor. The corrected flow source-verifies that predecessor before the single cycle. |
| Drain stdout/stderr or add a product-side acknowledgement hook | `rejected` | The local trace resolved the current failure without either change; a separately authorized bounded drain experiment is considered only if remote evidence later shows backend event emission without UI observation. |
| Local repair gate | `experimentally-verified`, remote pending | Helper regression, PTY 3/3, TUI 44/44, typecheck, Rust fmt, Clippy, workspace tests, and bins build are green. Commit/push and the resulting draft-PR CI remain required. |

### `CONSULT-2026-07-31-RUNTIME-GENERATION-08`

This records the MacBook Air coordinator's completed Browser/Pro decision
round for `0.34.5` and the corrected ruling delivered to the canonical
`fuji1 remote worker` session. Exact Browser provenance fields were not
restated in that delivery and are not inferred from an earlier record.

```text
consultation_id: CONSULT-2026-07-31-RUNTIME-GENERATION-08
packet_id: not supplied to fuji1
packet_revision: 0.34.5 immutable runtime generation and TurnBinding ruling
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: not restated in coordinator delivery
url_access_classification: no URL recorded or inferred
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: not restated in coordinator delivery
question_summary: choose the minimum immutable runtime generation, per-turn binding, event identity, and atomic tool/skill/MCP refresh boundary for 0.34.5
pro_conclusion: not separately quoted in the coordinator delivery; the corrected coordinator ruling below is controlling
macbook_air_ruling: one source-owned immutable RuntimeSnapshot and single atomic publisher; bind once after admission and before prompt/provider/tool behavior; retain the binding for all rounds; failed/no-op candidates preserve generation; successful concurrent publications allocate unique monotonic generations; events record identity only
ruling_scope: release 0.34.5 only
permitted_next_action: implement deterministic RED/GREEN for generation retention, failure/no-op preservation, complete-view publication, concurrent publication, real turn/schema/skill/dispatch binding, shell/event audit, and source-owned deferred MCP publication; bump and deliver 0.34.5 through the existing branch/PR
forbidden_next_action: desired-observed-effective reconciliation, plugin respawn declarations, namespace/catalog work, resident lease/effect fencing, Bundle work, updater, sandbox/new permission framework, merge, or start 0.34.6 before remote CI is green
required_verification: focused deterministic tests; shared projection/event replay; full workspace fmt/clippy/test/build; CI-required TUI gates for the version change; local executable; exact dirty/stash accounting; pushed remote CI green
remaining_owner_gates: every 0.34.6+ stage and all existing Bundle/key/updater/activation decisions
head_sha_and_relevant_delta_at_ruling: 709abafb81ba0f94656254d3ecb51b42e051a89d; clean isolated 0.34.4 branch/worktree, dirty main and three stashes protected
follow_up_of: CONSULT-2026-07-31-PTY-HARNESS-07
supersedes: broader target-generation/reconciliation language only for the bounded 0.34.5 prerequisite
final_disposition_and_resumption_decision: 0.34.5 may proceed in the existing isolated worktree/session/task/branch/PR; no later stage may begin before its remote CI gate
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| One engine-owned immutable snapshot and atomic publisher | `coordinator-adopted`, `source-verified`, `experimentally-verified` | `SessionEngine` owns one `RuntimeRegistry`; retained candidate-builder mutation is invisible; deferred MCP candidate members remain invisible until one publication. |
| One binding across prompt skills, schemas, resolution, and dispatch | `coordinator-adopted`, `source-verified`, `experimentally-verified` | The gated multi-round engine tracer publishes mid-turn and proves both rounds plus tool/skill execution retain N while the next turn sees N+1. |
| Failure/no-op preservation and concurrent monotonic publication | `coordinator-adopted`, `experimentally-verified` | Duplicate-candidate failure and logical remove/re-add no-op preserve N; eight concurrent complete candidates receive consecutive generations and never expose a mixed final view. |
| Lightweight event-sourced audit | `coordinator-adopted`, `source-verified`, `experimentally-verified` | `TurnBindingRecorded` carries session routing, message, and generation only; the shared reducer folds the optional message generation and serialization contains no registry payload. |
| Treat Pro as reconciliation, release, or owner approval | `rejected` | `0.34.6+`, release/merge/activation, and all Bundle/key/updater owner decisions remain independently gated. |

### `CONSULT-2026-07-31-RUNTIME-RECONCILIATION-09`

This records the MacBook Air coordinator's Browser/Pro proposal audit for
`0.34.6`, including the follow-up dependency and documentation corrections.
Pro remains advisory evidence; the coordinator ruling below controls the
implementation boundary and cannot approve release, security, or later-stage
owner decisions.

```text
consultation_id: CONSULT-2026-07-31-RUNTIME-RECONCILIATION-09
packet_id: not supplied to fuji1
packet_revision: 0.34.6 MCP/plugin desired-observed-effective proposal audit
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
url_access_classification: existing MacBook Air in-app Browser conversation; fuji1 did not open it
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: Pro
question_summary: audit the smallest source-owned MCP/plugin reconciliation seam, stale child cleanup, plugin declaration/lifecycle boundary, and deterministic TDD order for 0.34.6
pro_conclusion: use one app desired/observed reconciler feeding the existing RuntimeRegistry; keep I/O outside locks; close stale/unpublished owners; publish explicit removals before additions; retain old effective owners through TurnBinding; validate stable identities and the complete plugin initialize declaration
macbook_air_ruling: adopt with corrections—RuntimeReconciler has no effective cache/dispatch; RuntimeSnapshot alone owns effective manifests and clients; current-base publication must preserve unrelated skill changes; hya-server receives a narrow MCP control handle; plugin scope is tools plus RPC binding and re-handshake drift only; hooks and PermissionPlane remain unchanged; no new dependency
ruling_scope: release 0.34.6 only
permitted_next_action: deterministic RED/GREEN in the specified order; dynamic MCP configuration/control documentation; exact version/changelog/Trellis evidence; full local gates; atomic commit/push and same draft-PR remote CI
forbidden_next_action: plugin watcher or hot add/remove/reload claim, whole-plugin snapshot, generic hook/control plane, permission interceptor/framework, lock-held I/O, second effective/status authority, early owner termination, partial addition publication, lease/fencing, Bundle work, new dependency, merge, or 0.34.7
required_verification: stale-success close; drop-only removal with old/new binding; current-failure cleanup/no generation; identity/collision rejection; complete mixed MCP/plugin publication; plugin declaration-drift fail-closed; source-owner lifetime; app/server authority audit; zero-new-dependency proof; full release gates and remote CI green
remaining_owner_gates: every 0.34.7+ stage and all existing Bundle execution/key/updater/activation decisions
head_sha_and_relevant_delta_at_ruling: 95f4fe20b3750d376023384d869a52da1e84201f; clean isolated 0.34.5 baseline before the single 0.34.6 change set, dirty main and three stashes protected
follow_up_of: CONSULT-2026-07-31-RUNTIME-GENERATION-08
supersedes: broader plugin reconciliation/hot-reload wording for the bounded 0.34.6 stage
final_disposition_and_resumption_decision: implement 0.34.6 once in the existing session/task/worktree/branch/PR; do not start 0.34.7 before remote CI green
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| App reconciler coordinates desired/observed only | `Pro-advised`, `coordinator-adopted`, `source-verified`, `experimentally-verified` | The reconciler stores revision/ticket/outcome state and delegates complete publication to the one `RuntimeRegistry`; it has no resolver, dispatcher, or effective tool cache. |
| I/O and owner lifetime remain outside reconciliation locks | `Pro-advised`, `coordinator-adopted`, `experimentally-verified` | Stale/current failed staging owners are released after unlocking; published owners transfer into immutable snapshots and survive old `TurnBinding` Arcs. |
| Explicit removal is safety-priority drop-only | `Pro-advised`, `coordinator-adopted`, `experimentally-verified` | Removal publishes from the current effective snapshot before unrelated preparation; a later failure cannot restore it, and no generation is consumed by the failed addition. |
| Plugin consistency claim is tools plus RPC binding only | `coordinator-corrected`, `source-verified`, `experimentally-verified` | Complete initialize declaration drift closes the respawned process and calls fail closed; no watcher, plugin hot add/remove/reload, dynamic hook plane, or whole-plugin effective snapshot is claimed. |
| Server and manager may own effective/status state | `rejected` | `hya-server` receives only `McpControl`; the deleted HTTP state map is not replaced, and `McpManager` remains an I/O helper. Status is composed from reconciler observation plus the active runtime manifest. |
| Add a digest dependency to hya-plugin | `rejected`, `source-verified` | The plugin crate emits deterministic canonical declaration bytes using existing `serde_json`; the app uses its pre-existing digest dependency. Crate dependency topology does not grow. |
| Pro output authorizes release/security/later stages | `rejected` | Full local and remote gates, repository release rules, current `PermissionPlane`, and all later owner decisions remain independent requirements. |

### `CONSULT-2026-07-31-RESIDENT-FENCING-10`

This records the MacBook Air coordinator's completed Browser/Pro audit for
`0.34.7` and the controlling corrections delivered to the canonical
`fuji1 remote worker` session. The delivery authorized the bounded resident
recovery/fencing slice only after `0.34.6` closed green. It did not restate the
conversation URL, displayed model label, timestamps, or packet digest, so this
record does not infer them from an earlier consultation.

```text
consultation_id: CONSULT-2026-07-31-RESIDENT-FENCING-10
packet_id: not supplied to fuji1
packet_revision: 0.34.7 TTL-free resident claim/epoch recovery and minimal effect-fencing audit
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: not restated in coordinator delivery
url_access_classification: existing MacBook Air Browser/Pro consultation; no URL copied or inferred
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: not restated in coordinator delivery
question_summary: choose the minimum persistent resident identity, non-time-based claim/epoch model, recovery ordering, operation binding, and stale-result fence for 0.34.7
pro_conclusion: use the already durable resident identity; fence by a transactional monotonically increasing actor epoch before recovery; bind only resident-originated existing admission operations; resume queued-not-started work and abort running work; reject late old-claim commits
macbook_air_ruling: adopt with corrections—no TTL or supervisor; ordinary claims have one winner; recovery fences before abort/refund; one narrow store commit seam checks claim and optional operation binding before canonical event/transition commit; external effects are not claimed exactly once
ruling_scope: release 0.34.7 only
permitted_next_action: implement the ordered deterministic RED/GREEN matrix, exact 0.34.7 release metadata/docs, full local gates, atomic commit/push, and same draft-PR remote CI
forbidden_next_action: TTL, heartbeat, wall-clock expiry, background lease supervisor, distributed lock/consensus/HA/active-active, generic executor/outbox/effect framework, second operation journal, new permission/sandbox work, AgentBundle, 100/256 certification, updater, new dependency, merge, or 0.34.8
required_verification: one-winner concurrent claim; epoch-increment takeover; stale tool/child commit rejection while old TurnBinding survives; abort/refund once; queued-versus-running recovery; repeatable projection/terminal recovery; unchanged transient path; full release gates and remote CI green
remaining_owner_gates: every 0.34.8+ stage and all existing Bundle execution/key/updater/activation decisions
head_sha_and_relevant_delta_at_ruling: 680f9fb535fc48f71f9aead64cc3d3d30161678a; 0.34.6 remote CI run 30634501761 attempt 2 green; clean isolated worktree; protected main and three stashes untouched
follow_up_of: CONSULT-2026-07-31-RUNTIME-RECONCILIATION-09
supersedes: the generic P6 lease/effect-journal target only for the bounded 0.34.7 single-process resident correctness slice
final_disposition_and_resumption_decision: implement 0.34.7 once in the existing session/task/worktree/branch/PR; do not start 0.34.8 before remote CI green and a new MacBook Air authorization
```

Determination-level evidence and authoritative-HEAD corrections:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| Reuse the persisted resident actor identity | `Pro-advised`, `coordinator-adopted`, `source-verified` | Current `AgentRegistered.agent_session` is the resident's stable `SessionId` in the team-root event log and roster projection. `MemberId` belongs to the parent tree lifecycle and is not used as a second actor identity. |
| Extend the existing operation representation rather than create an effect journal | `coordinator-adopted`, `source-verified` | `0.34.4` is a literal `admission_journal` table with `OperationId`, source call, root, immutable fingerprint, units, state, and exact-release marker. Resident-only optional actor identity/epoch belongs there; no second operation table is justified. |
| Use one TTL-free transactional claim table | `Pro-advised`, `coordinator-adopted`, `source-verified`, `experimentally-verified` | Migration `0005_resident_actor_claim.sql` adds the sole coordination table. It does not replace the event projection, admission journal, `ResidentSupervisor`, or `RuntimeRegistry`, and has no time field, heartbeat, or lease task. Concurrent ordinary claim, takeover, and full-tuple release tests are green. |
| Fence before abort/refund and runtime recreation | `coordinator-corrected`, `source-verified`, `experimentally-verified` | Startup increments every active actor epoch while spawn/send/wait remain closed, then performs fail-closed admission recovery, terminalizes old running tool/message/child state, and recreates the existing supervisor. The app integration test observes running termination and queued scheduling before builder readiness. |
| Existing events already persist a resident work cursor/start boundary | `rejected`, `source-verified` | `MailSent` and `AgentActivityChanged` persist inbox and roster status, but `SlotState.cursor`, `pending`, and `initial` are memory-only and startup creates an empty team map. One minimal work-start marker is therefore permitted; no parallel resident read model or generic work journal is added. |
| Actor epoch and runtime generation are the same fence | `rejected`, `source-verified` | `RuntimeSnapshot`/`TurnBinding` lifetime is retained by `Arc` and remains orthogonal to the per-resident execution claim. Takeover must not terminate or republish registry owners. |
| Fencing provides arbitrary external exactly-once behavior | `rejected` | A pre-dispatch check plus result-commit fence prevents stale canonical-state advancement, but cannot reverse a file/network/API effect that completed before takeover. Running/in-flight work is aborted and never automatically retried. |

Implementation follow-up evidence:

- `source-verified`: `OwnerRunId` is process-stable; actor claim and operation
  actor binding are SQLite indexed point checks; root cleanup cannot mutate a
  resident-bound operation; old `TurnBinding` owners are untouched.
- `experimentally-verified`: the seven ordered RED/GREEN cases plus audit REDs
  cover repeated recovery with an already-started user turn, claim-less
  admission-finalize rejection, and terminalization of running child/tool/
  assistant projection state. The final authority audit additionally proves
  that non-actor startup recovery cannot consume actor-bound rows and that
  full-tuple claim release atomically aborts its bound admission before the
  claim becomes reusable, with governor refund applied once.
- `rejected`: a generic effect/outbox state machine, time lease, active-active
  takeover, automatic retry of running effects, new permission behavior,
  Bundle work, or a 100/256 capacity claim. None appears in `0.34.7`.
- Full workspace and remote-CI evidence remains a delivery gate and is not
  implied by this focused proof.

### `CONSULT-2026-07-31-PTY-CONTINUOUS-DRAIN-11`

This records the MacBook Air coordinator's bounded Browser/Pro audit after
draft-PR CI run `30643007465` failed twice in the byte-identical TypeScript PTY
smoke fixture. The audit authorizes only a test-local continuous pipe-drain
repair for release `0.34.7`; it does not reopen product scope or authorize a
blind rerun.

```text
consultation_id: CONSULT-2026-07-31-PTY-CONTINUOUS-DRAIN-11
packet_id: not supplied to fuji1
packet_revision: 0.34.7 PTY child-stream backpressure and bounded-diagnostics audit
packet_digest: not supplied
submitted_at_utc: not supplied
returned_at_utc: not supplied
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: not restated in coordinator delivery
url_access_classification: existing MacBook Air Browser/Pro consultation; no URL copied or inferred
requested_selection: ChatGPT Pro Model
displayed_model_label_exact: not restated in coordinator delivery
question_summary: determine the smallest deterministic repair for two 80-column PTY observation timeouts that drifted between root-frame startup and a later permission render while the fixture source remained unchanged
pro_conclusion: continuously drain every already-piped child stream from spawn to EOF with one reader, retain only a byte-bounded tail, and make the server readiness reader keep draining after detecting a marker across chunk boundaries
macbook_air_ruling: adopt exactly one test-only repair; first prove real stderr backpressure prevents a child from reaching stdout DONE, then add the smallest local drain/tail helper and wire server stdout/stderr plus recovery/main TUI stderr without changing product behavior, waits, retries, concurrency, or ignored stdout
ruling_scope: release 0.34.7 CI repair only
permitted_next_action: deterministic RED/GREEN for drains_child_stderr_while_waiting_for_stdout_marker; bounded stream diagnostics; existing TUI and full release gates; a new atomic follow-up commit; one fresh full CI run for the new SHA
forbidden_next_action: product/Rust/workflow/dependency/lockfile/version/changelog change, timeout or sleep increase, retry/repeated input, assertion weakening, test serialization change, generic process supervisor, unbounded log collection, multiple readers, blind rerun, amend of 6f3402e, or any 0.34.8 work
required_verification: expected behavioral RED rather than missing-helper RED; focused GREEN; typecheck/build; PTY 4/4; TUI 44/44; Rust fmt/clippy/test/build; executable and zero-INET gates; exact diff/accounting; new-SHA remote CI fully green
remaining_owner_gates: any event-applied/focus-changed sidechannel if drains prove backend event delivery without UI observation; every 0.34.8+ stage
head_sha_and_relevant_delta_at_ruling: 6f3402e10cd10c87a5547426df89075b2e18f1ba; clean isolated branch/worktree and PR #24; PTY fixture blob 1628cedcd840187ac22493a02b7574a658f35e30 matches green 0.34.6; only TUI package version changed
follow_up_of: CONSULT-2026-07-31-RESIDENT-FENCING-10 and the earlier PTY harness consultation
supersedes: the freeze on PTY edits only for this exact continuous-drain experiment; no other 0.34.7 or later scope changes
final_disposition_and_resumption_decision: perform one bounded test-only RED/GREEN and deliver it as a separate commit; if fresh CI still fails, stop with unique phase/frame/four-tail evidence and return to Mac Browser/Pro
```

Determination-level evidence at resumption:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| The failures establish a 0.34.7 product regression | `rejected`, `source-verified` | Attempts 1 and 2 failed different 80-column observation points while both 140-column cases passed; the PTY source blob is identical to green `0.34.6`, and `0.34.7` changes only the TUI package version. |
| Already-piped child streams may remain unread | `Pro-advised-pending-verification` | Current server, recovery PTY, and main PTY stderr are `pipe` streams with no reader. The required regression must independently demonstrate real child backpressure before this determination becomes experimental evidence. |
| A successful new CI run alone validates the repair | `rejected` | The behavioral RED, bounded GREEN, local TUI/workspace/zero-INET gates, exact diff, and fresh-SHA full remote run all remain required. |

#### Controlling correction to `CONSULT-2026-07-31-PTY-CONTINUOUS-DRAIN-11`

The MacBook Air coordinator returned a second Browser/Pro ruling after the
bounded experiment contradicted the original causal premise. This correction
supersedes the requested subprocess/backpressure RED while preserving the
test-harness lifecycle repair as an unproven CI-flake mitigation.

```text
correction_received_by_fuji1_date_utc: 2026-07-31
withdrawn_claim: OS pipe backpressure was reproduced or established as the cause of either CI timeout
source_experiment_correction: an unread Bun parent stderr JS stream still allowed 4 MiB process.stderr output and 64 MiB Bun FileSink output to reach stdout DONE; low-level fd2/node:fs attempts produced EAGAIN or sandbox errors rather than the required blocked-child behavior
corrected_macbook_air_ruling: do not expand child payloads or use fd2/EAGAIN/poll/IPC; validate only one-reader continuous consumption, cross-chunk readiness, byte-bounded tail retention, EOF/cancel settlement, spawn/cleanup wiring, and diagnostics
corrected_red: an injected ReadableStream splits a readiness marker across chunks, resolves readiness before later data, emits more than 64 KiB ending in a sentinel, and proves the same reader continues through EOF with a <=64 KiB byte tail and one getReader acquisition
claim_boundary: the change is a test-harness lifecycle, bounded-buffer, and diagnostics repair; its effect on the PTY CI flake is pending a fresh full run and is not a proven root-cause fix
forbidden_after_correction: subprocess backpressure experiment, larger payload, node:fs/fd2/EAGAIN/poll, IPC/handshake, product sidechannel, timeout/retry/concurrency change, or claim of established causality
```

Corrected determination status:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| Unread Bun stderr reproduced a blocked child | `rejected`, `experimentally-verified` | The accepted 4–64 MiB experiments reached stdout `DONE`; empty captured tails did not satisfy the proposed behavioral RED. Low-level write errors were fixture failures, not evidence of a blocked child. |
| A single reader can resolve readiness and continue bounded draining | `Pro-advised-pending-verification` | This is now the sole deterministic helper contract to prove with an injected `ReadableStream`; it makes no OS-pipe or CI-root-cause claim. |
| Continuous drain fixes the historical PTY flake | `benchmark-unconfirmed` | Only a new-SHA full CI run may provide evidence for this fixture-lifecycle mitigation; a failure returns unique phase/frame/tails to Mac Browser without an automatic rerun. |

### `CONSULT-2026-07-31-PTY-EVENT-ORDER-12`

The MacBook Air coordinator returned a narrower Browser/Pro audit after the
corrected one-reader drain contract passed but the same local focused run still
timed out at the 80-column `grandchild-permission-in-main` render. The drain
experiment is retained only as rejected working evidence. Product order may
change only if one exact causal diagnostic proves that Escape replies to a
newly pending permission before its first transcript render.

```text
consultation_id: CONSULT-2026-07-31-PTY-EVENT-ORDER-12
packet_revision: 0.34.7 PTY permission first-render versus Escape ordering audit
received_by_fuji1_date_utc: 2026-07-31
requested_selection: ChatGPT Pro Model
question_summary: distinguish a missed permission render from Escape propagating into the newly mounted permission prompt before its first visible frame
pro_conclusion: lock the exact pending permission ID and request/output cursors before Escape, then observe transcript-first-render, the existing permission-reply POST, and pending-list disappearance together inside the existing poll budget
macbook_air_ruling: run this causal diagnostic exactly once before any product edit; only ESCAPE_PROPAGATED_TO_NEW_PERMISSION_PROMPT authorizes the two narrowly ordered Escape handlers in the existing session route
ruling_scope: release 0.34.7 PTY repair only
permitted_next_action: test-only causal probe using the existing proxy request log, transcript, permission.list API, and waitFor poll seam; conditional minimal session-route ordering fix; removal of all disproven drain code before final verification
forbidden_next_action: product edit before exact confirmation, sidechannel, public API/Event, new timeout/sleep/retry, repeated Escape, assertion weakening, concurrency/workflow/dependency/lock/version/changelog change, generic key framework, amend, or 0.34.8 work
required_red: permission reply POST or disappearance of locked pending ID P is observed while the transcript since the locked cursor has never contained Permission required; emit ESCAPE_PROPAGATED_TO_NEW_PERMISSION_PROMPT with P, matching requests, callsite/phase, and last frame
disproof_condition: the original wait times out with no matching reply and P still pending; stop without product changes or another run
conditional_green: in the high-priority observation Escape handler, clear pending if needed, consume, then focus Main; in the fallback handler, clear pending if needed, preventDefault and stopPropagation, then focus Main
final_cleanup: remove startBoundedDrain, its helper test, drain wiring/tails/cleanup changes, and restore original server readiness; retain only causal regression evidence, minimal diagnostics, any proven product ordering fix, and this ledger
head_sha_and_relevant_delta_at_ruling: 6f3402e10cd10c87a5547426df89075b2e18f1ba; no repair commit/push/CI; three authorized dirty files only
final_disposition_and_resumption_decision: exact causal RED gates every product edit; a non-matching failure freezes the branch for another Mac Browser audit
```

Determination status before the one permitted focused run:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| Continuous draining fixes the 80-column timeout | `rejected`, `experimentally-verified` | The pure helper contract passed, but the local focused suite still failed at `grandchild-permission-in-main/80`; 140 columns passed and all four drained tails were empty. |
| Escape reaches the permission prompt before its first frame | `Pro-advised-pending-verification` | This is a causal hypothesis, not a source fact. It becomes actionable only through the exact locked-P/reply/render RED above. |
| Reordering Escape is currently authorized | `rejected` | No product edit is permitted until the exact diagnostic fires. |

### `CONSULT-2026-07-31-PTY-TRANSCRIPT-ORACLE-13`

The MacBook Air coordinator returned a Browser/Pro transcript-oracle audit
after the single event-order run passed the locked-permission seam at 80
columns but failed earlier at `m62d1 Main focus` at 140 columns. No product
change is authorized. The existing timeout is treated as an integration RED
against a test oracle that waited for an already-present draft to be emitted
again before sending the input marker that could causally prove Main focus.

```text
consultation_id: CONSULT-2026-07-31-PTY-TRANSCRIPT-ORACLE-13
packet_revision: 0.34.7 PTY confirmMainInput transcript-oracle audit
received_by_fuji1_date_utc: 2026-07-31
requested_selection: ChatGPT Pro Model
question_summary: determine whether confirmMainInput blocks on a stale transcript oracle before sending the existing marker that proves focus and input delivery
pro_conclusion: preserve one semantic Escape write/flush, remove the two pre-marker waits for rootDraft re-emission, immediately write the existing marker once, and require marker plus rootDraft together in the transcript delta
macbook_air_ruling: modify only confirmMainInput and run the current five-test focused PTY file exactly once; only an all-green result permits cleanup and release verification
ruling_scope: release 0.34.7 PTY test oracle only; no product change
permitted_next_action: one confirmMainInput diagnostic edit; one focused run; on all-green only, remove all bounded-drain code, retain the oracle and locked-P regressions, run complete release gates, commit/push once, and await one fresh CI run
forbidden_next_action: product/API/Event/key-order change, new timeout/sleep/retry, repeated Escape or marker, assertion weakening, screen emulator, dependency/concurrency/workflow/lock/version/changelog edit, amend, blind CI rerun, or 0.34.8 work
success_condition: both widths complete and the transcript delta after one Escape plus the existing marker contains both marker and rootDraft; the locked-P permission check also remains green
freeze_conditions: marker absent; marker present but rootDraft absent; any typed permission-order outcome; or any non-green focused result
final_cleanup_if_green: remove startBoundedDrain, helper contract test, stream drain wiring/tails/cleanup, and restore original server readiness reader; preserve confirmMainInput oracle, locked-P regression, concise callsite/phase/last-frame diagnostics, and Trellis evidence
head_sha_and_relevant_delta_at_ruling: 6f3402e10cd10c87a5547426df89075b2e18f1ba; no product edit, commit, push, or new CI; three authorized dirty files
final_disposition_and_resumption_decision: the one focused run is the sole gate to cleanup and full verification; any failure freezes without a second run
```

The prior event-order run is classified as follows:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| Escape consumed the pending permission before first render at 80 columns | `rejected`, `experimentally-verified` | The 80-column case passed the locked-P check: `Permission required` appeared while P remained pending, so `ESCAPE_PROPAGATED_TO_NEW_PERMISSION_PROMPT` did not fire. |
| The 140-column `m62d1 Main focus` timeout proves focus/input failure | `Pro-advised-pending-verification` | The marker had not yet been sent because two pre-marker waits required rootDraft to reappear. The revised oracle must send the existing marker before classifying focus. |
| Any TUI product change is authorized | `rejected` | This consultation is strictly test-only. |

Final evidence and disposition:

- `experimentally-verified`: the single permitted focused command
  `CI=true bun test test/pty-smoke.test.ts` passed all five intermediate tests:
  semantic-input flush, bounded-drain contract, basic PTY, 80-column workspace,
  and 140-column workspace. The 80-column case completed in 14.893 seconds and
  140 columns in 11.877 seconds. The revised `confirmMainInput` therefore
  supplied its existing marker before classification and observed both marker
  and `rootDraft`; the locked-P permission regression also remained green.
- `rejected`, `experimentally-verified`: bounded draining is not part of the
  repair. `startBoundedDrain`, its contract test, all stream wiring/tails, and
  its extra cleanup were removed. Both server launch sites use the exact
  original single readiness reader again.
- `source-verified`: the final code diff contains no product file. It retains
  only the `confirmMainInput` transcript oracle, the locked-P
  reply-versus-first-render check, and narrow callsite/phase/last-frame
  diagnostics in `pty-smoke.test.ts`, plus this task evidence.
- `experimentally-verified`: TUI typecheck/build passed and the one permitted
  post-cleanup full `CI=true bun test` run passed 44/44. Rust fmt, clippy,
  workspace tests, workspace/bins and locked launcher builds passed; `hya`,
  `hya-backend`, and `hya-ts` report `0.34.7`; the strace gate reports
  `OK: zero inet sockets`. The first sandboxed workspace-test attempt could not
  bind the existing OAuth loopback fixture (`Operation not permitted`); the
  identical command passed outside the restricted sandbox.
- `pending`: exact staging, the new follow-up commit, push, and one fresh-SHA
  remote CI run remain release gates. No CI rerun is authorized on failure.

### `CONSULT-2026-07-31-PTY-SSE-PASSTHROUGH-14`

The MacBook Air coordinator returned a Browser/Pro audit after the sole fresh
CI run for `326ec66be078d3a757cdf8011986b4814015cca2` failed at the locked
permission observation seam. The ruling authorizes a test-only, byte-preserving
trace only if the existing `/global/event` proxy already exposes one upstream
reader and one downstream writer. It explicitly forbids replacing a direct
`Response` pass-through with a transforming stream merely to create that seam.

```text
consultation_id: CONSULT-2026-07-31-PTY-SSE-PASSTHROUGH-14
packet_revision: 0.34.7 PTY single-subscription SSE pass-through audit
received_by_fuji1_date_utc: 2026-07-31
requested_selection: ChatGPT Pro Model
question_summary: determine whether exact permission P reaches and leaves the existing test proxy before introducing any deeper TUI event-decoding observation
pro_conclusion: first inspect the existing /global/event pass-through; only an already-exposed single reader/writer may receive a rolling byte matcher that preserves chunk identity, order, and payload bytes
macbook_air_ruling: if the proxy returns the upstream Response directly and has no lossless reader/writer seam, stop after recording the source fact; do not introduce a transform, second subscription, payload decode/re-encode, delay, product change, rerun, or 0.34.8 work
ruling_scope: read-only 0.34.7 PTY proxy audit and existing Trellis evidence only
head_sha_and_relevant_delta_at_ruling: 326ec66be078d3a757cdf8011986b4814015cca2; branch, upstream, and draft-PR head aligned; isolated worktree clean before evidence-only edits
ci_evidence: run 30648248714 passed 80 columns and failed 140 columns at grandchild-permission-in-main/140 with EVENT_PROPAGATION_HYPOTHESIS_DISPROVEN; P=perm_019fb910184c7eb3b4b756e7d6915dcc; no matching reply request; P remained pending; no rerun authorized
final_disposition_and_resumption_decision: the source stop condition fired; no RED, matcher, typecheck, focused PTY run, commit, push, CI, or product change is authorized until the MacBook Air coordinator supplies a new bounded ruling
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| The current fixture exposes the `/global/event` upstream reader and downstream writer | `rejected`, `source-verified` | `pty-smoke.test.ts` forwards unmatched routes with `response = await fetch(...)` followed by `return response`. `/global/event` has no special branch, `response.body` access, `getReader()`, `ReadableStream`, downstream writer, or `enqueue` seam. The only explicit reader in this fixture is the unrelated backend-process stdout readiness reader. |
| A rolling byte matcher can be added without changing the pass-through architecture | `rejected`, `source-verified` | Observing chunks would require consuming `response.body` and returning a replacement body stream. That would create the reader/writer transform that the ruling says not to invent, so the requested chunk-split RED is inapplicable at this HEAD. |
| The locked permission result proves the backend did not emit P | `rejected` | `permission.list` retaining P proves committed permission state. Because the direct pass-through has no observation seam, this run cannot distinguish backend emission, upstream subscription/cursor/filter behavior, or downstream/TUI consumption. |
| The existing remote failure is reproducible evidence | `experimentally-verified` | The sole run `30648248714` produced `EVENT_PROPAGATION_HYPOTHESIS_DISPROVEN` with no reply and P still pending. It is evidence for the missing transcript observation only, not evidence that P was absent from the backend SSE producer. |
| SSE chunk tracing or another focused run may proceed now | `pending` | A new coordinator ruling must first choose a source-evidenced observation seam. The next bounded audit may inspect backend-to-proxy subscription establishment, cursor/replay, and filters without changing TUI behavior; this task does not infer that design. |

### `CONSULT-2026-07-31-PENDING-SNAPSHOT-CATCHUP-15`

```text
consultation_id: CONSULT-2026-07-31-PENDING-SNAPSHOT-CATCHUP-15
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: choose the smallest source-owned recovery seam for permission/question asks committed before a live /global/event subscription or lost to broadcast lag
pro_conclusion: subscribe first, snapshot current pending asks without holding locks across stream construction, then emit connected, snapshot asks, and live events with stable-ID at-least-once semantics
macbook_air_ruling: adopt backend pending snapshot catch-up for permission and question; reject TUI bootstrap permission-list loading and a PTY-only connected wait as the root repair
ruling_scope: 0.34.7 CI repair only
permitted_next_action: strict server integration RED/GREEN for initial catch-up and lag catch-up, unchanged TUI behavior, full release gates, one atomic commit/push, and one fresh-SHA CI run
forbidden_next_action: cursor/event-log redesign, server dedup, second SSE subscription, TransformStream, TUI bootstrap change, new public Event/API, dependency/workflow/version/changelog change, retry/wait inflation, or 0.34.8
required_verification: pending permission and question appear after connected and before a live sentinel; lagged current pending IDs use the same snapshot helper; resolved requests never replay; focused PTY/TUI and complete Rust/executable/zero-INET gates remain green
head_sha_and_relevant_delta_at_ruling: 326ec66be078d3a757cdf8011986b4814015cca2; only this ledger and implement.md were dirty
follow_up_of: CONSULT-2026-07-31-PTY-SSE-PASSTHROUGH-14
final_disposition_and_resumption_decision: implement the bounded server catch-up once in the existing session/task/worktree/branch/PR; keep 0.34.8 blocked until the new SHA is fully green
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| Pending state can outlive a missed live broadcast | `Pro-advised`, `coordinator-adopted`, `source-verified` | `PermissionRequests` and `QuestionRequests` insert into their pending maps before a best-effort broadcast; `/global/event` has no pending snapshot or replay on the ruling HEAD. |
| Catch up from the backend pending owner | `Pro-advised`, `coordinator-adopted`, `experimentally-verified` | The first RED listed a stable pending permission with no SSE subscriber, connected `/global/event`, then received a live question sentinel before that permission, proving the missing behavior at the approved server seam. |
| TUI bootstrap list or PTY connected wait is the root repair | `rejected` | Those approaches either create a second synchronization path or only narrow test timing; neither repairs live subscribers that lag after connection. |
| Delivery is exactly once | `rejected` | Subscribe-before-snapshot intentionally permits snapshot-plus-live duplication. Stable request IDs make the contract at-least-once; the server does not deduplicate. |

Implementation disposition after strict TDD:

- `source-verified`: only `/global/event` is changed. It subscribes first,
  clones current permission/question typed views under their respective short
  mutex guards, releases the guards before JSON serialization, then emits
  connected, snapshot asks, and the unchanged live streams.
- `experimentally-verified`: pending-permission and pending-question tests each
  failed on the ruling HEAD when the live opposite-type sentinel arrived
  first, then passed with the bounded catch-up.
- `experimentally-verified`: capacity-one permission and question streams each
  failed because the first still-pending stable ID was absent after lag, then
  passed after their existing `/global/event` lag branch invoked the same
  snapshot owner. Tests use bounded scans and make no snapshot-order claim.
- `source-verified`: capacity injection is `#[cfg(test)]`; production exposes
  no new crate-visible control API. There is no sorting, server deduplication,
  cursor, event-log replay, second subscription, generic SSE helper, TUI
  bootstrap, dependency, version, changelog, or other SSE-route change.
- `experimentally-verified`: `cargo fmt --all --check` and the focused Compat
  permission/question integration suite pass (8/8). The broader hya-server
  sweep passed all reached relevant tests and stopped only at a sandbox-denied
  PTY allocation; that exact PTY test passed outside the sandbox.
- `pending`: coordinator diff-boundary review, full release gates, one atomic
  commit/push, and exactly one fresh-SHA CI run. 0.34.8 remains blocked.

### `CONSULT-2026-07-31-PENDING-SNAPSHOT-SIMPLIFICATION-16`

```text
consultation_id: CONSULT-2026-07-31-PENDING-SNAPSHOT-SIMPLIFICATION-16
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: reduce the 771-line pending catch-up diff without weakening deterministic initial-subscription or lag recovery coverage
pro_conclusion: retain real HTTP initial catch-up tests; replace the full capacity-injected ServerState lag fixture with one private lazy recovery helper tested through a real capacity-one tokio broadcast
macbook_air_ruling: adopt option A; delete all production capacity seams and the 216-line fixture, keep fixed capacity 256, add overall timeouts to the two HTTP loops, and compact duplicate Trellis evidence
ruling_scope: 0.34.7 pending snapshot catch-up simplification only
permitted_next_action: honest mutation checkpoint against Lagged-to-empty, minimal helper GREEN, focused tests, compact evidence, and coordinator diff review
forbidden_next_action: 257-event fixture, public API/configuration, generic SSE framework, other SSE/TUI/Event changes, dependency/version/changelog change, full gates before review, or 0.34.8
follow_up_of: CONSULT-2026-07-31-PENDING-SNAPSHOT-CATCHUP-15
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| Full ServerState lag fixture is required | `rejected`, `source-verified` | The lag contract is isolated to one `Result<Value, BroadcastStreamRecvError>` decision; app/session construction did not contribute to the invariant. |
| Production needs configurable broadcast capacity | `rejected` | Capacity injection existed only to force the deleted tests. Production `new`/`spawn` is restored to the direct fixed capacity of 256. |
| Private lazy helper preserves semantics | `experimentally-verified` | A real capacity-one broadcast mutation checkpoint failed with `left: []`, `right: [P1]`; GREEN snapshots once on Lagged and zero times for `Ok(P2)`. |
| User-visible initial catch-up remains covered | `experimentally-verified` | Both HTTP/SSE tests remain and the full Compat permission/question integration target passes 8/8 with bounded whole-loop diagnostics. |
| Cross-document evidence should remain duplicated | `rejected` | `implement.md` now keeps only a compact Consult14–16 execution index; complete rulings remain in this ledger. |
| Release completion | `pending` | Narrow fmt/diff checks pass; coordinator review precedes full release gates, staging, commit/push, and one fresh-SHA CI run. |

### `CONSULT-2026-07-31-PTY-SANDBOX-PROBE-17`

```text
consultation_id: CONSULT-2026-07-31-PTY-SANDBOX-PROBE-17
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: distinguish a sandbox-denied hya-backend loopback bind from a PTY product/startup regression without editing or rerunning the fixture blindly
pro_conclusion: run one exact backend-start probe with the fixture binary, environment, cwd and arguments; only explicit bind EPERM permits one non-sandbox focused run
macbook_air_ruling: choose the bounded probe option; reject assuming infrastructure failure without stderr and reject changing pty-smoke diagnostics
ruling_scope: 0.34.7 release-gate environment classification only
forbidden_next_action: repository edit, retry, sleep, PTY helper change, additional path, or 0.34.8
```

Evidence and disposition:

- `artifact-verified`: `target/debug/hya-backend` retained SHA-256
  `746e85099073f5f621857156ac0bb537aad641a5621ce15f7df10a9fe855f051`.
  The exact fixture-equivalent probe exited code 1 with no signal and empty
  stdout; stderr reported `bind 127.0.0.1:0` caused by
  `Operation not permitted (os error 1)`. Its dedicated temporary directory
  was inspected, removed, and confirmed absent.
- `experimentally-verified`: the earlier restricted focused failures were
  sandbox infrastructure failures. The one authorized non-sandbox focused
  run reached the product interaction flow instead of failing backend startup.
- `pending`: that run then exposed the independent 80-column
  `CONFIRM_MAIN_MARKER_MISSING` behavior, so Consult17 did not close the
  release gate and authorized no retry or product fix by itself.

### `CONSULT-2026-07-31-ESCAPE-MAIN-PROMPT-18`

```text
consultation_id: CONSULT-2026-07-31-ESCAPE-MAIN-PROMPT-18
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: close the observation Escape-to-Main input-ownership gap exposed by the unique 80-column marker failure
pro_conclusion: after dispatching focusMain, synchronously focus the existing Main Prompt ref only when it exists and no modal owns input; retain the reactive effect as fallback
macbook_air_ruling: choose product option A; reject a transcript/UI barrier and test-only tracing; preserve key order, chords, layout and the unchanged PTY regression
ruling_scope: 0.34.7 Escape-to-Main Prompt ownership only
permitted_next_action: one package-internal callback RED/GREEN, TUI typecheck/build, one non-sandbox focused PTY run, then one full TUI run with no intervening change
forbidden_next_action: public API/framework, new dependency, PTY/sleep/timeout/retry/marker change, backend/Event/permission/SSE change, version/changelog change, or 0.34.8
```

Determination-level evidence:

| Determination | Status | Independent disposition |
| --- | --- | --- |
| The callback must order workspace dispatch before Prompt focus | `source-verified`, `experimentally-verified` | RED compiled and failed with expected `[focusMain, focus, return]` versus actual `[focusMain, return]`; GREEN passes after adding only the guarded synchronous focus. |
| Missing Prompt ref or active modal may be focused | `rejected`, `experimentally-verified` | Both unit cases pass with dispatch only and no focus call. |
| A UI barrier or test-only trace is the selected repair | `rejected` | The coordinator selected A and explicitly forbade B/C. No such code was added. |
| The existing PTY fixture needs modification | `rejected`, `source-verified` | `pty-smoke.test.ts` remains byte-identical to HEAD; no wait, key, marker, timeout, retry, or assertion changed. |

TDD and gate evidence:

- RED: `bun test test/subagent-workspace.test.ts` produced 26 pass / 1 fail;
  the sole failure omitted `focus` between `focusMain` and `return`, while the
  absent-ref and modal-active cases passed.
- GREEN: the same target passed 27/27; `bun run typecheck` and
  `bun run build` passed.
- `experimentally-verified`: the single authorized non-sandbox focused PTY
  command passed 4/4 (80 columns 14.956 seconds; 140 columns 12.041 seconds).
  With no intervening edit, the single full `CI=true bun test` run passed
  47/47, including the three new focus contracts (80 columns 15.073 seconds;
  140 columns 12.326 seconds).
- `pending`: coordinator boundary review, exact staging, commit/push, and one
  fresh-SHA full CI run. Release remains 0.34.7 and 0.34.8 remains blocked.

### `CONSULT-2026-07-31-NATIVE-BUNDLE-CUTOVER-19`

```text
consultation_id: CONSULT-2026-07-31-NATIVE-BUNDLE-CUTOVER-19
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: close the native-only 0.34.8 built-in Bundle preparation/cutover plan after the 0.34.3-0.34.7 runtime prerequisites became remote-green
pro_conclusion: use one deterministic build-time prepared built-in catalog, retain stable agent identities and historical replay bytes, and cut runtime resolution over atomically without a legacy loader or second runtime
macbook_air_ruling: adopt the native-only two-commit plan with the corrections below; omitted agent alone selects general, explicit unknown IDs fail typed, role controls only TUI visibility, can_spawn alone controls reachability, and harness_access is separate from the narrowing resource_view
ruling_scope: release 0.34.8 only; commit 1 prepares but does not activate native built-ins, and commit 2 performs the one-authority runtime cutover and legacy deletion
permitted_next_action: implement exactly the two authorized atomic commits with strict RED/GREEN, full local gates, one push and green remote CI after each commit
forbidden_next_action: installed-package registry or empty installed slice, .hyabundle/CLI/7z/private envelope, JS/Rust runner, external Bundle execution, dynamic watcher, compatibility adapter/scanner/migration, dual catalog/runtime authority, new permission framework, sandbox/crypto/license work, 0.34.9+ API, or mutation of protected main/stashes
follow_up_of: CONSULT-2026-07-31-NATIVE-BUNDLE-BOOTSTRAP-05
final_disposition_and_resumption_decision: 0.34.8 is authorized on the existing canonical session/task/worktree/branch/PR after the green 0.34.7 tip 064ede0b4fe4601b84ccbe912c75980449527d2c
```

Determination-level evidence and coordinator corrections:

| Determination | Status | Required independent disposition |
| --- | --- | --- |
| Omitted agent resolves to stable ID `general`; an explicitly supplied unknown ID returns typed `UNKNOWN_AGENT_ID` | `Pro-advised`, coordinator-adopted; source characterization pending | RED/GREEN the explicit/omitted split. Preserve historical `AgentName` bytes; only continue operations that require a missing definition return `AGENT_DEFINITION_MISSING`. |
| The seven product agents and eight tracked development agents become two repo-native, immutable built-in Bundles | `Pro-advised`, coordinator-adopted; source verification pending | Freeze exact HEAD fields and behavior before preparation, then prove prepared sources match those fixtures and delete `.hya/agents` only in the cutover commit. |
| `crates/hya-bundle` solely owns v1 parsing, flat IR, validation, canonical bytes/digest/index, and prepared-catalog decode | `Pro-advised`, coordinator-adopted; TDD pending | Keep it dependency-light and free of core/app/server/runtime authority; app build-time preparation and runtime prepared decoding use the same canonical format. |
| `role` controls spawn permission or agent-facing roster visibility | `rejected` | `role` filters only the TUI selector. `can_spawn` is the sole catalog reachability edge; internal/model-facing rosters retain eligible subagents. |
| `none|basic|full` is a role or authorization policy | `rejected` | `harness_access` selects the Harness candidate set, then `resource_view` deterministically narrows/aliases/namespaces it; the existing PermissionPlane/plugin decisions remain final and cannot be broadened by a Bundle. |
| Runtime scans examples, ordinary Markdown, or old agent files | `rejected` | Runtime consumes embedded prepared bytes only. The preparer recognizes only exact `bundle.hya.md` inputs carrying both v1 frontmatter markers; the example claims prepare-validity, not installability or runtime discovery. |
| Executable JS/Rust/tool/MCP references without a current consumer may be accepted and ignored | `rejected` | Metadata may be prepared only where explicitly supported; an executable feature lacking a current consumer returns typed `UNSUPPORTED_BUNDLE_FEATURE`. Runner work remains 0.34.10. |
| One release implementation commit is sufficient | `rejected` by coordinator risk correction | Commit 1 prepares inert native sources and must pass remote CI before commit 2 activates the catalog, deletes old loaders, bumps 0.34.8, and ships docs/example/skill. There is no third or fourth commit. |

Consult19 is advisory provenance, not owner/security approval and not a
substitute for HEAD characterization, deterministic RED/GREEN evidence,
PermissionPlane verification, full gates, or remote CI. The MacBook Air
coordinator supplied the controlling corrections and routine implementation
authorization; every delivered claim remains `pending` until verified on the
fuji1 remote worker.

### `CONSULT-2026-07-31-RESERVED-SYSTEM-AGENTS-20`

```text
consultation_id: CONSULT-2026-07-31-RESERVED-SYSTEM-AGENTS-20
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: map the effective hidden native compaction/title/summary definitions into the role-only Bundle schema without exposing accidental ordinary spawn reachability
pro_conclusion: classify all three as subagent-role catalog entries but reserve their reachability to fixed Harness system operations; exclude them from every ordinary can_spawn graph
macbook_air_ruling: adopt option 1; ordinary explicit spawn returns typed AGENT_SPAWN_NOT_ALLOWED with no child and no general fallback, while existing fixed system callsites resolve the exact stable ID from the current TurnBinding catalog
ruling_scope: release 0.34.8 built-in source mapping and native cutover only
follow_up_of: CONSULT-2026-07-31-NATIVE-BUNDLE-CUTOVER-19
```

Determination-level evidence and controlling disposition:

| Determination | Status | Required independent disposition |
| --- | --- | --- |
| Preserve a second `hidden` field in Bundle IR | `rejected` | `role=subagent` is the sole selector-visibility mapping for `compaction`, `title`, and `summary`; no hidden boolean survives preparation. |
| Ordinary agents may spawn the three system definitions | `rejected`, coordinator-adopted security/correctness tightening | Exclude all three stable IDs from every ordinary `can_spawn`; Commit 2 returns typed `AGENT_SPAWN_NOT_ALLOWED`, creates no child, and never falls back to `general`. |
| Agent-facing roster/listing is role-filtered | `rejected` | It is the caller's `can_spawn`-reachable set. The three reserved definitions remain absent because they are unreachable, not because their role grants or denies spawning. |
| Harness needs a generic unchecked spawn API | `rejected` | Only the existing compaction/title/summary system callsites may resolve their compile-time fixed stable ID directly from the same TurnBinding snapshot. This is not agent spawn and exposes no arbitrary-ID bypass. |
| Historical identity must be rewritten | `rejected` | Event/projection `AgentName` bytes remain unchanged. Resume/continue resolves the same stable ID from the current snapshot without a caller `can_spawn` check; a missing definition remains typed `AGENT_DEFINITION_MISSING`. |

Commit 1 may now prepare the two native built-in source manifests with this
mapping. Runtime rejection, selector/eligible-roster behavior, fixed system
lookups, and replay/resume proofs remain Commit 2 RED/GREEN work. This ruling
supersedes only the Consult19 ambiguity; all other Consult19 constraints remain
in force.

### `CONSULT-2026-07-31-RUNTIME-CATALOG-AUTHORITY-21`

```text
consultation_id: CONSULT-2026-07-31-RUNTIME-CATALOG-AUTHORITY-21
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: choose the single runtime ownership and construction seam for the activating native Bundle cutover
pro_conclusion: construct RuntimeRegistry with one required Arc<BundleCatalog>, retain it in every immutable RuntimeSnapshot and TurnBinding, and remove every SessionEngine catalog/resolver authority
macbook_air_ruling: adopt option A only; production decodes the embedded prepared catalog once at app bootstrap, and tests construct explicit in-memory prepared catalogs through the production BundleCatalog constructor
ruling_scope: release 0.34.8 atomic commit 2 only
follow_up_of: CONSULT-2026-07-31-NATIVE-BUNDLE-CUTOVER-19 and CONSULT-2026-07-31-RESERVED-SYSTEM-AGENTS-20
```

Controlling disposition:

- `RuntimeRegistry` requires the catalog at construction and every complete
  tool/skill/MCP publication carries forward the unchanged `Arc`; there is no
  catalog replacement API in `0.34.8`.
- `SessionEngine`, tools, server, and TUI may consume definitions or minimal
  views from the current `TurnBinding`, but none may own a resolver, closure,
  cache, fallback catalog, or independently embedded copy.
- Empty/later-injected catalogs, overloaded constructors, test runtime
  fallbacks, synthesized `AgentSpec`, and missing-definition fallback to
  `general` are `rejected`. Tests use crate-local fixtures that exercise the
  production `PreparedBundle -> BundleCatalog::from_prepared -> Arc` path.
- Read-only event/projection replay remains catalog-independent. Any operation
  that continues execution resolves its stable `AgentName` through the new
  binding and fails typed `AGENT_DEFINITION_MISSING` when absent.
- Required Commit 2 evidence remains `pending`: construction-time ownership,
  binding pinning across a tool generation, omitted-versus-explicit resolution,
  caller `can_spawn` reachability, fixed system lookups, and invalid embedded
  catalog bootstrap failure without fallback.

### `CONSULT-2026-07-31-INLINE-AGENT-OVERLAY-22`

```text
consultation_id: CONSULT-2026-07-31-INLINE-AGENT-OVERLAY-22
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: preserve the effective one-spawn inline-agent behavior without creating a reusable definition or a second catalog authority
pro_conclusion: authorize the catalog base target first, then apply a child-scoped overlay to a clone of that bound AgentSpec
macbook_air_ruling: adopt option A with the verified HEAD model/category precedence and fail-closed unknown, can_spawn, restart, and ignored-description semantics below
ruling_scope: release 0.34.8 atomic commit 2 inline spawn resolution only
follow_up_of: CONSULT-2026-07-31-RUNTIME-CATALOG-AUTHORITY-21
```

Controlling disposition:

- Resolve the base target from the current `TurnBinding` first: omission alone
  selects `general`; an explicit unknown ID is `UNKNOWN_AGENT_ID`; the caller's
  `can_spawn` edge is required before admission, child creation, or events.
- Only after authorization may one explicit inline request clone that bound
  `AgentSpec` and overlay it for one child execution. It creates no Bundle,
  catalog/snapshot entry, namespace row, roster/TUI/list row, or reusable
  definition, and it never mutates the catalog.
- Preserve the source-verified precedence: spawn model, spawn category, inline
  model, Bundle model, inline category, Bundle category, then base model.
  Non-empty inline prompt replaces the bound prompt; non-empty inline name is
  only the child's public event/session identity. Resident is the OR of Bundle
  lifecycle, inline resident, and spawn-time resident.
- Replay remains identity-only. An in-process resident may retain its overlay;
  process-loss recovery cannot reconstruct it and returns
  `AGENT_DEFINITION_MISSING` when the recorded name has no catalog definition.
  A coincident same-name definition does not prove restoration of the lost
  inline prompt/model/category.
- Inline `description` is `B_PARSED_IGNORED` on current HEAD and must gain an
  honest existing consumer or be typed-rejected; silent acceptance is
  forbidden. No persistence, public API/event, catalog overlay, or external
  runner is authorized.

This ruling releases only the paused inline branch. Source verification,
RED/GREEN evidence, full local gates, and remote CI remain mandatory.

### `CONSULT-2026-07-31-COMPILED-RESOURCE-VIEW-23`

```text
consultation_id: CONSULT-2026-07-31-COMPILED-RESOURCE-VIEW-23
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: fix the canonical allow/deny/alias/namespace order for the immutable per-turn AgentBundle resource view
pro_conclusion: select bundle-local plus the harness_access baseline per resource kind, filter by canonical identity with deny winning, then assign collision-checked public names used identically by schema/prompt and dispatch
macbook_air_ruling: adopt the algorithm with one performance correction: compile once only for the actual bound agent from its retained TurnBinding, not eagerly for every agent on each RuntimeSnapshot publication
ruling_scope: release 0.34.8 atomic commit 2 resource visibility and routing only
follow_up_of: CONSULT-2026-07-31-NATIVE-BUNDLE-CUTOVER-19
```

Controlling disposition:

- Bundle-local resources are candidates independently of `harness_access`;
  `none` adds no Harness resources, `basic` adds only built-in-origin resources,
  and `full` adds the complete current bound Harness view.
- `allow` and `deny` resolve to canonical identities before naming; non-empty
  allow is a whitelist and deny wins. Missing, cross-kind, ambiguous, filtered,
  or access-ineligible references fail typed, and aliases cannot bypass this
  filtering.
- Namespace changes only bundle-local qualified spelling. Bundle-local wins a
  short-name collision, while the Harness resource remains available under its
  Harness-qualified spelling; removing the local candidate restores the
  Harness short name. Qualified stable identities cannot be overwritten.
- One `Arc<CompiledResourceView>` is built per actual turn/child from its
  retained `TurnBinding` and bound definition. Prompt/schema visibility, skill
  exposure, and dispatch consume that same map; dispatch has no global registry
  fallback and existing PermissionPlane/plugin authorization remains final.
- The ruling authorizes static skill/descriptor fixtures only. It does not add
  a bundle-local executable runner, parallel permission plane, global view
  cache, or any `0.34.9+` distribution behavior.

### `CONSULT-2026-07-31-REFERENCE-GUIDANCE-COMPOSITION-24`

```text
consultation_id: CONSULT-2026-07-31-REFERENCE-GUIDANCE-COMPOSITION-24
received_by_fuji1_date_utc: 2026-07-31
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: compose Bundle prompt replacement with project/reference guidance without a second authority, AgentSpec overlay, wire API, or per-round discovery
pro_conclusion: advisory only — keep guidance as a separate pre-rendered layer composed once with agent_base after exact Bundle resolution
macbook_air_ruling: adopt the composition and transport rules below; Pro remains advisory and the MacBook Air coordinator ruling is controlling
ruling_scope: release 0.34.8 atomic commit 2 reference/project guidance composition only
follow_up_of: CONSULT-2026-07-31-RUNTIME-CATALOG-AUTHORITY-21 and CONSULT-2026-07-31-NATIVE-BUNDLE-CUTOVER-19
```

Controlling disposition:

- `hya-core` uses the same real-turn `TurnBinding` and exact-resolves the stable
  `AgentName` once. The single prompt builder composes `agent_base` plus
  nonempty guidance once. Bundle prompt `Some` replaces only `agent_base`;
  Bundle prompt `None` preserves the existing Harness base/system prompt.
  Harness-owned guidance is never deleted.
- `hya-server` uses existing discovery, sorting, and separators to pre-render
  project `AGENTS.md` plus reference guidance and passes a request-scoped
  immutable `Option<Arc<str>>` (or existing equivalent) to core. No
  `AgentSpec` guidance overlay, catalog or snapshot storage, paths/parser/raw
  files, wire API, proto, `Event`, or persistence.
- User/task input remains a separate user message. Guidance is captured once
  per actual turn or activation and reused across provider rounds, with no
  per-round filesystem discovery.
- Root, resume, and fork-continue rediscover current project guidance and
  exact-resolve the current Bundle definition; missing remains
  `AGENT_DEFINITION_MISSING`. Read-only replay does neither lookup nor
  guidance discovery.
- Transient/resident semantics must first be characterized; prefer passing
  triggering-turn immutable guidance through the existing child activation,
  with no child scan or permanent resident definition storage, unless direct
  HEAD evidence proves current child-workdir discovery.
- Fixed title, summary, and compaction agents do not gain project/reference
  guidance unless characterization proves current behavior already does;
  their fixed Bundle prompt remains exact and isolated.
- No new prompt error; absent guidance is an empty layer. Non-goals include
  manifest or IR changes, a second composer or authority, persisted effective
  prompt, new Event/API, legacy loader, per-round discovery,
  sandbox/permission changes, or later-release API.
- Required behavioral tests: Bundle prompt replaces base while preserving
  guidance once and in order; Bundle prompt `None` preserves Harness base then
  guidance; user input stays a separate provider message; guidance captured
  once across rounds; transient/resident immutable semantics without
  `AgentSpec` overlay; fixed-system characterization; reuse replay no-lookup
  and continue missing-definition tests.

Current RED checkpoint (source-verified / experimentally verified):

- `crates/hya-server/tests/compat_reference_guidance_api.rs` was migrated to
  explicit `PreparedBundle` → `BundleCatalog` → `RuntimeRegistry` fixtures and
  added `compat_prompt_bundle_prompt_and_reference_guidance_parity`.
- Independent command
  `cargo test -p hya-server --test compat_reference_guidance_api
  compat_prompt_bundle_prompt_and_reference_guidance_parity -- --nocapture`
  failed behaviorally as expected: Bundle marker exactly once,
  `available_references` count 0. Full binary reported 4 passed and 1 expected
  failed.
- Source-verified cause: server currently concatenates guidance into
  `AgentSpec.system_prompt` before the turn, then exact Bundle prompt
  replacement overwrites it; skills appended later survive.
- Legacy disk prompt expectation was removed from that test; no GREEN has been
  implemented yet. Further prompt-composition, child/resident, fixed-system,
  replay/continue GREEN and gates remain pending.

### `CONSULT-2026-08-01-HYABUNDLE-DISTRIBUTION-25`

```text
consultation_id: CONSULT-2026-08-01-HYABUNDLE-DISTRIBUTION-25
received_by_fuji1_date_utc: 2026-08-01
safe_canonical_session_url: https://chatgpt.com/c/6a6bd036-10a4-83eb-8a05-a7cfcb31dc7e
displayed_model_label_exact: Pro
question_summary: define the 0.34.9 public/private .hyabundle inspector, safe staging, static-package activation, SQLite registry, lazy runtime publication, and exact CLI boundary
provenance: MacBook Air coordinator submitted the packet through the in-app Browser; Pro output is advisory evidence and the coordinator corrections below are controlling
execution_model: gpt-5.6-luna reasoning=max implementation turns edit files; the MacBook Air coordinator / canonical reviewer orchestrates, reviews, verifies, commits, pushes, and monitors CI
supersedes_current_rule: any current Grok-only implementation wording; historical Grok 402 quota failures remain historical evidence and are not erased
ruling_scope: 0.34.9 package distribution and registry only; no 0.34.10 runner or private activation
```

Controlling MacBook Air corrections:

1. Public `.hyabundle` is an unencrypted standard 7z archive identified by
   magic; private `.hyabundle` is an opaque `HYABNDL\0` v1 envelope. `install`
   and `info -f` require the exact lowercase suffix, but format detection uses
   only magic/version.
2. A public archive has exactly one root `bundle.hya.md` and otherwise only
   regular files/directories accepted by the existing v1 source contract.
   Reuse the existing source/preparer; do not add a second manifest, DTO, or
   loader.
3. Pure static public agents, prompts, skills, and resources may be prepared,
   published into the single `BundleCatalog`, and become callable by new
   turns. JS/Rust runners and executable tool/MCP/hook references are typed
   unsupported and must not spawn, open network, register plugins, or register
   MCP clients.
4. Private v1 performs only structural header/length/target/protocol/nonce/
   tag-layout/ciphertext-digest checks. `info -f` must state
   `authentication: unverified`, `payload: opaque`, and
   `activation: unsupported-in-0.34.9`; install returns
   `PRIVATE_ACTIVATION_UNSUPPORTED` without persistence or registry mutation.
   No signature, key service, decrypt, licensing, or attacker-authenticity
   claim is implemented.
5. **Superseding public-7z dependency resolution (Consult25 evidence).**
   sevenz-rust 0.6.1 is **NO-GO**. sevenz-rust2 0.21.3 is also **NO-GO**:
   its official Cargo.toml declares rust-version = 1.93, above workspace Rust
   1.91, which must not be raised. The sole conditional candidate is
   sevenz-rust2 = "=0.20.2" with default-features = false; its official
   Cargo.toml declares Rust 1.85, but it is usable only behind Hya's strict
   adapter or a minimal local patch. The stock high-level filesystem extractor
   is insufficient. Coordinator official evidence is the
   [0.20.2 source](https://docs.rs/crate/sevenz-rust2/0.20.2/source/) and
   [0.20.2 API](https://docs.rs/sevenz-rust2/0.20.2/sevenz_rust2/).
   Verified gaps are archive/FilesInfo unknown-property skipping, unbounded
   decode-memory/header allocation, no first-class special-entry type, and CRC
   only after declared remaining reaches zero. The strict adapter must use
   Password::empty, never the stock filesystem extractor, reject nonempty or
   unknown properties plus solid/encrypted/multivolume/special/anti/directory/
   extra entries, accept exactly root bundle.hya.md, enforce configured caps
   for raw bytes/header/entries/blocks/coders/LZMA memory/declared bytes/actual
   bytes/ratio, set thread = 1, whitelist only fixture-needed COPY/LZMA/LZMA2,
   require stream CRC and independently require actual equals declared plus
   callback count one before accepting output, and let Hya own newly created
   staging and all writes. If public APIs cannot express the property-skip or
   memory fixes, an Apache-2.0-noticed local patched 0.20.2 copy is allowed;
   no new parser or relaxed contract is allowed.
6. Fixed limits are archive 128 MiB, 4096 entries, one regular file 64 MiB,
   expanded sum 256 MiB, path bytes 1024, and depth 32. Reject absolute,
   drive, UNC, backslash, NUL, dot, dotdot, empty-segment, duplicate,
   case-collision, and non-UTF-8 paths; use `create_new`, open the input once,
   copy it to unique mode-0700 staging, and inspect/extract only that copy.
7. The sole installed state source is
   `<data_root>/bundles/registry.sqlite3`; builtins are not rows. Store one
   singleton generation and one active public prepared Bundle BLOB row per
   bundle ID with identity, source/prepared digests, and `installed_at`.
8. Stage/inspect/prepare outside the writer lock. In a short `BEGIN IMMEDIATE`
   with `synchronous=FULL`, reread generation and rows, build and validate the
   complete embedded-plus-installed catalog, replace the row, and increment
   generation atomically. Busy is typed immediately without retry; failures
   preserve old rows/bytes/generation; staging cleanup is conservative.
9. Same ID/source digest is idempotent with no generation advance. Same ID and
   version with a different digest is a content conflict. A different version
   is an explicit atomic replacement; uninstall increments generation. Builtins
   are reserved immutable and all full-catalog identity/reference collisions
   fail closed.
10. A narrow lazy seam checks DB generation only before a new root TurnBinding
    and existing TUI/agent catalog refresh. A changed generation reads all
    prepared BLOBs, validates the complete catalog, atomically publishes it,
    then binds. No provider-round, dispatch, or child hot-path DB query;
    children and old TurnBindings retain their pinned Arcs. No BUNDLE_IN_USE
    tracker is added in this release.
11. CLI is exactly `install`, `list`, `uninstall`, `info`, and `info -f`, with
    deterministic fields. `info -f` never writes or publishes; private info is
    metadata-only.
12. Reuse source/prepared/catalog typed errors and add only the smallest
    package/registry codes required by this behavior. No duplicated error stack,
    sandbox, new permission plane, legacy/compat/migration support,
    marketplace/remote registry, watcher/control API, or 0.34.10 placeholder.

The first RED is intentionally bytes-only: construct `HYABNDL\0` followed by
little-endian envelope version 1 and assert the future detector returns
private/v1 without receiving a filename or path. No product detector is added
until the RED is observed. Historical Grok quota failures remain recorded as
external execution evidence; they do not authorize a model substitution.

Execution-model supersession after this Consult25 record: Luna completed the
first private/v1 RED/GREEN as historical implementation evidence. The user
then selected `gpt-5.6-terra` with `reasoning=max` as the current implementation
model and `gpt-5.6-sol` with `reasoning=max` as the current canonical reviewer.

Current-rule supersession: `gpt-5.6-sol` with `reasoning=max` is the persistent
main orchestrator/reviewer and directly spawns one bounded
`gpt-5.6-terra` `reasoning=max` native implementation subagent per edit slice.
Terra edits only its assigned slice and never commits, pushes, creates topology,
or stashes; Sol defines REDs, reviews diffs, runs verification, and performs
commits, pushes, and CI monitoring after gates. Earlier Grok 402 failures, Luna
work, and MacBook Air-driven per-turn Sol/Terra alternation remain historical
evidence, but are superseded as current rules.

#### Consult25 dependency source-sync and bounded audit evidence

**Authoritative dependency correction.** `sevenz-rust` 0.6.1 is **NO-GO**;
`sevenz-rust2` 0.21.3 is **NO-GO** because its Rust 1.93 MSRV exceeds the
workspace. Exact `sevenz-rust2` 0.20.2 with `default-features = false` is
**GO** through the minimal Apache-noticed local strict patch: official commit,
checksum, and Rust 1.85 MSRV were verified.

The existing single bounded acquisition at
`/tmp/hya-sevenz-rust2-0202.4SdiMp` was reused and verified at detached
[424ebdb8fa98b78b8e1c18f73c9add6972fe5496](https://github.com/hasenbanck/sevenz-rust2/commit/424ebdb8fa98b78b8e1c18f73c9add6972fe5496)
(Update dependencies and release v0.20.2); Cargo version 0.20.2 and
rust-version 1.85 were verified and the canonical worktree was untouched by
the acquisition; no second fetch was issued. Coordinator evidence records that
the Mac artifact SHA-256
29225600349ef74beda5a9fffb36ac660a24613c0bde9315d0c49be1d51e9c24 matches
the official crates.io checksum and that .cargo_vcs_info points to that commit.

The source audit confirms a wrapper suffices for Hya-owned reader/staging,
metadata/coder policy, thread = 1, and exact byte/CRC/callback counting, but
cannot prevent reader.rs unknown-property skips, raw/encoded-header allocation,
or internal unbounded memory. The actual functional patch is limited to
`reader.rs`, `decoder.rs`, `error.rs`, and `lib.rs`: strict options, typed
structural/memory-limit propagation, re-export, and exact EOF. `archive.rs` is
byte-identical upstream; there is no new parser or filesystem extractor.

A direct standalone vendor test later hit crates.io DNS for uncached upstream
dev/default dependencies and was not retried. The workspace
`default-features = false` path compiles, tests, and passes Clippy.

#### Authoritative expansion-ratio resolution

**Controlling 0.34.9 ruling:** `MAX_EXPANSION_RATIO = 1000`. The denominator is
the checked sum of the distinct PackInfo stream sizes referenced by the
accepted file's block. The exact comparison is
`(expanded_bytes as u128) <= (packed_bytes as u128) * 1000_u128`. With a zero
denominator, `packed=0, expanded=0` accepts and `packed=0, expanded>0` rejects;
`expanded=0, packed>0` accepts by the same formula.

- **Source-verified:** the accepted file-to-block mapping and that block's
  PackInfo boundaries determine the denominator. The rule runs at metadata
  preflight and against cumulative actual decoded bytes before retaining each
  chunk. Archive length, `entry.compressed_size`, and configuration are not
  ratio inputs.
- **Experimentally verified:** the scalar boundary/zero contract completed
  RED→GREEN; the deterministic standard 7z fixture is 409 bytes total while
  declaring 320,000 expanded bytes over 304 referenced PackInfo bytes; and the
  streaming test rejects a crossing chunk before retention with buffer/count
  unchanged. The current package suite is 58 passed.
- **Discarded timeboxed probe:** a next-header size/CRC mutation was inspected
  but discarded without repository code or a retained test. The independent
  scalar contract, real-fixture metadata preflight, and streaming helper test
  are sufficient evidence, so the mutation probe is not part of the accepted
  proof or product surface.

### `CONSULT-2026-08-02-AGENTBUNDLE-SIDECAR-ACTIVATION-26`

```text
consultation_id: CONSULT-2026-08-02-AGENTBUNDLE-SIDECAR-ACTIVATION-26
received_by_fuji1_date_utc: 2026-08-02
source_control_thread: 019fb513-c030-7073-b073-0ad5442b7db4
remote_execution_thread: 019fbf81-c41e-7450-8c17-e4036548bc33
displayed_model_label_exact: Pro
question_summary: choose the external AgentBundle execution owner, method roles, transient/resident process lifetime, package closure, recovery semantics, and dependency seam after 0.34.10 public-static publication
pro_conclusion: choose one Harness-native agent runtime with one activation-scoped Bun extension sidecar; the sidecar supplies Bundle-local tools/hooks/event handlers through the existing plugin protocol and never runs the agent/model loop
source_correction: this corrected decision supersedes the initial result wherever it treated event as an invocation, delegated the agent loop, or assumed a terminal/artifact result envelope
coordinator_disposition: adopted for release 0.34.11; external execution and resident behavior stay in one release, 0.34.12 remains 100/256 certification, and 0.34.13 remains the independent updater
ruling_scope: public-only JS/Bun extension sidecars for transient and resident Bundle agents; no private activation, raw Rust activation, Bundle MCP owner, sandbox, second runtime, or later-stage work
verification_state: source-verified method roles, current process/catalog seams, and corrected dependency direction; implementation and exact-SHA CI pending
```

#### Corrected runtime and protocol authority

- `SessionEngine`/Harness remains the sole agent runtime and owns the Bundle
  `AgentSpec`, prompt/model/provider loop, tool dispatch, mailbox, spawn/send/
  wait, admission, cancellation, events, `MemberOutcome`, and recovery.
- The child is one activation-scoped extension sidecar. It may supply only
  Bundle-local public JS/Bun tools, hooks, and event handlers through the
  existing newline-delimited JSON-RPC v1 transport. Static-only Bundles start no
  child.
- There is no `agent/invoke`, delegated task/model loop, sidecar send/wait/step,
  terminal agent result, artifact result, or child-produced `MemberOutcome`.
  Bare or malformed stdout is protocol failure; stderr is bounded diagnostic
  data only.
- The only activation-specific wire addition is top-level initialize metadata
  `{ activation_id, lifecycle }`. Activation ACK requires a successful
  initialize response and declaration-drift validation before the first item
  becomes running or the model driver is polled.
- `initialize`, `tool/call`, and `hook/*` remain request/reply. `event` remains
  exactly the existing one-way `EventNotificationParams { envelope }` sent by
  `PluginClient::notify`, with no JSON-RPC id/result and no task/completion
  meaning. The existing `shutdown` operation gains an explicit lifecycle
  method; no second shutdown protocol is added.
- Preserve the 1 MiB frame cap, 5-second initialize timeout, and 30-second
  request/reply timeout. A Bundle tool/hook timeout taints the sidecar and
  requires terminate/reap plus the existing Harness running-work recovery.
  Bundle mode disables transparent client respawn/replay.

#### Process lifetime, context, permissions, and recovery

- `hya-plugin` owns the raw child, stdio reader/writer tasks, bounded stderr,
  exit observation, graceful shutdown, terminate/reap, kill-on-drop, and
  process loss. `hya-app` resolves/materializes the captured package closure
  and constructs the bound implementation. Harness/core retains only an opaque
  lifecycle handle and logical transitions.
- There is exactly one sidecar per executable agent activation: one child for a
  complete transient activation, or one healthy child reused across a
  resident's mailbox messages. There is no process per package/generation,
  pool, multiplexing, cross-activation reuse, TTL, heartbeat, idle reclaim,
  watcher, or sweeper.
- The sidecar receives no prompt, task, transcript, summary, model state,
  `TurnBinding`, `RuntimeSnapshot`, history, grants, or `MemberOutcome`. Its
  only new initialize context is activation ID and lifecycle; later it receives
  only existing tool/hook/event payloads. PID, stdio, stderr, process memory,
  activation metadata, and staging mutations are never persisted.
- Bundle tool resolution uses the activation's captured `TurnBinding`.
  Existing `PermissionPlane`/plugin policy runs before RPC; denial prevents the
  call. Hooks/events are activation-bound. The child cannot request Harness
  tools. Host tools/static skills/host MCP remain available through the
  compiled view; Bundle-declared MCP remains unsupported.
- Idle resident sidecar loss parks the logical resident without a process; the
  next accepted durable message lazily creates and ACKs a replacement. Loss or
  timeout during running work aborts that whole item under the existing actor
  epoch fence, with no model/tool/hook/event replay and exactly-once terminal/
  refund behavior. Durable queued-after work remains ordered and resumes only
  after a fresh PID/stdio/staging/ACK under a new actor epoch while retaining
  the original pinned `TurnBinding`/catalog generation.
- Replacement initialize/declaration failure disables/stops the activation,
  fails queued work once, releases the claim, and never loops restart. Explicit
  stop is final and idempotent: reject new sends, fence, abort current once,
  cancel queued items, stop/reap the child, remove resident/team-map/staging,
  and release claim/admission once. Host shutdown aborts running work but
  preserves durable queued resident work; startup never adopts a PID.

#### Package closure and supported implementation

- Release 0.34.11 keeps the strict public archive at one root bundle.hya.md plus exactly each path represented by the existing v1 agent prompt/resource/Extension contract. It reuses the existing source/prepared paths, contents, IDs, aliases, digests, cross-reference validation, staging, and current 7z limits; there is no generic helper or dependency closure.
- Missing declared entries and unreferenced archive entries, wrapper directories, normalized duplicates, traversal/absolute paths, non-regular entries, symlinks, hardlinks, and devices reject. Undeclared directory files are ignored. Directory and archive forms of the same declared closure produce identical prepared identities and canonical digests; validation precedes atomic publication, so failure preserves generation.
- Bun Compat is the only executable sidecar implementation admitted in this
  release. Private activation/publication, raw Rust activation, Bundle MCP,
  compile-on-activation, dylib/native command routes, arbitrary env, and
  resource profiles without an enforceable host mapping remain typed
  unsupported. The Rust example remains explicitly non-activatable.

#### Corrected dependency seam

Retain the shipped `hya-core -> hya-bundle` dependency and
`RuntimeSnapshot`-owned `Arc<BundleCatalog>`. The 0.34.11 sidecar lifecycle/
start seam must introduce no new Bundle, package, `PreparedResource`, source,
path, digest, or `hya-plugin` types. Its core-facing factory is preconfigured by
`hya-app`; start receives only activation identity/lifecycle, and the opaque
handle exposes ready/ACK, graceful shutdown, terminate/reap, and exit
observation. No command, cwd, env, manifest, `PluginClient`, copied package DTO,
or second RPC abstraction crosses that request.

The source-correction follow-up chose option A after direct HEAD verification:
keep `hya-core -> hya-bundle`, `hya-plugin -> hya-core`, and `hya-app ->` both;
keep `hya-bundle` independent of core/app/server; introduce no
`hya-core -> hya-plugin` edge or cycle; and retain one snapshot-owned catalog
authority. This supersedes the initial literal/global “no hya-core ->
hya-bundle” sentence and any test that would remove the shipped edge.

#### Executable selected-main follow-up arbitration

A source-backed follow-up selected option A for the root/main gap discovered during item-14 review. `hya-core` owns one dependency-light sidecar-resolution/lifecycle interface injected once by `hya-app`; reuse the existing `BundleSidecarEnvironment::factory_for` responsibility rather than adding a parallel resolver, cache, replacement API, server callback, or live catalog closure. After exact main `AgentName` resolution and the single root `TurnBinding` capture, each real root `run_turn` calls that interface exactly once with the captured binding and stable name. The app returns `None` for a static effective view or an opaque already-bound JS/Bun factory; core starts only `{ activation_id, lifecycle: transient }`.

An executable selected main owns exactly one transient sidecar for the full root turn. ACK and declaration-drift validation precede executable schema/dispatch and model polling. Normal completion shuts down/reaps; cancellation, process/protocol failure, in-flight failure, or post-ACK compilation failure terminates/reaps exactly once. Static main turns stay process-free. `ensure_main` actor synthesis remains `sidecar_factory: None`; a coordinating main does not borrow a resident child's process, and executable root and resident-child activations never share handles, IDs, or processes.

The exact dependency invariant is: retain the shipped `hya-core -> hya-bundle` dependency and `RuntimeSnapshot`-owned sole `Arc<BundleCatalog>`; core defines, stores, and calls only the one dependency-light sidecar resolution/lifecycle interface, app resolves/materializes from the captured binding and supplies an opaque factory, plugin owns the child, no `hya-core -> hya-plugin` edge is introduced, and no new Bundle/package/`PreparedResource`/source/path/content/digest/plugin type crosses the start seam.

The follow-up RED order is: (1) one captured binding/name, one resolver call, one transient start, and zero compilation/model polling before ACK; (2) static-main/process-free-coordinator/executable-root-plus-resident ownership matrix; (3) N/N+1 root generation pinning; (4) table-driven pre-model failure and exactly-once cleanup; (5) real Bun selected-main E2E through Harness permission/tool/hook/event/MemberOutcome and shutdown with no task/send/`agent/invoke`/terminal-result RPC.

#### Explicit-stop/mail linearization follow-up arbitration

The item-14 stop/mail race follow-up selects C public semantics with A-style SQLite writer serialization. A resident send linearizes only when its transaction commits the existing MailSent event. Explicit stop linearizes only when one bounded private BEGIN IMMEDIATE-equivalent transaction commits every existing queued/running cancellation/finalization record, the existing Failed activity, and claim release. The earlier TeamState stop_request/cancel is local coordination only. A send that owns the writer first commits before stop and must be canceled by that stop; a stop that owns it first releases the claim before commit, so the later send rechecks inside its transaction and rejects without MailSent. Ordinary mail remains epoch-independent.

The first stop caller installs one TeamState-scoped shared completion and starts an operation independent of any caller wait future. Duplicates await the same exact Result; no caller returns Ok before the durable commit plus sidecar/task/slot cleanup. SQLite/event storage remains the sole acceptance, ordering, terminalization, and restart authority. Direct resident target resolution, active-claim check, recipient validation, and MailSent append occur under one writer transaction. Channel resolution takes the same writer order, includes only resident subscribers with an active claim, retains nonresident recipients, preserves existing MailSent bytes and empty-recipient behavior, and never epoch-binds mail.

The required single store command acquires the writer before reads, validates/fences the current claim, accounts for all earlier accepted mail and queued/running work, applies existing abort/cancel/refund/finalize effects exactly once, appends the existing terminal activity, releases the claim last, and commits or fully rolls back. Process shutdown, task join, slot removal, and staging cleanup stay outside the transaction; shared completion publishes only afterward. A two-phase accepting_mail/stopping claim guard is conditionally authorized only if direct source proves authoritative delivery cannot be fully accounted/canceled in one bounded transaction; consolidation of existing split store calls is not infeasibility. No new public Event/API/DTO, event-byte change, second inbox/projection, epoch-bound mail, global lock, polling, or retry protocol is authorized.

Minimum REDs cover send-first, stop-first, duplicate completion/failure/retry, channel ordering, epoch independence, and transaction rollback/exactly-once. The fallback crash-recovery RED exists only if the feasibility proof activates that fallback.

#### Hook identity and activation-closure follow-up arbitration

The item-14 hook/resource isolation follow-up chooses the minimal A+B contract. During prepare, every existing v1 `hook_refs` spelling is resolved by the existing local/alias/canonical resolver, kind-checked as Hook, canonicalized back into the stable Hook resource ID, and duplicate-checked after canonicalization. The resolved Hook `local_id` is also the exact case-sensitive protocol name and must be one of `event`, `tool.execute.before`, or `tool.execute.after`; aliases select but never rename capability, and `audit` is invalid. `CompiledResourceView` retains these canonical Hook IDs alongside its selected Tool identities without importing plugin types into core.

For one captured activation, only selected Tool resources plus those canonical Hook resources participate. Each selector's normalized source path must exact-match exactly one `extensions.js` resource in that selector's owning bundle; cross-bundle refs therefore join in the referenced resource's namespace, never the selecting agent's bundle. The matched Extension is the Bun entrypoint. The activation deduplicates matched Extension stable IDs, sorts by canonical bundle/resource identity, and loads only those paths. A shared Extension may implement several selected capabilities at the same path, but generic or staged-only extensions are never auto-loaded. Every Extension must be reachable from at least one same-owner Tool/Hook selector, and every selected selector must have exactly one match; zero, ambiguous, prefix, basename, alias, equal-digest/different-path, and wrong-owner matches reject. Controlled Tool/Hook-to-Extension cross-kind same-path reuse stages one selected entrypoint file and preserves directory/archive digest equivalence; it does not create a helper-file carrier.

#### Self-contained JS entrypoint source correction

The helper-file follow-up chooses option A. The normative profile is: “The 0.34.11 public JS profile admits only self-contained selected Extension entrypoint files; no separate Bundle-local helper file kind or transitive JS source closure exists.” V1 source grammar and PreparedBundle/PreparedResource remain unchanged: no resources.files, helpers, dependencies, import list, generic bytes, or dependency graph. A selected Extension may implement several co-path selected Tool/Hook capabilities; distinct Extensions are independent entrypoints.

Strict closure remains bundle.hya.md plus exactly existing v1 agent/resource/extension paths. An undeclared directory helper is ignored and never enters prepared bytes/digest; the same unreferenced file in an archive is UnsafePackage. Declaring such a file as Extension makes it an independently selected/reachable entrypoint, never a helper. Directory/archive identity equivalence covers the declared closure only.

Every activation rematerializes only deduplicated selected Extension paths from the captured generation's PreparedResource bytes into a fresh staging directory; it never executes the authoring tree or stages unselected Extensions as import aids. Missing static relative imports fail during load/initialize before ACK, model polling, or dispatch with existing exactly-once process/staging cleanup. Deferred or dynamic relative imports are unsupported ordinary sidecar failures. Existing Bun/Node builtins or external dependencies retain only existing Compat behavior; 0.34.11 adds no packaging, pinning, availability, installation, portability, network, scanner, graph, or sandbox guarantee.

Required evidence is: unknown helper/dependency fields reject; directory extras are absent from prepared/catalog identity while archive extras reject; directory-source and archive activation both omit helpers and fail before ACK; one self-contained co-path Extension stages once for its aggregate exact declarations; other-agent and unreachable Extensions remain inactive/rejected; a misdeclared helper Extension is either unreachable or an exact-declaration entrypoint; and captured PreparedResource content wins over source mutation or N+1 publication. Generic Bundle files or a JS dependency closure are deferred to a separately assigned release and must not reserve a placeholder now.

#### Harness-prefixed hook_ref source correction

A second source-backed follow-up chooses option A and removes both prior harness:hook/* bypasses. Every accepted v1 PreparedAgent.hook_ref must use the existing Bundle resource-reference grammar, resolve to exactly one ExportKind::Hook resource, canonicalize to that Bundle Hook stable ID, and have local_id exactly event, tool.execute.before, or tool.execute.after. Valid local, alias, and canonical Bundle Hook refs remain unchanged; no harness-prefixed form is accepted. Harness-owned host hooks remain entirely under existing Harness/plugin ownership and outside AgentBundle hook_refs.

Source preparation and canonical PreparedCatalog validation both route every hook_ref through the normal reference parser, Bundle Hook resolution, kind check, canonical rewrite, duplicate-after-canonicalization check, supported-name validation, and exact-path Extension join. Existing invalid-reference, unknown-resource, wrong-kind, and supported-name errors keep their normal precedence; no compatibility or UnsupportedBundleFeature error is added. Invalid install/stage/publication preserves the last good generation, catalog Arc, and pinned bindings. Core adds no prefix filter and app adds no translation or fallback.

Required evidence covers all three supported-looking harness spellings plus unknown, empty, and minimally malformed suffixes; hand-crafted prepared forms; unchanged canonicalization and duplicate/name/path validation for real Bundle Hooks; generation/catalog/binding preservation; and a runtime boundary proving rejected input creates neither filter/fallback, sidecar, nor model polling.

After initialize, actual Hook registrations must equal the order-insensitive expected set derived from selected canonical `hook_refs`, and actual Tool declarations must independently equal the selected expected Tool set. Duplicate, missing, extra, unsupported, or unselected declarations reject before model polling or dispatch. Tool-only activations report zero hooks, hook-only activations report zero tools, and an empty executable selection starts no sidecar. Schema, dispatch, hooks, event fan-out, and entrypoint selection all consume the same captured `TurnBinding`/compiled view; `PermissionPlane` remains final before `tool/call`, static skills and host MCP remain available under existing policy, and Bundle MCP remains unsupported.

This correction removes the WIP all-Bundle tool/Extension binding without adding a second manifest, schema field, sidecar DTO, HookName mapping, provenance protocol, import scanner, catalog, or live resolver. Its strict RED order is prepare canonicalization; exact-path join/package closure; captured-view disjointness and N/N+1 pinning; exact tool/hook declaration sets; dispatch/permission isolation; deterministic split-entrypoint Bun E2E; then static/private/Rust/MCP/generation regressions. Earlier method-role and one-way-event evidence remains valid.

#### Strict RED order and release map

The required order is: (1) plugin method-role lock; (2) Harness ACK gate/no
delegated loop; (3) physical process ownership; (4) schema opening for
JS/Bun tools/hooks while unsupported combinations stay rejected; (5) exact
archive closure; (6) app-owned materialization with a seam-scoped proof that
core start sees only activation ID/lifecycle while the app resolves from the
captured catalog; (7) atomic executable publication; (8) namespace plus
`PermissionPlane`; (9) activation-bound hooks/event fan-out; (10) transient
E2E; (11) resident E2E; (12) cancellation/process-loss recovery; (13) explicit
stop and replacement-drift terminal behavior; (14) private/replay/admission/
epoch/dependency/full-gate regression. No boundary opens before its predecessor
is GREEN.

The release map is fixed: 0.34.9 inspector/registry and 0.34.10 CLI/lazy
public-static publication are closed; 0.34.11 owns public transient and resident
JS/Bun extension sidecars together; 0.34.12 is only 100/256 certification; and
0.34.13 is only the independent updater. The first Pro response's delegated
agent loop, `agent/invoke`, terminal/artifact envelope, sidecar send/wait, and
split transient/resident releases are rejected.
