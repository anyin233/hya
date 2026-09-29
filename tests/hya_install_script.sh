#!/usr/bin/env bash
# Contract test for scripts/hya-install.sh, the curl-pipe release installer
# that `hya update` also runs. Serves fake releases over file:// URLs.
set -Eeuo pipefail

root="$(cd "$(dirname "$0")/.." && pwd -P)"
installer="$root/scripts/hya-install.sh"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/hya-install-test.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT
target=x86_64-unknown-linux-gnu
releases="$scratch/releases"
prefix="$scratch/prefix"

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$@"; else shasum -a 256 "$@"; fi
}

# make_release VERSION [reported-version] [--corrupt]: publish a fake release
# whose `bin/hya --version` prints `hya <reported-version>`.
make_release() {
  local version=$1 reported=${2:-$1} corrupt=${3:-}
  local package="hya-$version-$target"
  local work="$scratch/build/$version"
  local out="$releases/download/v$version"
  mkdir -p "$work/$package/bin" "$work/$package/lib/hya/tui/src" \
    "$work/$package/lib/hya/bin" "$work/$package/bundles" "$out"
  printf '#!/bin/sh\necho "hya %s"\n' "$reported" >"$work/$package/bin/hya"
  chmod 755 "$work/$package/bin/hya"
  printf 'export const version = "%s";\n' "$version" >"$work/$package/lib/hya/tui/src/main.ts"
  printf '#!/bin/sh\necho 1.4.2\n' >"$work/$package/lib/hya/bin/bun"
  chmod 755 "$work/$package/lib/hya/bin/bun"
  printf '%s\n' "$version" >"$work/$package/bundles/hya-base-tools.hyabundle"
  tar -czf "$out/$package.tar.gz" -C "$work" "$package"
  (cd "$out" && sha256 "$package.tar.gz" >SHA256SUMS)
  # A real SHA256SUMS lists every target and the bundle assets.
  printf '%064d  hya-%s-aarch64-apple-darwin.tar.gz\n%064d  hya-base-tools-%s.hyabundle\n' \
    0 "$version" 0 "$version" >>"$out/SHA256SUMS"
  if [[ "$corrupt" == --corrupt ]]; then
    printf 'tampered' >>"$out/$package.tar.gz"
  fi
}

publish_latest() {
  mkdir -p "$releases/latest/download"
  cp "$releases/download/v$1/SHA256SUMS" "$releases/latest/download/SHA256SUMS"
}

install() {
  env HYA_RELEASES_URL="file://$releases" HYA_TARGET="$target" HOME="$scratch/home" \
    sh "$installer" --prefix "$prefix" "$@"
}

no_leftovers() {
  local left
  left="$(find "$prefix" -name '.hya-install.*' -print)"
  [[ -z "$left" ]] || fail "staging leftovers: $left"
}

make_release 9.9.1
publish_latest 9.9.1

# User content the installer must never touch.
mkdir -p "$prefix/lib/hya/claude-adapter" "$prefix/bundles"
echo keep >"$prefix/lib/hya/claude-adapter/marker"
echo mine >"$prefix/bundles/my-own.hyabundle"

# 1. Fresh install of the latest release.
out="$(install 2>&1)" || fail "fresh install failed: $out"
[[ "$("$prefix/bin/hya" --version)" == "hya 9.9.1" ]] || fail "installed binary is not 9.9.1"
grep -q 9.9.1 "$prefix/lib/hya/tui/src/main.ts" || fail "TUI not installed"
[[ -x "$prefix/lib/hya/bin/bun" ]] || fail "bundled Bun not installed"
grep -qx 9.9.1 "$prefix/bundles/hya-base-tools.hyabundle" || fail "bundle not installed"
[[ "$(cat "$prefix/lib/hya/claude-adapter/marker")" == keep ]] || fail "foreign lib entry touched"
[[ "$(cat "$prefix/bundles/my-own.hyabundle")" == mine ]] || fail "user bundle touched"
no_leftovers

