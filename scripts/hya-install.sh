#!/bin/sh
# hya release installer: download a published hya release for this machine,
# verify it against the release's SHA256SUMS, and install it into a prefix.
#
#   curl -fsSL https://hya.ed-aisys.com/install.sh | sh
#   curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --version 0.43.23 --prefix /opt/hya
#
# `hya update` runs this same script for the prefix of the running hya.
# Host it anywhere; it only reads release assets from HYA_RELEASES_URL.
#
# Environment (flags win over environment):
#   HYA_REPO          GitHub owner/repo of the releases     (anyin233/hya)
#   HYA_RELEASES_URL  release base URL; must serve latest/download/<asset>
#                     and download/v<version>/<asset>      (https://github.com/$HYA_REPO/releases)
#   HYA_VERSION       version to install                    (latest)
#   HYA_INSTALL_DIR   install prefix                        ($HOME/.local)
#   HYA_TARGET        override the detected target triple
set -eu

repo=${HYA_REPO:-anyin233/hya}
releases=${HYA_RELEASES_URL:-https://github.com/$repo/releases}
releases=${releases%/}
version=${HYA_VERSION:-}
prefix=${HYA_INSTALL_DIR:-}
force=0

usage() {
  cat <<'USAGE'
Usage: hya-install.sh [--version VERSION] [--prefix DIR] [--force]

Install or update hya from its GitHub release.

  --version VERSION  install this release (default: the latest release)
  --prefix DIR       install into DIR/bin, DIR/lib/hya, DIR/bundles
                     (default: $HOME/.local)
  --force            reinstall even when this version is already installed
  -h, --help         show this help
USAGE
}

say() {
  printf '%s\n' "$*"
}

die() {
  printf 'hya-install: %s\n' "$*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case $1 in
    --version)
      [ $# -ge 2 ] || die "--version needs a value"
      version=$2
      shift 2
      ;;
    --version=*)
      version=${1#--version=}
      shift
      ;;
    --prefix)
      [ $# -ge 2 ] || die "--prefix needs a value"
      prefix=$2
      shift 2
      ;;
    --prefix=*)
      prefix=${1#--prefix=}
      shift
      ;;
    --force)
      force=1
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      die "unknown argument: $1"
      ;;
  esac
done

version=${version#v}
[ -n "$prefix" ] || prefix=${HOME:?HOME is not set; pass --prefix}/.local
prefix=${prefix%/}
[ -n "$prefix" ] || die "the install prefix cannot be /"

detect_target() {
  if [ -n "${HYA_TARGET:-}" ]; then
    printf '%s\n' "$HYA_TARGET"
    return
  fi
  os=$(uname -s)
  arch=$(uname -m)
  case $os in
    Linux)
      case $arch in
        x86_64 | amd64) arch=x86_64 ;;
        aarch64 | arm64) arch=aarch64 ;;
        *) die "no hya release for Linux on $arch (x86_64 and aarch64 are published)" ;;
      esac
      if ldd --version 2>&1 | grep -qi musl; then
        die "hya releases are built for glibc Linux; this system uses musl"
      fi
      printf '%s-unknown-linux-gnu\n' "$arch"
      ;;
    Darwin)
      case $arch in
        arm64 | aarch64) arch=aarch64 ;;
        x86_64)
          # An x86_64 shell under Rosetta on Apple silicon still gets the native build.
          if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ]; then
            arch=aarch64
          fi
          ;;
        *) die "no hya release for macOS on $arch" ;;
      esac
      printf '%s-apple-darwin\n' "$arch"
      ;;
    *) die "no hya release for $os (Linux and macOS are published)" ;;
  esac
}

download() {
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$2" "$1"
  else
    die "curl or wget is required"
  fi
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    openssl dgst -sha256 -r "$1" | awk '{print $1}'
  fi
}

# `hya --version` prints `hya <version>`.
installed_version() {
  "$1" --version 2>/dev/null | awk 'NR == 1 { print $NF }'
}

target=$(detect_target)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/hya-install.XXXXXX")
stage=""
swapping=0

rollback() {
  pkg_dir=$1
  old=$stage/old
  if [ ! -e "$pkg_dir/bin/hya" ]; then
    rm -f "$prefix/bin/hya"
  fi
  if [ -e "$old/bin/hya" ]; then
    mv -f "$old/bin/hya" "$prefix/bin/hya"
  fi
  while IFS= read -r name; do
    if [ ! -e "$pkg_dir/lib/hya/$name" ]; then
      rm -rf "${prefix:?}/lib/hya/$name"
    fi
    if [ -e "$old/lib/$name" ]; then
      mv "$old/lib/$name" "$prefix/lib/hya/$name"
    fi
  done <"$stage/lib.list"
  while IFS= read -r name; do
    if [ ! -e "$pkg_dir/bundles/$name" ]; then
      rm -f "$prefix/bundles/$name"
    fi
  done <"$stage/bundles.list"
  for bundle in "$old"/bundles/hya-*.hyabundle; do
    [ -e "$bundle" ] || continue
    mv -f "$bundle" "$prefix/bundles/"
  done
}

