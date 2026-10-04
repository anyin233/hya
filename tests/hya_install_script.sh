#!/usr/bin/env bash
# Contract tests for the curl-pipe release installer (`scripts/hya-install.sh`)
# that installs the backend and frontend releases, together or one side alone.
set -Eeuo pipefail

root="$(cd "$(dirname "$0")/.." && pwd -P)"
installer="$root/scripts/hya-install.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/hya-install-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
target=x86_64-unknown-linux-gnu
releases="$scratch/releases"
backend_prefix="$scratch/backend-prefix"
frontend_prefix="$scratch/frontend-prefix"
both_prefix="$scratch/both-prefix"
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
  mkdir -p "$releases/download/backend/$version"
  cp "$archive" "$releases/download/backend/$version/"
  (cd "$releases/download/backend/$version" && sha256 "./$(basename "$archive")" > SHA256SUMS)
  if [[ "$corrupt" == 1 ]]; then printf '%s\n' corrupt >>"$releases/download/backend/$version/$(basename "$archive")"; fi
}

make_frontend_release() {
  local version=$1 corrupt=${2:-0}
  local package="hya-frontend-${version}-${target}" archive="$releases/hya-frontend-${version}-${target}.tar.gz"
  rm -rf "$scratch/$package"
  mkdir -p "$scratch/$package/lib/hya/bin" "$scratch/$package/lib/hya/tui/src" \
    "$scratch/$package/lib/hya/tui/node_modules" "$scratch/$package/lib/hya/tui-web/src" \
    "$scratch/$package/lib/hya/tui-web/web" "$scratch/$package/lib/hya/tui-web/node_modules" \
    "$scratch/$package/lib/hya/tui-sdk/src"
  cat >"$scratch/$package/lib/hya/bin/bun" <<EOF
#!/bin/sh
if [ "\${1:-}" = --version ]; then printf '%s\\n' '$version'; else printf 'bun\\n'; fi
EOF
  chmod 755 "$scratch/$package/lib/hya/bin/bun"
  printf 'export const frontendVersion = "%s"\n' "$version" >"$scratch/$package/lib/hya/tui/frontend-version.ts"
  printf '%s\n' tui >"$scratch/$package/lib/hya/tui/src/main.ts"
  printf '%s\n' web >"$scratch/$package/lib/hya/tui-web/src/main.ts"
  printf '%s\n' sdk >"$scratch/$package/lib/hya/tui-sdk/src/main.ts"
  printf '%s\n' '<html></html>' >"$scratch/$package/lib/hya/tui-web/web/index.html"
  tar -czf "$archive" -C "$scratch" "$package"
  mkdir -p "$releases/download/frontend/$version"
  cp "$archive" "$releases/download/frontend/$version/"
  (cd "$releases/download/frontend/$version" && sha256 "./$(basename "$archive")" > SHA256SUMS)
  if [[ "$corrupt" == 1 ]]; then printf '%s\n' corrupt >>"$releases/download/frontend/$version/$(basename "$archive")"; fi
}

publish_latest() {
  local side=$1 version=$2
  mkdir -p "$releases/latest/download/$side"
  cp "$releases/download/$side/$version/SHA256SUMS" "$releases/latest/download/$side/SHA256SUMS"
}

install_into() {
  local prefix=$1
  shift
  env HYA_RELEASES_URL="file://$releases" HYA_TARGET="$target" HOME="$scratch/home" \
    sh "$installer" --prefix "$prefix" "$@"
}
backend_install() { install_into "$backend_prefix" --backend-only "$@"; }
frontend_install() { install_into "$frontend_prefix" --tui-only "$@"; }
frontend_version_of() { awk -F '"' '/frontendVersion =/ { print $2; exit }' "$1/lib/hya/tui/frontend-version.ts"; }

