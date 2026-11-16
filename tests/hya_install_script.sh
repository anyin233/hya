#!/usr/bin/env bash
# Contract tests for the curl-pipe release installer (`scripts/hya-install.sh`)
# that installs unified `v<version>` releases (`hya-<version>-<target>.tar.gz`).
set -Eeuo pipefail

root="$(cd "$(dirname "$0")/.." && pwd -P)"
installer="$root/scripts/hya-install.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/hya-install-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
target=x86_64-unknown-linux-gnu
releases="$scratch/releases"
prefix="$scratch/prefix"
mkdir -p "$releases"

fail() { printf 'hya installer contract: %s\n' "$*" >&2; exit 1; }
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

make_release() {
  local version=$1 reported=${2:-$1} corrupt=${3:-0} frontend_reported=${4:-$1}
  local package="hya-${version}-${target}" archive="$releases/hya-${version}-${target}.tar.gz"
  rm -rf "$scratch/$package"
  mkdir -p \
    "$scratch/$package/bin" \
    "$scratch/$package/bundles" \
    "$scratch/$package/lib/hya/bin" \
    "$scratch/$package/lib/hya/bun-adapter/src" \
    "$scratch/$package/lib/hya/tui/src" \
    "$scratch/$package/lib/hya/tui/node_modules" \
    "$scratch/$package/lib/hya/tui-web/src" \
    "$scratch/$package/lib/hya/tui-web/web" \
    "$scratch/$package/lib/hya/tui-web/node_modules" \
    "$scratch/$package/lib/hya/tui-sdk/src"
  cat >"$scratch/$package/bin/hya" <<EOF
#!/bin/sh
case "\${1:-}" in
  --version) printf 'hya %s\\n' '$reported' ;;
  serve) exit 1 ;;
  *) printf 'hya\\n' ;;
esac
EOF
  chmod 755 "$scratch/$package/bin/hya"
  cat >"$scratch/$package/lib/hya/bin/bun" <<EOF
#!/bin/sh
if [ "\${1:-}" = --version ]; then printf '1.4.2\\n'; else printf 'bun\\n'; fi
EOF
  chmod 755 "$scratch/$package/lib/hya/bin/bun"
  printf '%s\n' backend >"$scratch/$package/lib/hya/bun-adapter/src/main.ts"
  printf 'export const frontendVersion = "%s"\n' "$frontend_reported" >"$scratch/$package/lib/hya/tui/frontend-version.ts"
  printf '%s\n' tui >"$scratch/$package/lib/hya/tui/src/main.ts"
  printf '%s\n' web >"$scratch/$package/lib/hya/tui-web/src/main.ts"
  printf '%s\n' sdk >"$scratch/$package/lib/hya/tui-sdk/src/main.ts"
  printf '%s\n' '<html></html>' >"$scratch/$package/lib/hya/tui-web/web/index.html"
  printf '%s\n' "$version" >"$scratch/$package/bundles/hya-base-tools.hyabundle"
  tar -czf "$archive" -C "$scratch" "$package"
  mkdir -p "$releases/download/v$version"
  cp "$archive" "$releases/download/v$version/"
  (cd "$releases/download/v$version" && sha256 "./$(basename "$archive")" > SHA256SUMS)
  if [[ "$corrupt" == 1 ]]; then printf '%s\n' corrupt >>"$releases/download/v$version/$(basename "$archive")"; fi
}

publish_latest() {
  local version=$1
  mkdir -p "$releases/latest/download"
  cp "$releases/download/v$version/SHA256SUMS" "$releases/latest/download/SHA256SUMS"
}

install_into() {
  local dest=$1
  shift
  env HYA_RELEASES_URL="file://$releases" HYA_TARGET="$target" HOME="$scratch/home" \
    sh "$installer" --prefix "$dest" "$@"
}

frontend_version_of() { awk -F '"' '/frontendVersion =/ { print $2; exit }' "$1/lib/hya/tui/frontend-version.ts"; }

