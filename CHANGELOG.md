# 0.43.24

## Fixes

- **The installer no longer asks you to add a directory that is already on `PATH`.** `hya-install.sh` and `hya update` compared `PATH` entries with the install prefix as text. When `PATH` reaches `<prefix>/bin` through a symlink, for example `/tmp` and `/private/tmp` on macOS or a symlinked `~/.local`, they printed a needless "Add … to PATH" hint. They now compare real directories.
