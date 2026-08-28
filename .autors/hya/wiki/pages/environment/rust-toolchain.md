---
title: Rust Toolchain Constraint
description: How to select the rustc version this workspace requires on this machine.
---

# Rust Toolchain Constraint

The workspace declares `rust-version = "1.91"` in root `Cargo.toml`, but there is
no `rust-toolchain.toml` pin. The machine default installed by rustup is
`1.90-x86_64-unknown-linux-gnu`, so plain `cargo clippy`/`cargo test` aborts
every package with `requires rustc 1.91` (exit 101) before any check runs.
`cargo fmt` happens to pass under 1.90 because it never compiles.

Toolchain `1.91.1-x86_64-unknown-linux-gnu` is installed on this box.

Prefix gate commands with the toolchain override:

```sh
RUSTUP_TOOLCHAIN=1.91.1 cargo clippy --workspace --all-targets -- -D warnings
```

First use of a fresh toolchain recompiles all dependencies; a full cold
workspace clippy can exceed ten minutes — run it under a durable/background
runner rather than the foreground shell. Installing `rustfmt`/`clippy`
components must be done for each toolchain separately
(`rustup component add ...` applies to the active one only).

A future fix could add a `rust-toolchain.toml` pinning `1.91.1` so plain
commands work; not done as of 0.35.2 to keep working-tree scope minimal.