# 2. Same version again is a no-op unless forced.
touch "$prefix/lib/hya/tui/untouched"
out="$(install 2>&1)" || fail "repeat install failed: $out"
grep -q "already installed" <<<"$out" || fail "repeat install did not report up to date: $out"
[[ -e "$prefix/lib/hya/tui/untouched" ]] || fail "up-to-date install replaced files"
out="$(install --force 2>&1)" || fail "forced install failed: $out"
[[ ! -e "$prefix/lib/hya/tui/untouched" ]] || fail "--force did not reinstall"
no_leftovers

# 3. A checksum mismatch aborts before touching the install.
make_release 9.9.2 9.9.2 --corrupt
if out="$(install --version 9.9.2 2>&1)"; then fail "tampered archive installed"; fi
grep -qi checksum <<<"$out" || fail "tamper failure does not name the checksum: $out"
[[ "$("$prefix/bin/hya" --version)" == "hya 9.9.1" ]] || fail "tampered install changed the binary"
no_leftovers

# 4. A release whose binary fails verification rolls back completely.
make_release 9.9.3 0.0.0
if out="$(install --version v9.9.3 2>&1)"; then fail "unverifiable release installed"; fi
[[ "$("$prefix/bin/hya" --version)" == "hya 9.9.1" ]] || fail "rollback did not restore the binary"
grep -q 9.9.1 "$prefix/lib/hya/tui/src/main.ts" || fail "rollback did not restore the TUI"
grep -qx 9.9.1 "$prefix/bundles/hya-base-tools.hyabundle" || fail "rollback did not restore bundles"
[[ "$(cat "$prefix/lib/hya/claude-adapter/marker")" == keep ]] || fail "rollback touched foreign lib entry"
no_leftovers

# 5. Updating to a newer latest release.
make_release 9.10.0
publish_latest 9.10.0
out="$(install 2>&1)" || fail "update failed: $out"
[[ "$("$prefix/bin/hya" --version)" == "hya 9.10.0" ]] || fail "update did not install 9.10.0"
grep -qx 9.10.0 "$prefix/bundles/hya-base-tools.hyabundle" || fail "update did not replace bundles"
no_leftovers

# 6. Unknown versions and targets fail with a clear message.
if out="$(install --version 1.0.0 2>&1)"; then fail "missing release installed"; fi
grep -q "1.0.0" <<<"$out" || fail "missing release error does not name the version: $out"
if out="$(env HYA_RELEASES_URL="file://$releases" HYA_TARGET=riscv64gc-unknown-linux-gnu \
  HOME="$scratch/home" sh "$installer" --prefix "$prefix" 2>&1)"; then
  fail "release without that target installed"
fi
grep -q riscv64gc-unknown-linux-gnu <<<"$out" || fail "missing target error does not name it: $out"
[[ "$("$prefix/bin/hya" --version)" == "hya 9.10.0" ]] || fail "failed install changed the binary"

# 7. The PATH hint compares real directories: a PATH entry that reaches
# <prefix>/bin through a symlink (macOS /tmp -> /private/tmp) needs no hint.
ln -s "$prefix" "$scratch/prefix-alias"
out="$(env PATH="$scratch/prefix-alias/bin:$PATH" HYA_RELEASES_URL="file://$releases" HYA_TARGET="$target" \
  HOME="$scratch/home" sh "$installer" --prefix "$prefix" --force 2>&1)" || fail "forced install failed: $out"
if grep -q "to PATH" <<<"$out"; then fail "PATH hint shown although PATH reaches $prefix/bin: $out"; fi
out="$(install --force 2>&1)" || fail "forced install failed: $out"
grep -q "to PATH" <<<"$out" || fail "PATH hint missing when $prefix/bin is not on PATH: $out"

echo "hya-install.sh contract: ok"
