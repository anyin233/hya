# Implement Workflow Language and Runtime

## Outcome

Replace the narrowed YAML Stage-list contract with one compiled Mermaid-inspired Workflow document and extend the governed executor with truthful transient, loop, and resident Stage semantics.

## Requirements

- Add the shared `hya-workflow` compiler module with one validated `CompiledWorkflow` interface.
- Accept Markdown frontmatter plus the exact simplified `flowchart TD` grammar defined by the parent design; reject the old `stages:`/`needs:` format.
- Derive deterministic Stage order, direct-predecessor joins, required input interpolation, failure policy, loop/verifier rules, and canonical revision in the compiler.
- Preserve governed same-level transient fan-out and target-Agent roster/resource/sidecar resolution.
- Route explicit actor keys through `ResidentSupervisor` and durable mail. Do not infer reuse from repeated Agent ids.
- Make completion, collect-all failure, fail-fast, and cancellation match the parent design.
- Remove obsolete parser/planner exports and migrate every current compiler/executor caller and fixture.

## Acceptance Criteria

- [ ] Linear and fan-out/fan-in documents compile to exact deterministic plans; malformed and old-format documents fail with source-located typed errors before spawn.
- [ ] Direct predecessor evidence is automatic, ordered, typed, UTF-8 safe, and bounded to 4,000 bytes per predecessor.
- [ ] Same-level transient Stages overlap provider execution and remain one governed batch.
- [ ] Two sequential nodes sharing an actor key use one resident Session and two durable mail/work boundaries; same-level reuse fails preflight.
- [ ] Repeated transient Agent ids still produce distinct Sessions.
- [ ] Loop, resident failure, `fail_fast`, `collect_all`, and cancellation behavior pass focused and mutation tests.
- [ ] `hya-workflow` and touched `hya-core` gates pass with no warnings.

## Exclusions

- No WorkflowBundle, Session Workflow Events, HTTP command routes, SDK DTOs, or TUI presentation in this child.
