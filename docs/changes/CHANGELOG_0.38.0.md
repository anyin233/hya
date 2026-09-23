# 0.38.0

## One `hya` command

- **Breaking:** the shipped executable is now `hya` (formerly `hya-backend`). Every terminal interface goes through this one command, and subcommands select what you control: `exec`/`run`/`-p`/`loop`, `serve`/`rpc`, `sessions`/`tail-session`, `login`/`oauth`/`auth`/`models`, `agent`/`bundle`/`workflow`, and `update`. The Cargo package keeps its name, so build with `cargo build -p hya-backend --bin hya`.
- **Breaking:** the standalone `hya-updater` binary is removed. Its commands moved to `hya update version|status|recover|apply|discard|init-roots` with the same flags, and `hya update version` now prints `hya update <version> protocol <n>`. The command code lives in the independent `hya-updater` library (`hya_updater::cli`), which still has no runtime dependencies. `hya` runs `update` before it loads any config, bundles, providers, plugins, MCP, or session store. Owner-gated activation (`--owner-authorized-activation`) is unchanged.
- Release archives and `install.sh` now place `bin/hya`. After a successful install, `install.sh` deletes a leftover `bin/hya-backend` from an earlier release. A failed install keeps it.
- Re-login hints, bundle-authoring and self-update skills, the E2E harness, `startup-bench`, `release-rehearsal`, CI, and the docs all use the `hya` name.
