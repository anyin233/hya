# Resume and Complete Argus Work

## Outcome

Recover the unfinished Argus workspace without losing unrelated user work, then complete the original Workflow platform request end to end. The finished product lets users author, package, install, select, run, observe, and recover governed Agent Workflows through one shared runtime.

## Authoritative Recovered Scope

Argus implemented and tested a useful first slice, but it narrowed the original request. The recovered operator request remains authoritative:

- Users load their own Workflow DAGs from a simplified Mermaid-inspired document.
- Graphs support ordered stages, fan-out, explicit fan-in, user-selected failure policy, iterative stages, and repeated interaction with the same resident Agent.
- A `.hyabundle` can carry one complete Workflow and every Agent that Workflow requires. It can also carry optional tool and Skill contributions through the same plugin contribution contract used elsewhere by the runtime.
- Installed example bundles include a simple Workflow, a multi-perspective Workflow, and one full Argus-like Workflow. They are examples, not built-in policy.
- A Session remembers its selected Workflow and current/last run state. Switching Workflow does not discard the Session transcript.
- CLI, Agent tool, native `/workflow` command, server, SDK, and TypeScript TUI use the same Workflow executor and typed state.
- The TUI shows the selected Workflow, current graph stage or stages, current task label, Agent counts, progress, and terminal outcome through the existing sidebar plugin system.

## Product Requirements

### R1. Recovered Workspace Ownership

1. Preserve user and Trellis changes already present in the working tree.
2. Treat the existing provider retry/timeout/fallback work, `find`-tool repair, and the current narrowed Workflow implementation as predecessor work. Verify it before building on it.
3. Remove superseded Workflow code, documents, fixtures, and claims during the clean cutover. Do not leave compatibility aliases or dual authoring formats.
4. Keep generated/runtime artifacts out of commits.

### R2. Workflow Authoring Contract

1. A Workflow is one Markdown file under a Workflow discovery root.
2. YAML frontmatter declares Workflow metadata, required inputs, failure policy, and a map of graph-node definitions. The Markdown body contains one simplified `flowchart TD` graph.
3. The graph supports standalone node identifiers and `-->` edges with `&` fan-out/fan-in sugar. Graph declaration order is the deterministic stage and join order.
4. Node definitions declare a target Agent, a directive, an optional human-readable title, optional loop/verifier settings, and an optional resident actor key.
5. Graph edges automatically supply bounded direct-predecessor evidence to the downstream directive. Runtime inputs use explicit `{{input.<name>}}` placeholders. Stage-output placeholders are not part of the public format.
6. The compiler rejects malformed or unsupported Mermaid syntax, missing or duplicate nodes, dangling/self/cyclic edges, invalid identifiers, invalid input placeholders, invalid loop/verifier combinations, and unsafe resident actor reuse before any Agent starts.
7. The old YAML `stages:`/`needs:` author format is rejected. This is a clean cutover because the current implementation is unpublished predecessor work.

### R3. Governed Workflow Execution

1. The graph schedules Stage activations. It does not create a second Agent scheduler.
2. Transient fan-out continues to use one governed Team batch per topological level. Fan-in uses bounded, typed `MemberEvidence` in deterministic graph order.
3. `fail_fast` stops later levels after the current level settles. `collect_all` continues eligible later levels, includes failed predecessor evidence, and reports an overall failed outcome when any Stage failed.
4. Loop stages continue to use the shared iteration driver and a separate verifier Agent. Graph cycles remain invalid.
5. An explicit actor key binds sequential nodes to one resident Agent Session. Repeating an Agent id without an actor key still creates distinct transient Sessions.
6. Resident activation uses the existing resident supervisor and durable mail wake path. The first and later directives use the same mail contract. Two same-level nodes cannot target the same actor key.
7. A Workflow run becomes terminal only after all planned activations are terminal and no activation owned by that run remains active. A successful resident remains idle and addressable after the run. Workflow cancellation stops new scheduling, waits for already-admitted resident work to settle, and does not silently destroy reusable actors.
8. All targets, verifier targets, inputs, graph constraints, and minimum spawn budget are preflighted before the first spawn or mail append.

