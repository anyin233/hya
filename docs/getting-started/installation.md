# Installation

You can install Hya with a single command on **Linux** (`x86_64` or `ARM64`) and **macOS** (Apple silicon `M1`/`M2`/`M3`/`M4`). On **Windows**, Hya runs inside WSL 2 (Ubuntu or Debian).

The installer downloads everything you need in one bundle: the `hya` command, built-in tools and skills, the terminal UI, the browser WebUI, and a bundled copy of Bun. You do not need to install Rust, Node, or Bun yourself.

---

## Install on Linux or macOS

Open your terminal and run:

```sh
curl -fsSL https://hya.ed-aisys.com/install.sh | sh
```

This puts Hya inside `~/.local` (`~/.local/bin/hya`). It never touches system folders or asks for `sudo`, and it does not edit your shell config files behind your back.

If the installer tells you `~/.local/bin` is not on your `PATH`, add it to your shell profile (`~/.zshrc` on macOS, `~/.bashrc` on Linux):

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Open a new terminal tab and check that it works:

```sh
hya --version
```

> **Note on Linux:** Prebuilt releases need standard Linux (`glibc`, used by Ubuntu, Debian, Fedora, Arch, etc.). Alpine Linux (`musl`) and Intel Macs are not supported as prebuilt downloads—build from source there instead.

---

## Install on Windows (WSL 2)

Hya does not have a native `.exe` build. Instead, you run it inside **Windows Subsystem for Linux (WSL 2)**, where it works at full speed and still opens in your normal Windows browser.

### Step 1: Turn on WSL 2 with Ubuntu

Open **PowerShell as Administrator** and run:

```powershell
wsl --install -d Ubuntu
```

Restart Windows if asked. When Ubuntu opens for the first time, pick a Linux username and password.

Check that Ubuntu is running on WSL version **2** (not 1):

```powershell
wsl -l -v
```

If `VERSION` says `1`, upgrade it with `wsl --set-version Ubuntu 2`.

### Step 2: Install Hya inside Ubuntu

Open **Ubuntu** (Windows Terminal works best because it supports copy-paste colors cleanly) and run:

```sh
sudo apt update && sudo apt install -y curl tar coreutils ca-certificates
curl -fsSL https://hya.ed-aisys.com/install.sh | sh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
hya --version
```

> [!IMPORTANT]
> **Keep your code inside Linux (`~/code/...`), not on `/mnt/c/...`.**
> Reading and watching files across the Windows `C:` drive boundary in WSL is very slow. Put your projects in your Ubuntu home folder (`~/code/my-app`). You can still browse those files in Windows Explorer at `\\wsl$\Ubuntu\home\<your-name>`.

When you run `hya` inside Ubuntu, WSL 2 forwards ports automatically—just open `http://localhost:3250` in Edge, Chrome, or Firefox on Windows!

---

## Update Hya

Whenever a new release comes out, run:

```sh
hya update
```

`hya update` checks the download's SHA-256 checksum before swapping any files. If anything goes wrong during the update, it rolls back automatically so you are never left with a broken install. If Hya's background server is already running, `hya update` restarts it cleanly and reconnects your open chats.

### Pin a specific version or install location

```sh
# Install or downgrade to an exact version
hya update --version v0.47.0

# Reinstall the current version from scratch
hya update --force

# Install straight into a custom folder from the web installer
curl -fsSL https://hya.ed-aisys.com/install.sh | sh -s -- --version v0.47.0 --prefix /opt/hya
```

---

## Build from source (Optional)

Only do this if you want to run Hya from a Git branch or modify Hya's code yourself. You will need:

- **Git**
- **Rust `1.91` or newer** ([rustup.rs](https://rustup.rs))
- **Bun `1.4.2` or newer** ([bun.sh](https://bun.sh))
- On Ubuntu/Debian: `sudo apt install -y build-essential pkg-config libssl-dev unzip`

Clone the repository and run the source installer:

```sh
git clone https://github.com/chrono-foundry/hya.git
cd hya
./install.sh --prefix "$HOME/.local"
```

---

## Uninstall

Stop the background server first, then delete the installed files:

```sh
hya serve stop
rm -f  ~/.local/bin/hya
rm -rf ~/.local/lib/hya
rm -f  ~/.local/bundles/hya-*.hyabundle
```

Your chat history (`~/.local/state/hya`) and settings (`~/.config/hya`) stay untouched in case you reinstall later. Delete those two folders too if you want everything gone.
