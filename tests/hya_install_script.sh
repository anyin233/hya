#!/usr/bin/env bash
# Contract tests for the side-specific curl-pipe release installers.
set -Eeuo pipefail

root="$(cd "$(dirname "$0")/.." && pwd -P)"
backend_installer="$root/scripts/hya-install.sh"
frontend_installer="$root/scripts/hya-tui-install.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/hya-install-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
target=x86_64-unknown-linux-gnu
releases="$scratch/releases"
backend_prefix="$scratch/backend-prefix"
frontend_prefix="$scratch/frontend-prefix"
mkdir -p "$releases"

fail() { printf 'hya installer contract: %s\n' "$*" >&2; exit 1; }
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

make_backend_release() {
  local version=$1 reported=${2:-$1} corrupt=${3:-0}
  local package="hya-backend-${version}-${target}" archive="$releases/hya-backend-${version}-${target}.tar.gz"
  rm -rf "$scratch/$package"
  mkdir -p "$scratch/$package/bin" "$scratch/$package/lib/hya/bun-adapter/src" "$scratch/$package/bundles"
  cat >"$scratch/$package/bin/hya" <<EOF
#!/bin/sh
case "\${1:-}" in
  --version) printf 'hya %s\\n' '$reported' ;;
  serve) exit 1 ;;
  *) printf 'hya backend\\n' ;;
esac
EOF
  chmod 755 "$scratch/$package/bin/hya"
  printf '%s\n' backend >"$scratch/$package/lib/hya/bun-adapter/src/main.ts"
  printf '%s\n' "$version" >"$scratch/$package/bundles/hya-base-tools.hyabundle"
  tar -czf "$archive" -C "$scratch" "$package"
  mkdir -p "$releases/download/backend/v$version"
  cp "$archive" "$releases/download/backend/v$version/"
  (cd "$releases/download/backend/v$version" && sha256 "$(basename "$archive")" > SHA256SUMS)
  if [[ "$corrupt" == 1 ]]; then printf '%s\n' corrupt >>"$releases/download/backend/v$version/$(basename "$archive")"; fi
}

make_frontend_release() {
  local version=$1 corrupt=${2:-0}
  local package="hya-frontend-${version}-${target}" archive="$releases/hya-frontend-${version}-${target}.tar.gz"
  rm -rf "$scratch/$package"
  mkdir -p "$scratch/$package/lib/hya/bin" "$scratch/$package/lib/hya/tui/src" \
    "$scratch/$package/lib/hya/tui/node_modules" "$scratch/$package/lib/hya/tui-web/src" \
    "$scratch/$package/lib/hya/tui-web/web" "$scratch/$package/lib/hya/tui-web/node_modules"
  cat >"$scratch/$package/lib/hya/bin/bun" <<EOF
#!/bin/sh
if [ "\${1:-}" = --version ]; then printf '%s\\n' '$version'; else printf 'bun\\n'; fi
EOF
  chmod 755 "$scratch/$package/lib/hya/bin/bun"
  printf 'export const frontendVersion = "%s"\n' "$version" >"$scratch/$package/lib/hya/tui/frontend-version.ts"
  printf '%s\n' tui >"$scratch/$package/lib/hya/tui/src/main.ts"
  printf '%s\n' web >"$scratch/$package/lib/hya/tui-web/src/main.ts"
  printf '%s\n' '<html></html>' >"$scratch/$package/lib/hya/tui-web/web/index.html"
  tar -czf "$archive" -C "$scratch" "$package"
  mkdir -p "$releases/download/frontend/v$version"
  cp "$archive" "$releases/download/frontend/v$version/"
  (cd "$releases/download/frontend/v$version" && sha256 "$(basename "$archive")" > SHA256SUMS)
  if [[ "$corrupt" == 1 ]]; then printf '%s\n' corrupt >>"$releases/download/frontend/v$version/$(basename "$archive")"; fi
}

publish_latest() {
  local side=$1 version=$2
  mkdir -p "$releases/latest/download/$side"
  cp "$releases/download/$side/v$version/SHA256SUMS" "$releases/latest/download/$side/SHA256SUMS"
}

