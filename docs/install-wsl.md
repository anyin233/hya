# Install hya on Windows with WSL

hya has no native Windows build. On Windows it runs inside the Windows
Subsystem for Linux (WSL 2) as an ordinary Linux program: the release
installer, the TUI, the WebUI, and the backend daemon all work unchanged.
This tutorial takes a fresh Windows machine to a running `hya`.

You will:

1. [Set up WSL 2 with Ubuntu](#1-set-up-wsl-2-with-ubuntu)
2. [Install hya](#2-install-hya) — from a release (recommended) or from source
3. [Run hya](#3-run-hya) — the TUI in your terminal, the WebUI in your Windows browser
4. [Add a model provider](#4-add-a-model-provider)

[Troubleshooting](#troubleshooting), [updating](#update), and
[uninstalling](#uninstall) are at the end.

## Requirements

| Requirement | Why |
| --- | --- |
| Windows 10 22H2 or Windows 11 | WSL 2 with localhost forwarding |
| WSL **2** (not WSL 1) | hya runs a background daemon and Bun; WSL 1 is not supported |
| A **glibc** distribution — Ubuntu 22.04/24.04 or Debian | Releases are built for `x86_64-unknown-linux-gnu` / `aarch64-unknown-linux-gnu`; Alpine (musl) is rejected by the installer |
| x86_64 or ARM64 PC | The two Linux release targets |

Windows Terminal is recommended: it supports the OSC 52 clipboard sequence
the TUI uses for copying, and renders the TUI's colors and box drawing
correctly.

## 1. Set up WSL 2 with Ubuntu

Open **PowerShell as Administrator** and run:

```powershell
wsl --install -d Ubuntu
```

Restart Windows if asked. On first launch Ubuntu asks for a Linux user name
and password; these are separate from your Windows account.

Confirm the distribution runs on WSL 2:

```powershell
wsl -l -v
```

The `VERSION` column must say `2`. If it says `1`, convert it:

```powershell
wsl --set-version Ubuntu 2
```

Everything from here on runs **inside the Ubuntu shell** (open "Ubuntu" from
the Start menu, or a Ubuntu tab in Windows Terminal). Bring the base system up
to date and make sure the installer's tools are present:

```sh
sudo apt update && sudo apt upgrade -y
sudo apt install -y curl tar coreutils ca-certificates
```

> **Work in the Linux file system.** Keep your projects under your Linux home
> (`~/code/...`), not under `/mnt/c/...`. Files on the Windows drive are
> reached through a slow translation layer, so tool calls, searches, and file
> watching are much slower there, and Linux permissions are not preserved.
> From Windows you can still open the Linux files at `\\wsl$\Ubuntu\home\<you>`.

## 2. Install hya

### Option A: Install a release (recommended)

One command installs the latest backend and frontend into `~/.local`. The
release ships its own Bun, so there is nothing else to install:

```sh
curl -fsSL https://hya.ed-aisys.com/install.sh | sh
```

The installer verifies each download against the release's `SHA256SUMS`
before replacing anything. It does not edit your shell startup files; if it
reports that `~/.local/bin` is not on `PATH`, add it permanently:

```sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

Check the install:

```sh
hya --version
```

Installer options (`--version`, `--prefix`, `--backend-only`, `--tui-only`,
`--force`) are described in [Install and update](install.md#installer-interface).

### Option B: Build from source

Use this to run an unreleased branch or to develop hya. It needs a C toolchain
and OpenSSL headers (for native Rust dependencies), a Rust toolchain, and Bun.

```sh
# Build tools and libraries
sudo apt install -y build-essential pkg-config libssl-dev git unzip

# Rust (the workspace requires Rust 1.91 or newer, edition 2024)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"

# Bun (hya pins 1.4.2 for its runtime)
curl -fsSL https://bun.sh/install | bash -s "bun-v1.4.2"
source ~/.bashrc
```

Clone into the Linux file system and run the source installer:

```sh
mkdir -p ~/code && cd ~/code
git clone https://github.com/anyin233/hya.git
cd hya
./install.sh --prefix "$HOME/.local"
```

`./install.sh` builds the release profile and installs `bin/hya`, the
first-party bundles, and the TUI/WebUI programs under `~/.local`. The first
build takes a while; give WSL enough memory (see
[the build is killed](#the-source-build-is-killed-or-runs-out-of-memory)).
Add `~/.local/bin` to `PATH` as in Option A, then check `hya --version`.

## 3. Run hya

Go to a project and start hya:

```sh
cd ~/code/my-project
hya
```

This starts three things:

- the **TUI** in your terminal;
- the **WebUI** on `http://127.0.0.1:3250` — the same TUI rendered in a browser;
- a **backend daemon** that keeps running after you quit, so the next start
  is instant.

**Open the WebUI from Windows.** WSL 2 forwards `localhost` ports to Windows,
so open <http://localhost:3250> in your Windows browser (Edge, Chrome,
Firefox). Use `hya --port <N>` if 3250 is taken.

Useful first keys and commands:

| Input | Action |
| --- | --- |
| `?` | List every key and command |
| `/exit` | Stop the TUI and the WebUI |
| `hya serve status` | Show the backend daemon (URL, pid, version, database) |
| `hya serve stop` | Stop the daemon |

Sessions are stored in `~/.local/state/hya/sessions.db` and configuration in
`~/.config/hya/config.yaml`, both inside the Linux file system.

Without a provider, hya runs on an offline "dev" provider that echoes your
prompts, so you can explore the interface before adding API keys.

## 4. Add a model provider

The guided setup asks for a base URL, a protocol, and an API key, then fetches
the model list:

```sh
hya provider add
hya provider list
```

For OAuth logins, the default device-code flow works well in WSL: hya prints a
URL and a code, and you open the URL in your Windows browser:

```sh
hya oauth login --provider <name> --type openai-codex
```

See the [Getting Started](getting-started.md) guide and the
[`hya oauth login`](cli.md) reference for the other provider types and flags.

## Update

```sh
hya update                  # backend and frontend
hya update --backend-only   # backend only
hya update --tui-only       # frontend only
hya serve restart           # make a running daemon use the new backend
```

A source install updates by pulling and re-running the installer:

```sh
cd ~/code/hya && git pull && ./install.sh --prefix "$HOME/.local"
hya serve restart
```

## Uninstall

Stop the daemon, then remove the installed files from the prefix:

```sh
hya serve stop
rm -f  ~/.local/bin/hya
rm -rf ~/.local/lib/hya
rm -f  ~/.local/bundles/hya-*.hyabundle
```

Your sessions (`~/.local/state/hya`) and configuration (`~/.config/hya`) are
left in place; delete them too for a clean removal. To remove the whole Linux
environment instead, run `wsl --unregister Ubuntu` in PowerShell — this
**permanently deletes** everything inside that distribution.

## Troubleshooting

### `hya: command not found`

`~/.local/bin` is not on `PATH`. Add the `export PATH=...` line from
[Option A](#option-a-install-a-release-recommended) to `~/.bashrc` and open a
new shell.

### The installer says the system uses musl

You are on a musl distribution such as Alpine. Install Ubuntu or Debian with
`wsl --install -d Ubuntu` and install hya there.

### The WebUI does not open from Windows

1. Check that hya reports the WebUI as listening (the status bar shows
   `WebUI unavailable` otherwise; `/status` gives the reason, often a port
   already in use — retry with `hya --port 8000`).
2. Use `http://localhost:3250`, not the WSL VM's IP address.
3. If `localhost` still does not connect, localhost forwarding may be turned
   off. In `%UserProfile%\.wslconfig` on Windows, make sure it is not
   disabled:

   ```ini
   [wsl2]
   localhostForwarding=true
   ```

   Then run `wsl --shutdown` in PowerShell and reopen Ubuntu. If you use
   `networkingMode=mirrored`, `localhost` works directly as well.

### A browser does not open automatically during login

hya opens a browser on Linux with `xdg-open`, which usually has no browser to
launch inside WSL. hya always prints the URL as well; copy it into your
Windows browser. To make links open in Windows automatically, install
`wslu` and point `BROWSER` at it:

```sh
sudo apt install -y wslu
echo 'export BROWSER=wslview' >> ~/.bashrc
```

### Copying from the TUI does nothing

The TUI copies through the terminal's OSC 52 clipboard sequence. Windows
Terminal supports it; the legacy console host (`conhost.exe`) does not. Run
Ubuntu in Windows Terminal, or copy from the WebUI instead.

### The source build is killed or runs out of memory

WSL 2 limits its VM to part of your RAM. Compiling the release profile can
exceed it, and the linker is killed. Raise the limit in
`%UserProfile%\.wslconfig`:

```ini
[wsl2]
memory=12GB
swap=8GB
```

Run `wsl --shutdown` in PowerShell and rebuild. Building with
`./install.sh --profile dev` also uses less memory.

### Everything is slow

Check that the project lives under `~/`, not `/mnt/c/`. Move it into the
Linux file system (see the note in [step 1](#1-set-up-wsl-2-with-ubuntu)).

### The daemon is gone after a while

WSL shuts its VM down a few minutes after the last Linux process exits; this
also stops the hya daemon. Nothing is lost: sessions are on disk, and the next
`hya` starts a new daemon automatically.

For problems that are not specific to WSL, see the general
[Troubleshooting](troubleshooting.md) guide.
