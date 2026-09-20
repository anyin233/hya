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

Installs the backend binary and the Compat sidecar:
  hya-backend  backend CLI/API for login, exec, serve, and models
  lib/hya/compat-adapter  production Compat sidecar and dependencies
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

case "$profile" in
  release)
    build_cmd=(cargo build --locked --profile release -p hya-backend --bins)
    target_dir=${CARGO_TARGET_DIR:-target}/release
    ;;
  dev|debug)
    build_cmd=(cargo build --locked -p hya-backend --bins)
    target_dir=${CARGO_TARGET_DIR:-target}/debug
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
lib_dir="$(dirname "$bin_dir")/lib/hya"
compat_source="$(pwd -P)/crates/hya-plugin-compat/adapter"

tmp_backend="$bin_dir/.hya-backend.tmp.$$"
tmp_compat="$lib_dir/.compat-adapter.tmp.$$"
bak_backend="$bin_dir/.hya-backend.bak.$$"
bak_compat="$lib_dir/.compat-adapter.bak.$$"
rollback_enabled=0
install_complete=0
had_backend=0
had_compat=0
placed_backend=0
placed_compat=0

cleanup_leftovers() {
  rm -f "$tmp_backend"
  rm -rf "$tmp_compat"
  if [[ "$install_complete" -eq 1 ]]; then
    rm -f "$bak_backend"
    rm -rf "$bak_compat"
  fi
}

restore_install() {
  if [[ "$rollback_enabled" -eq 0 || "$install_complete" -eq 1 ]]; then
    return 0
  fi

  [[ "$placed_backend" -eq 1 ]] && rm -f "$bin_dir/hya-backend"
  [[ "$placed_compat" -eq 1 ]] && rm -rf "$lib_dir/compat-adapter"
  if [[ "$had_backend" -eq 1 && -e "$bak_backend" ]]; then
    mv -f "$bak_backend" "$bin_dir/hya-backend"
  fi
  if [[ "$had_compat" -eq 1 && -e "$bak_compat" ]]; then
    mv -f "$bak_compat" "$lib_dir/compat-adapter"
  fi
}

on_error() {
  local status=$?
  [[ "$status" -ne 0 ]] || status=1
  trap - ERR INT TERM
  set +e
  restore_install
  cleanup_leftovers
  exit "$status"
}

preflight_path() {
  local path=$1
  if [[ "$dry_run" -ne 0 ]]; then
    return 0
  fi

  local probe=$path
  while [[ ! -e "$probe" ]]; do
    probe=$(dirname "$probe")
  done

  if [[ ! -d "$probe" || ! -w "$probe" ]]; then
    echo "Cannot write to $path." >&2
    echo "Rerun with: sudo ./install.sh" >&2
    echo "Or use a user-writable directory: ./install.sh --bin-dir \"$HOME/.local/bin\"" >&2
    exit 1
  fi
}

trap on_error ERR INT TERM
say "Installing hya-backend to $bin_dir"
say "Installing Compat adapter to $lib_dir/compat-adapter"
say "Rollback backup path: $bak_backend"
say "Rollback backup path: $bak_compat"
say "Permission preflight: $bin_dir"
preflight_path "$bin_dir"
preflight_path "$lib_dir"
say "Bun preflight: bun"
run bun --version
run "${build_cmd[@]}"
run mkdir -p "$bin_dir" "$lib_dir" "$tmp_compat/src"
run install -m 0755 "$target_dir/hya-backend" "$tmp_backend"
run cp "$compat_source/package.json" "$compat_source/bun.lock" "$tmp_compat/"
run cp -R "$compat_source/src/." "$tmp_compat/src/"
say "+ (cd $tmp_compat && bun install --frozen-lockfile --production)"
if [[ "$dry_run" -eq 0 ]]; then
  (cd "$tmp_compat" && bun install --frozen-lockfile --production)
fi
[[ "$dry_run" -ne 0 ]] || rollback_enabled=1
if [[ -e "$bin_dir/hya-backend" ]]; then
  had_backend=1
  run mv -f "$bin_dir/hya-backend" "$bak_backend"
fi
if [[ -e "$lib_dir/compat-adapter" ]]; then
  had_compat=1
  run mv "$lib_dir/compat-adapter" "$bak_compat"
fi
placed_backend=1
run mv -f "$tmp_backend" "$bin_dir/hya-backend"
placed_compat=1
run mv "$tmp_compat" "$lib_dir/compat-adapter"

if [[ "$dry_run" -eq 0 ]]; then
  "$bin_dir/hya-backend" --version >/dev/null
  "$bin_dir/hya-backend" --help >/dev/null
  test -f "$lib_dir/compat-adapter/package.json"
  test -f "$lib_dir/compat-adapter/bun.lock"
  test -f "$lib_dir/compat-adapter/src/main.ts"
  test -d "$lib_dir/compat-adapter/node_modules"
  resolved=$(command -v hya-backend 2>/dev/null || true)
  if [[ "$resolved" != "$bin_dir/hya-backend" ]]; then
    echo "hya-backend is not first on PATH. Add this to your shell profile: export PATH=\"$bin_dir:\$PATH\"" >&2
    echo "expected: $bin_dir/hya-backend" >&2
    echo "resolved: ${resolved:-<missing>}" >&2
    false
  fi
  install_complete=1
  cleanup_leftovers
  say "hya-backend is on PATH: $resolved"
else
  say "+ $bin_dir/hya-backend --version"
  say "+ $bin_dir/hya-backend --help"
  say "+ test -f $lib_dir/compat-adapter/package.json"
  say "+ test -f $lib_dir/compat-adapter/bun.lock"
  say "+ test -f $lib_dir/compat-adapter/src/main.ts"
  say "+ test -d $lib_dir/compat-adapter/node_modules"
  say "+ PATH check: command -v hya-backend must resolve to $bin_dir/hya-backend"
fi

cat <<'GUIDANCE'

API setup:
  hya-backend works offline by default. To use a live provider, create:
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
    hya-backend serve
GUIDANCE
