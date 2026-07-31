# Native AgentBundle capability parity matrix

## Status, authority, and compatibility boundary

This matrix was re-characterized directly from the isolated `fuji1` worktree at
committed `0.34.7` HEAD
`064ede0b4fe4601b84ccbe912c75980449527d2c`. The protected dirty-main
CodeGraph index is navigation-only and is not evidence for this matrix. Direct
source reads and the frozen catalog test are authoritative.

The owner override received on 2026-07-31 drops all old agent-file support:

- no old-agent adapter, synthetic representation, old-source catalog/CLI row,
  or agent-file execution fallback may ship;
- every built-in agent must be supplied by a native `AgentBundle`;
- the old JSON/JSONC/Markdown agent parsers, discovery paths, and execution
  branches are deleted in the same cutover that makes the built-in bundle
  authoritative;
- old configuration files are not migrated, rewritten, listed, inspected, or
  executed;
- stored `AgentName` values remain decodable and replayable. Data-protocol
  compatibility does not imply that a removed user-defined agent can execute.

The current parsers are inspected below only as a behavioral oracle for native
Bundle capability coverage and built-in migration. Their acceptance behavior is
not a compatibility promise.

Consult19 and Consult20 are the controlling 0.34.8 rulings. They use
deterministic build-time preparation, authoritative embedded bytes plus a
digest-bound index, immutable built-in origin, explicit unknown-ID rejection,
and reserved system-agent reachability. Startup does not call the package
installer.

## Evidence classes and gates

- **A_EFFECTIVE** — current HEAD has an observable end-to-end effect. Native
  Bundle must preserve semantic behavior. Deterministic prompt text, tool
  schemas, event ordering, event payloads, IDs, and projection output require
  byte-for-byte differential or frozen-golden parity.
- **B_PARSED_IGNORED** — current HEAD accepts and retains a value but does not
  deliver its advertised runtime effect. Native Bundle must implement it with
  RED→GREEN coverage or reject it at the native loader with a typed error. It
  must never be silently ignored.
- **C_DOC_ONLY** — documentation or a built-in description claims behavior that
  current source does not provide. Bundle GA requires implementation plus tests,
  or explicit rejection/removal and a documentation correction.

`SOURCE-CONFIRMED` below means source establishes the current behavior. It does
not close the future characterization gate. `GA-BLOCKING` means a native
Bundle mapping and the named test/rejection evidence are still required.

## Field-level matrix

The “Current parser” column records what the soon-to-be-deleted parser accepts;
it is not a support commitment.

