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
contains "$script" "set -Eeuo pipefail"


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
repo=$(pwd -P)
relative_dry_run=$(bash ./install.sh --dry-run --bin-dir bin --profile debug)
contains "$relative_dry_run" "PATH check: command -v hya must resolve to $repo/bin/hya"


contains "$dry_run" 'XDG_CONFIG_HOME/hya/config.yaml'
contains "$dry_run" 'hya-backend login anthropic "$ANTHROPIC_API_KEY"'
contains "$dry_run" "hya-backend models"
contains "$dry_run" "hya"
