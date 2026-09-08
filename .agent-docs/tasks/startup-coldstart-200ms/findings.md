# Findings — cold start ≤200ms pursuit

## Baseline (pre-opt, from startup-time-profile)

| Mode | shell_p50 | sync_p50 | backend_p50 |
|---|---:|---:|---:|
| F0 (empty DB) | ~2079 ms | ~1795 ms | ~18 ms |
| Realistic (84MB DB, 52 sessions) | ~6774 ms | ~6751 ms | ~720 ms |

## After slice (0.36.24, release + `dist/`, overlap default on)

| Mode | shell_p50 | sync_p50 | backend_p50 | bun_entry_p50 |
|---|---:|---:|---:|---:|
| F0 | **810 ms** | **532 ms** | **20 ms** | ~240 ms |
| Realistic | **859 ms** | **591 ms** | **282 ms** | ~208 ms |

Logs: `e2e-f0-after.log`, `e2e-realistic-after.log`.

## What landed

1. **Lazy bootstrap sessions** — `/tui/bootstrap` returns `sessions: []`; TUI `listSessions()` after paint.
2. **Prebundled TUI** — `bun run build` → `dist/boot.js`; launcher prefers dist.
3. **FIFO overlap default on** — Bun load ∥ backend listen; `HYA_STARTUP_OVERLAP=0` disables.
4. **Narrow Workflow recovery** — only sessions with `json_extract(payload,'$.type')='workflow_run_started'` are replayed (user DB had 0 such sessions → big listen win).
5. **sevenz vendor `rlib`-only** — stops Cargo output-filename collision that broke release builds.

## Why ≤200ms is still out of reach

F0 timeline (typical):

```
0    hya_ts_start
20   backend_listen
7    bun_spawn (overlap)
240  bun_entry (OpenTUI/native + boot chunk)   ← hard floor ~200–250ms
245  boot_got_url
508  theme_resolved
532  sync_complete
810  shell_paint   ← ~280ms provider/OpenTUI tree after sync
```

Even an empty Bun stub is ~8–9ms; **compiled OpenTUI graph alone is already ≥200ms** before sync/paint. Hitting ≤200ms wall-to-shell needs a thinner first paint (native/minimal shell) or `bun --compile` + deferred UI graph — not more backend micro-opts.

## Remaining levers (priority)

1. Defer heavy Solid providers / emit a minimal shell before full App (~280ms F0 gap).
2. Further cut realistic `backend_listen` (~282ms): profile `open_store` + `resolve_runtime` + residual recovery; optional listen-before-recovery gate.
3. Ship `dist/` in install/packaging so production always hits the bundled path.
4. Optional: `bun build --compile` experiment for bun_entry floor.