| Field path / behavior | Exact parser or source symbol | Current parser | Effective `AgentSpec` | Prompt / model request | TUI visibility | Subagent resolver | Permission / tool / skill / MCP view | Event / replay / fork / restore | Native Bundle mapping | Required characterization or TDD gate | Class | Status |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Built-in catalog membership: `build`, `plan`, `general`, `explore`, `compaction`, `title`, `summary` | `compat::agent_catalog::NATIVE_AGENTS`, `native_entries`; `compat::agent_native_prompts::get` | Not parsed; compiled into the binary | Entry selected by name and overlaid onto the server base spec | Four built-ins have exact embedded prompt text; others inherit the server base prompt | `mode`/`hidden` determine current list placement | All names are directly resolvable on the characterization HEAD | All currently share the Harness registry and runtime permission plane | Public names enter session/member/roster events | Prepared `hya/core-agents`, explicit stable IDs, `origin=builtin`, `immutable=true`; Consult20 maps the three reserved system IDs to `role=subagent` with no ordinary inbound `can_spawn` | Direct source characterization freezes current fields; `builtin_prompt_sources_match_current_effective_prompt_bytes`, `builtin_sources_prepare_the_frozen_native_agent_catalog`, and `builtin_stable_ids_round_trip_through_historical_replay_and_fork_fixtures` freeze prepared mapping and identity. Commit 2 still owes API/TUI/spawn/boot proofs | A_EFFECTIVE | SOURCE-CHARACTERIZED; PREPARED-SOURCE/REPLAY-VERIFIED; CUTOVER-GA-BLOCKING |
| Agent ID: config map key or Markdown `name` | `agent_sources::append_inline_agents`; `agent_disk_sources::AgentFrontmatter::name`, `disk_agent`; `agent_catalog::apply_change` | Accepted; Markdown `name` overrides path-derived ID | Becomes `AgentSpec.name` for selected/spawned work | Copied into assistant `Message::Assistant.agent`; does not alter prompt text | TUI/API key and display name | `subagent_type` lookup and inline name override | Task permission resource is the requested `subagent_type` | `SessionCreated.agent`, `AgentSwitched.agent`, `MemberSpawned.subagent_type`, and `AgentRegistered.agent_type` persist it | Public built-in agent ID remains unqualified at API/event boundary; binding may resolve internally to a qualified Bundle catalog ID | Exact public ID/API JSON/event JSON/replay/fork golden; no event rewrite to a qualified ID | A_EFFECTIVE | SOURCE-CONFIRMED; GA-BLOCKING |
| Source discovery and override order | `agent_sources::config_paths`, `config_agents`; `agent_disk_sources::disk_agents`, `global_disk_agents`; `agent_catalog::merged_entries`, `apply_change` | Native defaults → optional global Markdown → four workdir JSON/JSONC sources → workdir Markdown; later matching names overlay | Produces the effective catalog entry before `AgentSpec` projection | Can change prompt/model/reasoning through the winning entry | Can change all catalog metadata | Can change spawn resolution | Can change catalog-only policy metadata | Source/provenance is not currently persisted | No old discovery mapping. Built-ins come only from embedded native built-in packages; ordinary installed bundles come from the Bundle registry/generation | Prove all old discovery paths and callers are absent in the same `0.34.8` cutover and built-ins still expose the frozen effective catalog | A_EFFECTIVE | SOURCE-CONFIRMED; `0.34.8` ATOMIC CUTOVER |
| `description` | `AgentChange::description`, `AgentFrontmatter::description`, `InlineAgent::description`, `AgentEntry::description` | Accepted and merged | Not represented | No request effect | Shown in agent dialog/list and `list_agents` | Inline description is carried into `SpawnMember` but not copied into `AgentSpec` | No policy effect | Member description is evented from task input, not catalog description | Agent catalog descriptor field; inline description needs an explicit consumer or typed rejection | API/TUI/list-agents golden; separate RED→GREEN or typed-reject test for inline description | A_EFFECTIVE for catalog; B_PARSED_IGNORED for inline runtime | SOURCE-CONFIRMED; GA-BLOCKING |
| `mode` / primary-mode directories | `InlineAgent::mode`; `AgentFrontmatter::mode`; `append_inline_agents(primary)`; `disk_agent(primary)`; `agent_defaults::selectable`; TUI `createAgent` | Accepts strings; `.opencode/mode(s)` forces `"primary"`; new entry defaults to `"all"` | Not represented | No request effect | Exactly `"subagent"` is excluded from the current main picker/default selection | Resolver does not prohibit spawning `"primary"`/`"all"` agents | No policy effect | Agent ID, not mode, is persisted | `role: main|subagent` controls selector visibility only; it does not grant spawn. Map ordinary effective mode to role, with Consult20's explicit reserved-system exception | Differential TUI/default-selection/list-agents tests plus `can_spawn` reachability tests | A_EFFECTIVE | SOURCE-CHARACTERIZED; PREPARED ROLE MAPPING VERIFIED; CUTOVER-GA-BLOCKING |
| `hidden` | `InlineAgent::hidden`; `AgentFrontmatter::hidden`; `AgentEntry::hidden`; `agent_definitions`; TUI `createAgent` | Accepted; default false | Not represented | No request effect | Current hidden entries are omitted from picker and model-facing `list_agents` | Direct name resolution still finds a hidden entry | No policy effect | ID can still exist in historical events | No native `hidden` field. Consult20 maps only `compaction`/`title`/`summary` to `role=subagent` and removes their ordinary `can_spawn` reachability. All old-file hidden declarations disappear with the parser | Prepared schema rejects `hidden` as unknown; Commit 2 proves selector exclusion, eligible-roster exclusion, typed spawn rejection, fixed system lookup, and replay/resume identity | A_EFFECTIVE current behavior with owner-approved tightening | SOURCE-CHARACTERIZED; CONSULT20 RESOLVED; CUTOVER-GA-BLOCKING |
| `model` | Both agent parsers; `AgentEntry::model`; TUI `createModel`; `subagent_resolve::resolve_subagent` | Accepted and merged | Spawn resolver writes `AgentSpec.model`; selected main model is persisted by session creation/switch | Determines provider route and request model; TUI uses configured model as a candidate | Invalid configured model produces a TUI warning | Precedence: spawn model > spawn category > entry/inline model > entry/inline category > base model | Model-dependent tool-schema filter still applies | `SessionCreated.model`/`ModelSwitched.model` replay; fork copies projected model | Agent model policy plus unchanged public session model/event contract | Main and subagent differential request test, provider fallback, session switch, replay/fork/restart | A_EFFECTIVE | SOURCE-CONFIRMED; partial tests exist; GA-BLOCKING |
| `category` and category fallback | Both agent parsers; `CategoryRegistry::resolve_servable`; `subagent_resolve::resolve_subagent` | Accepted and merged | Resolves to concrete `AgentSpec.model` for spawned agents | Concrete winner reaches provider request | Listed by model-facing `list_agents`; not a direct main-picker model | Ordered servable failover; spawn category overrides entry/inline category | Model-dependent tool filter follows resolved model | Only concrete model/agent ID is evented, not category provenance | Native Bundle model policy must preserve the characterized precedence and failover | Differential healthy/unhealthy-provider matrix and exact concrete model/event output | A_EFFECTIVE | SOURCE-CONFIRMED; tests exist; GA-BLOCKING |
| `resident` / spawn `resident` | Both agent parsers; `TaskInput`/`TaskMemberInput`; `subagent_resolve::ResolvedSubagent.resident`; `hya-app::runtime` spawn routing | Accepted; defaults false | Not a field on `AgentSpec` | No direct request field | Resident mode appears in roster/subagent UI, not main selection | Entry or inline `resident` OR spawn-time `resident`; current root session lifetime remains Harness-owned | Same registry and permission plane | `AgentRegistered.mode` and roster projection replay transient/resident | `spawn_lifecycle: transient|resident` controls only behavior when the catalog agent is spawned. A TUI-selected main remains a Harness-owned root session | Differential transient/resident spawn, roster event order/payload, restart, send/wake, and root-main lifecycle tests | A_EFFECTIVE | SOURCE-CONFIRMED; partial tests exist; GA-BLOCKING |
| Markdown body / inline `prompt` / config `system` or `prompt` | `agent_disk_sources::parse_agent_file`, `disk_agent`; `agent_sources::append_inline_agents`; `reference::apply_agent_entry`; `subagent_resolve` inline override | Body becomes prompt; config `system` wins over `prompt`; inline non-empty prompt wins | Becomes `AgentSpec.system_prompt` | Copied exactly to `CompletionRequest.system`, then skill index/reference guidance may be appended by existing Harness steps | Prompt is returned in catalog APIs but not rendered in picker | Entry/inline prompt specializes spawned agent | Resource view does not change | Prompt bytes are not stored in session events; replay re-resolves definition on later execution today | Native built-in Bundle prompt resource; deterministic final prompt compiler must preserve exact ordering/whitespace | Byte-for-byte final prompt golden for every built-in, with/without skills and reference guidance; replay-after-restart behavior | A_EFFECTIVE | SOURCE-CONFIRMED; GA-BLOCKING |
| `variant` and reasoning-bearing `options` keys | Parsers; `agent_options::from_config`; `reasoning_options::resolve_reasoning`; `reference::apply_agent_entry` | Accepts `variant`, `options`, and flattened extras | Resolves only to `AgentSpec.reasoning` | Reaches `CompletionRequest.reasoning`; model-ref variant outranks agent variant; provider/model bundle and agent options are merged | Variant/options are exposed by agent APIs | Applied after final subagent model resolution | No resource-view effect | Reasoning is request state, not separately persisted as an agent field | Model policy/reasoning overlay in Bundle IR; preserve current precedence and supported key aliases | Differential matrix for `reasoningEffort`, `reasoning_effort`, `effort`, nested reasoning/thinking, Google thinking config, disabled variant, and model-ref variant | A_EFFECTIVE for recognized reasoning signals | SOURCE-CONFIRMED; tests exist; GA-BLOCKING |
| Non-reasoning `options` and flattened unknown frontmatter/config fields | `agent_options::from_config`; `AgentOptions`; `AgentEntry::options` | Accepted, merged, and returned by APIs; `"name"` and `"tools"` are removed from flattened extras | Not represented | No request effect outside recognized reasoning keys | Metadata only | Ignored | Ignored | Not persisted as agent semantics | Native loader must either define a real consumer or return a typed unsupported-field error | One RED→GREEN test per supported key, otherwise exact typed-reject fixture; no catch-all silent map | B_PARSED_IGNORED | SOURCE-CONFIRMED; GA-BLOCKING |
| `temperature` | Both parsers; `AgentEntry::temperature`; API/TUI bootstrap serialization | Accepted and surfaced | Not represented | `request_from_messages` hard-codes `CompletionRequest.temperature = None` | Metadata only | Ignored | No effect | Not persisted | Implement request propagation or typed reject | RED proving parsed value currently fails to reach fake provider, then GREEN or typed-reject test | B_PARSED_IGNORED | SOURCE-CONFIRMED; GA-BLOCKING |
| `top_p` | Both parsers; `AgentEntry::top_p`; API/TUI bootstrap serialization | Accepted and surfaced | Not represented | `CompletionRequest` has no `top_p` field | Metadata only | Ignored | No effect | Not persisted | Add an end-to-end supported request field or typed reject | RED at loader/request boundary, then protocol conformance or typed-reject test | B_PARSED_IGNORED | SOURCE-CONFIRMED; GA-BLOCKING |
| `steps` / `maxSteps` | Both parsers; `AgentEntry::steps`; API serializers | Accepted; `steps` wins over `maxSteps` | Not represented | Does not set output tokens or loop limit | Metadata only | Ignored | No effect | Not persisted | Define an enforced turn/step budget or typed reject | Boundary RED proving no current enforcement, then budget behavior or typed-reject test | B_PARSED_IGNORED | SOURCE-CONFIRMED; GA-BLOCKING |
| `request.headers` | Both parsers; `AgentEntry::request_headers`; metadata API | Accepted, merged, and exposed | Not represented | `request_from_messages` sets empty headers; provider can forward headers only when they exist on `CompletionRequest` | Metadata only | Ignored | No effect | Not persisted | Implement a validated/redacted request overlay or typed reject; never silently accept secret-bearing headers | Fake-provider/HTTP capture RED, validation and secret-log tests, then GREEN or typed reject | B_PARSED_IGNORED | SOURCE-CONFIRMED; GA-BLOCKING |
| `request.body` | Both parsers; `AgentEntry::request_body`; metadata API | Accepted, merged, and exposed | Not represented | `CompletionRequest` has no generic body overlay | Metadata only | Ignored | No effect | Not persisted | Define protocol-owned safe request options or typed reject; do not add an untyped wire-body escape hatch by accident | Protocol fixture RED followed by explicitly supported-key tests or typed reject | B_PARSED_IGNORED | SOURCE-CONFIRMED; GA-BLOCKING |
| Per-agent `permissions`, legacy `permission`, legacy `tools`, Markdown `readonly` | `agent_permission_config::rules`; `agent_disk_sources::readonly_permissions`; `AgentEntry::permissions`; comment in `subagent_resolve` | Accepted and converted to catalog `PermissionRule`s; `readonly` becomes deny edit | Not represented | Does not change request schemas | Returned in agent metadata | Source explicitly says per-agent permissions are not layered onto the child session | Current runtime uses the existing Harness `PermissionPlane`; per-agent entries do not narrow execution | Session permission events are a separate API concept and do not make catalog rules effective | The `0.34.8` v1 schema rejects `permission_overlay` and all old permission spellings because no effective Bundle consumer exists. A future typed narrowing field must reach existing dispatch before acceptance | `invalid_schema_references_and_executable_features_fail_typed` proves preparation rejects these fields; the native `plan` description still requires enforcement or correction before cutover GA | B_PARSED_IGNORED; built-in plan claim also C_DOC_ONLY | SOURCE-CONFIRMED; V1 TYPED-REJECT VERIFIED; PLAN CLAIM GA-BLOCKING |
| Built-in `plan` description: “Disallows all edit tools” | `agent_catalog::NATIVE_AGENTS`; absence of per-agent enforcement in `reference::apply_agent_entry`, `AgentSpec`, and `subagent_resolve` | Not a parser rule; descriptive metadata only | No plan-specific policy | Full current tool schemas are still advertised subject only to model filter | Selectable main entry | May also be spawned | Same Harness permission plane as other agents | No plan-policy event | Implement a real narrowing overlay in the native built-in bundle or correct the claim before GA | End-to-end edit/apply-patch/write deny tests through normal ask/deny/error events, or explicit documentation correction | C_DOC_ONLY | SOURCE-CONFIRMED; SECURITY/GA-BLOCKING |
| `color` | Both parsers; `AgentEntry::color`; TUI `createAgent().color` | Accepted and merged | Not represented | No request effect | Chooses configured theme/hex color | Ignored | No effect | Not persisted | Catalog presentation metadata | TUI golden for named/hex/default colors | A_EFFECTIVE | SOURCE-CONFIRMED; GA-BLOCKING |
| `disable` / `disabled` | Both parsers; `AgentChange::remove`; `agent_catalog::apply_change` | Either true removes the matching effective entry | Entry cannot be selected | No request occurs | Removed from APIs/picker | Unknown-name fallback may still run `general`; see conflict below | No direct effect | Historical event IDs remain replayable | Old parser is deleted, so no file-compat mapping. Native installed bundle enable/disable is registry/generation state, not this field | Cutover test proves old files cannot add/remove agents; registry generation test owns future enable/disable | A_EFFECTIVE current behavior, intentionally not preserved as file support | SOURCE-CONFIRMED; deletion target |
| Workdir/global `default_agent` and catalog sort | `AgentConfig::default_agent`; `agent_catalog::configured_default`; `agent_defaults::{sort,selected_name}` | Later workdir config wins; server config fallback; invalid/nonselectable choice falls back to first selectable, normally build | Chooses session `AgentName` at creation | Selected session model/prompt then apply | First picker entry/current default | No spawn effect | No effect | Selected ID is persisted and replayed | Embedded native built-in Bundle plus Harness Bundle-era config default; removed old workdir key is not supported | Default/invalid/hidden/subagent selection golden and exact `SessionCreated` payload | A_EFFECTIVE | SOURCE-CONFIRMED; GA-BLOCKING; `0.34.8` FIXTURE REQUIRED |
| Unknown `subagent_type` fallback | `subagent_resolve::resolve_subagent`; test `unknown_type_keeps_name_and_falls_back` | Any requested string is accepted by task input | Keeps requested `AgentSpec.name` but takes native `general` prompt/reasoning/model fallback | Sends fallback content under the unknown agent name | Unknown name is not catalog-listed | Explicit source-tested fallback to `general` | Task permission is checked against the unknown requested string | Events persist the unknown requested ID | Consult19 intentionally replaces this accidental fallback: omitted agent alone selects stable `general`; explicit unknown returns typed `UNKNOWN_AGENT_ID`, creates nothing, and never runs `general` | Characterization remains as breaking-change evidence; Commit 2 RED/GREEN proves explicit/omitted split and no child/event side effects | A_EFFECTIVE with owner-approved fail-closed correction | SOURCE-CHARACTERIZED; CONSULT19 RESOLVED; CUTOVER-GA-BLOCKING |
| Main-agent and reserved-system spawn reachability | `TaskTool::execute`; `subagent_resolve::resolve_subagent`; Consult20 | Task currently accepts any `subagent_type`; resolver does not filter mode/hidden | A primary or hidden entry can currently become a child `AgentSpec` | Uses that entry's resolved prompt/model | Current mode/hidden affects picker only | Current behavior permits every exact name | Existing Task permission applies; no catalog graph exists | Normal member/session events | `can_spawn` is the sole ordinary reachability graph. Preserve ordinary main/subagent reachability, but exclude `compaction`/`title`/`summary`; their existing fixed Harness operations perform same-snapshot exact lookup, not agent spawn | Prepared graph fixture plus Commit 2 typed `AGENT_SPAWN_NOT_ALLOWED`/no-child/no-fallback tests and fixed-callsite snapshot tests | A_EFFECTIVE with owner-approved reserved-ID tightening | SOURCE-CHARACTERIZED; CONSULT20 RESOLVED; PREPARED GRAPH VERIFIED; CUTOVER-GA-BLOCKING |
| Full Harness resource view for current agents | `engine::turn::messages::filtered_tool_schemas`; `SessionEngine` planes; runtime registry wiring | No per-agent resource parser is effective | `AgentSpec` carries no resource view | Every current agent sees the same ordered registry schemas, except model-specific `apply_patch` versus edit/write filter | TUI MCP toggles global manager state | Child receives the same engine registry/planes | Existing `PermissionPlane` authorizes calls; skill/MCP/builtin/plugin tools share registry/planes | Tool call/result/error events use existing stable names | Built-ins set `harness_access: full`; `resource_view` is a separate deterministic narrowing/alias layer. Neither expands PermissionPlane/plugin authority | Byte-for-byte schema list plus successful/denied builtin, skill, MCP, and plugin calls for main/transient/resident | A_EFFECTIVE | SOURCE-CHARACTERIZED; PREPARED ACCESS MAPPING VERIFIED; CUTOVER-GA-BLOCKING |
| Bundle `resource_profile` | No current agent parser/`AgentSpec` consumer; global `SubagentLimits` owns `max_depth`, `max_concurrency`, `per_run_budget`, `per_team_turn_budget`, and `per_team_message_budget` | Not a current agent field | No per-agent profile | No request effect | None | Global admission/governor only | No resource-view or permission effect | Admission events use global units | v1 source presence returns typed `UNSUPPORTED_BUNDLE_FEATURE`; there is no unconstrained string/default profile in prepared IR | `unsupported_resource_profile_is_rejected_as_a_feature_not_ignored` | target-only field without complete consumer | SOURCE-VERIFIED; TYPED-REJECT VERIFIED |
| Skill `name`, `description`, body, `disable` | `hya_tool::skill_catalog::{SkillFrontmatter,parse_skill,discover_skills}`; `skills_section`; `SkillPlane::require` | Required name/description; body retained; disabled skills skipped; first discovered name wins | Skill index is appended by `effective_agent_for_projection`, not stored on base spec | Exact name/description index enters system prompt; `skill` tool returns body and sampled files | Skill catalog/dialog surfaces metadata | Same workdir-scoped plane for children | Skill call is checked by existing `PermissionPlane`; body loads progressively | Tool events persist invocation/result, not definition | Bundle-local Markdown skill resource plus built-in/full resource-view resolution | Exact skill index/body/schema/order differential; permission allow/ask/deny and main/transient/resident invocation | A_EFFECTIVE | SOURCE-CONFIRMED; GA-BLOCKING |
| Skill `allowed-tools` | `SkillFrontmatter::allowed_tools`; `ParsedSkill`; `SkillCatalogEntry`; `SkillPlane::require` drops it | Parsed and retained in discovery entry | Not represented | Does not filter schemas | Not exposed by current `SkillInfo` | No effect | Does not constrain tool calls | Not persisted | Implement a real Harness-policy-narrowing skill view or typed reject | RED demonstrating current unrestricted call, then enforcement or typed-reject test | B_PARSED_IGNORED | SOURCE-CONFIRMED; SECURITY/GA-BLOCKING |
| Skill `model` | `SkillFrontmatter::model`; `ParsedSkill`; `SkillCatalogEntry`; `SkillPlane::require` drops it | Parsed and retained in discovery entry | Not represented | Does not switch model | Not exposed | No effect | No effect | Not persisted | Define explicit skill-scoped model semantics or typed reject | Fake-provider RED then model selection test or typed-reject test | B_PARSED_IGNORED | SOURCE-CONFIRMED; GA-BLOCKING |
| Skill `license` | `SkillFrontmatter::license` with `#[allow(dead_code)]` | Parsed | Not represented | No effect | Not exposed | No effect | No effect | Not persisted | Either preserved as explicit descriptive metadata or typed rejected | Inspector/catalog test or typed-reject fixture | B_PARSED_IGNORED | SOURCE-CONFIRMED; GA-BLOCKING |
| Inline ephemeral agent fields | `TaskTool::InlineAgentInput::into_inline`; `spawn::InlineAgent`; `subagent_resolve::resolve_subagent` | Runtime task JSON accepts name/prompt/description/category/model/resident | Name/prompt/model/reasoning reach child spec; description does not | Prompt/model/reasoning effective | Not a catalog/TUI entry | Inline model/category beat disk entry within their tiers; spawn overrides still win; resident ORs | Same full Harness view | Name/lifecycle appear in session/member/roster events; inline definition itself is not persisted | This is Harness-native spawn input, not a legacy file. Native Bundle execution must preserve the supported override precedence or explicitly version it | Differential inline name/prompt/model/category/resident/event test; description implement-or-reject test | A_EFFECTIVE except description B_PARSED_IGNORED | SOURCE-CONFIRMED; GA-BLOCKING |
| Task `task_id`, `background`, `members`, `command`, prompt/description | `TaskInput`, `TaskMemberInput`, `TaskTool::execute`; spawner/runtime | Runtime JSON, not file parsing | Controls resume/new child, batch, background, and task prompt | Member prompt is admitted as child user input; command is result metadata | Background/resident outcomes surface in task/TUI views | Shared spawn path with model/category/inline/resident overrides | Task permission per requested agent; admission governs work | Existing member/session/roster events | Remains Harness/SessionEngine ABI, never Bundle runner ABI | Schema golden plus resume, batch, background, transient/resident, cancellation/admission, and event-order tests | A_EFFECTIVE | SOURCE-CONFIRMED; 0.34.3 covers only admission subset; full GA gate later |
| Prompt `parts[type=agent]` / manual `@agent` attachment | TUI prompt parts; `session_prompt_legacy::prompt_parts`; `record_user_prompt_context`; `Projection::UserPromptContextRecorded` | HTTP prompt accepts agent attachment values | Does not switch `AgentSpec` or spawn | Agent attachment metadata is not added to provider message parts | TUI can author/display the attachment | No resolver call | No resource effect | Attachment is durably recorded, replayed, and copied on fork | Preserve metadata round-trip if the UI surface remains; do not claim invocation semantics unless implemented | Exact event/replay/fork fixture; separate RED→GREEN execution test or documentation/UI correction for any “manual invocation” claim | A_EFFECTIVE for metadata; B_PARSED_IGNORED/C_DOC_ONLY for invocation | SOURCE-CONFIRMED; GA-BLOCKING |
| Session agent/model identity across create, switch, replay, fork, and restart | `SessionEngine::{create_with_id,switch_agent,switch_model,replay}`; `Event::{SessionCreated,AgentSwitched,ModelSwitched}`; `Projection::apply_event`; `compat::session_fork::fork` | API accepts an agent string independently of file availability | Active name/model are rebuilt from projection; definition is resolved only when executing | Provider uses projected model; prompt is resolved from currently available definition | Session/TUI reports projected agent/model | Child sessions persist their public agent IDs | Resource authority remains current Harness state, not historical bundle authority | Event wire values and order are the compatibility boundary; fork copies projected name/model | Keep historical IDs byte-for-byte. Replay must not require old files. Continuing a session whose definition no longer exists must fail typed and must not silently execute the base agent under the old ID | Old event corpus decode/projection golden; restart/fork fixtures; missing-definition continuation typed-error test; no event rewrite/migration | A_EFFECTIVE | SOURCE-CONFIRMED; DATA-COMPAT/GA-BLOCKING |