on_exit() {
  status=$1
  trap - EXIT
  if [ "$status" -ne 0 ] && [ "$swapping" -eq 1 ]; then
    if (rollback "$package_dir"); then
      printf 'hya-install: restored the previous installation in %s\n' "$prefix" >&2
    else
      printf 'hya-install: rollback failed; the previous installation is in %s\n' "$stage/old" >&2
      stage=""
    fi
  fi
  [ -z "$stage" ] || rm -rf "$stage"
  rm -rf "$tmp"
  exit "$status"
}
trap 'on_exit $?' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [ -n "$version" ]; then
  sums_url=$releases/download/v$version/SHA256SUMS
  label=$version
else
  sums_url=$releases/latest/download/SHA256SUMS
  label=latest
fi
download "$sums_url" "$tmp/SHA256SUMS" || die "no hya release $label at $sums_url"

archive=""
expected=""
while read -r sum name; do
  name=${name#\*}
  case $name in
    hya-*-"$target".tar.gz)
      if [ -z "$version" ] || [ "$name" = "hya-$version-$target.tar.gz" ]; then
        archive=$name
        expected=$sum
      fi
      ;;
  esac
done <"$tmp/SHA256SUMS"
[ -n "$archive" ] || die "hya release $label has no archive for $target"
release=${archive#hya-}
release=${release%-"$target".tar.gz}

if [ "$force" -eq 0 ] && [ -x "$prefix/bin/hya" ] &&
  [ "$(installed_version "$prefix/bin/hya" || true)" = "$release" ]; then
  say "hya $release is already installed in $prefix (pass --force to reinstall)"
  exit 0
fi

say "Downloading hya $release for $target"
download "$releases/download/v$release/$archive" "$tmp/$archive" ||
  die "could not download $releases/download/v$release/$archive"
actual=$(sha256_of "$tmp/$archive")
[ "$actual" = "$expected" ] ||
  die "checksum mismatch for $archive: expected $expected, got $actual"

# Unpack beside the install so every placement is a same-filesystem rename.
mkdir -p "$prefix/bin" "$prefix/lib/hya" "$prefix/bundles"
stage=$prefix/.hya-install.$$
mkdir -p "$stage/new" "$stage/old/bin" "$stage/old/lib" "$stage/old/bundles"
tar -xzf "$tmp/$archive" -C "$stage/new"
package_dir=$stage/new/hya-$release-$target
[ -f "$package_dir/bin/hya" ] || die "$archive has no hya-$release-$target/bin/hya"
(cd "$package_dir/lib/hya" && for name in *; do if [ -e "$name" ]; then printf '%s\n' "$name"; fi; done) >"$stage/lib.list"
(cd "$package_dir/bundles" && for name in hya-*.hyabundle; do if [ -e "$name" ]; then printf '%s\n' "$name"; fi; done) >"$stage/bundles.list"

swapping=1
if [ -e "$prefix/bin/hya" ]; then
  mv -f "$prefix/bin/hya" "$stage/old/bin/hya"
fi
mv -f "$package_dir/bin/hya" "$prefix/bin/hya"
while IFS= read -r name; do
  if [ -e "$prefix/lib/hya/$name" ]; then
    mv "$prefix/lib/hya/$name" "$stage/old/lib/$name"
  fi
  mv "$package_dir/lib/hya/$name" "$prefix/lib/hya/$name"
done <"$stage/lib.list"
# Earlier releases' first-party bundles are replaced as a set.
for bundle in "$prefix"/bundles/hya-*.hyabundle; do
  [ -e "$bundle" ] || continue
  mv -f "$bundle" "$stage/old/bundles/"
done
while IFS= read -r name; do
  mv -f "$package_dir/bundles/$name" "$prefix/bundles/$name"
done <"$stage/bundles.list"

placed=$(installed_version "$prefix/bin/hya" || true)
[ "$placed" = "$release" ] ||
  die "the installed hya reports version '${placed:-none}', expected $release"
swapping=0

say "Installed hya $release to $prefix/bin/hya"
case ":${PATH:-}:" in
  *":$prefix/bin:"*) ;;
  *) say "Add $prefix/bin to PATH, for example: export PATH=\"$prefix/bin:\$PATH\"" ;;
esac
if "$prefix/bin/hya" serve status >/dev/null 2>&1; then
  say "The running hya backend keeps its old version until you run: hya serve restart"
fi
