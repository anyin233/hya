# 0.34.11

- Keep `SessionEngine` as the only agent runtime while activating an optional
  Bun Compat sidecar per public transient or resident Bundle activation;
  static-only Bundles remain process-free.
- Gate first work and model polling on initialize ACK plus declaration-drift
  validation, exact Tool/Hook declaration sets, and canonical `hook_refs`;
  preserve JSON-RPC request/notification roles, bound stderr, and make
  shutdown, terminate, reap, exit observation, and process loss explicit.
- Resolve the selected exact-path owning-bundle Tool/Hook closure and JS
  entrypoints from the captured TurnBinding; staged-vs-active isolation keeps
  staged Extensions inactive, while the existing permission plane runs before
  RPC and tool results return through the existing Harness event/projection path.
- Expand strict public archives to the exact root manifest plus existing-v1
  declared agent prompt/resource/Extension closure, with canonical
  directory/archive identity and generation-preserving failure. Self-contained
  selected JS entrypoints have no helper/import closure. Private activation, raw
  Rust extensions, Bundle-declared MCP, and unenforceable resource profiles
  remain unsupported.
- Reuse one healthy resident sidecar across mailbox messages while keeping its
  state volatile; fence running loss, preserve queued-after mail for fresh ACK,
  and make explicit stop final and idempotent. No TTL, heartbeat, idle reclaim,
  process adoption, or permission expansion is introduced.
- Add transient, resident, and disjoint multi-agent Bun authoring examples,
  the 0.34.11 runtime/CLI contract, and an updated built-in AgentBundle
  authoring skill.
