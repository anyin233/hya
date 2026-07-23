# Implement: startup performance

## Order

1. **WP-0** Measurement harness + `HYA_STARTUP_TRACE` marks (no behavior change)
2. **WP-2** TUI shell first paint (theme default, shellReady, KV/theme gates)
3. **WP-1** Backend listen-before MCP/plugin + hot-register
4. Re-measure F0 ledger
5. **WP-4** Bootstrap/complete path cheap snapshots / optional batch
6. **WP-3B** Parallel Bun only if still over budget
7. **WP-5** CI soft gate
8. **WP-6** Bundle split / store micro-opts only if needed

## Validation

```sh
cargo test -p xtask
cargo test -p hya-app -p hya-backend -p hya-sdk -p hya-ts
cd packages/hya-tui-ts && bun test && bun run typecheck
cargo xtask startup-bench --mode backend --runs 5
cargo xtask startup-bench --mode e2e --runs 5   # when PTY path ready
```

## Rollback

Per-WP env flags or revert single atomic commits.