# Backend-only install: it provides hya and backend runtime, never frontend files.
make_backend_release 9.9.1
publish_latest backend 9.9.1
mkdir -p "$backend_prefix/lib/hya/claude-adapter" "$backend_prefix/bundles"
printf keep >"$backend_prefix/lib/hya/claude-adapter/marker"
printf mine >"$backend_prefix/bundles/my-own.hyabundle"
out="$(backend_install 2>&1)" || fail "backend install failed: $out"
[[ "$("$backend_prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'backend version mismatch'
[[ ! -e "$backend_prefix/lib/hya/tui" && ! -e "$backend_prefix/lib/hya/tui-web" && ! -e "$backend_prefix/lib/hya/tui-sdk" ]] || fail 'backend installed frontend files'
[[ "$(cat "$backend_prefix/lib/hya/claude-adapter/marker")" == keep ]] || fail 'foreign lib entry changed'
[[ "$(cat "$backend_prefix/bundles/my-own.hyabundle")" == mine ]] || fail 'foreign bundle changed'

# Reinstall is a no-op; force replaces the backend payload.
touch "$backend_prefix/lib/hya/bun-adapter/untouched"
out="$(backend_install 2>&1)" || fail "backend repeat failed: $out"
grep -q 'backend 9.9.1 is already installed' <<<"$out" || fail "backend no-op was not reported: $out"
[[ -e "$backend_prefix/lib/hya/bun-adapter/untouched" ]] || fail 'backend no-op replaced files'
out="$(backend_install --force 2>&1)" || fail "backend force failed: $out"
[[ ! -e "$backend_prefix/lib/hya/bun-adapter/untouched" ]] || fail 'backend force did not replace files'

# Checksum failure must not touch the installed backend.
make_backend_release 9.9.2 9.9.2 1
if out="$(backend_install --version 9.9.2 2>&1)"; then fail 'corrupt backend installed'; fi
grep -qi checksum <<<"$out" || fail "backend checksum error is unclear: $out"
[[ "$("$backend_prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'checksum failure changed backend'

# A backend that reports the wrong version is rolled back.
make_backend_release 9.9.3 0.0.1
if out="$(backend_install --version 9.9.3 2>&1)"; then fail 'misreporting backend installed'; fi
grep -q 'restored the previous backend' <<<"$out" || fail "backend rollback was not reported: $out"
[[ "$("$backend_prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'backend rollback did not restore hya'
[[ "$(cat "$backend_prefix/bundles/hya-base-tools.hyabundle")" == 9.9.1 ]] || fail 'backend rollback did not restore bundles'

# Frontend-only install has no hya command and says how to add it.
make_frontend_release 8.8.1
publish_latest frontend 8.8.1
out="$(frontend_install 2>&1)" || fail "frontend install failed: $out"
grep -q -- '--backend-only' <<<"$out" || fail "frontend-only install did not mention the missing backend: $out"
[[ ! -e "$frontend_prefix/bin/hya" ]] || fail 'frontend-only install created hya'
[[ -x "$frontend_prefix/lib/hya/bin/bun" ]] || fail 'frontend did not install Bun'
[[ -f "$frontend_prefix/lib/hya/tui/src/main.ts" && -f "$frontend_prefix/lib/hya/tui-web/src/main.ts" && -f "$frontend_prefix/lib/hya/tui-sdk/src/main.ts" ]] || fail 'frontend runtime missing'
out="$(frontend_install 2>&1)" || fail "frontend repeat failed: $out"
grep -q 'frontend 8.8.1 is already installed' <<<"$out" || fail "frontend no-op was not reported: $out"

# --tui-only beside a backend adds the frontend and leaves bin/hya alone.
cp -a "$backend_prefix" "$scratch/combined"
install_into "$scratch/combined" --tui-only --version 8.8.1 >/dev/null
[[ "$("$scratch/combined/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'frontend install changed backend command'
[[ -f "$scratch/combined/lib/hya/tui/src/main.ts" ]] || fail 'frontend was not added beside backend'

# Without a side flag both latest releases are installed, each at its own version.
out="$(install_into "$both_prefix" 2>&1)" || fail "combined install failed: $out"
[[ "$("$both_prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'combined install missed the backend'
[[ "$(frontend_version_of "$both_prefix")" == 8.8.1 ]] || fail 'combined install missed the frontend'
[[ -f "$both_prefix/bundles/hya-base-tools.hyabundle" && -f "$both_prefix/lib/hya/bun-adapter/src/main.ts" ]] || fail 'combined install missed backend payload'

# Updating both: a newer frontend is installed while the current backend is a no-op.
make_frontend_release 8.8.2
publish_latest frontend 8.8.2
out="$(install_into "$both_prefix" 2>&1)" || fail "combined update failed: $out"
grep -q 'backend 9.9.1 is already installed' <<<"$out" || fail "combined update reinstalled the backend: $out"
[[ "$(frontend_version_of "$both_prefix")" == 8.8.2 ]] || fail 'combined update did not update the frontend'

# A failed frontend after a successful backend keeps the new backend and the old frontend.
make_backend_release 9.9.4
publish_latest backend 9.9.4
make_frontend_release 8.8.3 1
publish_latest frontend 8.8.3
if out="$(install_into "$both_prefix" 2>&1)"; then fail 'corrupt frontend installed'; fi
grep -q 'backend 9.9.4' <<<"$out" || fail "partial failure did not report the installed backend: $out"
[[ "$("$both_prefix/bin/hya" --version)" == 'hya 9.9.4' ]] || fail 'backend was not updated before the frontend failure'
[[ "$(frontend_version_of "$both_prefix")" == 8.8.2 ]] || fail 'corrupt frontend changed the installed frontend'

# --backend-only updates only the backend.
make_frontend_release 8.8.4
publish_latest frontend 8.8.4
make_backend_release 9.9.5
publish_latest backend 9.9.5
install_into "$both_prefix" --backend-only >/dev/null
[[ "$("$both_prefix/bin/hya" --version)" == 'hya 9.9.5' ]] || fail '--backend-only did not update the backend'
[[ "$(frontend_version_of "$both_prefix")" == 8.8.2 ]] || fail '--backend-only changed the frontend'
install_into "$both_prefix" --tui-only >/dev/null
[[ "$(frontend_version_of "$both_prefix")" == 8.8.4 ]] || fail '--tui-only did not update the frontend'

# Conflicting side flags, unknown versions, and unknown targets fail without changes.
if out="$(install_into "$both_prefix" --backend-only --tui-only 2>&1)"; then fail 'conflicting side flags accepted'; fi
grep -q 'cannot be combined' <<<"$out" || fail "side flag conflict is unclear: $out"
if out="$(backend_install --version 1.0.0 2>&1)"; then fail 'unknown backend installed'; fi
grep -q 1.0.0 <<<"$out" || fail 'unknown backend error omitted version'
if out="$(env HYA_RELEASES_URL="file://$releases" HYA_TARGET=riscv64gc-unknown-linux-gnu \
  sh "$installer" --prefix "$backend_prefix" 2>&1)"; then fail 'unknown target installed'; fi
grep -q riscv64gc-unknown-linux-gnu <<<"$out" || fail 'unknown target error omitted target'
[[ "$("$backend_prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'failed installs changed the backend'

printf 'hya installer contract: ok\n'
