# Install Script Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a source-checkout installer that puts both real hya binaries on PATH and directs users to configure provider API access.

**Architecture:** A root Bash installer builds the existing `hya` frontend/native-backend binary and `hya-backend` CLI/API binary, preflights permissions before Cargo work, installs through temp files with backup/restore rollback until verification passes, verifies the installed `hya` is the one found on PATH, and prints provider setup guidance. Dry-run shell tests pin the behavior without writing to system directories.

**Tech Stack:** Bash, Cargo workspace, existing Rust crates `hya` and `hya-backend`, shell smoke tests.

## Global Constraints

- `hya` must be installed from `crates/hya`; do not alias it to `hya-backend`.
- `hya-backend` must also be installed for login, headless execution, and server/API modes.
- Existing CLI surface: `hya-backend login <provider> <token>` is defined in `crates/hya-backend/src/cli_args.rs:109-115`; `hya-backend models` is defined in `crates/hya-backend/src/cli_args.rs:127-137`.
- Default install location is `/usr/local/bin` via `--prefix /usr/local`; `--bin-dir` overrides it.
- Release builds use `cargo build --locked --profile release -p hya -p hya-backend --bins` and `target/release`.
- Dev/debug builds use plain `cargo build --locked -p hya -p hya-backend --bins` and `target/debug`; never pass `--profile debug`.
- Before building, preflight target permissions; if `$bin_dir` or the nearest existing parent is not writable, fail before Cargo with `rerun with sudo ./install.sh` or `use --bin-dir ~/.local/bin` guidance.
- Rollback contract: copy new binaries to `$bin_dir/.hya.tmp.$$` and `$bin_dir/.hya-backend.tmp.$$`; before replacing existing binaries, move old binaries to `$bin_dir/.hya.bak.$$` and `$bin_dir/.hya-backend.bak.$$`; on any ERR/INT/TERM before all verification passes, remove new partial installs and restore backups.
- After install, `command -v hya` must resolve to the installed `$bin_dir/hya`; fail with the exact PATH fix if it resolves elsewhere or is missing.
- `install.sh` must be executable so users can run `./install.sh` directly.
- Update `[workspace.package].version` from current `0.28.3` to `0.28.4`.
- Move the current root `0.28.3` changelog to `docs/changes/CHANGELOG_0.28.3.md`; root `CHANGELOG.md` must contain only `0.28.4` notes.
- Refresh `Cargo.lock` after the version bump so the installer's own `cargo build --locked` can run.
- Do not add dependencies.

---

## File Structure

- Create: `install.sh` — root source installer for both real binaries and setup guidance.
- Create: `tests/install_script.sh` — shell tests for installer CLI/dry-run behavior.
- Modify: `Cargo.toml` — workspace version bump.
- Modify: `Cargo.lock` — workspace package version entries refreshed after the version bump.
- Modify: `CHANGELOG.md` — newest-version-only release notes.
- Create: `docs/changes/CHANGELOG_0.28.3.md` — archived previous root changelog.

### Task 1: Red install-script contract test

**Files:**
- Create: `tests/install_script.sh`

**Interfaces:**
- Consumes: future root executable `./install.sh`.
- Produces: regression checks used by Tasks 2-3.

- [x] **Step 1: Create failing shell test**