## Built-in native Bundle cutover contract

The cutover must preserve the public identities and effective behavior of:

| Public ID | Current selection class | Current hidden state | Current prompt source | Native cutover note |
| --- | --- | --- | --- | --- |
| `build` | main (`primary`) | visible | server base prompt when no entry prompt exists | Native Bundle must preserve the base-prompt composition seam and current default selection |
| `plan` | main (`primary`) | visible | server base prompt | Its “no edits” description is currently not enforced; close the C-class gate before GA |
| `general` | subagent | visible to model catalog, hidden from main picker | falls back to base prompt | Preserve exact-ID spawnability; omitted agent selects `general`, but an explicit unknown ID never falls back to it |
| `explore` | subagent | visible to model catalog, hidden from main picker | embedded `explore.txt`, trimmed at end | Freeze exact bytes |
| `compaction` | reserved system operation; native `role=subagent` | excluded from selector and eligible ordinary roster | embedded `compaction.txt`, trimmed at end | No ordinary inbound `can_spawn`; fixed Harness exact lookup from the current TurnBinding catalog; preserve historical ID |
| `title` | reserved system operation; native `role=subagent` | excluded from selector and eligible ordinary roster | embedded `title.txt`, trimmed at end | No ordinary inbound `can_spawn`; fixed Harness exact lookup from the current TurnBinding catalog; preserve historical ID |
| `summary` | reserved system operation; native `role=subagent` | excluded from selector and eligible ordinary roster | embedded `summary.txt`, trimmed at end | No ordinary inbound `can_spawn`; fixed Harness exact lookup from the current TurnBinding catalog; preserve historical ID |

