# Progress

## Done
1. Lazy `/tui/bootstrap` sessions — GREEN
2. Prebundled TUI (`dist/boot.js`) — GREEN
3. FIFO overlap **default on** — GREEN (`cargo test -p hya-ts`)
4. Workflow recovery candidate filter — GREEN (`workflow_recovery`)
5. sevenz `rlib`-only vendor patch — unblocks release builds
6. CHANGELOG 0.36.24; archived 0.36.23 → `docs/changes/`
7. Re-profile: F0 shell **810ms** / sync **532ms**; realistic shell **859ms** / sync **591ms** / backend **282ms**

## Verdict vs 200ms
Not achieved. OpenTUI `bun_entry` alone ~240ms. Next: defer providers / minimal first paint; ship `dist/` in packaging.

## Verify run
- `cargo test -p hya-store --test workflow_recovery`
- `cargo test -p hya-ts`
- release `hya-ts` + `hya-backend` rebuilt
- `bun run build` in `packages/hya-tui-ts`