Create `tests/install_script.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

contains() {
  local haystack=$1
  local needle=$2
  [[ "$haystack" == *"$needle"* ]] || fail "expected output to contain: $needle"
}

not_contains() {
  local haystack=$1
  local needle=$2
  [[ "$haystack" != *"$needle"* ]] || fail "expected output not to contain: $needle"
}

help=$(bash ./install.sh --help)
[[ -x ./install.sh ]] || fail "install.sh must be executable"
contains "$help" "--prefix DIR"
contains "$help" "--bin-dir DIR"
contains "$help" "--profile release|dev|debug"
contains "$help" "--dry-run"

dry_run=$(bash ./install.sh --dry-run --prefix /tmp/hya-install-test --profile debug)
contains "$dry_run" "Permission preflight: /tmp/hya-install-test/bin"
[[ "$dry_run" == *"Permission preflight: /tmp/hya-install-test/bin"*"cargo build --locked -p hya -p hya-backend --bins"* ]] || fail "permission preflight must run before cargo build"
contains "$dry_run" "cargo build --locked -p hya -p hya-backend --bins"
not_contains "$dry_run" "--profile debug"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya.tmp"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya-backend.tmp"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya.bak"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya-backend.bak"
contains "$dry_run" "/tmp/hya-install-test/bin/hya"
contains "$dry_run" "/tmp/hya-install-test/bin/hya-backend"
contains "$dry_run" "PATH check: command -v hya must resolve to /tmp/hya-install-test/bin/hya"
contains "$dry_run" 'XDG_CONFIG_HOME/hya/config.yaml'
contains "$dry_run" 'hya-backend login anthropic "$ANTHROPIC_API_KEY"'
contains "$dry_run" "hya-backend models"
contains "$dry_run" "hya"
```

- [x] **Step 2: Run test to verify RED**

Run: `bash tests/install_script.sh`

Observed: FAIL with `bash: ./install.sh: No such file or directory`, which is the expected missing-feature failure.

### Task 2: Installer CLI skeleton and profile mapping

**Files:**
- Create: `install.sh`

**Interfaces:**
- Produces: argument variables `prefix`, `bin_dir`, `profile`, `dry_run`; arrays `build_cmd`; string `target_dir`.

- [ ] **Step 1: Create executable script with usage and helpers**

Create `install.sh`, then run `chmod +x install.sh`:

```bash
#!/usr/bin/env bash
set -Eeuo pipefail

prefix=/usr/local
bin_dir=""
profile=release
dry_run=0

usage() {
  cat <<'USAGE'
Usage: ./install.sh [OPTIONS]

Build and install hya from this source checkout.

Options:
  --prefix DIR                 Install into DIR/bin (default: /usr/local)
  --bin-dir DIR                Install directly into DIR; overrides --prefix
  --profile release|dev|debug  Cargo build profile (default: release)
  --dry-run                    Print actions without building or installing
  -h, --help                   Show this help

Installs both binaries:
  hya          user-facing TUI/frontend with native in-process hya backend
  hya-backend  backend CLI/API for login, exec, serve, and models
USAGE
}

say() {
  printf '%s\n' "$*"
}

run() {
  say "+ $*"
  if [[ "$dry_run" -eq 0 ]]; then
    "$@"
  fi
}
```

- [ ] **Step 2: Add argument parser**

Append:

```bash
while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix)
      [[ $# -ge 2 ]] || { echo "--prefix requires DIR" >&2; exit 2; }
      prefix=$2
      shift 2
      ;;
    --bin-dir)
      [[ $# -ge 2 ]] || { echo "--bin-dir requires DIR" >&2; exit 2; }
      bin_dir=$2
      shift 2
      ;;
    --profile)
      [[ $# -ge 2 ]] || { echo "--profile requires release, dev, or debug" >&2; exit 2; }
      profile=$2
      shift 2
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done
```
- [ ] **Step 3: Add profile mapping and bin-dir resolution**

Append:

```bash
case "$profile" in
  release)
    build_cmd=(cargo build --locked --profile release -p hya -p hya-backend --bins)
    target_dir=target/release
    ;;
  dev|debug)
    build_cmd=(cargo build --locked -p hya -p hya-backend --bins)
    target_dir=target/debug
    ;;
  *)
    echo "unsupported profile '$profile'; use release, dev, or debug" >&2
    exit 2
    ;;
esac

if [[ -z "$bin_dir" ]]; then
  bin_dir=${prefix%/}/bin
fi
cd "$(dirname "$0")"
if [[ "$bin_dir" != /* ]]; then
  bin_dir="$(pwd -P)/$bin_dir"
fi


```

- [ ] **Step 4: Check partial expected state**

Run: `bash ./install.sh --help`

Expected: usage prints and exits 0. The full `tests/install_script.sh` still fails until later steps add dry-run output.

### Task 3: Permission preflight, rollback install, verification, guidance

**Files:**
- Modify: `install.sh`