The native bundle is the sole executable source after cutover. No production
fallback may consult the deleted old parsers. Rollback selects only retained
verified native generations at a higher epoch; it never reintroduces a second
catalog authority.

## GA verification gate

Bundle GA is blocked until all of the following hold:

1. Every row above has a final native mapping or an explicit typed-reject/doc
   correction.
2. Every A row has a differential or frozen-golden test. Deterministic final
   prompt bytes, tool-schema bytes/order, public IDs, event types/order/payloads,
   projection JSON, replay, and fork output are bit-for-bit equal.
3. Every B row has an independent RED proving the current missing effect,
   followed by GREEN behavior or a typed native-loader rejection. Parser
   acceptance is not mislabeled as parity.
4. Every C row is implemented and tested or the claim is removed/corrected.
5. A real corpus covering all seven built-ins, main selection, transient and
   resident spawn, inline spawn, permissions, tools, skills, MCP, events,
   replay, fork, and process restart passes.
6. TUI/API/spawn behavior has no unapproved regression.
7. The old agent JSON/JSONC/Markdown discovery/parser/execution paths and their
   production callers are deleted in the same cutover that activates the
   native built-in bundle.
8. Historical event/session `AgentName` values still decode and replay without
   old files. Missing definitions never cause silent execution under another
   prompt/model.