### R4. Workflow Bundle Contract

1. `.hyabundle` supports two explicit payload kinds: `AgentBundle` and `WorkflowBundle`.
2. `AgentBundle` keeps the ADR-0012 invariant of exactly one Agent. `WorkflowBundle` carries exactly one Workflow plus a closed set of all Agent definitions referenced by that Workflow.
3. Both payload kinds use the same package verification, prepare, digest, install, registry, list, removal, and resource ownership path. A WorkflowBundle occupies one installed bundle row.
4. Installed Workflow identity is stable and typed. Bundle Workflow ids use `bundle:<bundle-id>/workflow/<workflow-id>`.
5. WorkflowBundle preparation rejects missing/extra Agent closure, invalid Workflow source, invalid Agent/resource ownership, digest mismatch, path escape, and unsupported prepared-format versions before activation.
6. Bundle-provided tools and Skills enter runtime composition as typed plugin contributions. Static bundled Skills may use an in-process adapter, while executable tools use the existing sidecar/plugin protocol. Bundle authors do not get a second tool or Skill interface.
7. The installed Compat adapter is included and resolvable in release installations before executable example bundles are accepted.

### R5. First-Party Workflow Bundles

1. Ship one simplified `plan -> impl -> review` WorkflowBundle in the ordinary first-party catalog. It is selectable without a separate download but is never auto-selected.
2. Ship one full Argus-like investigate/plan/execute/review/verify WorkflowBundle as an installable example. Its ordinary DAG demonstrates fan-out/fan-in and multi-perspective review.
3. Both bundles are self-contained, include all referenced Agents, and use only declared package resources plus host-provided public capabilities.
4. Both validate, resolve, and execute through the same public package/runtime paths as third-party bundles. No engine branch recognizes either topology.

### R6. Durable Session Workflow State

1. Workflow source identity and revision are typed. A revision changes when canonical Workflow content changes; an installed revision also changes when its owning prepared bundle changes.
2. The event log records Workflow selection, run start, each Stage transition, child/resident Session binding, and terminal run outcome.
3. Projection replay reconstructs the selected Workflow and current/last run without a parallel read model.
4. A missing or changed selected source remains visible as unavailable/stale and cannot execute until explicitly reselected. The runtime does not silently run different content.
5. A nonterminal run from a dead runtime is reconciled to `interrupted` before the Session state is served or another run starts. No Stage side effect is replayed automatically.
6. Workflow switching changes only the selected Workflow. Existing user/assistant messages remain in the Session.

### R7. One Control Plane Across Surfaces

1. One app-owned Workflow control adapter provides list, info, select, run, state, and stale-run reconciliation.
2. The Agent `workflow` tool preserves its stable operation identity and calls that adapter. It does not bypass durable admission.
3. Backend CLI Workflow commands use the same compiler, catalog, input validation, and executor.
4. Native `/workflow` commands are intercepted before model admission. Supported forms are `list`, `info <name>`, `use <name>`, `run [<name>] [key=value ...]`, and `state`.
5. Native, legacy Compat, and v2 command routes share the same command decision. `/workflow` never becomes a model prompt, while unrelated slash commands keep current behavior.
6. A typed server/SDK state resource exposes selection, revision/staleness, stages, Agent counts, progress, child Session links, and outcome. Clients refresh from event-driven invalidation, not polling.

### R8. TypeScript TUI Presentation

1. Register Workflow presentation as a built-in `sidebar_content` plugin. Do not create the prohibited roster sidebar or a second polling client.
2. Show the selected Workflow and revision state even when no run is active.
3. During a run, show active stage title/id, graph level progress, active/total Agent counts, completed/total stages, and failed/cancelled/interrupted state.
4. Use existing run-tree navigation for child Session links and existing command/prompt paths for `/workflow` actions.
5. Preserve the existing TUI design tokens, compact terminal layout, and narrow-terminal behavior.

### R9. Release and Quality Contract

