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
contains "$script" "set -Eeuo pipefail"
contains "$script" "scripts/prune-sdk-server.ts"
contains "$release_workflow" "scripts/prune-sdk-server.ts"
contains "$script" "packages/hya-tui-ts/bunfig.toml"
contains "$script" "packages/hya-tui-ts/tsconfig.json"
contains "$release_workflow" "packages/hya-tui-ts/bunfig.toml"
contains "$release_workflow" "packages/hya-tui-ts/tsconfig.json"
contains "$release_workflow" "for path in dist/index.js dist/index.d.ts dist/server.js dist/server.d.ts dist/v2/index.js dist/v2/index.d.ts dist/v2/server.js dist/v2/server.d.ts dist/process.js dist/process.d.ts"
contains "$release_workflow" "! grep -F '\".\"'"
contains "$release_workflow" "HYA_RELEASE_BUN_INVOCATION"
contains "$release_workflow" '"$packaged_binary" "$project" --server http://127.0.0.1:54321 --bun "$mock_bun"'
contains "$ci_workflow" "cargo build --locked -p hya -p hya-backend -p hya-ts --bins"

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
contains "$help" "hya-ts"
contains "$help" "lib/hya/hya-tui-ts"

dry_run=$(bash ./install.sh --dry-run --prefix /tmp/hya-install-test --profile debug)
contains "$dry_run" "Permission preflight: /tmp/hya-install-test/bin"
[[ "$dry_run" == *"Bun preflight: bun"*"cargo build --locked -p hya -p hya-backend -p hya-ts --bins"* ]] || fail "Bun preflight must run before cargo build"

contains "$dry_run" "cargo build --locked -p hya -p hya-backend -p hya-ts --bins"
contains "$dry_run" "bun install --frozen-lockfile --production"
not_contains "$dry_run" "--profile debug"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya.tmp"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya-backend.tmp"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya-ts.tmp"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya.bak"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya-backend.bak"
contains "$dry_run" "/tmp/hya-install-test/bin/.hya-ts.bak"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/.hya-tui-ts.tmp"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/.hya-tui-ts.bak"


contains "$dry_run" "/tmp/hya-install-test/bin/hya"
contains "$dry_run" "/tmp/hya-install-test/bin/hya-backend"
contains "$dry_run" "/tmp/hya-install-test/bin/hya-ts"
contains "$dry_run" "/tmp/hya-install-test/lib/hya/hya-tui-ts"
contains "$dry_run" "PATH check: command -v hya must resolve to /tmp/hya-install-test/bin/hya"
repo=$(pwd -P)
contains "$dry_run" "/tmp/hya-install-test/bin/hya $repo --server http://127.0.0.1:1 --bun /bin/true"
relative_dry_run=$(bash ./install.sh --dry-run --bin-dir bin --profile debug)
contains "$relative_dry_run" "PATH check: command -v hya must resolve to $repo/bin/hya"
contains "$relative_dry_run" "$repo/lib/hya/hya-tui-ts"


contains "$dry_run" 'XDG_CONFIG_HOME/hya/config.yaml'
contains "$dry_run" 'hya-backend login anthropic "$ANTHROPIC_API_KEY"'
contains "$dry_run" "hya-backend models"
contains "$dry_run" "hya"

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
cat >"$out/hya" <<'FAKE_HYA'
#!/usr/bin/env bash
set -euo pipefail
[[ "${HYA_INSTALL_SMOKE_FAIL:-}" != hya ]] || exit 91
exec "$(dirname "$0")/hya-ts" "$@"
FAKE_HYA
cat >"$out/hya-backend" <<'FAKE_BACKEND'
#!/usr/bin/env bash
set -euo pipefail
[[ "${HYA_INSTALL_SMOKE_FAIL:-}" != hya-backend ]] || exit 91
[[ "${1:-}" == --help ]] || exit 2
FAKE_BACKEND
cat >"$out/hya-ts" <<'FAKE_TS'
#!/usr/bin/env bash
set -euo pipefail
[[ "${HYA_INSTALL_SMOKE_FAIL:-}" != hya-ts ]] || exit 91
case "${1:-}" in
  --help|--version) exit 0 ;;
esac
project=$1
shift
server=
bun=
while [[ $# -gt 0 ]]; do
  case "$1" in
    --server) server=$2; shift 2 ;;
    --bun) bun=$2; shift 2 ;;
    *) shift ;;
  esac
done
runtime="$(cd "$(dirname "$0")/../lib/hya/hya-tui-ts" && pwd -P)"
project="$(cd "$project" && pwd -P)"
cd "$runtime"
exec "$bun" src/main.tsx --url "$server" --project "$project"
FAKE_TS
chmod +x "$out/hya" "$out/hya-backend" "$out/hya-ts"
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
if [[ "${1:-}" == *"scripts/prune-sdk-server.ts" ]]; then
  exec "${HYA_REAL_BUN:?}" "$@"