**Interfaces:**
- Consumes: variables from Task 2.
- Produces: complete installer behavior required by the test.

- [ ] **Step 1: Add backup/restore state and error trap**

Append immediately after Task 2's `cd "$(dirname "$0")"` resolution block. Task 3 snippets must remain in this order: state/functions, preflight function, build+mkdir+trap+install, verification, guidance.

```bash
tmp_hya="$bin_dir/.hya.tmp.$$"
tmp_backend="$bin_dir/.hya-backend.tmp.$$"
bak_hya="$bin_dir/.hya.bak.$$"
bak_backend="$bin_dir/.hya-backend.bak.$$"
rollback_enabled=0
install_complete=0
had_hya=0
had_backend=0
placed_hya=0
placed_backend=0

cleanup_leftovers() {
  rm -f "$tmp_hya" "$tmp_backend"
  [[ -d "$bin_dir" ]] || return 0
  if [[ "$install_complete" -eq 1 ]]; then
    rm -f "$bak_hya" "$bak_backend"
  fi
}

restore_install() {
  if [[ "$rollback_enabled" -eq 0 || "$install_complete" -eq 1 ]]; then
    return 0
  fi

  [[ "$placed_hya" -eq 1 ]] && rm -f "$bin_dir/hya"
  [[ "$placed_backend" -eq 1 ]] && rm -f "$bin_dir/hya-backend"
  if [[ "$had_hya" -eq 1 && -e "$bak_hya" ]]; then
    mv -f "$bak_hya" "$bin_dir/hya"
  fi
  if [[ "$had_backend" -eq 1 && -e "$bak_backend" ]]; then
    mv -f "$bak_backend" "$bin_dir/hya-backend"
  fi
}

on_error() {
  local status=$?
  restore_install
  cleanup_leftovers
  exit "$status"
}


# The -E/errtrace shell option is required so ERR rollback fires for failures inside run() and other functions. Install the ERR trap only after mkdir -p "$bin_dir" succeeds, so rollback cleanup never masks a directory-creation failure before install state exists.
```

- [ ] **Step 2: Add permission preflight**

Append:

```bash
preflight_permissions() {
  say "Permission preflight: $bin_dir"
  if [[ "$dry_run" -ne 0 ]]; then
    return 0
  fi

  local probe=$bin_dir
  while [[ ! -e "$probe" ]]; do
    probe=$(dirname "$probe")
  done

  if [[ ! -d "$probe" || ! -w "$probe" ]]; then
    echo "Cannot write to $bin_dir." >&2
    echo "Rerun with: sudo ./install.sh" >&2
    echo "Or use a user-writable directory: ./install.sh --bin-dir \"$HOME/.local/bin\"" >&2
    exit 1
  fi
}
```

- [ ] **Step 3: Add build, backup, and install commands**

Append:

```bash
say "Installing hya to $bin_dir"
say "Rollback backup path: $bak_hya"
say "Rollback backup path: $bak_backend"
preflight_permissions
run "${build_cmd[@]}"
run mkdir -p "$bin_dir"
trap on_error ERR INT TERM
run install -m 0755 "$target_dir/hya" "$tmp_hya"
run install -m 0755 "$target_dir/hya-backend" "$tmp_backend"
rollback_enabled=1
if [[ -e "$bin_dir/hya" ]]; then
  run mv -f "$bin_dir/hya" "$bak_hya"
  had_hya=1
fi
if [[ -e "$bin_dir/hya-backend" ]]; then
  run mv -f "$bin_dir/hya-backend" "$bak_backend"
  had_backend=1
fi
placed_hya=1
run mv -f "$tmp_hya" "$bin_dir/hya"
placed_backend=1
run mv -f "$tmp_backend" "$bin_dir/hya-backend"
```

- [ ] **Step 4: Add installed-path and PATH verification**

Append:

