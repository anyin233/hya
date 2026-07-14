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

Installs three binaries and the Bun runtime:
  hya          user-facing TUI/frontend with native in-process hya backend
  hya-backend  backend CLI/API for login, exec, serve, and models
  hya-ts       TypeScript terminal frontend launcher
  lib/hya/hya-tui-ts  prepared TypeScript runtime
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
    build_cmd=(cargo build --locked --profile release -p hya -p hya-backend -p hya-ts --bins)
    target_dir=${CARGO_TARGET_DIR:-target}/release
    ;;
  dev|debug)
    build_cmd=(cargo build --locked -p hya -p hya-backend -p hya-ts --bins)
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

tmp_hya="$bin_dir/.hya.tmp.$$"
tmp_backend="$bin_dir/.hya-backend.tmp.$$"
tmp_ts="$bin_dir/.hya-ts.tmp.$$"
tmp_runtime="$lib_dir/.hya-tui-ts.tmp.$$"
bak_hya="$bin_dir/.hya.bak.$$"
bak_backend="$bin_dir/.hya-backend.bak.$$"
bak_ts="$bin_dir/.hya-ts.bak.$$"
bak_runtime="$lib_dir/.hya-tui-ts.bak.$$"
rollback_enabled=0
install_complete=0
had_hya=0
had_backend=0
had_ts=0
had_runtime=0
placed_hya=0
placed_backend=0
placed_ts=0
placed_runtime=0

cleanup_leftovers() {
  rm -f "$tmp_hya" "$tmp_backend" "$tmp_ts"
  rm -rf "$tmp_runtime"
  if [[ "$install_complete" -eq 1 ]]; then
    rm -f "$bak_hya" "$bak_backend" "$bak_ts"
    rm -rf "$bak_runtime"
  fi
}

restore_install() {
  if [[ "$rollback_enabled" -eq 0 || "$install_complete" -eq 1 ]]; then
    return 0
  fi

  [[ "$placed_hya" -eq 1 ]] && rm -f "$bin_dir/hya"
  [[ "$placed_backend" -eq 1 ]] && rm -f "$bin_dir/hya-backend"
  [[ "$placed_ts" -eq 1 ]] && rm -f "$bin_dir/hya-ts"
  [[ "$placed_runtime" -eq 1 ]] && rm -rf "$lib_dir/hya-tui-ts"
  if [[ "$had_hya" -eq 1 && -e "$bak_hya" ]]; then
    mv -f "$bak_hya" "$bin_dir/hya"
  fi
  if [[ "$had_backend" -eq 1 && -e "$bak_backend" ]]; then
    mv -f "$bak_backend" "$bin_dir/hya-backend"
  fi
  if [[ "$had_ts" -eq 1 && -e "$bak_ts" ]]; then
    mv -f "$bak_ts" "$bin_dir/hya-ts"
  fi
  if [[ "$had_runtime" -eq 1 && -e "$bak_runtime" ]]; then
    mv -f "$bak_runtime" "$lib_dir/hya-tui-ts"
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
say "Installing hya to $bin_dir"
say "Installing hya-tui-ts to $lib_dir/hya-tui-ts"
say "Rollback backup path: $bak_hya"
say "Rollback backup path: $bak_backend"
say "Rollback backup path: $bak_ts"
say "Rollback backup path: $bak_runtime"
say "Permission preflight: $bin_dir"
preflight_path "$bin_dir"
preflight_path "$lib_dir"
say "Bun preflight: bun"
run bun --version
run "${build_cmd[@]}"
run mkdir -p "$bin_dir" "$lib_dir" "$tmp_runtime/src"
run install -m 0755 "$target_dir/hya" "$tmp_hya"
run install -m 0755 "$target_dir/hya-backend" "$tmp_backend"
run install -m 0755 "$target_dir/hya-ts" "$tmp_ts"
run cp packages/hya-tui-ts/package.json packages/hya-tui-ts/bun.lock \
  packages/hya-tui-ts/bunfig.toml packages/hya-tui-ts/tsconfig.json \
  packages/hya-tui-ts/LICENSE packages/hya-tui-ts/UPSTREAM.md "$tmp_runtime/"
run cp -R packages/hya-tui-ts/src/. "$tmp_runtime/src/"
say "+ (cd $tmp_runtime && bun install --frozen-lockfile --production)"
if [[ "$dry_run" -eq 0 ]]; then
  (cd "$tmp_runtime" && bun install --frozen-lockfile --production)
fi
run bun packages/hya-tui-ts/scripts/prune-sdk-server.ts "$tmp_runtime"
[[ "$dry_run" -ne 0 ]] || rollback_enabled=1
if [[ -e "$bin_dir/hya" ]]; then
  had_hya=1
  run mv -f "$bin_dir/hya" "$bak_hya"
fi
if [[ -e "$bin_dir/hya-backend" ]]; then
  had_backend=1
  run mv -f "$bin_dir/hya-backend" "$bak_backend"
fi
if [[ -e "$bin_dir/hya-ts" ]]; then
  had_ts=1
  run mv -f "$bin_dir/hya-ts" "$bak_ts"
fi
if [[ -e "$lib_dir/hya-tui-ts" ]]; then
  had_runtime=1
  run mv "$lib_dir/hya-tui-ts" "$bak_runtime"
fi
placed_hya=1
run mv -f "$tmp_hya" "$bin_dir/hya"
placed_backend=1
run mv -f "$tmp_backend" "$bin_dir/hya-backend"
placed_ts=1
run mv -f "$tmp_ts" "$bin_dir/hya-ts"
placed_runtime=1
run mv "$tmp_runtime" "$lib_dir/hya-tui-ts"

if [[ "$dry_run" -eq 0 ]]; then
  "$bin_dir/hya" --version >/dev/null
  "$bin_dir/hya-backend" --help >/dev/null
  "$bin_dir/hya-ts" --help >/dev/null
  test -f "$lib_dir/hya-tui-ts/src/main.tsx"
  test -f "$lib_dir/hya-tui-ts/bunfig.toml"
  test -f "$lib_dir/hya-tui-ts/tsconfig.json"
  test -f "$lib_dir/hya-tui-ts/LICENSE"
  test -f "$lib_dir/hya-tui-ts/UPSTREAM.md"
  test -d "$lib_dir/hya-tui-ts/node_modules"
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
  say "+ $bin_dir/hya-ts --help"
  say "+ test -f $lib_dir/hya-tui-ts/src/main.tsx"
  say "+ test -f $lib_dir/hya-tui-ts/bunfig.toml"
  say "+ test -f $lib_dir/hya-tui-ts/tsconfig.json"
  say "+ test -f $lib_dir/hya-tui-ts/LICENSE"
  say "+ test -f $lib_dir/hya-tui-ts/UPSTREAM.md"
  say "+ test -d $lib_dir/hya-tui-ts/node_modules"
  say "+ PATH check: command -v hya must resolve to $bin_dir/hya"
fi

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
