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
contains "$script" 'cd "$tmp_compat" && bun install --frozen-lockfile --production'
not_contains "$script" 'cp -R "$compat_source/node_modules/."'
contains "$script" "crates/hya-plugin-compat/adapter"
contains "$script" "lib/hya/compat-adapter"
contains "$release_workflow" "crates/hya-plugin-compat/adapter"
contains "$release_workflow" "lib/hya/compat-adapter"
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
contains "$help" "hya-backend"
contains "$help" "lib/hya/compat-adapter"
not_contains "$help" "hya-ts"
not_contains "$help" "hya-tui-ts"

dry_run=$(bash ./install.sh --dry-run --prefix /tmp/hya-install-test --profile debug)
contains "$dry_run" "Permission preflight: /tmp/hya-install-test/bin"
[[ "$dry_run" == *"Bun preflight: bun"*"cargo build --locked -p hya-backend --bins"* ]] || fail "Bun preflight must run before cargo build"

contains "$dry_run" "cargo build --locked -p hya-backend --bins"
contains "$dry_run" "bun install --frozen-lockfile --production"
not_contains "$dry_run" "--profile debug"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya-backend.tmp"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya-backend.bak"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/.compat-adapter.tmp"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/.compat-adapter.bak"


contains "$dry_run" "/tmp/hya-install-test/bin/hya-backend"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/compat-adapter"
contains "$dry_run" "PATH check: command -v hya-backend must resolve to /tmp/hya-install-test/bin/hya-backend"
repo=$(pwd -P)
relative_dry_run=$(bash ./install.sh --dry-run --bin-dir bin --profile debug)
contains "$relative_dry_run" "PATH check: command -v hya-backend must resolve to $repo/bin/hya-backend"
contains "$relative_dry_run" "$repo/lib/hya/compat-adapter"


contains "$dry_run" 'XDG_CONFIG_HOME/hya/config.yaml'
contains "$dry_run" 'hya-backend login anthropic "$ANTHROPIC_API_KEY"'
contains "$dry_run" "hya-backend models"
contains "$dry_run" "hya-backend serve"

fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
real_bun=$(command -v bun)
fake_bin="$fixture/fake-bin"
target="$fixture/target"
install_root="$fixture/install"
mkdir -p "$fake_bin"

cat >"$fake_bin/cargo" <<'FAKE_CARGO'
#!/usr/bin/env bash
set -euo pipefail
test -f "${HYA_BUN_PREFLIGHT_MARKER:?}"
profile=debug
[[ " $* " == *" --profile release "* ]] && profile=release
out="${CARGO_TARGET_DIR:?}/$profile"
mkdir -p "$out"
cat >"$out/hya-backend" <<'FAKE_BACKEND'
#!/usr/bin/env bash
set -euo pipefail
[[ "${HYA_INSTALL_SMOKE_FAIL:-}" != hya-backend ]] || exit 91
case "${1:-}" in
  --help|--version) exit 0 ;;
esac
exit 2
FAKE_BACKEND
chmod +x "$out/hya-backend"
FAKE_CARGO
chmod +x "$fake_bin/cargo"

cat >"$fake_bin/bun" <<'FAKE_BUN'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${1:-}" == "--version" ]]; then
  : >"${HYA_BUN_PREFLIGHT_MARKER:?}"
  printf '%s\n' 1.3.14
  exit 0
fi
[[ "$*" == "install --frozen-lockfile --production" ]]
test -f package.json
test -f bun.lock
if [[ "${HYA_FAIL_COMPAT_INSTALL:-0}" == 1 ]]; then
  exit 1
fi
if grep -Fq '"name": "@hya/compat-adapter"' package.json; then
  mkdir -p node_modules
  cp -R "${HYA_TEST_COMPAT_NODE_MODULES:?}/." node_modules/
  exit 0
fi
exit 1
FAKE_BUN
chmod +x "$fake_bin/bun"

compat_node_modules="$(pwd -P)/crates/hya-plugin-compat/adapter/node_modules"
[[ -d "$compat_node_modules" ]] || fail "missing Compat adapter test dependencies"
PATH="$fake_bin:$install_root/bin:$PATH" CARGO_TARGET_DIR="$target" HYA_BUN_PREFLIGHT_MARKER="$fixture/bun-ready" HYA_REAL_BUN="$real_bun" \
  HYA_TEST_COMPAT_NODE_MODULES="$compat_node_modules" bash ./install.sh --prefix "$install_root" --profile debug >/dev/null

