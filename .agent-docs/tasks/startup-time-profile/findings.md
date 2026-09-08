# Findings — hya interactive startup profile

Machine: Linux development host. Date: 2026-09-08.
Binaries: `target/release/{hya,hya-ts,hya-backend}` @ workspace `0.36.23`.
Harness: `.agent-docs/tasks/startup-time-profile/profile-e2e.mjs` (`HYA_STARTUP_TRACE=1`, PTY via `/usr/bin/script`).

Clock: wall deltas from harness `Date.now()` at spawn; Bun `mono_ms` is `performance.now()` inside the TUI process.

## Verdict

Cold start is **not** backend-bound on a clean fixture. It is dominated by:

1. **Bun loading `src/main.tsx` as TypeScript source** (~1.4–1.7 s) — launcher never uses the existing `bun build` `dist/` output.
2. On this machine’s real state: **`/tui/bootstrap` session hydration** (~3.5–4.1 s) replaying up to 100 sessions against a **201 535-row `event_log`** / **84 MB** `sessions.db`.
3. **Owned-mode sequencing**: `hya-ts` awaits backend listen before spawning Bun, so backend cost cannot overlap Bun load.

Product budgets from historical PRD (shell ≤100 ms from Bun entry; sync ≤500 ms from `hya` start) are **missed by ~4–13×** on F0 for shell/sync wall clocks, and much worse on realistic state.

## Startup chain (owned interactive)

```text
hya (exec) → hya-ts
  → resolve runtime / backend bin
  → spawn hya-backend serve   ──WAIT──→ listen URL
  → spawn bun src/main.tsx --url …
       → transpile+load ~182 modules   ── ~1.4 s
       → bun_entry → theme_resolved → SyncProvider bootstrap
       → App shell_paint (+ plugin host async)
```

Existing marks: `hya_ts_start`, `backend_spawn`, `backend_listen`, `bun_entry`, `theme_resolved`, `shell_paint`, `plugin_host_done`, `sync_partial`, `sync_complete`.

## Measured waterfalls

### A. F0 (isolated HOME, `HYA_DB=""`, minimal config) — n=5

| Mark | p50 Δ from spawn | Notes |
| --- | ---: | --- |
| `hya_ts_start` | 6 ms | |
| `backend_listen` | **18 ms** | in-memory store |
| `bun_entry` | **1736 ms** | `mono_ms` ≈ 1715 → almost all is Bun process+module load |
| `theme_resolved` | 1766 ms | +~30 ms after entry |
| `sync_complete` (bundle) | **1795 ms** | bootstrap itself ~30 ms once Bun is up |
| `shell_paint` | **2079 ms** | ~280 ms after sync (provider tree below SyncProvider) |

Summary: **shell p50 ≈ 2.08 s**, **sync p50 ≈ 1.80 s**. Backend is ~1% of wall time.

### B. Realistic (real `~/.config/hya` + 84 MB `sessions.db`) — n=3

| Mark | p50 Δ from spawn |
| --- | ---: |
| `backend_listen` | **720 ms** |
| `bun_entry` | **2354 ms** |
| `sync_complete` | **6751 ms** |
| `shell_paint` | **6774 ms** |

### C. Backend-only decompositions

| Scenario | ready |
| --- | ---: |
| Isolated HOME + empty db + minimal config | **16–28 ms** |
| Real config + empty temp db (`startup-bench`) | **p50 112 ms / p95 248 ms** |
| Isolated + copy of 84 MB `sessions.db` | **~666 ms** |
| Tiny `hya-backend --help` (warm) | &lt; 10 ms |

MCP `codegraph` is configured but deferred (`HYA_DEFER_SIDEPLANES` default on). No `plugins:` in config; `PluginHost::connect_all` still runs on the listen path with an empty list (cheap here).

### D. Bun module load (isolated)

| Workload | wall |
| --- | ---: |
| Empty Bun stub | ~230 ms |
| `bun src/main.tsx` (fail fast, no `--url`) | **~1.63–1.69 s** |
| `await import("./src/main.tsx")` | **~1410–1490 ms** |
| `bun run build` → `dist/main.js` (1.0 MB, 182 modules) | **~387–404 ms** to same fail-fast |
| **Prebundle save** | **~1.2–1.3 s** |

`packages/hya-tui-ts` already has `"build": "bun build src/main.tsx --outdir dist ..."`, but `hya-ts` always launches `src/main.tsx`.

### E. `/tui/bootstrap` cost

| DB | `/tui/bootstrap` | `/session` | payload |
| --- | ---: | ---: | --- |
| Empty | ~20 ms | — | ~78 KB |
| 84 MB / 52 sessions / 201 535 events | **~3.57–4.08 s** | **~3.92 s** | ~102 KB, 52 sessions |

