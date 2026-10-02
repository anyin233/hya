#!/bin/sh
# hya installer: download the newest published hya backend and frontend
# releases for this machine from GitHub, verify each archive against its
# release's SHA256SUMS, and install both into one prefix.
#
#   curl -fsSL https://hya.ed-aisys.com/install.sh | sh
#   curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --tui-only
#   curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --backend-only --version 0.44.0 --prefix /opt/hya
#
# Releases do not carry this script: hya.ed-aisys.com/install.sh serves it from
# the repository's main branch, and `hya update` runs the copy compiled into
# the running hya for that hya's prefix. It only reads release assets from
# HYA_RELEASES_URL.
#
# Environment (flags win over environment):
#   HYA_REPO              GitHub owner/repo of the releases     (anyin233/hya)
#   HYA_RELEASES_URL      release base URL; must serve
#                         download/<side>/<version>/<asset> and, for the latest
#                         release, latest/download/<side>/SHA256SUMS or the
#                         GitHub releases API (side is backend or frontend)
#   HYA_RELEASES_API_URL  GitHub releases API used to find the newest side tag
#   HYA_VERSION           version of each selected side         (latest)
#   HYA_INSTALL_DIR       install prefix                        ($HOME/.local)
#   HYA_TARGET            override the detected target triple
set -eu

repo=${HYA_REPO:-anyin233/hya}
releases=${HYA_RELEASES_URL:-https://github.com/$repo/releases}
releases=${releases%/}
version=${HYA_VERSION:-}
prefix=${HYA_INSTALL_DIR:-}
force=0
backend=1
frontend=1

usage() {
  cat <<'USAGE'
Usage: install.sh [--backend-only | --tui-only] [--version VERSION] [--prefix DIR] [--force]

Install or update hya from its GitHub releases. The backend (bin/hya, bundles,
Bun adapter) and the frontend (Bun, TUI, WebUI) are released separately; both
are installed unless one side is selected.

  --backend-only     install only the backend release
  --tui-only         install only the frontend (TUI and WebUI) release
  --version VERSION  install this release of each selected side
                     (default: the latest release of each side)
  --prefix DIR       install into DIR/bin, DIR/lib/hya, DIR/bundles
                     (default: $HOME/.local)
  --force            reinstall a side even when that version is installed
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

only=""
while [ $# -gt 0 ]; do
  case $1 in
    --backend-only | --tui-only)
      [ -z "$only" ] || [ "$only" = "$1" ] ||
        die "--backend-only and --tui-only cannot be combined"
      only=$1
      shift
      ;;
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
case $only in
  --backend-only) frontend=0 ;;
  --tui-only) backend=0 ;;
esac

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
        arm64 | aarch64) ;;
        x86_64)
          # An x86_64 shell under Rosetta on Apple silicon still gets the native build.
          if [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" != 1 ]; then
            die "no hya release for Intel Macs (Apple silicon is published)"
          fi
          ;;
        *) die "no hya release for macOS on $arch" ;;
      esac
      printf 'aarch64-apple-darwin\n'
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

