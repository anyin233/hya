<p align="center">
  <img src="docs/assets/hya-logo.png" alt="Hya" width="50%"><br>
  <strong>Tidy memory, your words safe, many paws on the code.</strong>
</p>

<p align="center">
  <a href="https://chrono-foundry.github.io/hya/">Docs</a> ·
  <a href="docs/getting-started/quickstart.md">Quick Start</a> ·
  <a href="#install">Install</a> ·
  <a href="#license">License</a>
</p>

---

`hya` is an open-source coding agent written in Rust, built for long sessions
and teams of agents.

- **Tidy memory.** Context changes only through three explicit edits: what
  tools observe, what the model says, and rewrites that free space. When the
  window fills up, `hya` compacts step by step and keeps your messages word
  for word. → [Kernel](docs/architecture/kernel-and-context.md) · [Compaction](docs/guide/context-and-memory.md)
- **Many paws.** Spawn subagents that pass on short handoff notes instead of
  whole transcripts, talk over channels, and run together as
  [Workflows](docs/guide/workflows.md). →
  [Subagents](docs/architecture/engine-and-subagents.md)
- **Wherever you work.** Terminal TUI and browser WebUI side by side, editor
  integration over [ACP](docs/guide/editor-integration.md), one API for every
  client, secure remote access, and providers from OpenAI, Anthropic, Google
  Gemini, and Grok. It even runs offline with no API key.

## Install

```sh
curl -fsSL https://hya.ed-aisys.com/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"   # if not already on PATH
hya                                    # Terminal TUI here + WebUI on http://127.0.0.1:3250
```

Add a model provider with `hya provider add` (or `/key` inside the TUI). See
[Quick Start](docs/getting-started/quickstart.md) for providers, source builds, and
headless runs, [Install & Update](docs/getting-started/installation.md) for updates and version
pinning, and [WSL 2](docs/getting-started/installation.md#windows-wsl-2) for Windows.

## Documentation

Everything else lives on the [documentation site](https://chrono-foundry.github.io/hya/)
and in [`docs/`](docs/README.md).

## Status

`hya` is under active development (unified version `0.50.0`, minimum compatible
backend `0.46.0` for remote/daemon connections).

## License

Licensed under either `MIT` or `Apache-2.0` at your option. See the repository
license files and [`THIRD_PARTY_NOTICES`](THIRD_PARTY_NOTICES).
