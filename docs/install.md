# Install and update from a release

Every `v*.*.*` tag builds a complete hya install package for each supported
platform and publishes it to the tag's GitHub Release. The package holds
everything bare `hya` needs, including Bun, so a machine needs no Rust toolchain
and no separately installed Bun. `scripts/hya-install.sh`, which the release also
publishes as `hya-install.sh`, installs a package with one command. Bare
`hya update` runs the same script to move an installed hya to the latest
release.

Building from a source checkout (`./install.sh`) is still supported; see the
[README](../README.md#build-from-source).

## Install

```sh
curl -fsSL https://github.com/anyin233/hya/releases/latest/download/hya-install.sh | sh
```

The script:

1. Detects the target triple from `uname`. An x86_64 shell under Rosetta on
   Apple silicon gets the arm64 build. musl Linux, Windows, and other CPUs are
   refused.
2. Downloads `SHA256SUMS` of the release, either the latest release or
   `--version`. It then picks `hya-<version>-<target>.tar.gz` from that file.
3. Exits early if `<prefix>/bin/hya --version` already reports that version,
   unless you pass `--force`.
4. Downloads the archive and refuses it if the SHA-256 checksum differs.
5. Unpacks it into `<prefix>/.hya-install.<pid>` and moves each piece into
   place with a rename. It replaces `bin/hya`, each directory the archive ships
   under `lib/hya/`, and the `bundles/hya-*.hyabundle` set. It does not touch
   other entries under `lib/hya/` or other bundles.
6. Runs the installed `hya --version`. If any step after the first move fails,
   it restores the previous files and removes the staging directory.

The script does not edit shell startup files. If `<prefix>/bin` is not on
`PATH`, it prints the `export PATH=…` line to add. If a backend daemon is
running, it keeps the old version until you run `hya serve restart`.

### Options

Pass flags through `sh -s --`:

```sh
curl -fsSL https://github.com/anyin233/hya/releases/latest/download/hya-install.sh \
  | sh -s -- --version 0.43.23 --prefix /opt/hya
```

| Flag | Environment | Default | Meaning |
| --- | --- | --- | --- |
| `--version VERSION` | `HYA_VERSION` | latest release | Release to install. `0.43.23` and `v0.43.23` both work. |
| `--prefix DIR` | `HYA_INSTALL_DIR` | `$HOME/.local` | Install into `DIR/bin`, `DIR/lib/hya`, `DIR/bundles`. |
| `--force` | — | off | Reinstall even when that version is already installed. |
| — | `HYA_REPO` | `anyin233/hya` | GitHub `owner/repo` of the releases. |
| — | `HYA_RELEASES_URL` | `https://github.com/$HYA_REPO/releases` | Release base URL (a mirror or `file://` tree). |
| — | `HYA_TARGET` | detected | Target triple override. |

Flags take precedence over the environment. The script needs `curl` or `wget`,
`tar`, and one of `sha256sum`, `shasum`, or `openssl`.

### Hosting the script elsewhere

The script is self-contained POSIX `sh`. Copy `scripts/hya-install.sh` (or the
`hya-install.sh` release asset) to any web host, for example
`https://example.com/hya/install.sh`, and run
`curl -fsSL https://example.com/hya/install.sh | sh`. Downloads still come from
`HYA_RELEASES_URL`. A mirror must serve the same paths as GitHub:

```text
<HYA_RELEASES_URL>/latest/download/SHA256SUMS
<HYA_RELEASES_URL>/download/v<version>/SHA256SUMS
<HYA_RELEASES_URL>/download/v<version>/hya-<version>-<target>.tar.gz
```

## Update

```sh
hya update                     # latest release
hya update --version 0.43.23   # a specific release (also a downgrade)
hya update --force             # reinstall the current version
hya update --prefix /opt/hya   # a prefix other than the running hya's
```

Bare `hya update` pipes the installer compiled into the binary to `sh -s`. The
prefix is the directory above the real (symlink-resolved) `bin/hya`, and it
must contain `lib/hya`. A `target/debug/hya` of a source checkout is refused
unless you pass `--prefix`. `HYA_REPO`, `HYA_RELEASES_URL`, and `HYA_TARGET`
apply as they do for the curl install. The `hya update` subcommands
(`status`, `apply`, `authorize`, …) are the separate signed-release updater in
[self-update.md](self-update.md). You cannot combine them with the bare options.

Trust model: this path relies on HTTPS to the release host and on the
release's `SHA256SUMS`. GitHub build-provenance attestations cover every
archive, `SHA256SUMS`, and `hya-install.sh`. Check one with
`gh attestation verify <file> --repo anyin233/hya`. For an owner-gated,
signature-verified activation, use the signed updater instead.

## Release package

Tag push `vX.Y.Z` runs `.github/workflows/release.yml`. The tag must match
`[workspace.package].version`, and the first heading of `CHANGELOG.md` must be
that version. A tag with a `-` suffix, such as `v1.2.3-rc1`, is published as a
prerelease, which `releases/latest` skips.

| Target | Runner |
| --- | --- |
| `x86_64-unknown-linux-gnu` | `ubuntu-22.04` |
| `aarch64-unknown-linux-gnu` | `ubuntu-22.04-arm` |
| `aarch64-apple-darwin` | `macos-15` |
| `x86_64-apple-darwin` | `macos-15-intel` |

Release assets:

| Asset | Contents |
| --- | --- |
| `hya-<version>-<target>.tar.gz` | The complete install package, described below. |
| `hya-<bundle>[-<target>]-<version>.hyabundle` | Each first-party bundle as a standalone package. |
| `SHA256SUMS` | Checksums of every archive and bundle asset. The installer reads it. |
| `hya-install.sh` | The installer above. |

Archive layout (`hya-<version>-<target>/`):

```text
bin/hya                    the backend, CLI, and TUI launcher
lib/hya/bin/bun            pinned Bun (1.4.2) that runs the three programs below
lib/hya/bun-adapter/       JavaScript bundle extension host
lib/hya/tui/               terminal UI, with the target's OpenTUI native package
lib/hya/tui-web/           WebUI host
bundles/hya-*.hyabundle    first-party bundles loaded at startup
examples/                  example bundle (the installer does not install it)
README.md
THIRD_PARTY_NOTICES        includes the notice for the bundled Bun
```

hya looks for Bun in this order: `$BUN`, then `<prefix>/lib/hya/bin/bun` next
to the running `bin/hya`, then `bun` on `PATH`.

Every build job smoke-tests its package before upload. It installs the archive
with `scripts/hya-install.sh` from a `file://` release tree, then checks that
the installed hya's bare `hya update` reports the release as current.
`cargo run -p xtask -- release-rehearsal` runs the same checks locally.