# The newest non-prerelease `<side>/<version>` tag. GitHub's single `latest`
# release belongs to whichever side published last, so it cannot name a side.
latest_version() {
  api=${HYA_RELEASES_API_URL:-https://api.github.com/repos/$repo/releases?per_page=100}
  api_file=$tmp/releases.json
  download "$api" "$api_file" || die "could not list $1 releases at $api"
  latest=$(awk -v side="$1/" '
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
  [ -n "$latest" ] || die "no published $1 release found"
  printf '%s\n' "$latest"
}

# Resolve the side's release: download its SHA256SUMS and set `archive`,
# `expected`, and `release` for `hya-<side>-<release>-<target>.tar.gz`.
resolve_release() {
  side=$1
  want=$version
  sums=$tmp/$side.SHA256SUMS
  if [ -n "$want" ]; then
    label=$want
    sums_url=$releases/download/$side/$want/SHA256SUMS
    download "$sums_url" "$sums" || die "no hya $side release $label at $sums_url"
  else
    label=latest
    case $releases in
      https://github.com/* | http://github.com/*)
        want=$(latest_version "$side")
        sums_url=$releases/download/$side/$want/SHA256SUMS
        download "$sums_url" "$sums" || die "no hya $side release $want at $sums_url"
        ;;
      *)
        # Mirrors: a side-specific latest path, else a single latest release.
        sums_url=$releases/latest/download/$side/SHA256SUMS
        if ! download "$sums_url" "$sums" 2>/dev/null; then
          sums_url=$releases/latest/download/SHA256SUMS
          download "$sums_url" "$sums" || die "no hya $side release $label at $sums_url"
        fi
        ;;
    esac
  fi
  archive=""
  expected=""
  while read -r sum name; do
    name=${name#\*}
    name=${name#./}
    case $name in
      hya-$side-*-$target.tar.gz)
        if [ -z "$want" ] || [ "$name" = "hya-$side-$want-$target.tar.gz" ]; then
          archive=$name
          expected=$sum
        fi
        ;;
    esac
  done <"$sums"
  [ -n "$archive" ] || die "hya $side release $label has no archive for $target"
  release=${archive#hya-"$side"-}
  release=${release%-"$target".tar.gz}
}

# Download and verify the resolved archive, then unpack it into a fresh
# stage beside the install so every placement is a same-filesystem rename.
fetch_and_unpack() {
  side=$1
  say "Downloading hya $side $release for $target"
  download "$releases/download/$side/$release/$archive" "$tmp/$archive" ||
    die "could not download $releases/download/$side/$release/$archive"
  actual=$(sha256_of "$tmp/$archive")
  [ "$actual" = "$expected" ] ||
    die "checksum mismatch for $archive: expected $expected, got $actual"
  mkdir -p "$prefix/lib/hya"
  stage=$prefix/.hya-install-$side.$$
  mkdir -p "$stage/new" "$stage/old"
  tar -xzf "$tmp/$archive" -C "$stage/new"
  package_dir=$stage/new/hya-$side-$release-$target
  [ -d "$package_dir" ] || die "$archive has no hya-$side-$release-$target directory"
}

# `hya --version` prints `hya <version>`.
installed_backend_version() {
  "$1" --version 2>/dev/null | awk 'NR == 1 { print $NF }'
}

installed_frontend_version() {
  awk -F '"' '/frontendVersion =/ { print $2; exit }' "$1"
}

rollback_backend() {
  old=$stage/old
  if [ ! -e "$package_dir/bin/hya" ]; then
    rm -f "$prefix/bin/hya"
  fi
  if [ -e "$old/bin/hya" ]; then
    mv -f "$old/bin/hya" "$prefix/bin/hya"
  fi
  while IFS= read -r name; do
    if [ ! -e "$package_dir/lib/hya/$name" ]; then
      rm -rf "${prefix:?}/lib/hya/$name"
    fi
    if [ -e "$old/lib/$name" ]; then
      mv "$old/lib/$name" "$prefix/lib/hya/$name"
    fi
  done <"$stage/lib.list"
  while IFS= read -r name; do
    if [ ! -e "$package_dir/bundles/$name" ]; then
      rm -f "$prefix/bundles/$name"
    fi
  done <"$stage/bundles.list"
  for bundle in "$old"/bundles/hya-*.hyabundle; do
    [ -e "$bundle" ] || continue
    mv -f "$bundle" "$prefix/bundles/"
  done
}

rollback_frontend() {
  old=$stage/old
  if [ ! -e "$package_dir/lib/hya/bin/bun" ]; then
    rm -f "$prefix/lib/hya/bin/bun"
  fi
  for name in tui tui-web; do
    if [ ! -e "$package_dir/lib/hya/$name" ]; then
      rm -rf "${prefix:?}/lib/hya/$name"
    fi
    if [ -e "$old/$name" ]; then
      mv "$old/$name" "$prefix/lib/hya/$name"
    fi
  done
  if [ -e "$old/bun" ]; then
    mv -f "$old/bun" "$prefix/lib/hya/bin/bun"
  fi
}

install_backend() {
  resolve_release backend
  if [ "$force" -eq 0 ] && [ -x "$prefix/bin/hya" ] &&
    [ "$(installed_backend_version "$prefix/bin/hya" || true)" = "$release" ]; then
    say "hya backend $release is already installed in $prefix (pass --force to reinstall)"
    return 0
  fi
  fetch_and_unpack backend
  mkdir -p "$prefix/bin" "$prefix/bundles" "$stage/old/bin" "$stage/old/lib" "$stage/old/bundles"
  [ -f "$package_dir/bin/hya" ] || die "$archive has no hya-backend-$release-$target/bin/hya"
  for name in tui tui-web; do
    [ ! -e "$package_dir/lib/hya/$name" ] || die "$archive unexpectedly contains frontend lib/hya/$name"
  done
  (cd "$package_dir/lib/hya" && for name in *; do if [ -e "$name" ]; then printf '%s\n' "$name"; fi; done) >"$stage/lib.list"
  (cd "$package_dir/bundles" && for name in hya-*.hyabundle; do if [ -e "$name" ]; then printf '%s\n' "$name"; fi; done) >"$stage/bundles.list"

  swapping=backend
  if [ -e "$prefix/bin/hya" ]; then
    mv -f "$prefix/bin/hya" "$stage/old/bin/hya"
  fi
  mv -f "$package_dir/bin/hya" "$prefix/bin/hya"
  # The backend archive ships lib/hya/bin/bun for the adapter; it replaces the
  # same runtime a frontend install placed there.
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

  placed=$(installed_backend_version "$prefix/bin/hya" || true)
  [ "$placed" = "$release" ] ||
    die "the installed hya reports version '${placed:-none}', expected $release"
  swapping=""
  rm -rf "$stage"
  stage=""
  installed="${installed:+$installed, }backend $release"
  say "Installed hya backend $release to $prefix/bin/hya"
  if "$prefix/bin/hya" serve status >/dev/null 2>&1; then
    say "The running hya backend keeps its old version until you run: hya serve restart"
  fi
}

install_frontend() {
  resolve_release frontend
  version_file=$prefix/lib/hya/tui/frontend-version.ts
  if [ "$force" -eq 0 ] && [ -f "$version_file" ] &&
    [ "$(installed_frontend_version "$version_file" || true)" = "$release" ]; then
    say "hya frontend $release is already installed in $prefix (pass --force to reinstall)"
    return 0
  fi
  fetch_and_unpack frontend
  [ ! -e "$package_dir/bin/hya" ] || die "$archive unexpectedly contains backend bin/hya"
  [ -x "$package_dir/lib/hya/bin/bun" ] || die "$archive has no frontend lib/hya/bin/bun"
  for name in tui tui-web; do
    [ -e "$package_dir/lib/hya/$name" ] || die "$archive has no frontend lib/hya/$name"
  done

  swapping=frontend
  if [ -e "$prefix/lib/hya/bin/bun" ]; then
    mv -f "$prefix/lib/hya/bin/bun" "$stage/old/bun"
  fi
  for name in tui tui-web; do
    if [ -e "$prefix/lib/hya/$name" ]; then
      mv "$prefix/lib/hya/$name" "$stage/old/$name"
    fi
  done
  mkdir -p "$prefix/lib/hya/bin"
  mv -f "$package_dir/lib/hya/bin/bun" "$prefix/lib/hya/bin/bun"
  for name in tui tui-web; do
    mv "$package_dir/lib/hya/$name" "$prefix/lib/hya/$name"
  done

  placed=$(installed_frontend_version "$version_file" || true)
  [ "$placed" = "$release" ] ||
    die "the installed frontend reports version '${placed:-none}', expected $release"
  swapping=""
  rm -rf "$stage"
  stage=""
  installed="${installed:+$installed, }frontend $release"
  say "Installed hya frontend $release to $prefix/lib/hya"
}

on_exit() {
  status=$1
  trap - EXIT
  if [ "$status" -ne 0 ] && [ -n "$swapping" ]; then
    if (rollback_"$swapping"); then
      printf 'hya-install: restored the previous %s installation in %s\n' "$swapping" "$prefix" >&2
    else
      printf 'hya-install: %s rollback failed; the previous installation is in %s\n' "$swapping" "$stage/old" >&2
      stage=""
    fi
  fi
  if [ "$status" -ne 0 ] && [ -n "$installed" ]; then
    printf 'hya-install: already installed before the failure: %s\n' "$installed" >&2
  fi
  [ -z "$stage" ] || rm -rf "$stage"
  rm -rf "$tmp"
  exit "$status"
}

target=$(detect_target)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/hya-install.XXXXXX")
stage=""
swapping=""
installed=""
package_dir=""
trap 'on_exit $?' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Call the installers as plain commands: `set -e` does not apply inside a
# function invoked from an `||`/`&&` list.
if [ "$backend" -eq 1 ]; then
  install_backend
fi
if [ "$frontend" -eq 1 ]; then
  install_frontend
fi

if [ -x "$prefix/bin/hya" ]; then
  # Compare real directories: a PATH entry may reach <prefix>/bin through a
  # symlink (macOS /tmp is /private/tmp).
  real_bin=$(cd "$prefix/bin" && pwd -P)
  on_path=0
  old_ifs=$IFS
  IFS=:
  set -f
  for dir in ${PATH:-}; do
    if [ -n "$dir" ] && [ "$(cd "$dir" 2>/dev/null && pwd -P)" = "$real_bin" ]; then
      on_path=1
    fi
  done
  set +f
  IFS=$old_ifs
  if [ "$on_path" -eq 0 ]; then
    say "Add $prefix/bin to PATH, for example: export PATH=\"$prefix/bin:\$PATH\""
  fi
elif [ "$frontend" -eq 1 ]; then
  say "The frontend needs the hya backend in $prefix/bin/hya: rerun with --backend-only to add it"
fi
