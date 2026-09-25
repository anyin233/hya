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
  --prefix DIR                 Install into DIR/bin, DIR/bundles, and DIR/lib/hya (default: /usr/local)
  --bin-dir DIR                Install the backend into DIR, which must be named bin;
                               bundles go to DIR/../bundles and Bun programs to
                               DIR/../lib/hya. Overrides --prefix
  --profile release|dev|debug  Cargo build profile (default: release)
  --dry-run                    Print actions without building or installing
  -h, --help                   Show this help

Installs the release layout:
  bin/hya                   unified CLI: exec, serve, login, bundles, models, update, ...
  bundles/hya-*.hyabundle   the twelve trusted first-party bundles it loads at startup
  lib/hya/bun-adapter       Bun adapter for JavaScript bundle extensions
  lib/hya/tui               terminal UI that bare `hya` starts
  lib/hya/tui-web           WebUI host that serves the TUI to the browser

Requires Bun on PATH: each lib/hya program gets its production dependencies
from `bun install --frozen-lockfile --production`.
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

tool_libraries=(-p hya-base-tools -p hya-extended-tools -p hya-network-tools -p hya-channel-tools -p hya-todo-tools)
case "$profile" in
  release)
    profile_args=(--profile release)
    target_dir=${CARGO_TARGET_DIR:-target}/release
    ;;
  dev|debug)
    profile_args=()
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
bin_dir=${bin_dir%/}
if [[ "$(basename "$bin_dir")" != bin ]]; then
  echo "--bin-dir must be a directory named bin: hya loads its first-party bundles from ../bundles" >&2
  exit 2
fi

