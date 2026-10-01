# Install and update from a release

Backend and frontend are released independently. A backend release is tagged
`backend/<version>` and publishes the backend archive and `hya-install.sh`;
a frontend release is tagged `frontend/<version>` and publishes the frontend
archive and `hya-tui-install.sh`. The two sides MAY have different versions.
The backend package contains the `hya` executable, backend bundles, and the
adapter, but no TUI or WebUI. The frontend package contains Bun, the TUI, and
the WebUI, but no `bin/hya`.

Building from a source checkout (`./install.sh`) is still supported and remains
a separate source-install path; see the [README](../README.md#build-from-source).


## Install the backend

The backend installer is a release asset of the `backend/<version>` release:

```sh
curl -fsSL https://github.com/anyin233/hya/releases/download/backend/<version>/hya-install.sh | sh
```

For an unpinned install, use the side-specific `latest` release asset (or copy
the script from `scripts/hya-install.sh`). The script detects the target,
downloads and verifies `SHA256SUMS`, then installs
`hya-backend-<version>-<target>.tar.gz` into `$HOME/.local` by default. That
archive contains `bin/hya`, backend bundles, and `lib/hya/bun-adapter`; it
does not contain the TUI or WebUI. The backend works headlessly (`hya serve`,
`hya exec`, `hya run`, and related commands) without the frontend. A bare
`hya` requires the optional frontend; when it is absent, the CLI explains how
to run `hya update tui`.

The backend script does not edit shell startup files. If `<prefix>/bin` is not
on `PATH`, it prints the export line to add. If a backend daemon is running,
the old version remains active until `hya serve restart`.

### Backend options

Pass flags through `sh -s --`:

```sh
curl -fsSL https://github.com/anyin233/hya/releases/download/backend/<version>/hya-install.sh \
  | sh -s -- --version <version> --prefix /opt/hya
```

`--version`, `--prefix`, and `--force` are supported; `HYA_REPO`,
`HYA_RELEASES_URL`, and `HYA_TARGET` can override the repository, release base,
and target. The script needs `curl` or `wget`, `tar`, and a SHA-256 utility.

## Install the frontend

The frontend installer is a release asset of the `frontend/<version>` release:

```sh
curl -fsSL https://github.com/anyin233/hya/releases/download/frontend/<version>/hya-tui-install.sh | sh
```

It installs `hya-frontend-<version>-<target>.tar.gz` into the same prefix by
default. The archive contains `lib/hya/bin/bun`, `lib/hya/tui`, and
`lib/hya/tui-web`, and intentionally adds no `bin/hya`; install the backend
first if `hya` is not already available. Frontend updates use `hya update tui`.

Both installers accept `--version`, `--prefix`, and `--force`. Their release
body includes the corresponding pinned one-click command. A side's unpinned
`latest` is the latest release for that side, not necessarily the latest
release of the other side; use compatible versions (the frontend declares its
minimum backend version).

## Update

```sh
hya update                     # latest backend release
hya update --version <version> # a specific backend release
hya update --force             # reinstall the current backend version
hya update --prefix /opt/hya   # a prefix other than the running hya's
hya update tui                 # latest frontend release
hya update tui --version <version> --prefix /opt/hya --force
```

Both update paths use their side-specific installer and support `--version`,
`--prefix`, and `--force`. Backend updates verify `SHA256SUMS`; a running
backend keeps the old version until `hya serve restart`. The signed updater
commands (`status`, `apply`, `authorize`, and so on) remain separate; see
[self-update.md](self-update.md).

## Release package

Backend and frontend release tags and archives are independent:

| Side | Tag | Installer asset | Archive | Contents |
| --- | --- | --- | --- | --- |
| Backend | `backend/<version>` | `hya-install.sh` | `hya-backend-<version>-<target>.tar.gz` | `bin/hya`, backend bundles, `lib/hya/bun-adapter`; no TUI/WebUI |
| Frontend | `frontend/<version>` | `hya-tui-install.sh` | `hya-frontend-<version>-<target>.tar.gz` | `lib/hya/bin/bun`, `lib/hya/tui`, `lib/hya/tui-web`; no `bin/hya` |

Supported targets remain `x86_64-unknown-linux-gnu`,
`aarch64-unknown-linux-gnu`, and `aarch64-apple-darwin`; Intel macOS is not
built. Each release also publishes `SHA256SUMS`, and each installer verifies
the checksum before replacing files. The release body contains the pinned
one-click command for that side. `latest` is side-specific: a frontend latest
release and a backend latest release MAY have different versions and must be
matched using the frontend minimum-backend compatibility contract.

The backend archive's Bun adapter is not the frontend runtime. Install both
sides when using bare `hya`; backend-only commands remain usable without the
frontend. The source checkout installer (`./install.sh`) is separate and may
still install a combined development layout.

### Release workflow

The release workflow packages only side-specific tags. `backend/<version>`
builds and publishes the backend archive and installer; `frontend/<version>`
builds and publishes the frontend archive and installer. Each release body
contains a pinned one-click install command for its own side. The workflow
smoke-tests archive contents, verifies `SHA256SUMS`, and publishes provenance
attestations for the archive, side assets, checksum file, and installer.

Pushes to `main` that change `Cargo.lock`, a Cargo manifest, or this workflow,
the daily schedule, and `workflow_dispatch` refresh one Rust dependency cache
per target. Cache-refresh runs do not package, attest, upload, or publish.
