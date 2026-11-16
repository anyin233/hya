# Install and update from a release

`hya` publishes unified releases tagged `v<version>` (`v0.47.0` for stable
releases, or `v0.47.0-dev.1` for pre-releases). Each target ships one combined
archive, `hya-<version>-<target>.tar.gz`, containing the `hya` executable, the
thirteen first-party bundles, the Argus example bundle, a pinned Bun runtime,
the Bun plugin adapter, the TUI, the WebUI host, and the TUI extension SDK.

One installer, `scripts/hya-install.sh`, installs or updates the unified
release. Releases do not carry the installer script as an asset:
`https://hya.ed-aisys.com/install.sh` serves it from the repository's `main`
branch, and `hya update` runs the copy compiled into the running `hya`. The
installer downloads `SHA256SUMS`, verifies the archive's SHA-256 checksum, and
swaps it into the prefix atomically.

Building from a source checkout (`./install.sh`) is still supported and remains
a separate source-install path; see the [README](../README.md#quick-install).

On Windows, install inside WSL 2; see
[Install on Windows with WSL](install-wsl.md).

## Install

```sh
curl -fsSL https://hya.ed-aisys.com/install.sh | sh
```

Without options this installs the latest stable release into `$HOME/.local`.
Pass flags through `sh -s --`:

```sh
# Pin a stable or pre-release version (`v` prefix optional) into a custom prefix.
curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --version v0.47.0 --prefix /opt/hya
curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --version v0.47.1-dev.1
```

The archive is unpacked into a staging directory beside the install, swapped in
by same-filesystem renames, and verified (`bin/hya --version` and
`lib/hya/tui/frontend-version.ts` must both match the release version). If a
check fails, the installer rolls back to the previous installation. If the
target version is already installed, it exits `0` without downloading unless
`--force` is given.

The script does not edit shell startup files. If `<prefix>/bin` is not on
`PATH`, it prints the export line to add. If a backend daemon is running, the
old backend version remains active until `hya serve restart` (which `hya update`
runs automatically when `bin/hya` changes).

### Latest-release resolution

Without `--version`, the installer reads `<releases>/latest/download/SHA256SUMS`.
GitHub's `releases/latest` automatically points to the newest non-prerelease
`v<version>` release and skips `-dev.*` / `-rc.*` pre-releases. With
`--version <ver>` (leading `v` optional), it reads
`<releases>/download/v<ver>/SHA256SUMS` and downloads
`<releases>/download/v<ver>/hya-<ver>-<target>.tar.gz`.

### Installer interface

| Flag | Meaning |
| --- | --- |
| `--version VERSION` | Install this release (`0.47.0`, `v0.47.0`, or `v0.47.1-dev.1`) instead of the latest stable release. |
| `--prefix DIR` | Install into `DIR/bin`, `DIR/lib/hya`, `DIR/bundles` (default `$HOME/.local`). |
| `--force` | Reinstall even when that version is already installed. |
| `-h`, `--help` | Print usage. |

| Environment | Default | Meaning |
| --- | --- | --- |
| `HYA_REPO` | `chrono-foundry/hya` | GitHub owner/repo of the releases. |
| `HYA_RELEASES_URL` | `https://github.com/$HYA_REPO/releases` | Release base; serves `latest/download/SHA256SUMS` and `download/v<version>/<asset>`. `file://` works for mirrors and tests. |
| `HYA_VERSION` | latest | Same as `--version`. |
| `HYA_INSTALL_DIR` | `$HOME/.local` | Same as `--prefix`. |
| `HYA_TARGET` | detected | Target triple override. |

Flags win over the environment. Exit status is `0` on success (including an
already-installed no-op) and non-zero on any failure. The script needs `curl`
or `wget`, `tar`, and a SHA-256 utility (`sha256sum`, `shasum`, or `openssl`).

## Update

```sh
hya update                         # latest stable release
hya update --version v0.47.1-dev.1 # pin a specific release or pre-release
hya update --force                 # reinstall the current version
hya update --prefix /opt/hya       # a prefix other than the running hya's
```

`hya update` pipes the installer compiled into the running `hya` to `sh` with
`--prefix <prefix>` and the given flags, so it accepts `--version`, `--force`,
and `--prefix` with the same meaning. Without `--prefix` the prefix is the real
`<prefix>` of the running `<prefix>/bin/hya` beside `<prefix>/lib/hya`; a
build-tree `hya` is refused. `hya update version` prints the updater's package
version.

After a successful `hya update`, the running daemon for `--db` (the default
session database when omitted) reloads its bundles if only bundle contents
changed. A changed backend executable runs the installed `hya --db <db> serve
restart`: stop drains interrupted turns, start restores them with a restore
prompt, and connected TUIs automatically discover the new URL and reconnect.
No running daemon is started solely by an update.

## Release package

Each `v<version>` tag publishes unified target archives:

| Tag | Archive | Contents |
| --- | --- | --- |
| `v<version>` (e.g. `v0.47.0`, `v0.47.1-dev.1`) | `hya-<version>-<target>.tar.gz` | `bin/hya`, `bundles/hya-*.hyabundle`, `examples/hya-argus-example.hyabundle`, `lib/hya/bin/bun`, `lib/hya/bun-adapter`, `lib/hya/tui`, `lib/hya/tui-web`, `lib/hya/tui-sdk` |

Supported targets are `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
and `aarch64-apple-darwin`; Intel macOS is not built. Each release also
publishes standalone first-party `.hyabundle` assets and `SHA256SUMS`, which
the installer verifies before replacing files.

### Release tags

Repository Git tags follow a single pure-version rule (no frontend/backend split
prefixes and no non-version milestone tags):

- **Format:** `vx.x.x` for stable releases (for example `v0.47.0`) or
  `vx.x.x-<suffix>` for pre-releases (for example `v0.47.1-dev.1`, suffix
  optional), matching `^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$`.
- **Forbidden tags:** `backend/*`, `frontend/*`, `phase*`, bare versions without
  a leading `v` (`0.47.0`, `0.45.10-dev.1`), and `+` build metadata.
- **Stable vs. pre-release:** tags without `-` publish as stable GitHub
  Releases (`Latest`); tags with `-` (such as `-dev.1`) publish as GitHub
  Pre-releases and are installed only when explicitly requested via
  `--version v0.47.1-dev.1` (or `--version 0.47.1-dev.1`).

### Hosted installer URL

`https://hya.ed-aisys.com/install.sh` is a Cloudflare redirect to
`https://raw.githubusercontent.com/chrono-foundry/hya/main/scripts/hya-install.sh`,
so a change to the installer reaches new installs when it lands on `main`.

### Release workflow

`.github/workflows/release.yml` packages `v<version>` tags matching
`^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$`. Tags containing `-` (such as
`v0.47.1-dev.1`) publish as GitHub Pre-releases. The workflow smoke-tests the
archive and installer, verifies `SHA256SUMS`, and publishes build provenance
attestations for the archives, `.hyabundle` assets, and checksum file.

Pushes to `main` that change `Cargo.lock`, a Cargo manifest, or this workflow,
the daily schedule, and `workflow_dispatch` refresh one Rust dependency cache
per target. Cache-refresh runs do not package, attest, upload, or publish.