```bash
if [[ "$dry_run" -eq 0 ]]; then
  "$bin_dir/hya" --version >/dev/null
  "$bin_dir/hya-backend" --help >/dev/null
  resolved=$(command -v hya 2>/dev/null || true)
  if [[ "$resolved" != "$bin_dir/hya" ]]; then
    echo "hya is not first on PATH. Add this to your shell profile: export PATH=\"$bin_dir:\$PATH\"" >&2
    echo "expected: $bin_dir/hya" >&2
    echo "resolved: ${resolved:-<missing>}" >&2
    false
  fi
  install_complete=1
  cleanup_leftovers
  say "hya is on PATH: $resolved"
else
  say "+ $bin_dir/hya --version"
  say "+ $bin_dir/hya-backend --help"
  say "+ PATH check: command -v hya must resolve to $bin_dir/hya"
fi
```

- [ ] **Step 5: Add API setup guidance**

Append:

```bash
cat <<'GUIDANCE'

API setup:
  hya works offline by default. To use a live provider, create:
    $XDG_CONFIG_HOME/hya/config.yaml
  or, if XDG_CONFIG_HOME is unset:
    ~/.config/hya/config.yaml

  Minimal Anthropic config:
    default_model: claude-sonnet-4-6
    providers:
      anthropic:
        kind: anthropic
        base_url: https://api.anthropic.com/v1
        api_key: "{env:ANTHROPIC_API_KEY}"
        models: [claude-sonnet-4-6]

  Then run:
    hya-backend login anthropic "$ANTHROPIC_API_KEY"
    hya-backend models
    hya
GUIDANCE
```

- [ ] **Step 6: Run focused script checks**

Run: `bash tests/install_script.sh && bash -n install.sh && bash -n tests/install_script.sh`

Expected: PASS and exit 0.

### Task 4: Version and changelog

**Files:**
- Modify: `Cargo.toml`
- Modify: `CHANGELOG.md`
- Modify: `Cargo.lock`
- Create: `docs/changes/CHANGELOG_0.28.3.md`

**Interfaces:**
- Consumes: current root changelog content for version `0.28.3`.
- Produces: release-rule-compliant current and archived changelog files.

- [ ] **Step 1: Archive current changelog**

Create `docs/changes/CHANGELOG_0.28.3.md` with exactly the current root changelog content:

```markdown
# 0.28.3

- Fixed GitHub CI on Rust 1.96 by updating SDK event parsing for the new clippy lint.
- Fixed the release workflow after the binary/package rename from `yaca` to `hya`.
```

- [ ] **Step 2: Bump workspace version**

Change `Cargo.toml` line under `[workspace.package]`:

```toml
version = "0.28.4"
```

- [ ] **Step 3: Refresh Cargo.lock workspace package versions**

Run: `cargo metadata --format-version=1 >/dev/null`

Expected: exit 0 and `Cargo.lock` workspace package entries, including `[[package]] name = "hya"`, update from `0.28.3` to `0.28.4`.

- [ ] **Step 4: Replace root changelog**

Replace `CHANGELOG.md` with:

```markdown
# 0.28.4

- Added a source install script that installs both `hya` and `hya-backend`, verifies PATH availability, and prints provider API setup guidance.
```

- [ ] **Step 5: Verify version/changelog text**

Run: `bash tests/install_script.sh && bash -n install.sh && bash -n tests/install_script.sh`

Expected: PASS and exit 0.

- [ ] **Step 6: Verify lock version changed**

Run: `cargo metadata --locked --format-version=1 >/dev/null`

Expected: exit 0, proving `Cargo.lock` matches the bumped workspace version.

### Task 5: Final verification and review

**Files:**
- All changed files.

**Interfaces:**
- Consumes: completed Tasks 1-4.
- Produces: evidence that the feature works and does not break the workspace.

- [ ] **Step 1: Run installer dry-run smoke**

Run: `bash ./install.sh --dry-run --prefix /tmp/hya-install-smoke --profile release`

Expected output includes:

```text
Permission preflight: /tmp/hya-install-smoke/bin
Rollback backup path: /tmp/hya-install-smoke/bin/.hya.bak
Rollback backup path: /tmp/hya-install-smoke/bin/.hya-backend.bak
cargo build --locked --profile release -p hya -p hya-backend --bins
/tmp/hya-install-smoke/bin/.hya.tmp
/tmp/hya-install-smoke/bin/.hya-backend.tmp
/tmp/hya-install-smoke/bin/hya
/tmp/hya-install-smoke/bin/hya-backend
hya-backend login anthropic "$ANTHROPIC_API_KEY"
```