Root cause in `crates/hya-server/src/compat/tui.rs`: bootstrap `take(100)` then **`load_session` per row** (full projection replay) before first paint/sync-complete.

## Critical-path accounting (approximate)

### F0 (~2.08 s to shell_paint)

| Slice | ms | Share | On critical path? |
| --- | ---: | ---: | --- |
| hya-ts + backend listen | ~18 | 1% | yes (sequential today) |
| Bun runtime + **TS graph load** | ~1710 | **82%** | yes |
| theme + bootstrap HTTP | ~60 | 3% | yes |
| Provider tree → App `shell_paint` | ~280 | 13% | yes |
| Plugin host | after paint (async) | — | overlapped already |

### Realistic (~6.8 s to shell)

| Slice | ms | Share |
| --- | ---: | --- |
| Backend listen (large DB open/recovery) | ~720 | 11% |
| Bun TS load (after listen) | ~1600 | 24% |
| Bootstrap session hydration | **~3800–4300** | **~60%** |
| Remaining UI | ~20 | &lt;1% |

## Overlap candidates (async / parallel)

| Opportunity | What is sequential today | Recoverable time | Notes |
| --- | --- | ---: | --- |
| **Spawn Bun while backend starts** | `ServerHandle::spawn_hya_backend` awaited before Bun | F0 ~**18 ms**; realistic ~**700 ms** | Bun load (~1.6 s) hides backend if overlapped; need connect-retry or splash until URL ready |
| **Parallelize bootstrap internals** | `load_session` loop is effectively serial-heavy I/O | part of ~4 s | Better: don’t load full projections (see lazy) |
| **Shell paint vs sync** | Already partially concurrent; F0 still paints after sync | ~0 on realistic | `shell_paint` mark ≠ first pixel; StartupLoading still gates chrome on `ready` |
| MCP connect | Already deferred by default | 0 on critical path | Keep |

Upper bound if Bun∥backend only: realistic wall ≈ `max(720, 1600) + 4000 ≈ 5.6 s` (still bootstrap-bound). F0 ≈ `max(18, 1710) + 340 ≈ 2.05 s` (almost unchanged until prebundle).

## Lazy-load / defer candidates

| Opportunity | Blocks today | Recoverable time | Approach |
| --- | --- | ---: | --- |
| **Ship / run `dist/main.js` instead of `src/main.tsx`** | TS transpile of 182 modules | **~1.2–1.3 s** | Wire launcher + install packaging to `dist/`; keep src for dev via env |
| **Bootstrap: metadata-only session list** | `load_session` × ≤100 | **~3.5–4.1 s** realistic | List id/title/mtime only; hydrate on session open |
| **Defer large-DB recovery past listen** | open + recover before bind | **~600–700 ms** realistic | Listen after open store; move `recover_*` to background with readiness flag |
| **Lazy provider tree below first chrome** | ~280 ms F0 Sync→App | **~200–300 ms** F0 | Paint shell skeleton before Local/Dialog/Frecency/Editor providers |
| **Command full catalog** | Not on bootstrap (templates omitted) | small | `/command` ~208 KB but ~4 ms — low priority |
| **MCP `codegraph`** | Deferred | 0 to listen | Already OK; ensure status UX |

## Already done (don’t re-litigate)

- Theme probe not blocking (`HYA_WAIT_THEME` off).
- Shell routes not gated on plugin host (`HYA_SYNC_PLUGIN_START` off).
- MCP deferred (`HYA_DEFER_SIDEPLANES` default on).
- `/tui/bootstrap` single-RTT path exists (used; `detail: bundle`).

## Recommended optimization order (impact × difficulty)

1. **Lazy session list in `/tui/bootstrap`** — largest realistic win (~4 s); correct product semantics for home screen.
2. **Launch prebundled `dist/main.js`** — largest F0/clean win (~1.2 s); build already exists.
3. **Overlap Bun spawn with backend listen** — realistic ~0.7 s; F0 negligible; needs URL handoff design.
4. **Defer DB recovery / archive old `event_log`** — realistic ~0.6–0.7 s listen; ops hygiene for 84 MB DB.
5. **Defer heavy TUI providers past first paint** — ~0.3 s F0 polish toward 100 ms-from-Bun-entry budget (still needs #2).

## Evidence commands

```sh
HYA_BACKEND_BIN=$PWD/target/release/hya-backend cargo run -p xtask --release -- startup-bench --mode backend --runs 5
bun .agent-docs/tasks/startup-time-profile/profile-e2e.mjs --runs 5 --mode f0
bun .agent-docs/tasks/startup-time-profile/profile-e2e.mjs --runs 3 --mode realistic
# in packages/hya-tui-ts:
bun run build && /usr/bin/time -f '%e' bun dist/main.js ; /usr/bin/time -f '%e' bun src/main.tsx
```
