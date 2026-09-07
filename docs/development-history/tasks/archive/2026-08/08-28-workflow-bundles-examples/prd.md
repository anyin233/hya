# Implement Workflow Bundles and Examples

## Outcome

Make one complete Workflow plus its Agent closure a first-class `.hyabundle` payload, route bundle tools/Skills through the shared plugin contribution interface, and ship the required first-party/example bundles.

## Requirements

- Preserve `AgentBundle` as exactly one Agent and add a distinct `WorkflowBundle` source/prepared payload behind the same package/install/catalog interface.
- Bump prepared format to v2 without a v1 shim; keep AgentBundle source compatibility and reinstall diagnostics.
- Compile one Workflow at prepare time and validate its complete reachable non-built-in Agent closure, package closure, canonical digest, paths, resources, and collisions.
- Index all packaged Agents and the Workflow in immutable runtime catalogs while preserving owner-scoped resource/sidecar behavior and pinned old bindings.
- Deepen plugin initialization with one typed contribution set for tools, Skills, hooks, and adapters; remove direct core parsing of bundled Skills.
- Package and resolve the production Compat adapter outside the repository.
- Ship a non-auto-selected first-party `plan -> impl -> review` WorkflowBundle and an installable full Argus example with ordinary fan-out/fan-in.

## Acceptance Criteria

- [x] AgentBundle one-Agent tests remain green and WorkflowBundle invalid states are unrepresentable after preparation.
- [x] Prepared-v2 and public package bytes are deterministic and fail closed on missing/extra/tampered Workflow, Agent, resource, or extension files.
- [x] One registry transaction installs one Workflow plus every Agent; failure leaves the prior row and generation unchanged.
- [x] Plugin tool/Skill declarations must exactly match selected prepared ids/content digests before activation.
- [x] An installed distribution starts a real Compat sidecar without workspace paths or environment overrides.
- [x] The simple first-party Workflow is selectable but not auto-selected; the full Argus example installs and resolves as an ordinary package.
- [x] Focused bundle/plugin/store/app/backend/installer gates and named mutation tests pass.

## Exclusions

- No Session selection Events, slash routes, SDK state, or TUI renderer in this child.