- [ ] **Step 2: Run real temp-bin install smoke**

Run:

```sh
tmp=$(mktemp -d)
PATH="$tmp/bin:$PATH" ./install.sh --bin-dir "$tmp/bin" --profile dev
PATH="$tmp/bin:$PATH" hya --version
PATH="$tmp/bin:$PATH" hya-backend --help >/dev/null
```

Expected: installer exits 0, `command -v hya` inside the installer resolves to `$tmp/bin/hya`, `hya --version` prints `hya 0.28.4` per `crates/hya/src/main.rs:21-23`, and `hya-backend --help` exits 0.

- [ ] **Step 3: Run required project checks for touched areas**

Project `AGENTS.md` requires the CI-equivalent Rust checks after any fix, feature, or refactor; this is intentionally broader than the shell-only touched files.

Run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all exit 0.

- [ ] **Step 4: Build local executable**

Run: `cargo build --workspace`

Expected: exit 0.

- [ ] **Step 5: Request code review**

Ask a reviewer to check the installer behavior, version/changelog compliance, and test adequacy. Fix Critical/Important findings before reporting done.

## Plan Review

### Round 1 — InstallPlanReview3 — VERDICT: FAIL

D1 PASS
D2 FAIL: Task 2 Step 1 is a whole 159-line script creation, not atomic/executable in 1-3 tool calls -> split installer into small steps (parse args, resolve build command, preflight, install, verify/guidance) with per-step checks [docs/superpowers/plans/2026-06-29-install-script.md:111]
D3 FAIL: Plan assumes hya-backend has login/models subcommands but does not cite or verify CLI surface -> reference existing hya-backend command definitions or add a read-only precheck before hard-coding guidance [docs/superpowers/plans/2026-06-29-install-script.md:269]
D4 FAIL: Rollback/recovery is missing for partially installed binaries after copying hya then failing on hya-backend or PATH verification -> add temp-file install/restore/remove-on-failure behavior and explicit abort criteria [docs/superpowers/plans/2026-06-29-install-script.md:230]
D5 PASS
D6 PASS
VERDICT: FAIL

### Round 2 — InstallPlanReview5 — VERDICT: FAIL

D1 PASS
D2 PASS
D3 FAIL: Version bump plan omits Cargo.lock even though workspace packages are locked at 0.28.2 -> add Cargo.lock update/check after changing workspace version [docs/superpowers/plans/2026-06-29-install-script.md:416, Cargo.lock:936]
D4 PASS
D5 FAIL: Final verification only dry-runs installer and never proves real binaries install/PATH-check from a temp bin dir -> add non-dry-run temp --bin-dir smoke with PATH pointed at it [docs/superpowers/plans/2026-06-29-install-script.md:449]
D6 PASS
VERDICT: FAIL

### Round 3 — InstallPlanReview6 — VERDICT: FAIL

D1 PASS
D2 FAIL: final verification has duplicate Step 3 labels, so execution order/evidence tracking is ambiguous -> renumber final steps uniquely [docs/superpowers/plans/2026-06-29-install-script.md:495, docs/superpowers/plans/2026-06-29-install-script.md:507]
D3 PASS
D4 PASS
D5 FAIL: focused check runs `bash -n install.sh tests/install_script.sh`, but bash -n only checks the first file unless invoked per file -> change to `bash -n install.sh && bash -n tests/install_script.sh` [docs/superpowers/plans/2026-06-29-install-script.md:393]
D6 PASS
VERDICT: FAIL

### Round 4 — InstallPlanReview7 — VERDICT: FAIL

