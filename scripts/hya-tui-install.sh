#!/bin/sh
# Install the published hya frontend runtime (Bun, TUI, and WebUI).
set -eu
repo=${HYA_REPO:-anyin233/hya}
releases=${HYA_RELEASES_URL:-https://github.com/$repo/releases}
releases=${releases%/}
version=${HYA_VERSION:-}
prefix=${HYA_INSTALL_DIR:-}
force=0
usage() {
  cat <<'USAGE'
Usage: hya-tui-install.sh [--version VERSION] [--prefix DIR] [--force]

Install or update the hya frontend runtime from its release archive.
  --version VERSION  install this release (default: latest)
  --prefix DIR       install into DIR/lib/hya (default: $HOME/.local)
  --force            reinstall even when this version is already installed
  -h, --help         show this help
USAGE
}
say() { printf '%s\n' "$*"; }
die() { printf 'hya-tui-install: %s\n' "$*" >&2; exit 1; }
while [ $# -gt 0 ]; do
  case $1 in
    --version) [ $# -ge 2 ] || die '--version needs a value'; version=$2; shift 2 ;;
    --version=*) version=${1#--version=}; shift ;;
    --prefix) [ $# -ge 2 ] || die '--prefix needs a value'; prefix=$2; shift 2 ;;
    --prefix=*) prefix=${1#--prefix=}; shift ;;
    --force) force=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done
version=${version#v}
[ -n "$prefix" ] || prefix=${HOME:?HOME is not set; pass --prefix}/.local
prefix=${prefix%/}; [ -n "$prefix" ] || die 'the install prefix cannot be /'
detect_target() {
  [ -n "${HYA_TARGET:-}" ] && { printf '%s\n' "$HYA_TARGET"; return; }
  os=$(uname -s); arch=$(uname -m)
  case "$os" in
    Linux)
      case "$arch" in
        x86_64|amd64) printf '%s\n' x86_64-unknown-linux-gnu ;;
        aarch64|arm64) printf '%s\n' aarch64-unknown-linux-gnu ;;
        *) die "no hya frontend release for Linux on $arch" ;;
      esac ;;
    Darwin)
      case "$arch" in
        arm64|aarch64) printf '%s\n' aarch64-apple-darwin ;;
        x86_64)
          [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" = 1 ] || die "no hya frontend release for Intel Macs (Apple silicon is published)"
          printf '%s\n' aarch64-apple-darwin ;;
        *) die "no hya frontend release for macOS on $arch" ;;
      esac ;;
    *) die "no hya frontend release for $os/$arch" ;;
  esac
}
download() {
  if command -v curl >/dev/null 2>&1; then curl -fsSL --retry 3 -o "$2" "$1"
  elif command -v wget >/dev/null 2>&1; then wget -q -O "$2" "$1"
  else die 'curl or wget is required'; fi
}
sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | awk '{print $1}'
  else openssl dgst -sha256 -r "$1" | awk '{print $1}'; fi
}