fi
[[ "$*" == "install --frozen-lockfile --production" ]]
test -f package.json
test -f bun.lock
mkdir -p node_modules/runtime-dependency
printf '%s\n' '{"name":"runtime-dependency"}' >node_modules/runtime-dependency/package.json
mkdir -p node_modules/@opentui/solid
printf '%s\n' '{"name":"@opentui/solid","exports":{"./preload":"./preload.js"}}' >node_modules/@opentui/solid/package.json
: >node_modules/@opentui/solid/preload.js
sdk=node_modules/@opencode-ai/sdk
mkdir -p "$sdk/dist/v2"
cat >"$sdk/package.json" <<'SDK_PACKAGE'
{
  "name": "@opencode-ai/sdk",
  "exports": {
    ".": "./dist/index.js",
    "./server": "./dist/server.js",
    "./v2": "./dist/v2/index.js",
    "./v2/client": "./dist/v2/client.js",
    "./v2/server": "./dist/v2/server.js"
  }
}
SDK_PACKAGE
cat >"$sdk/dist/v2/client.js" <<'SDK_CLIENT'
export function createOpencodeClient() { return {} }
SDK_CLIENT
touch "$sdk/dist/v2/client.d.ts" "$sdk/dist/index.js" "$sdk/dist/index.d.ts" \
  "$sdk/dist/v2/index.js" "$sdk/dist/v2/index.d.ts" \
  "$sdk/dist/server.js" "$sdk/dist/server.d.ts" "$sdk/dist/v2/server.js" \
  "$sdk/dist/v2/server.d.ts" "$sdk/dist/process.js" "$sdk/dist/process.d.ts"
FAKE_BUN
chmod +x "$fake_bin/bun"

PATH="$fake_bin:$install_root/bin:$PATH" CARGO_TARGET_DIR="$target" HYA_BUN_PREFLIGHT_MARKER="$fixture/bun-ready" HYA_REAL_BUN="$real_bun" \
  bash ./install.sh --prefix "$install_root" --profile debug >/dev/null

for name in hya hya-backend hya-ts; do
  [[ -x "$install_root/bin/$name" ]] || fail "missing installed binary: $name"
done
runtime="$install_root/lib/hya/hya-tui-ts"
for path in package.json bun.lock bunfig.toml tsconfig.json src/main.tsx LICENSE UPSTREAM.md NOTICE node_modules/runtime-dependency/package.json; do
  [[ -e "$runtime/$path" ]] || fail "missing installed runtime path: $path"
done
sdk="$runtime/node_modules/@opencode-ai/sdk"
[[ -f "$sdk/dist/v2/client.js" ]] || fail "runtime pruning removed SDK client code"
for path in dist/index.js dist/index.d.ts dist/server.js dist/server.d.ts dist/v2/index.js dist/v2/index.d.ts dist/v2/server.js dist/v2/server.d.ts dist/process.js dist/process.d.ts; do
  [[ ! -e "$sdk/$path" ]] || fail "installed runtime contains SDK server code: $path"
done
sdk_package=$(<"$sdk/package.json")
not_contains "$sdk_package" '"./server"'
not_contains "$sdk_package" '"./v2/server"'
not_contains "$sdk_package" '"."'
for path in test dist; do
  [[ ! -e "$runtime/$path" ]] || fail "installed runtime contains build/test-only path: $path"
done

project="$fixture/project"
mock_bun="$fixture/mock-bun"
bun_invocation="$fixture/bun-invocation"
mkdir -p "$project"
cat >"$mock_bun" <<'MOCK_BUN'
#!/usr/bin/env bash
set -euo pipefail
printf 'cwd=%s\n' "$PWD" >"${HYA_INSTALL_BUN_INVOCATION:?}"
printf 'arg=%s\n' "$@" >>"$HYA_INSTALL_BUN_INVOCATION"
exit 23
MOCK_BUN
chmod +x "$mock_bun"
set +e
HYA_INSTALL_BUN_INVOCATION="$bun_invocation" "$install_root/bin/hya" "$project" \
  --server http://127.0.0.1:54321 --bun "$mock_bun" >/dev/null 2>&1
status=$?
set -e
[[ "$status" -eq 23 ]] || fail "installed hya did not propagate mock Bun status: $status"
invocation=$(<"$bun_invocation")
contains "$invocation" "cwd=$runtime"
contains "$invocation" "arg=src/main.tsx"
contains "$invocation" "arg=--url"
contains "$invocation" "arg=http://127.0.0.1:54321"
contains "$invocation" "arg=--project"
contains "$invocation" "arg=$(cd "$project" && pwd -P)"

rollback_root="$fixture/rollback"
mkdir -p "$rollback_root/bin" "$rollback_root/lib/hya/hya-tui-ts"
for name in hya hya-backend hya-ts; do
  printf 'old-%s\n' "$name" >"$rollback_root/bin/$name"
done
printf 'old-runtime\n' >"$rollback_root/lib/hya/hya-tui-ts/marker"

if PATH="$fake_bin:$rollback_root/bin:$PATH" CARGO_TARGET_DIR="$target" HYA_BUN_PREFLIGHT_MARKER="$fixture/bun-ready" HYA_REAL_BUN="$real_bun" \
  HYA_INSTALL_SMOKE_FAIL=hya-ts bash ./install.sh --bin-dir "$rollback_root/bin" --profile debug >/dev/null 2>&1; then
  fail "install should fail when a post-placement smoke fails"
fi
for name in hya hya-backend hya-ts; do
  [[ $(<"$rollback_root/bin/$name") == "old-$name" ]] || fail "rollback did not restore $name"
done
[[ $(<"$rollback_root/lib/hya/hya-tui-ts/marker") == old-runtime ]] || fail "rollback did not restore runtime"
if compgen -G "$rollback_root/bin/.*.tmp.*" >/dev/null ||
  compgen -G "$rollback_root/bin/.*.bak.*" >/dev/null ||
  compgen -G "$rollback_root/lib/hya/.*.tmp.*" >/dev/null ||
  compgen -G "$rollback_root/lib/hya/.*.bak.*" >/dev/null; then
  fail "installer left temporary or backup paths after rollback"
fi