D1 PASS: install goal, binary constraints, and version/changelog constraints are concrete [docs/superpowers/plans/2026-06-29-install-script.md:5, docs/superpowers/plans/2026-06-29-install-script.md:13, docs/superpowers/plans/2026-06-29-install-script.md:23]
D2 PASS: tasks are ordered red test -> installer -> version/changelog -> final verification, with executable steps [docs/superpowers/plans/2026-06-29-install-script.md:39, docs/superpowers/plans/2026-06-29-install-script.md:106, docs/superpowers/plans/2026-06-29-install-script.md:224, docs/superpowers/plans/2026-06-29-install-script.md:397, docs/superpowers/plans/2026-06-29-install-script.md:455]
D3 PASS: cited real binaries and backend CLI surfaces exist [crates/hya/Cargo.toml:8, crates/hya-backend/Cargo.toml:8, crates/hya-backend/src/cli_args.rs:109, crates/hya-backend/src/cli_args.rs:127]
D4 PASS: plan specifies permission preflight, temp installs, backups, trap restore, and PATH abort [docs/superpowers/plans/2026-06-29-install-script.md:19, docs/superpowers/plans/2026-06-29-install-script.md:20, docs/superpowers/plans/2026-06-29-install-script.md:233, docs/superpowers/plans/2026-06-29-install-script.md:337]
D5 FAIL: Task 4 Step 5 uses `bash -n install.sh tests/install_script.sh`, which syntax-checks only `install.sh` and leaves the test script unchecked -> change to `bash tests/install_script.sh && bash -n install.sh && bash -n tests/install_script.sh` [docs/superpowers/plans/2026-06-29-install-script.md:445]
D6 PASS: scope stays limited to installer, shell test, version/lock/changelog, with no new dependencies [docs/superpowers/plans/2026-06-29-install-script.md:26, docs/superpowers/plans/2026-06-29-install-script.md:32, docs/superpowers/plans/2026-06-29-install-script.md:34, docs/superpowers/plans/2026-06-29-install-script.md:37]
VERDICT: FAIL

### Round 5 — InstallPlanReview8 — VERDICT: FAIL

D1 FAIL: plan claims 0.28.2 current state, but repo already has root version/changelog and archive at 0.28.3/0.28.2, so goal state is no longer concrete -> refresh plan preconditions against current repo state [docs/superpowers/plans/2026-06-29-install-script.md:23]
D2 FAIL: Task 2 Step 1 creates most of install.sh in one block rather than a 1-3-tool atomic step -> split into parser/usage creation and subsequent executable checks [docs/superpowers/plans/2026-06-29-install-script.md:114]
D3 FAIL: plan assumptions about current version/changelog are stale; Cargo.toml is already 0.28.3 while Cargo.lock remains 0.28.2 -> reconcile manifest/lock/changelog facts before execution [docs/superpowers/plans/2026-06-29-install-script.md:23, Cargo.toml:8, Cargo.lock:936]
D4 PASS: preflight, temp files, backups, trap restore, PATH abort, and recovery behavior are explicit [docs/superpowers/plans/2026-06-29-install-script.md:19]
D5 FAIL: final required checks include full workspace clippy/test/build, exceeding the stated touched-area verification for this installer/version task -> narrow or justify project-wide gates [docs/superpowers/plans/2026-06-29-install-script.md:495]
D6 PASS: scope is limited to installer, shell test, version/lock/changelog, and explicitly forbids dependencies [docs/superpowers/plans/2026-06-29-install-script.md:26]
VERDICT: FAIL

### Round 6 — InstallPlanReview10 — VERDICT: FAIL

D1 PASS: goal, binary/install constraints, and release state are concrete and falsifiable [docs/superpowers/plans/2026-06-29-install-script.md:5]
D2 FAIL: Task 3 Step 1 says append before invoking build/install but Task 3 Step 2 also says append, so Step 2 would be placed after trap plus before build only by inference -> state exact insertion order/location after Task 2 resolution block [docs/superpowers/plans/2026-06-29-install-script.md:238]
D3 FAIL: plan assumes `hya --version` prints `hya 0.28.4`, but no cited source proves the `hya` binary supports `--version` or that format -> cite Args/clap version behavior or adjust smoke expectation [docs/superpowers/plans/2026-06-29-install-script.md:501]
D4 FAIL: rollback trap is installed before `mkdir -p "$bin_dir"`; if creating the target directory fails, cleanup/restore touches files under a non-existent or unwritable dir and may mask the original abort -> enable rollback only after temp paths can be cleaned or guard cleanup with existence/writability checks [docs/superpowers/plans/2026-06-29-install-script.md:286]
D5 PASS: shell contract, syntax checks, lock check, real temp-bin smoke, and workspace gates are concrete [docs/superpowers/plans/2026-06-29-install-script.md:398]
D6 PASS: scope is limited to installer, shell test, version/lock/changelog and forbids dependencies [docs/superpowers/plans/2026-06-29-install-script.md:26]
VERDICT: FAIL