latest_version() {
  api=${HYA_RELEASES_API_URL:-https://api.github.com/repos/$repo/releases?per_page=100}
  api_file=$tmp/releases.json
  download "$api" "$api_file" || die "could not list frontend releases at $api"
  latest=$(awk -v side="frontend/" '
    BEGIN { RS="\\}," }
    {
      if (index($0, "\"prerelease\": false") == 0) next
      marker = "\"tag_name\": \"" side
      start = index($0, marker)
      if (start == 0) next
      rest = substr($0, start + length(marker))
      end = index(rest, "\"")
      if (end > 0) { print substr(rest, 1, end - 1); exit }
    }
  ' "$api_file")
  [ -n "$latest" ] || die "no published frontend release found"
  printf '%s\n' "$latest"
}
installed_frontend_version() { awk -F '"' '/frontendVersion =/{print $2; exit}' "$1"; }
target=$(detect_target)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/hya-tui-install.XXXXXX")
stage=; swapping=0
rollback() {
  pkg=$1
  if [ ! -e "$pkg/lib/hya/bin/bun" ]; then rm -f "$prefix/lib/hya/bin/bun"; fi
  for name in tui tui-web; do
    if [ ! -e "$pkg/lib/hya/$name" ]; then rm -rf "$prefix/lib/hya/$name"; fi
    [ ! -e "$stage/old/$name" ] || mv "$stage/old/$name" "$prefix/lib/hya/$name"
  done
  [ ! -e "$stage/old/bun" ] || mv "$stage/old/bun" "$prefix/lib/hya/bin/bun"
}
on_exit() {
  status=$1; trap - EXIT
  if [ "$status" -ne 0 ] && [ "$swapping" -eq 1 ]; then
    if rollback "$package_dir"; then say "Restored the previous frontend installation in $prefix" >&2
    else say "Frontend rollback failed; previous files are in $stage/old" >&2; stage=; fi
  fi
  [ -z "$stage" ] || rm -rf "$stage"; rm -rf "$tmp"; exit "$status"
}
trap 'on_exit $?' EXIT
trap 'exit 130' INT; trap 'exit 143' TERM
if [ -n "$version" ]; then
  sums_url=$releases/download/frontend/$version/SHA256SUMS; label=$version
  download "$sums_url" "$tmp/SHA256SUMS" || die "no hya frontend release $label at $sums_url"
else
  sums_url=$releases/latest/download/frontend/SHA256SUMS; label=latest
  if ! download "$sums_url" "$tmp/SHA256SUMS"; then
    case "$releases" in
      https://github.com/*|http://github.com/*)
        version=$(latest_version)
        sums_url=$releases/download/frontend/$version/SHA256SUMS
        download "$sums_url" "$tmp/SHA256SUMS" || die "no hya frontend release $label at $sums_url"
        ;;
      *)
        sums_url=$releases/latest/download/SHA256SUMS
        download "$sums_url" "$tmp/SHA256SUMS" || die "no hya frontend release $label at $sums_url"
        ;;
    esac
  fi
fi
archive=; expected=
while read -r sum name; do
  name=${name#\*}
  name=${name#./}
  case $name in
    hya-frontend-*-$target.tar.gz)
      if [ -z "$version" ] || [ "$name" = "hya-frontend-$version-$target.tar.gz" ]; then archive=$name; expected=$sum; fi ;;
  esac
done <"$tmp/SHA256SUMS"
[ -n "$archive" ] || die "hya frontend release $label has no archive for $target"
release=${archive#hya-frontend-}; release=${release%-"$target".tar.gz}
if [ "$force" -eq 0 ] && [ -f "$prefix/lib/hya/tui/frontend-version.ts" ] && [ "$(installed_frontend_version "$prefix/lib/hya/tui/frontend-version.ts" || true)" = "$release" ]; then
  say "hya frontend $release is already installed in $prefix (pass --force to reinstall)"; exit 0
fi
say "Downloading hya frontend $release for $target"
download "$releases/download/frontend/$release/$archive" "$tmp/$archive" || die "could not download $releases/download/frontend/$release/$archive"
actual=$(sha256_of "$tmp/$archive"); [ "$actual" = "$expected" ] || die "checksum mismatch for $archive: expected $expected, got $actual"
mkdir -p "$prefix/lib/hya"; stage=$prefix/.hya-tui-install.$$; mkdir -p "$stage/new" "$stage/old"
tar -xzf "$tmp/$archive" -C "$stage/new"; package_dir=$stage/new/hya-frontend-$release-$target
[ ! -e "$package_dir/bin/hya" ] || die "$archive unexpectedly contains backend bin/hya"
[ -x "$package_dir/lib/hya/bin/bun" ] || die "$archive has no frontend lib/hya/bin/bun"
for name in tui tui-web; do [ -e "$package_dir/lib/hya/$name" ] || die "$archive has no frontend lib/hya/$name"; done
swapping=1
mkdir -p "$prefix/lib/hya"
if [ -e "$prefix/lib/hya/bin/bun" ]; then mkdir -p "$stage/old"; mv "$prefix/lib/hya/bin/bun" "$stage/old/bun"; fi
for name in tui tui-web; do if [ -e "$prefix/lib/hya/$name" ]; then mv "$prefix/lib/hya/$name" "$stage/old/$name"; fi; done
mkdir -p "$prefix/lib/hya/bin"; mv "$package_dir/lib/hya/bin/bun" "$prefix/lib/hya/bin/bun"
for name in tui tui-web; do mv "$package_dir/lib/hya/$name" "$prefix/lib/hya/$name"; done
placed=$(installed_frontend_version "$prefix/lib/hya/tui/frontend-version.ts" || true); [ "$placed" = "$release" ] || die "installed frontend reports version '${placed:-none}', expected $release"
swapping=0; say "Installed hya frontend $release to $prefix/lib/hya"