[[ -x "$install_root/bin/hya-backend" ]] || fail "missing installed binary: hya-backend"
compat_adapter="$install_root/lib/hya/compat-adapter"
for path in package.json bun.lock src/main.ts node_modules/@opencode-ai/plugin/package.json node_modules/@opencode-ai/sdk/package.json; do
  [[ -e "$compat_adapter/$path" ]] || fail "missing installed Compat adapter path: $path"
done


# A failed Compat dependency install must abort before placement and preserve
# the previously installed adapter and binary.
failed_install="$fixture/install-failed-compat"
failed_target="$fixture/target-failed-compat"
failed_output="$fixture/failed-compat-output"
mkdir -p "$failed_install/bin" "$failed_install/lib/hya/compat-adapter"
printf 'old-binary\n' >"$failed_install/bin/hya-backend"
printf 'old-compat\n' >"$failed_install/lib/hya/compat-adapter/marker"
if PATH="$fake_bin:$failed_install/bin:$PATH" CARGO_TARGET_DIR="$failed_target" HYA_BUN_PREFLIGHT_MARKER="$fixture/bun-ready" HYA_REAL_BUN="$real_bun" \
  HYA_TEST_COMPAT_NODE_MODULES="$compat_node_modules" HYA_FAIL_COMPAT_INSTALL=1 \
  bash ./install.sh --prefix "$failed_install" --profile debug >"$failed_output" 2>&1; then
  fail "installer accepted a failed Compat dependency install"
fi
[[ $(<"$failed_install/bin/hya-backend") == old-binary ]] ||
  fail "failed dependency install replaced the previous binary"
[[ $(<"$failed_install/lib/hya/compat-adapter/marker") == old-compat ]] ||
  fail "failed dependency install replaced the previous Compat adapter"
if compgen -G "$failed_install/bin/.*.tmp.*" >/dev/null ||
  compgen -G "$failed_install/bin/.*.bak.*" >/dev/null ||
  compgen -G "$failed_install/lib/hya/.*.tmp.*" >/dev/null ||
  compgen -G "$failed_install/lib/hya/.*.bak.*" >/dev/null; then
  fail "installer left temporary or backup paths after failed dependency install"
fi

# Run the packaged adapter from outside the checkout. This verifies the release
# artifact is self-contained and does not depend on HYA_COMPAT_ADAPTER_DIR.
compat_probe="$fixture/compat-probe"
compat_output="$fixture/compat-output"
mkdir -p "$compat_probe"
(
  cd "$compat_probe"
  env -u HYA_COMPAT_ADAPTER_DIR COMPAT_PURE=1 HYA_DIRECTORY="$compat_probe" HYA_WORKTREE="$compat_probe" \
    "$real_bun" run "$compat_adapter/src/main.ts" >"$compat_output" <<'COMPAT_REQUESTS'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol_version":1,"host":{"name":"hya","version":"test"}}}
{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}
COMPAT_REQUESTS
)
compat_result=$(<"$compat_output")
contains "$compat_result" '"protocol_version":1'
contains "$compat_result" '"hooks":[]'
contains "$compat_result" '"tools":[]'
contains "$compat_result" '"id":2,"result":{}'

rollback_root="$fixture/rollback"
mkdir -p "$rollback_root/bin" "$rollback_root/lib/hya/compat-adapter"
printf 'old-hya-backend\n' >"$rollback_root/bin/hya-backend"
printf 'old-compat\n' >"$rollback_root/lib/hya/compat-adapter/marker"

if PATH="$fake_bin:$rollback_root/bin:$PATH" CARGO_TARGET_DIR="$target" HYA_BUN_PREFLIGHT_MARKER="$fixture/bun-ready" HYA_REAL_BUN="$real_bun" \
  HYA_TEST_COMPAT_NODE_MODULES="$compat_node_modules" HYA_INSTALL_SMOKE_FAIL=hya-backend bash ./install.sh --bin-dir "$rollback_root/bin" --profile debug >/dev/null 2>&1; then
  fail "install should fail when a post-placement smoke fails"
fi
[[ $(<"$rollback_root/bin/hya-backend") == old-hya-backend ]] || fail "rollback did not restore hya-backend"
[[ $(<"$rollback_root/lib/hya/compat-adapter/marker") == old-compat ]] || fail "rollback did not restore Compat adapter"
if compgen -G "$rollback_root/bin/.*.tmp.*" >/dev/null ||
  compgen -G "$rollback_root/bin/.*.bak.*" >/dev/null ||
  compgen -G "$rollback_root/lib/hya/.*.tmp.*" >/dev/null ||
  compgen -G "$rollback_root/lib/hya/.*.bak.*" >/dev/null; then
  fail "installer left temporary or backup paths after rollback"
fi