1. Each observable contract is introduced test-first with a verified expected failure, then the smallest passing implementation.
2. Mutation checks prove that fan-out is concurrent, resident actor reuse is real, Workflow state is replay-derived, `/workflow` does not call the model, bundle Agent closure is enforced, and the TUI is driven by typed synchronized state.
3. Run all touched-area gates, the workspace CI-equivalent Rust gate, process E2E where required, TypeScript TUI checks, a built executable smoke scenario, and a real TUI visual check.
4. Preserve an auditable baseline commit for the recovered `0.35.2` predecessor work, then deliver the completed breaking Workflow platform as `0.36.0` with aligned Cargo/package versions and changelogs.
5. Commit and push only after the applicable gate passes. Do not stage unrelated user files.

## Out of Scope

- Hardcoded engine policy for the first-party plan/implement/review topology.
- Arbitrary cyclic graphs; repetition is a loop Stage or resident mail activation.
- Automatic resume/replay of an in-flight Workflow after process death.
- A second scheduler, projection reducer, permission plane, plugin ABI, or TUI polling loop.
- Silent backward compatibility for the unpublished `stages:`/`needs:` format or old prepared bundle format.
- Publishing release tags or GitHub Releases unless the user separately requests a release.

## Child Deliverables and Order

1. **Recovered baseline** — verify, correct, version, commit, and push the existing Argus predecessor work.
2. **Workflow language and runtime** — compiled author format, deterministic joins, resident activation, failure/cancellation semantics.
3. **Workflow bundles and examples** — second payload kind, plugin contributions, Compat adapter packaging, installed catalog, first-party simple bundle, and full Argus example.
4. **Durable Workflow control** — events/projection, app control adapter, CLI/tool/server/SDK/native command integration and recovery.
5. **Workflow TUI** — event-driven typed resource and sidebar plugin presentation.
6. **Integration and release** — end-to-end mutation checks, docs/domain/ADR updates, `0.36.0`, full gates, executable/TUI smoke, final commit and push.

Children execute in that order except that bundle preparation and durable state implementation may proceed in parallel after the compiled Workflow interface is stable. The parent owns final cross-child review and release acceptance.

## Acceptance Criteria

1. A user-authored frontmatter-plus-`flowchart TD` document compiles into the expected deterministic linear or fan-out/fan-in plan; the old author format fails with a named source error before spawn.
2. A fan-out level overlaps provider work, its join receives bounded direct-predecessor evidence in graph order, and one failed branch follows the selected failure policy without being reported as overall success.
3. Two sequential nodes with one actor key produce one resident child Session and two durable mail/work boundaries; two same-level uses fail preflight; repeated Agent ids without actor keys remain distinct Sessions.
4. A WorkflowBundle cannot prepare or install unless it carries exactly one valid Workflow and the complete referenced Agent closure. A valid package installs as one row and resolves its Workflow, Agents, Skills, and tools through the normal runtime catalog.
5. The first-party simple bundle is selectable but not auto-selected; the full Argus example validates and installs as an ordinary package; process E2E and bounded smoke scenarios execute both.
6. Event replay alone reconstructs selected Workflow plus current/last Stage state. Restart reconciliation marks stale active work interrupted without replaying a Stage. Switching selection leaves all prior messages unchanged.
7. CLI, Agent tool, and native `/workflow` run the same compiled revision and return the same validation/status semantics. A provider-call counter remains zero for native list/info/use/state/run command handling itself.
8. The TUI sidebar renders selected, active, successful, failed, cancelled, interrupted, and stale/unavailable states from the typed synchronized resource; narrow layouts remain usable.
9. Applicable focused tests, mutation tests, Rust workspace gates, process E2E, TUI typecheck/tests, executable smoke, and actual TUI visual verification pass.
10. Root `CHANGELOG.md`, archived changelog history, `Cargo.toml`, `Cargo.lock`, and `packages/hya-tui-ts/package.json` agree on `0.36.0`; verified commits are pushed and the worktree contains only intentional user/Trellis state.
