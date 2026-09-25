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

first_party=(base-tools extended-tools network-tools channel-tools todo-tools core-skills core-commands core-agents agent-channels goal-loop plan-impl-review subagents)

help=$(bash ./install.sh --help)
[[ -x ./install.sh ]] || fail "install.sh must be executable"
script=$(<./install.sh)
ci_workflow=$(<.github/workflows/ci.yml)
release_workflow=$(<.github/workflows/release.yml)
package_helper=$(<scripts/package-argus-example.sh)
contains "$release_workflow" "scripts/package-argus-example.sh"
contains "$release_workflow" "examples/hya-argus-example.hyabundle"
[[ -x ./scripts/package-argus-example.sh ]] || fail "Argus package helper must be executable"
contains "$package_helper" "package-bundle"
not_contains "$package_helper" "7z a"
contains "$script" "set -Eeuo pipefail"
contains "$script" '(cd "$tmp" && bun install --frozen-lockfile --production)'
contains "$script" "crates/hya-plugin-bun/adapter"
contains "$script" "packages/hya-tui"
contains "$script" "packages/hya-tui-web"
contains "$script" "stage-first-party-bundles"
contains "$release_workflow" "crates/hya-plugin-bun/adapter"
contains "$release_workflow" "lib/hya/bun-adapter"
contains "$release_workflow" "lib/hya/tui"
contains "$release_workflow" "lib/hya/tui-web"
contains "$release_workflow" "stage-first-party-bundles"
not_contains "$script" "compat"
not_contains "$release_workflow" "compat-adapter"
not_contains "$script" "hya-tui-ts"
not_contains "$release_workflow" "hya-tui-ts"
not_contains "$ci_workflow" "hya-tui-ts"

