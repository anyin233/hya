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
