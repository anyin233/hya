# Implementation Plan

1. Add red source/prepare tests proving AgentBundle remains singular and WorkflowBundle requires exactly one compiled Workflow plus Agent closure.
2. Introduce separate strict manifests and `PreparedInstallableBundle` v2. Migrate every prepared literal/caller; keep source AgentBundle fixtures green and reject v1 registry bytes with reinstall state.
3. Add red canonical digest/package closure tests for Workflow source, every Agent prompt, resource, and extension. Extend the deterministic package writer/inspector.
4. Add red catalog tests for multi-Agent locations, Workflow qualified/bare resolution, collisions, semantic identity, and owner resource views. Generalize indexes without optional invalid states.
5. Add red atomic registry tests for all Agent reserved/collision checks and invalid Workflow compile. Keep registry SQL unchanged; move orchestration only where dependency direction requires.
6. Add red plugin contribution tests for Skill declaration/content/digest equality and zero-Skill compatibility. Extend protocol/host/Compat adapter and route prepared static Skills through the same contribution interface. Remove direct core Skill parsing.
7. Add red install-layout test that launches a real Compat sidecar outside the repository. Build/package/resolve the adjacent production adapter and update installer/release rollback paths.
8. Add red runtime refresh tests proving Workflow plus Agents publish together, old bindings stay pinned, and install causes no selection/run.
9. Author the simple and full Argus source trees. Add deterministic prepare/package tests; merge the simple prepared bundle into the first-party catalog and keep Argus as an inactive release example.
10. Extend bundle list/info output to show payload kind, Workflow id, Agent closure, and state without regressing AgentBundle output.
11. Run the three bundle/plugin mutations, revert them, then run the focused child gate and installer smoke.
12. Run Trellis check review, correct findings, and finish only with no workspace-path dependency in installed execution.

Rollback: no SQL migration is applied. If this child cannot pass, leave prepared format/catalog edits uncommitted and retain the `0.35.2` runtime. Never add a v1 compatibility shim to mask incomplete v2 work.
