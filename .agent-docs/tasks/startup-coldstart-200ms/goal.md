# Goal

Drive interactive `hya` cold start toward ≤200ms wall from process start to shell paint / sync-complete on F0, and eliminate multi-second realistic-path costs.

## Scope
1. Lazy `/tui/bootstrap` session list (no per-session full replay)
2. Ship/run prebundled or compiled TUI (solid-plugin bundle)
3. Overlap Bun/TUI spawn with backend listen
4. Defer store recovery past HTTP listen where safe
5. Measure with existing startup trace harness

## Constraint
OpenTUI compiled floor ≈260ms to `bun_entry` on this host; if ≤200ms full shell is unreachable without thinner first paint, document the floor and ship the maximum reduction.

## Acceptance
- F0 profile re-run with numbers
- Realistic bootstrap no longer ~4s
- Tests for bootstrap session laziness
- Verification gates for touched crates