### Round 7 — InstallPlanReview11 — VERDICT: FAIL

D1 PASS: install goal, binary constraints, version/changelog target, and dependency non-goal are concrete and falsifiable [docs/superpowers/plans/2026-06-29-install-script.md:5]
D2 FAIL: Task 3 Step 1's bash append block contains un-commented prose, so copying it makes invalid shell -> move the errtrace/trap note outside the code fence or comment it [docs/superpowers/plans/2026-06-29-install-script.md:293]
D3 PASS: referenced binaries, CLI commands, version source, lock versions, and changelog facts exist [crates/hya/Cargo.toml:8, crates/hya-backend/Cargo.toml:8, crates/hya-backend/src/cli_args.rs:109, Cargo.lock:937]
D4 PASS: permission abort, temp files, backups, ERR/INT/TERM restore, and PATH abort are explicit [docs/superpowers/plans/2026-06-29-install-script.md:19]
D5 PASS: shell contract, syntax checks, real temp-bin smoke, lock check, and workspace gates are concrete [docs/superpowers/plans/2026-06-29-install-script.md:404]
D6 PASS: scope stays limited to installer, shell test, version/lock/changelog, and forbids new dependencies [docs/superpowers/plans/2026-06-29-install-script.md:26]
VERDICT: FAIL

### Round 8 — InstallPlanReview12 — VERDICT: FAIL

D1 PASS: goal, binary constraints, version/changelog target, and no-dependency non-goal are concrete and falsifiable [docs/superpowers/plans/2026-06-29-install-script.md:5]
D2 PASS: tasks are ordered red test -> installer skeleton -> rollback/verification/guidance -> version/changelog -> final verification, with actionable steps [docs/superpowers/plans/2026-06-29-install-script.md:39]
D3 PASS: referenced package versions, binaries, version output, backend login/models commands, and changelog facts exist [Cargo.toml:8]
D4 FAIL: rollback only deletes installed files whose flags are set after successful mv, so a failed mv can leave a new partial binary in place -> set installed flags before mv or test paths directly during restore [docs/superpowers/plans/2026-06-29-install-script.md:344]
D5 PASS: shell contract, syntax checks, lock check, temp-bin install smoke, and workspace gates are concrete [docs/superpowers/plans/2026-06-29-install-script.md:404]
D6 PASS: scope stays limited to installer, shell test, version/lock/changelog, and explicitly forbids new dependencies [docs/superpowers/plans/2026-06-29-install-script.md:26]
VERDICT: FAIL

### Round 9 — InstallPlanReview14 — VERDICT: PASS

D1 PASS: goal, binary constraints, version/changelog target, and dependency non-goal are concrete and falsifiable [docs/superpowers/plans/2026-06-29-install-script.md:5]
D2 PASS: tasks are ordered red test -> installer skeleton -> rollback/verification/guidance -> version/changelog -> final verification with executable 1-3 call steps [docs/superpowers/plans/2026-06-29-install-script.md:39]
D3 PASS: cited versions, binaries, hya --version behavior, backend login/models commands, changelog state, and project verification rule exist [Cargo.toml:8]
D4 PASS: permission preflight, temp files, backup/restore trap, PATH abort, and recovery behavior are explicit [docs/superpowers/plans/2026-06-29-install-script.md:19]
D5 PASS: shell contract, syntax checks, lock check, temp-bin install smoke, Rust gates, and build command have concrete expected results [docs/superpowers/plans/2026-06-29-install-script.md:400]
D6 PASS: scope stays limited to installer, shell test, version/lock/changelog, and explicitly forbids new dependencies [docs/superpowers/plans/2026-06-29-install-script.md:26]
VERDICT: PASS
