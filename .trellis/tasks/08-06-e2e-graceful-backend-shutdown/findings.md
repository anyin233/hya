# Findings — graceful e2e backend shutdown

## R2 answered: the backend had no signal handler, and that was the blocker

The PRD asked whether `hya-backend serve` handles SIGTERM, and said that if it
does not, that becomes the finding. It did not — the only signal handling in the
workspace was `crates/hya-ts/src/main.rs:354-360`.

This is decisive rather than incidental: **the default SIGTERM disposition also
skips atexit handlers**, exactly like SIGKILL. Changing only the harness (R1 in
isolation) would have produced a tidier kill that still flushed no `.profraw`,
leaving `hya-server` and `hya-core` at 0.0%. The harness change was necessary but
not sufficient.

## A second finding: the graceful path existed and was unreachable

`crates/hya-backend/src/serve.rs` already called `built.shutdown()` *after*
`axum::serve(...).await`. Without `.with_graceful_shutdown(...)`, `axum::serve`
never returns — so that teardown was **dead code in practice**. The clean-exit
machinery had been written and simply never given a trigger, which is why the
product change is 42 additive lines and zero deletions.

## The bug the plan did not anticipate, caught by measurement

The obvious implementation — register the signal lazily inside the shutdown
future — **does not work**, and fails in the way that would have looked fine:

| Probe | Exit | Elapsed |
| --- | ---: | ---: |
| SIGTERM ~7 ms after the listen line, lazy registration | **-15 (signal death)** | 7.5 ms |
| SIGTERM 500 ms after the listen line, lazy registration | 0 | 7.3 ms |
| SIGTERM immediately after the listen line, **eager** registration (×3) | **0, 0, 0** | 15.3 / 15.4 / 15.3 ms |

`tokio::signal` registers on first poll, which happens *after*
`println!("hya server listening on …")`. Any caller that parses the listen line
and signals promptly — precisely what the e2e harness does — loses the race and
gets the default disposition. Fixed by registering before the announcement.

Had this shipped, the coverage numbers would have been *intermittently* wrong
depending on how fast the harness signalled, which is far worse than being
uniformly zero.

## Verified by falsification, not by inspection

A regression test was run against the **pre-change** binary via the new
`HYA_E2E_BACKEND_BIN` override, and failed for the right reason:
`left: None, right: Some(0)` — a signal death means atexit handlers, and the
coverage flush, were skipped. The same test passes against the current binary.
This also exercised R5 end to end.

## R4 — Track P coverage, measured

`.profraw` files produced: **6 → 77**.

| Crate | Before | After |
| --- | ---: | ---: |
| **hya-server** | **0.0%** | **21.6%** |
| **hya-core** | **0.0%** | **51.2%** |
| **hya-client** | **0.0%** | **98.3%** |
| hya-backend | 23.9% | 31.7% |
| hya-store | 8.8% | 49.0% |
| hya-app | 0.2% | 37.8% |

`hya-server` and `hya-core` moving off 0.0% is the specific signal the PRD named,
and `hya-client` at 98.3% closes the "misleading 0.0%" artifact the baseline
flagged, since it is harness-only.

**Honesty caveat, recorded in the doc:** the Track P total (38.9% of 42,675
lines) has a **different denominator** than the workspace baseline (63,386
lines), because only crates reachable from `cargo test -p hya-e2e` are
instrumented. The two percentages are **not additive**; the per-crate rows are
the usable signal, not the total.

## Cost (PRD constraint)

**3.49 s → 3.54 s** for the same 27 scenarios — ~2 ms per backend, inside noise.
The grace period is a bounded poll that returns as soon as the child is reaped,
not a fixed sleep, which is what the PRD required.

## R3 — no orphans, verified deliberately

Normal run: `pgrep -af hya-e2e` matched nothing; the two stray `hya-backend`
processes present belong to other work (a `.worktrees/…` build and a
`target/release` build) and were byte-identical before and after, so Track P
neither created nor removed them.

Sabotaged run (`panic!("DELIBERATE SABOTAGE - orphan check")` injected into
`p01_session_prompt.rs`): the scenario failed as intended and `pgrep -af hya-e2e`
still matched nothing — `Drop` runs during unwind and the unconditional SIGKILL
escalation holds. Sabotage reverted, suite re-confirmed green.

`backend.rs:211` (the startup-failure path) was given the same group treatment
rather than left as a single-pid `kill()`: the child is now a group leader, and
with `HYA_DEFER_SIDEPLANES=0` it may already have spawned MCP children by the
time readiness times out, so a single-pid SIGKILL there would orphan exactly what
the group signal exists to catch.

## Residual risk, recorded not silently accepted

`SIGINT` is now handled alongside `SIGTERM`/`SIGHUP`. Because the signal streams
stay registered for the process lifetime, **a second Ctrl-C no longer falls back
to the default disposition** — if `built.shutdown()` ever hangs, an operator must
send SIGKILL rather than pressing Ctrl-C again.

This is a deliberate trade: draining on Ctrl-C matches `hya-ts` and is what makes
the clean exit work at all. It is recorded here because it is a real change in
operator experience, and because a future hang in `shutdown()` would present as
"Ctrl-C does nothing", which is confusing without this note.

## Version

This is a **user-visible product behaviour change**: `hya-backend serve` now
exits 0 on SIGTERM/SIGINT/SIGHUP instead of dying by signal, which changes the
exit status any supervisor or wrapper script observes. AGENTS.md line 39 requires
an explicit version bump for every fix or feature change, so this task bumps
`0.34.13 → 0.34.14` and rotates the changelog per AGENTS.md lines 40-41.

Verified safe to do: `.github/workflows/release.yml` triggers only on `v*.*.*`
tags, so a version bump on `main` publishes nothing. No tag is created here.

### The bump is a six-site chain, not a one-line edit

Bumping only `[workspace.package].version` **failed the gate**:
`crates/hya/tests/version_metadata.rs` is a release-consistency gate that pins
the version across the whole repo. It caught the incomplete bump immediately —
`left: "0.34.14", right: "0.34.13"`.

The full chain, all updated:

| Site | |
| --- | --- |
| `Cargo.toml` `[workspace.package].version` | 0.34.14 |
| `crates/hya/tests/version_metadata.rs` `EXPECTED_RELEASE` | 0.34.14 |
| `README.md` "workspace version \`…\`" | 0.34.14 |
| `CHANGELOG.md` first heading | `# 0.34.14` |
| `packages/hya-tui-ts/package.json` | 0.34.14 |
| `Cargo.lock` (21 `hya*` packages, via `cargo update -w`) | 0.34.14 |

Updating `EXPECTED_RELEASE` is **not** weakening an assertion — the constant is
the release stage marker the test exists to enforce, and its own message says so
("the … stage must bump the workspace package version before release metadata
checks"). The old root `CHANGELOG.md` was moved to
`docs/changes/CHANGELOG_0.34.13.md` per AGENTS.md lines 40-41, because the
release workflow reads the root file verbatim as the GitHub Release notes.

**Process note:** the background wrapper reported the failing gate as "exit code
0" because its final statement succeeded. The per-step exit codes are the real
signal — step 4 was `exit=101`. Always read the per-step exits, not the wrapper's
status.