# Unified install places binary, bundles, Bun, adapter, TUI, WebUI, and TUI SDK while preserving foreign files.
make_release 9.9.1
publish_latest 9.9.1
mkdir -p "$prefix/lib/hya/claude-adapter" "$prefix/bundles"
printf keep >"$prefix/lib/hya/claude-adapter/marker"
printf mine >"$prefix/bundles/my-own.hyabundle"
out="$(install_into "$prefix" 2>&1)" || fail "unified install failed: $out"
[[ "$("$prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'version mismatch'
[[ "$(frontend_version_of "$prefix")" == '9.9.1' ]] || fail 'frontend version mismatch'
[[ -x "$prefix/lib/hya/bin/bun" ]] || fail 'missing packaged Bun'
[[ -f "$prefix/lib/hya/bun-adapter/src/main.ts" && -f "$prefix/lib/hya/tui/src/main.ts" && -f "$prefix/lib/hya/tui-web/src/main.ts" && -f "$prefix/lib/hya/tui-sdk/src/main.ts" ]] || fail 'missing runtime payload'
[[ "$(cat "$prefix/lib/hya/claude-adapter/marker")" == keep ]] || fail 'foreign lib entry changed'
[[ "$(cat "$prefix/bundles/my-own.hyabundle")" == mine ]] || fail 'foreign bundle changed'

# Reinstall is a no-op; --force replaces the payload.
touch "$prefix/lib/hya/bun-adapter/untouched"
out="$(install_into "$prefix" 2>&1)" || fail "repeat install failed: $out"
grep -q 'hya 9.9.1 is already installed' <<<"$out" || fail "no-op was not reported: $out"
[[ -e "$prefix/lib/hya/bun-adapter/untouched" ]] || fail 'no-op replaced files'
out="$(install_into "$prefix" --force 2>&1)" || fail "force install failed: $out"
[[ ! -e "$prefix/lib/hya/bun-adapter/untouched" ]] || fail 'force did not replace files'

# Checksum failure must not touch the installed release.
make_release 9.9.2 9.9.2 1
if out="$(install_into "$prefix" --version 9.9.2 2>&1)"; then fail 'corrupt release installed'; fi
grep -qi checksum <<<"$out" || fail "checksum error is unclear: $out"
[[ "$("$prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'checksum failure changed installation'

# A release whose binary or frontend reports the wrong version is rolled back.
make_release 9.9.3 0.0.1
if out="$(install_into "$prefix" --version v9.9.3 2>&1)"; then fail 'misreporting binary installed'; fi
grep -q 'restored the previous installation' <<<"$out" || fail "rollback was not reported: $out"
[[ "$("$prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'rollback did not restore hya'
[[ "$(cat "$prefix/bundles/hya-base-tools.hyabundle")" == 9.9.1 ]] || fail 'rollback did not restore bundles'

make_release 9.9.4 9.9.4 0 0.0.2
if out="$(install_into "$prefix" --version 9.9.4 2>&1)"; then fail 'misreporting frontend installed'; fi
grep -q 'restored the previous installation' <<<"$out" || fail "frontend rollback was not reported: $out"
[[ "$("$prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'frontend rollback did not restore hya'
[[ "$(frontend_version_of "$prefix")" == '9.9.1' ]] || fail 'frontend rollback did not restore frontend'

# Pre-release versions (`vx.x.x-dev.x` and `x.x.x-dev.x`) install when pinned with --version, while latest stays stable.
make_release 9.10.0-dev.1
out="$(install_into "$scratch/prerelease" --version v9.10.0-dev.1 2>&1)" || fail "prerelease install failed: $out"
[[ "$("$scratch/prerelease/bin/hya" --version)" == 'hya 9.10.0-dev.1' ]] || fail 'prerelease version mismatch'
[[ "$(frontend_version_of "$scratch/prerelease")" == '9.10.0-dev.1' ]] || fail 'prerelease frontend mismatch'

# Retired split flags, unknown versions, and unknown targets fail without changes.
if out="$(install_into "$prefix" --backend-only 2>&1)"; then fail 'retired --backend-only accepted'; fi
if out="$(install_into "$prefix" --tui-only 2>&1)"; then fail 'retired --tui-only accepted'; fi
if out="$(install_into "$prefix" --version 1.0.0 2>&1)"; then fail 'unknown version installed'; fi
grep -q 1.0.0 <<<"$out" || fail 'unknown version error omitted version'
if out="$(env HYA_RELEASES_URL="file://$releases" HYA_TARGET=riscv64gc-unknown-linux-gnu \
  sh "$installer" --prefix "$prefix" 2>&1)"; then fail 'unknown target installed'; fi
grep -q riscv64gc-unknown-linux-gnu <<<"$out" || fail 'unknown target error omitted target'
[[ "$("$prefix/bin/hya" --version)" == 'hya 9.9.1' ]] || fail 'failed installs changed installation'

printf 'hya installer contract: ok\n'
