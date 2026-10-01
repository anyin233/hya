# Install and update from a release

Backend and frontend are released independently. A backend release is tagged
`backend/<version>` and publishes the backend archive; a frontend release is
tagged `frontend/<version>` and publishes the frontend archive. Tags and
download URLs use the version directly; neither side uses a `v`-prefixed
release tag. The two sides MAY have different versions. The backend package
contains the `hya` executable, backend bundles, and the adapter, but no TUI or
WebUI. The frontend package contains Bun, the TUI, and the WebUI, but no
`bin/hya`.

One installer, `scripts/hya-install.sh`, installs either side or both. Releases
do not carry it: `https://hya.ed-aisys.com/install.sh` serves it from the
repository's `main` branch, and `hya update` runs the copy compiled into the
running `hya`. The installer finds the newest release of each side on GitHub,
verifies each archive against that release's `SHA256SUMS`, and installs it.

Building from a source checkout (`./install.sh`) is still supported and remains
a separate source-install path; see the [README](../README.md#build-from-source).

## Install

```sh
curl -fsSL https://hya.ed-aisys.com/install.sh | sh
```

Without options this installs the latest backend and the latest frontend into
`$HOME/.local`, backend first. Select one side with `--backend-only` or
`--tui-only`; pass flags through `sh -s --`:

```sh
# Headless server: hya serve, hya exec, hya run, … without TUI/WebUI.
curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --backend-only
# Add the frontend beside an installed backend.
curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --tui-only
# Pin one side's release into another prefix.
curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --backend-only --version <version> --prefix /opt/hya
```

Each side is installed on its own: unpacked beside the install, swapped in by
rename, then checked (`bin/hya --version` for the backend,
`lib/hya/tui/frontend-version.ts` for the frontend). A side whose check fails
is rolled back to its previous files. If the frontend fails after the backend
succeeded, the new backend stays installed and the installer names it in the
error. A side whose installed version already matches is skipped unless
`--force` is given.

The script does not edit shell startup files. If `<prefix>/bin` is not on
`PATH`, it prints the export line to add. If a backend daemon is running, the
old backend version remains active until `hya serve restart`. A frontend-only
install has no `bin/hya`; the installer says so, and bare `hya` needs both
sides.

### Latest-release resolution

GitHub's single `latest` release belongs to whichever side was published last,
so it cannot name a side. For GitHub release URLs the installer asks the
releases API (`HYA_RELEASES_API_URL`) for each selected side's newest
non-prerelease `<side>/<version>` tag and reads
`<releases>/download/<side>/<version>/SHA256SUMS`. Other release hosts serve
`<releases>/latest/download/<side>/SHA256SUMS`, else a single
`<releases>/latest/download/SHA256SUMS`. The two sides' latest versions MAY
differ; the frontend declares the minimum backend version it accepts and
refuses an older backend at startup.

### Installer interface

| Flag | Meaning |
| --- | --- |
| `--backend-only` | Install only the backend release (`bin/hya`, `bundles/hya-*.hyabundle`, `lib/hya/bun-adapter`, `lib/hya/bin/bun`). |
| `--tui-only` | Install only the frontend release (`lib/hya/bin/bun`, `lib/hya/tui`, `lib/hya/tui-web`). Cannot be combined with `--backend-only`. |
| `--version VERSION` | Install this release of each selected side (`0.44.0` or `v0.44.0`) instead of its latest. Without a side flag both sides must have that version; pin one side with `--backend-only`/`--tui-only` when versions differ. |
| `--prefix DIR` | Install into `DIR/bin`, `DIR/lib/hya`, `DIR/bundles` (default `$HOME/.local`). |
| `--force` | Reinstall a side even when that version is installed. |
| `-h`, `--help` | Print usage. |

| Environment | Default | Meaning |
| --- | --- | --- |
| `HYA_REPO` | `anyin233/hya` | GitHub owner/repo of the releases. |
| `HYA_RELEASES_URL` | `https://github.com/$HYA_REPO/releases` | Release base; serves `download/<side>/<version>/<asset>`. `file://` works for mirrors and tests. |
| `HYA_RELEASES_API_URL` | `https://api.github.com/repos/$HYA_REPO/releases?per_page=100` | Release list used to find the newest `<side>/` tag. |
| `HYA_VERSION` | latest | Same as `--version`. |
| `HYA_INSTALL_DIR` | `$HOME/.local` | Same as `--prefix`. |
| `HYA_TARGET` | detected | Target triple override. |

Flags win over the environment. Exit status is `0` on success (including an
already-installed no-op) and non-zero on any failure. The script needs `curl`
or `wget`, `tar`, and a SHA-256 utility (`sha256sum`, `shasum`, or `openssl`).

## Update

```sh
hya update                         # latest backend and frontend releases
hya update --backend-only          # only the backend
hya update --tui-only              # only the frontend (adds it if missing)
hya update --backend-only --version <version>
hya update --force                 # reinstall the current versions
hya update --prefix /opt/hya       # a prefix other than the running hya's
```

`hya update` pipes the installer compiled into the running `hya` to `sh` with
`--prefix <prefix>` and the given flags, so it accepts the installer's
`--backend-only`, `--tui-only`, `--version`, `--force`, and `--prefix` with the
same meaning. Without `--prefix` the prefix is the real `<prefix>` of the
running `<prefix>/bin/hya` beside `<prefix>/lib/hya`; a build-tree `hya` is
refused. The signed updater commands (`status`, `apply`, `authorize`, and so
on) remain separate and cannot be combined with these options; see
[self-update.md](self-update.md).

## Release package

Backend and frontend release tags and archives are independent:

| Side | Tag | Archive | Contents |
| --- | --- | --- | --- |
| Backend | `backend/<version>` | `hya-backend-<version>-<target>.tar.gz` | `bin/hya`, backend bundles, `lib/hya/bun-adapter`; no TUI/WebUI |
| Frontend | `frontend/<version>` | `hya-frontend-<version>-<target>.tar.gz` | `lib/hya/bin/bun`, `lib/hya/tui`, `lib/hya/tui-web`; no `bin/hya` |

Supported targets remain `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, and `aarch64-apple-darwin`; Intel macOS is not
built. Each release also publishes `SHA256SUMS`, which the installer verifies
before replacing files. The release body contains the pinned one-click command
for that side (`… | sh -s -- --backend-only --version <version>` or
`--tui-only`).

The backend archive's Bun adapter is not the frontend runtime. Install both
sides when using bare `hya`; backend-only commands remain usable without the
frontend. The source checkout installer (`./install.sh`) is separate and may
still install a combined development layout.

### Hosted installer URL

`https://hya.ed-aisys.com/install.sh` is a Cloudflare redirect to
`https://raw.githubusercontent.com/anyin233/hya/main/scripts/hya-install.sh`,
so a change to the installer reaches new installs when it lands on `main`.

### Release workflow

The release workflow packages only side-specific tags. `backend/<version>`
builds and publishes the backend archive; `frontend/<version>` builds and
publishes the frontend archive. Neither publishes an installer script. Each
release body contains a pinned one-click install command for its own side. The
workflow smoke-tests archive contents, verifies `SHA256SUMS`, and publishes
provenance attestations for the archive, side assets, and checksum file.

Pushes to `main` that change `Cargo.lock`, a Cargo manifest, or this workflow,
the daily schedule, and `workflow_dispatch` refresh one Rust dependency cache
per target. Cache-refresh runs do not package, attest, upload, or publish.