backend_install() {
  env HYA_RELEASES_URL="file://$releases" HYA_TARGET="$target" HOME="$scratch/home" \
    sh "$backend_installer" --prefix "$backend_prefix" "$@"
}
frontend_install() {
  env HYA_RELEASES_URL="file://$releases" HYA_TARGET="$target" HOME="$scratch/home" \
    sh "$frontend_installer" --prefix "$frontend_prefix" "$@"
}

# Backend-only install: it provides hya and backend runtime, never frontend files.
make_backend_release 9.9.1
publish_latest backend 9.9.1
mkdir -p "$backend_prefix/lib/hya/claude-adapter" "$backend_prefix/bundles"
printf keep >"$backend_prefix/lib/hya/claude-adapter/marker"
printf mine >"$backend_prefix/bundles/my-own.hyabundle"
out="$(backend_install 2>&1)" || fail "backend install failed: $out"
[[ "$("$backend_prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'backend version mismatch'
[[ ! -e "$backend_prefix/lib/hya/tui" && ! -e "$backend_prefix/lib/hya/tui-web" ]] || fail 'backend installed frontend files'
[[ "$(cat "$backend_prefix/lib/hya/claude-adapter/marker")" == keep ]] || fail 'foreign lib entry changed'
[[ "$(cat "$backend_prefix/bundles/my-own.hyabundle")" == mine ]] || fail 'foreign bundle changed'

# Reinstall is a no-op; force replaces the backend payload.
touch "$backend_prefix/lib/hya/bun-adapter/untouched"
out="$(backend_install 2>&1)" || fail "backend repeat failed: $out"
grep -q 'already installed' <<<"$out" || fail 'backend no-op was not reported'
[[ -e "$backend_prefix/lib/hya/bun-adapter/untouched" ]] || fail 'backend no-op replaced files'
out="$(backend_install --force 2>&1)" || fail "backend force failed: $out"
[[ ! -e "$backend_prefix/lib/hya/bun-adapter/untouched" ]] || fail 'backend force did not replace files'

# Checksum failure must not touch the installed backend.
make_backend_release 9.9.2 9.9.2 1
if out="$(backend_install --version 9.9.2 2>&1)"; then fail 'corrupt backend installed'; fi
grep -qi checksum <<<"$out" || fail "backend checksum error is unclear: $out"
[[ "$("$backend_prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'checksum failure changed backend'

# Frontend-only install has no hya command; adding it beside a backend preserves bin/hya.
make_frontend_release 8.8.1
publish_latest frontend 8.8.1
out="$(frontend_install 2>&1)" || fail "frontend install failed: $out"
[[ ! -e "$frontend_prefix/bin/hya" ]] || fail 'frontend-only install created hya'
[[ -x "$frontend_prefix/lib/hya/bin/bun" ]] || fail 'frontend did not install Bun'
[[ -f "$frontend_prefix/lib/hya/tui/src/main.ts" && -f "$frontend_prefix/lib/hya/tui-web/src/main.ts" ]] || fail 'frontend runtime missing'

cp -a "$backend_prefix" "$scratch/combined"
env HYA_RELEASES_URL="file://$releases" HYA_TARGET="$target" HOME="$scratch/home" \
  sh "$frontend_installer" --prefix "$scratch/combined" --version 8.8.1 >/dev/null
[[ "$("$scratch/combined/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'frontend install changed backend command'
[[ -f "$scratch/combined/lib/hya/tui/src/main.ts" ]] || fail 'frontend was not added beside backend'

# Unknown versions and targets fail without changing the current component.
if out="$(backend_install --version 1.0.0 2>&1)"; then fail 'unknown backend installed'; fi
grep -q 1.0.0 <<<"$out" || fail 'unknown backend error omitted version'
if out="$(env HYA_RELEASES_URL="file://$releases" HYA_TARGET=riscv64gc-unknown-linux-gnu \
  sh "$backend_installer" --prefix "$backend_prefix" 2>&1)"; then fail 'unknown target installed'; fi
grep -q riscv64gc-unknown-linux-gnu <<<"$out" || fail 'unknown target error omitted target'

printf 'side-specific hya installer contract: ok\n'