cd "$(dirname "$0")"
if [[ "$bin_dir" != /* ]]; then
  bin_dir="$(pwd -P)/$bin_dir"
fi
if [[ "$target_dir" != /* ]]; then
  target_dir="$(pwd -P)/$target_dir"
fi
root_dir="$(dirname "$bin_dir")"
lib_dir="$root_dir/lib/hya"
bundles_dir="$root_dir/bundles"
source_dir="$(pwd -P)"

# Bun programs installed under lib/hya: name, source directory, top-level
# files, and recursively copied directories (the release archive layout).
lib_names=(bun-adapter tui tui-web)
lib_sources=(
  "$source_dir/crates/hya-plugin-bun/adapter"
  "$source_dir/packages/hya-tui"
  "$source_dir/packages/hya-tui-web"
)
lib_files=(
  "package.json bun.lock"
  "package.json bun.lock bunfig.toml tsconfig.json"
  "package.json bun.lock tsconfig.json"
)
lib_dirs=("src" "src" "src web")
had_lib=(0 0 0)
placed_lib=(0 0 0)

lib_tmp() {
  printf '%s\n' "$lib_dir/.$1.tmp.$$"
}

lib_bak() {
  printf '%s\n' "$lib_dir/.$1.bak.$$"
}

tmp_backend="$bin_dir/.hya.tmp.$$"
tmp_bundles="$bundles_dir/.hya-bundles.tmp.$$"
bak_backend="$bin_dir/.hya.bak.$$"
bak_bundles="$bundles_dir/.hya-bundles.bak.$$"
rollback_enabled=0
install_complete=0
had_backend=0
placed_backend=0
placed_bundles=()

cleanup_leftovers() {
  local name
  rm -f "$tmp_backend"
  rm -rf "$tmp_bundles"
  for name in "${lib_names[@]}"; do
    rm -rf "$(lib_tmp "$name")"
  done
  if [[ "$install_complete" -eq 1 ]]; then
    rm -f "$bak_backend"
    rm -rf "$bak_bundles"
    for name in "${lib_names[@]}"; do
      rm -rf "$(lib_bak "$name")"
    done
  fi
}

restore_install() {
  if [[ "$rollback_enabled" -eq 0 || "$install_complete" -eq 1 ]]; then
    return 0
  fi

  if [[ "$placed_backend" -eq 1 ]]; then
    rm -f "$bin_dir/hya"
  fi
  local index
  for index in "${!lib_names[@]}"; do
    if [[ "${placed_lib[$index]}" -eq 1 ]]; then
      rm -rf "$lib_dir/${lib_names[$index]}"
    fi
  done
  local bundle
  for bundle in ${placed_bundles[@]+"${placed_bundles[@]}"}; do
    rm -f "$bundles_dir/$bundle"
  done
  if [[ "$had_backend" -eq 1 && -e "$bak_backend" ]]; then
    mv -f "$bak_backend" "$bin_dir/hya"
  fi
  for index in "${!lib_names[@]}"; do
    local bak
    bak=$(lib_bak "${lib_names[$index]}")
    if [[ "${had_lib[$index]}" -eq 1 && -e "$bak" ]]; then
      mv "$bak" "$lib_dir/${lib_names[$index]}"
    fi
  done
  if [[ -d "$bak_bundles" ]]; then
    for bundle in "$bak_bundles"/hya-*.hyabundle; do
      if [[ -e "$bundle" ]]; then
        mv -f "$bundle" "$bundles_dir/"
      fi
    done
    rmdir "$bak_bundles"
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
    echo "Or use a user-writable prefix: ./install.sh --prefix \"$HOME/.local\"" >&2
    exit 1
  fi
}

# Copy one lib/hya program into its temporary directory and install its
# production dependencies there, so the live install is replaced by a rename.
stage_lib() {
  local index=$1 name=${lib_names[$1]} source=${lib_sources[$1]} tmp file dir
  tmp=$(lib_tmp "$name")
  run mkdir -p "$tmp"
  for file in ${lib_files[$index]}; do
    run cp "$source/$file" "$tmp/"
  done
  for dir in ${lib_dirs[$index]}; do
    run mkdir -p "$tmp/$dir"
    run cp -R "$source/$dir/." "$tmp/$dir/"
  done
  say "+ (cd $tmp && bun install --frozen-lockfile --production)"
  if [[ "$dry_run" -eq 0 ]]; then
    (cd "$tmp" && bun install --frozen-lockfile --production)
  fi
}

trap on_error ERR INT TERM
say "Installing hya to $bin_dir"
say "Installing first-party bundles to $bundles_dir"
say "Installing Bun adapter, TUI, and WebUI host to $lib_dir/{bun-adapter,tui,tui-web}"
say "Rollback backup paths: $bak_backend $bak_bundles $(lib_bak '{bun-adapter,tui,tui-web}')"
say "Permission preflight: $bin_dir $bundles_dir $lib_dir"
preflight_path "$bin_dir"
preflight_path "$bundles_dir"
preflight_path "$lib_dir"
say "Bun preflight: bun"
run bun --version
run cargo build --locked ${profile_args[@]+"${profile_args[@]}"} -p hya-backend --bins
run cargo build --locked ${profile_args[@]+"${profile_args[@]}"} "${tool_libraries[@]}" --lib
run mkdir -p "$bin_dir" "$lib_dir" "$bundles_dir"
run install -m 0755 "$target_dir/hya" "$tmp_backend"
run cargo run --locked -p xtask -- stage-first-party-bundles --library-dir "$target_dir" --package-root "$tmp_bundles"
for index in "${!lib_names[@]}"; do
  stage_lib "$index"
done
[[ "$dry_run" -ne 0 ]] || rollback_enabled=1
if [[ -e "$bin_dir/hya" ]]; then
  had_backend=1
  run mv -f "$bin_dir/hya" "$bak_backend"
fi
for index in "${!lib_names[@]}"; do
  name=${lib_names[$index]}
  if [[ -e "$lib_dir/$name" ]]; then
    had_lib[index]=1
    run mv "$lib_dir/$name" "$(lib_bak "$name")"
  fi
done
run mkdir -p "$bak_bundles"
if [[ "$dry_run" -eq 0 ]]; then
  for bundle in "$bundles_dir"/hya-*.hyabundle; do
    if [[ -e "$bundle" ]]; then
      mv -f "$bundle" "$bak_bundles/"
    fi
  done
  for bundle in "$tmp_bundles/bundles"/hya-*.hyabundle; do
    name=$(basename "$bundle")
    placed_bundles+=("$name")
    mv -f "$bundle" "$bundles_dir/$name"
  done
else
  say "+ move existing $bundles_dir/hya-*.hyabundle to $bak_bundles"
  say "+ move $tmp_bundles/bundles/hya-*.hyabundle to $bundles_dir"
fi
placed_backend=1
run mv -f "$tmp_backend" "$bin_dir/hya"
for index in "${!lib_names[@]}"; do
  name=${lib_names[$index]}
  placed_lib[index]=1
  run mv "$(lib_tmp "$name")" "$lib_dir/$name"
done

verify_home="${TMPDIR:-/tmp}/hya-install-verify.$$"
first_party=(base-tools extended-tools network-tools channel-tools todo-tools core-skills core-commands core-agents agent-channels goal-loop plan-impl-review subagents)
if [[ "$dry_run" -eq 0 ]]; then
  "$bin_dir/hya" --version >/dev/null
  "$bin_dir/hya" --help >/dev/null
  mkdir -p "$verify_home"
  listing=$(HOME="$verify_home" XDG_CONFIG_HOME="$verify_home/config" XDG_DATA_HOME="$verify_home/data" \
    XDG_STATE_HOME="$verify_home/state" XDG_CACHE_HOME="$verify_home/cache" \
    "$bin_dir/hya" bundle list)
  rm -rf "$verify_home"
  for bundle in "${first_party[@]}"; do
    grep -q "^hya/$bundle " <<<"$listing"
  done
  test -f "$lib_dir/bun-adapter/package.json"
  test -f "$lib_dir/bun-adapter/bun.lock"
  test -f "$lib_dir/bun-adapter/src/main.ts"
  test -d "$lib_dir/bun-adapter/node_modules"
  test -f "$lib_dir/tui/src/main.ts"
  test -d "$lib_dir/tui/node_modules/@opentui/core"
  test -f "$lib_dir/tui-web/src/main.ts"
  test -f "$lib_dir/tui-web/web/index.html"
  test -d "$lib_dir/tui-web/node_modules/@xterm/xterm"
  mkdir -p "$verify_home"
  (cd "$verify_home" && env -u HYA_TUI_DIR -u HYA_TUI_WEB_DIR bun "$lib_dir/tui/src/main.ts" --help >/dev/null)
  (cd "$verify_home" && env -u HYA_TUI_DIR -u HYA_TUI_WEB_DIR bun "$lib_dir/tui-web/src/main.ts" --help >/dev/null)
  rm -rf "$verify_home"
  resolved=$(command -v hya 2>/dev/null || true)
  if [[ "$resolved" != "$bin_dir/hya" ]]; then
    echo "hya is not first on PATH. Add this to your shell profile: export PATH=\"$bin_dir:\$PATH\"" >&2
    echo "expected: $bin_dir/hya" >&2
    echo "resolved: ${resolved:-<missing>}" >&2
    false
  fi
  install_complete=1
  cleanup_leftovers
  # Releases before 0.38.0 shipped the executable as bin/hya-backend.
  if [[ -e "$bin_dir/hya-backend" ]]; then
    rm -f "$bin_dir/hya-backend"
    say "Removed legacy $bin_dir/hya-backend (now: hya)"
  fi
  say "hya is on PATH: $resolved"
else
  say "+ $bin_dir/hya --version"
  say "+ $bin_dir/hya --help"
  say "+ $bin_dir/hya bundle list (isolated HOME) must list: ${first_party[*]}"
  say "+ test -f $lib_dir/bun-adapter/package.json"
  say "+ test -f $lib_dir/bun-adapter/bun.lock"
  say "+ test -f $lib_dir/bun-adapter/src/main.ts"
  say "+ test -d $lib_dir/bun-adapter/node_modules"
  say "+ test -f $lib_dir/tui/src/main.ts; test -d $lib_dir/tui/node_modules/@opentui/core"
  say "+ test -f $lib_dir/tui-web/src/main.ts $lib_dir/tui-web/web/index.html; test -d $lib_dir/tui-web/node_modules/@xterm/xterm"
  say "+ bun $lib_dir/tui/src/main.ts --help; bun $lib_dir/tui-web/src/main.ts --help (outside the checkout)"
  say "+ PATH check: command -v hya must resolve to $bin_dir/hya"
  say "+ rm -f $bin_dir/hya-backend (legacy executable name, if present)"
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
    hya login anthropic "$ANTHROPIC_API_KEY"
    hya models
    hya serve
GUIDANCE
