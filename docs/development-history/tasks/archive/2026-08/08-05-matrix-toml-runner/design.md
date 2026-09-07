# Design — Make matrix.toml a driven registry

## Why a checker and not a runner

The PRD scopes this to **validation**, not execution, and child 1 supplied the
evidence for why validation is the urgent half: two registered scenarios
(`I.nested`, `I.bundle_cli`) were **red** while `matrix.toml` advertised them as
coverage, and nothing noticed for weeks. A registry that lies about coverage is
worse than no registry — it is a false green.

Turning `matrix.toml` into a test *executor* is a much larger question (tag
selection, per-scenario timeouts, sharding). Not now.

## Home: `cargo xtask matrix-check`

`crates/xtask` already hosts repo tooling (`sync-compat`, `startup-bench`) with a
trivial dispatcher in `main.rs`. Adding `matrix-check` there costs one arm and
one module, and CI can call it as a normal step.

`xtask` needs a TOML parser; `toml` is already a workspace dependency, so this
is `toml = { workspace = true }` in `crates/xtask/Cargo.toml`.

## What it validates

The registry spans three tracks with very different verifiability. Being honest
about that difference is the whole design.

| Track | Points at | What can be checked |
| --- | --- | --- |
| **P** | `crates/hya-e2e/tests/*.rs` | path exists **and** bidirectional drift against real `#[tokio::test]` functions |
| **T** | `packages/hya-tui-ts/test/*.ts` | path exists only |
| **I** | other crates' tests | path exists only |

Bidirectional drift is restricted to Track P deliberately. Track T is
TypeScript — Rust attribute scanning does not apply. Track I points into other
crates whose test functions are *not* meant to map 1:1 to registry rows (they
are index pointers, explicitly "not duplicated"). Claiming to verify those would
be the clever-and-wrong option the PRD warns against.

### Checks

1. **Every `path` exists**, resolved from the repo root.
2. **IDs are unique and well-formed** (`T<major>.<minor>` or `I.<name>`).
3. **Track P forward drift**: every Track P entry's file contains at least one
   test function.
4. **Track P reverse drift**: every test function under `crates/hya-e2e/tests/`
   lives in a file some entry points at. An unregistered test is as much a
   registry failure as a phantom entry — that direction is what catches a new
   scenario landing without being registered.
5. **No undeclared numbering gaps** — see below.

## The many-to-many problem

Registry rows do **not** map 1:1 to test functions, and pretending otherwise
would produce constant false failures:

- `p01_session_prompt.rs` carries **two** IDs (`T0.1`, `T1.2`) in **one**
  function.
- `p02_tool_loop_fs.rs` carries **three** (`T1.3`–`T1.5`) in one function.
- `p03_permissions.rs` has one ID and **two** functions.

So the unit of correspondence is the **file**, not the function: an entry is
satisfied if its file exists and holds ≥1 test; a test is covered if its file is
referenced by ≥1 entry. Anything stricter is noise.

## Numbering gaps become explicit

`T1.1`, `T1.6`, `T2.4`–`T2.6` currently appear nowhere in the repo — undeclared
holes that nobody tracks. Fix the *class*: add a `[[retired]]` table.

```toml
[[retired]]
id = "T1.1"
reason = "folded into T0.1 — backend boot is asserted by the session-prompt scenario"
```

The checker then requires every ID in a track's numeric range to be either
**used** or **retired**, and fails on anything else. After that, a hole cannot
reappear silently.

**Coordination with child 3:** `08-05-e2e-swarm-tool-scenarios` is claiming
`T2.4`, `T2.5`, `T2.6` for real scenarios and adding `T2.9`–`T2.11`. So the
gaps this task must actually resolve are **`T1.1` and `T1.6`**, plus whatever
child 3 leaves unused. Land after child 3, or re-check the live set at
implementation time — do not hard-code today's list.

## Detecting Rust test functions conservatively

A regex over Rust source is fragile, and a false CI failure here would erode
trust in the gate this repo just built. So: scan for a line whose trimmed form
starts with `async fn ` or `fn ` **and** which is preceded, within the previous
few non-empty lines, by an attribute containing `test`. Files under
`crates/hya-e2e/tests/` are a small, controlled, uniformly-styled set — all 19
current functions are top-level `async fn` with `#[tokio::test]`.

If the scan finds **zero** functions in a file that the registry references,
that is reported as a failure rather than silently passing — a parser that
quietly finds nothing is indistinguishable from a file with no tests, and the
loud version is the safe default.

## CI wiring

One step, alongside the gate child 2 just built, carrying the same
`if: ${{ !cancelled() }}` so it reports independently:

```yaml
- name: matrix registry check
  if: ${{ !cancelled() }}
  run: cargo run -p xtask -- matrix-check
```

It is cheap (file reads only, no test execution), so it can sit early without
the cost concerns that shaped child 2.

## Risks

| Risk | Mitigation |
| --- | --- |
| False failure from the regex-ish scan | Restrict to `crates/hya-e2e/tests/`; require ≥1 match per referenced file; verify against the known-good current tree (19 functions) before wiring to CI |
| Gap rule fights child 3's in-flight IDs | Read the live registry at implementation time; land after child 3 |
| Checker becomes a maintenance burden | Keep it to file-level correspondence; resist per-function mapping |

## Out of scope

- Executing scenarios by tag or ID.
- Enforcing `timeout_secs` at runtime.
- Backfilling retired IDs as real scenarios — the PRD requires a decision and a
  record, not implementation.