## Historical round-six uncertainty packet

```text
schema_version: 1
packet_id: UQ-AGENTBUNDLE-BOOTSTRAP-AND-UNKNOWN-ID-2026-07-31
task_id: modular-harness-native-swarm-runtime-refresh
prepared_by: fuji1 remote worker
authoritative_head_sha: 267bfc3c6c66e46fe8514e2e70657489f853b7f0
trigger_class:
  - architecture invariant conflict
  - two consequential designs lack an owner criterion
blocked_gate: native built-in AgentBundle cutover (not 0.34.3)
exact_questions:
  - Which single authority bootstraps the native built-in Bundle before the
    ordinary installed-bundle registry/runner is available: embedded,
    preinstalled, or registry-seeded?
  - Does the source-tested unknown subagent fallback to `general` remain an
    A-class native capability, or does the prior fail-closed missing-ID ruling
    intentionally break it at cutover?
why_uncertain:
  - Deleting old discovery in the same cutover requires a recovery-safe native
    source for built-ins, but the owner has deferred bootstrapping order.
  - Current HEAD explicitly tests unknown-name fallback, while the accepted
    stable-ID resolver design says missing IDs fail closed.
verified_facts:
  - Built-ins currently originate in agent_catalog::NATIVE_AGENTS and embedded
    prompt files.
  - resolve_subagent keeps the requested unknown AgentName and applies the
    `general` entry; an explicit test freezes that behavior.
  - SessionCreated/AgentSwitched/MemberSpawned/AgentRegistered persist public
    AgentName values, and Projection/fork replay those values.
  - The owner has forbidden old-agent adapters, synthetic representations, old
    file execution support, and dual runtime/catalog authorities.
candidates:
  - Embedded native built-in Bundle: simplest recovery and no install ordering;
    requires one compiler path shared with installed bundles.
  - Preinstalled/registry-seeded built-in Bundle: exercises distribution state
    earlier; startup and rollback depend on registry availability.
  - Unknown IDs preserve `general` fallback: behavioral parity, but conflicts
    with fail-closed identity and can hide typos.
  - Unknown IDs fail typed: safer stable-ID semantics, but an explicit breaking
    change to a source-tested behavior.
failure_impact:
  - No authoritative built-in source can make the agent runtime unbootable.
  - Dual fallback sources can drift and make rollback non-deterministic.
  - Silent unknown-ID fallback can execute the wrong prompt/model; abrupt
    rejection can break current task callers and historical continuation.
determinations_requested:
  - bootstrap authority and recovery/rollback rule
  - explicit resolution of unknown-ID parity versus fail-closed behavior
data_classification: public repository architecture; no secrets or private binary
redaction_attestation: only symbol names and behavior summaries are included
```

### Round-six disposition

- **Bootstrap resolved:** use the repo-native build-time preparer, authoritative
  embedded package bytes, digest-bound index, immutable built-in origin, and
  boot-without-install contract in `0.34.8`.
- **Old-source handling resolved:** no detector, adapter, translation,
  migration, or fallback is added. Old files are outside discovery; an
  explicitly supplied old source at a Bundle-only boundary returns a typed
  unsupported-source/format error.
- **Unknown-new-spawn behavior resolved by Consult19:** only omission selects
  `general`; an explicit unknown ID fails typed and never executes a general
  definition under unknown identity.
- **Reserved native reachability resolved by Consult20:** `compaction`,
  `title`, and `summary` map to `role=subagent`, remain outside every ordinary
  `can_spawn` graph, and are available only to their existing fixed Harness
  system lookup from the same turn snapshot. Historical replay/resume keeps the
  exact stable ID and does not apply a new-spawn reachability check.