for workflow in "$ci_workflow" "$release_workflow"; do
  while IFS= read -r line; do
    [[ "$line" =~ uses:[[:space:]]*([^[:space:]#]+) ]] || continue
    ref=${BASH_REMATCH[1]}
    [[ "$ref" =~ @[0-9a-f]{40}$ ]] || fail "workflow action is not pinned to a commit: $ref"
  done <<<"$workflow"
done

contains "$help" "--prefix DIR"
contains "$help" "--bin-dir DIR"
contains "$help" "--profile release|dev|debug"
contains "$help" "--dry-run"
contains "$help" "bin/hya "
not_contains "$help" "hya-backend"
contains "$help" "bundles/hya-*.hyabundle"
contains "$help" "lib/hya/bun-adapter"
contains "$help" "lib/hya/tui "
contains "$help" "lib/hya/tui-web"
not_contains "$help" "compat"

dry_run=$(bash ./install.sh --dry-run --prefix /tmp/hya-install-test --profile debug)
contains "$dry_run" "Permission preflight: /tmp/hya-install-test/bin /tmp/hya-install-test/bundles /tmp/hya-install-test/lib/hya"
[[ "$dry_run" == *"Bun preflight: bun"*"cargo build --locked -p hya-backend --bins"* ]] || fail "Bun preflight must run before cargo build"
contains "$dry_run" "cargo build --locked -p hya-backend --bins"
contains "$dry_run" "cargo build --locked -p hya-base-tools -p hya-extended-tools -p hya-network-tools -p hya-channel-tools -p hya-todo-tools --lib"
contains "$dry_run" "stage-first-party-bundles --library-dir"
contains "$dry_run" "bun install --frozen-lockfile --production"
not_contains "$dry_run" "--profile debug"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya.tmp"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya.bak"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/.bun-adapter.tmp"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/.{bun-adapter,tui,tui-web}.bak"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/.tui.tmp"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/.tui-web.tmp"
contains "$dry_run" "/tmp/hya-install-test/bundles/.hya-bundles.tmp"
contains "$dry_run" "/tmp/hya-install-test/bundles/.hya-bundles.bak"
contains "$dry_run" "/tmp/hya-install-test/bin/hya"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/bun-adapter"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/tui/src/main.ts"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/tui-web/src/main.ts"
contains "$dry_run" "bundle list (isolated HOME) must list: ${first_party[*]}"
contains "$dry_run" "PATH check: command -v hya must resolve to /tmp/hya-install-test/bin/hya"
contains "$dry_run" "rm -f /tmp/hya-install-test/bin/hya-backend (legacy executable name, if present)"
[[ ! -e /tmp/hya-install-test ]] || fail "dry run created /tmp/hya-install-test"
repo=$(pwd -P)
relative_dry_run=$(bash ./install.sh --dry-run --bin-dir bin --profile debug)
contains "$relative_dry_run" "PATH check: command -v hya must resolve to $repo/bin/hya"
contains "$relative_dry_run" "$repo/bundles"
contains "$relative_dry_run" "$repo/lib/hya/bun-adapter"
if bash ./install.sh --dry-run --bin-dir /tmp/hya-install-test/tools >/dev/null 2>&1; then
  fail "installer accepted a --bin-dir the backend cannot find bundles from"
fi

contains "$dry_run" 'XDG_CONFIG_HOME/hya/config.yaml'
contains "$dry_run" 'hya login anthropic "$ANTHROPIC_API_KEY"'
contains "$dry_run" "hya models"
contains "$dry_run" "hya serve"

fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
fake_bin="$fixture/fake-bin"
target="$fixture/target"
mkdir -p "$fake_bin"

# Fake cargo: builds a scripted backend and stages twelve placeholder bundles.
cat >"$fake_bin/cargo" <<'FAKE_CARGO'
#!/usr/bin/env bash
set -euo pipefail
test -f "${HYA_BUN_PREFLIGHT_MARKER:?}"
if [[ " $* " == *" stage-first-party-bundles "* ]]; then
  package_root=""
  while [[ $# -gt 0 ]]; do
    [[ "$1" == --package-root ]] && package_root=$2
    shift
  done
  mkdir -p "${package_root:?}/bundles"
  for bundle in ${HYA_TEST_FIRST_PARTY:?}; do
    printf 'new-%s\n' "$bundle" >"$package_root/bundles/hya-$bundle.hyabundle"
  done
  exit 0
fi
profile=debug
[[ " $* " == *" --profile release "* ]] && profile=release
out="${CARGO_TARGET_DIR:?}/$profile"
mkdir -p "$out"
[[ " $* " == *" --bins "* ]] || exit 0
cat >"$out/hya" <<'FAKE_BACKEND'
#!/usr/bin/env bash
set -euo pipefail
[[ "${HYA_INSTALL_SMOKE_FAIL:-}" != hya ]] || exit 91
case "${1:-}" in
  --help|--version) exit 0 ;;
  bundle)
    bundles_dir="$(dirname "$0")/../bundles"
    for bundle in ${HYA_TEST_FIRST_PARTY:?}; do
      [[ -f "$bundles_dir/hya-$bundle.hyabundle" ]] && printf 'hya/%s 0.0.0  active Plugin -\n' "$bundle"
    done
    exit 0
    ;;
esac
exit 2
FAKE_BACKEND
chmod +x "$out/hya"
FAKE_CARGO
chmod +x "$fake_bin/cargo"

cat >"$fake_bin/bun" <<'FAKE_BUN'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  : >"${HYA_BUN_PREFLIGHT_MARKER:?}"
  printf '%s\n' 1.4.2
  exit 0
fi
# The installer probes the installed TUI and WebUI host entry points.
if [[ "${2:-}" == "--help" && "${1:-}" == */src/main.ts ]]; then
  test -f "$1"
  exit 0
fi
[[ "$*" == "install --frozen-lockfile --production" ]]
test -f package.json
test -f bun.lock
if [[ "${HYA_FAIL_ADAPTER_INSTALL:-0}" == 1 ]]; then
  exit 1
fi
mkdir -p node_modules
if grep -Fq '"name": "@hya/tui",' package.json; then
  mkdir -p node_modules/@opentui/core
elif grep -Fq '"name": "@hya/tui-web",' package.json; then
  mkdir -p node_modules/@xterm/xterm
else
  grep -Fq '"name": "@hya/bun-adapter",' package.json
fi
FAKE_BUN
chmod +x "$fake_bin/bun"

run_install() {
  local root=$1
  shift
  PATH="$fake_bin:$root/bin:$PATH" CARGO_TARGET_DIR="$target" \
    HYA_BUN_PREFLIGHT_MARKER="$fixture/bun-ready" HYA_TEST_FIRST_PARTY="${first_party[*]}" \
    "$@" bash ./install.sh --prefix "$root" --profile debug
}

no_leftovers() {
  local root=$1
  ! compgen -G "$root/bin/.*.tmp.*" >/dev/null &&
    ! compgen -G "$root/bin/.*.bak.*" >/dev/null &&
    ! compgen -G "$root/lib/hya/.*.tmp.*" >/dev/null &&
    ! compgen -G "$root/lib/hya/.*.bak.*" >/dev/null &&
    ! compgen -G "$root/bundles/.hya-bundles.*" >/dev/null
}

install_root="$fixture/install"
mkdir -p "$install_root/bundles"
printf 'unrelated\n' >"$install_root/bundles/other.txt"
mkdir -p "$install_root/bin"
printf 'legacy-binary\n' >"$install_root/bin/hya-backend"
run_install "$install_root" env >/dev/null
[[ -x "$install_root/bin/hya" ]] || fail "missing installed binary: hya"
[[ ! -e "$install_root/bin/hya-backend" ]] || fail "installer kept the legacy hya-backend executable"
for bundle in "${first_party[@]}"; do
  [[ $(<"$install_root/bundles/hya-$bundle.hyabundle") == "new-$bundle" ]] ||
    fail "missing installed first-party bundle: $bundle"
done
[[ $(<"$install_root/bundles/other.txt") == unrelated ]] || fail "installer touched an unrelated bundles/ file"
bun_adapter="$install_root/lib/hya/bun-adapter"
for path in package.json bun.lock src/main.ts node_modules; do
  [[ -e "$bun_adapter/$path" ]] || fail "missing installed Bun adapter path: $path"
done
for path in package.json bun.lock bunfig.toml tsconfig.json src/main.ts node_modules/@opentui/core; do
  [[ -e "$install_root/lib/hya/tui/$path" ]] || fail "missing installed TUI path: $path"
done
for path in package.json bun.lock tsconfig.json src/main.ts web/index.html node_modules/@xterm/xterm; do
  [[ -e "$install_root/lib/hya/tui-web/$path" ]] || fail "missing installed WebUI host path: $path"
done
no_leftovers "$install_root" || fail "installer left temporary or backup paths after a successful install"

seed_previous_install() {
  local root=$1
  mkdir -p "$root/bin" "$root/lib/hya/bun-adapter" "$root/bundles"
  printf 'old-binary\n' >"$root/bin/hya"
  printf 'old-adapter\n' >"$root/lib/hya/bun-adapter/marker"
  printf 'old-core-agents\n' >"$root/bundles/hya-core-agents.hyabundle"
}

assert_previous_install() {
  local root=$1
  local reason=$2
  [[ $(<"$root/bin/hya") == old-binary ]] || fail "$reason replaced the previous binary"
  [[ $(<"$root/lib/hya/bun-adapter/marker") == old-adapter ]] || fail "$reason replaced the previous Bun adapter"
  [[ $(<"$root/bundles/hya-core-agents.hyabundle") == old-core-agents ]] ||
    fail "$reason replaced the previous first-party bundle"
  [[ ! -e "$root/bundles/hya-core-skills.hyabundle" ]] || fail "$reason left a new first-party bundle"
  no_leftovers "$root" || fail "installer left temporary or backup paths after $reason"
}

# A failed adapter dependency install must abort before placement.
failed_install="$fixture/install-failed-adapter"
seed_previous_install "$failed_install"
if run_install "$failed_install" env HYA_FAIL_ADAPTER_INSTALL=1 >/dev/null 2>&1; then
  fail "installer accepted a failed Bun adapter dependency install"
fi
assert_previous_install "$failed_install" "a failed dependency install"

# A post-placement smoke failure must roll every component back.
rollback_root="$fixture/rollback"
seed_previous_install "$rollback_root"
if run_install "$rollback_root" env HYA_INSTALL_SMOKE_FAIL=hya >/dev/null 2>&1; then
  fail "install should fail when a post-placement smoke fails"
fi
assert_previous_install "$rollback_root" "a post-placement rollback"

# Run the installed adapter from outside the checkout when a real Bun exists.
# This verifies the installed runtime does not depend on HYA_BUN_ADAPTER_DIR.
if real_bun=$(command -v bun); then
  probe="$fixture/adapter-probe"
  mkdir -p "$probe"
  cp -R "$repo/crates/hya-plugin-bun/adapter/src/." "$bun_adapter/src/"
  result=$(
    cd "$probe"
    env -u HYA_BUN_ADAPTER_DIR HYA_DIRECTORY="$probe" HYA_WORKTREE="$probe" \
      "$real_bun" run "$bun_adapter/src/main.ts" <<'ADAPTER_REQUESTS'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1,"host":{"name":"hya","version":"test"}}}
{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}
ADAPTER_REQUESTS
  )
  contains "$result" '"protocol_version":1'
  contains "$result" '"hooks":[]'
  contains "$result" '"tools":[]'
  contains "$result" '"id":2,"result":{}'
else
  echo "install_script: no bun on PATH; skipped the installed adapter probe"
fi

echo "install_script: ok"
