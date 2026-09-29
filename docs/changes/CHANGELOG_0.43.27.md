# 0.43.27

## Breaking

- **Intel Macs are no longer supported.** The release builds `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, and `aarch64-apple-darwin`; `x86_64-apple-darwin` is gone. 0.43.25 is the last release with an Intel Mac package. The installer, and so bare `hya update`, now stops on an Intel Mac with "no hya release for Intel Macs". An x86_64 shell under Rosetta on Apple silicon still gets the arm64 build. `cargo run -p xtask -- release-rehearsal` accepts only the three remaining targets.

## Build

- **Release builds restore a warm Rust dependency cache.** A tag run cannot read caches saved by another tag, so every release compiled all dependencies from scratch and also saved about 2 GB of caches that no later release could use. The release workflow now also runs on `main` when `Cargo.lock`, a `Cargo.toml`, or `release.yml` changes, daily, and on `workflow_dispatch`. Those runs only build and save one dependency cache per target (including xtask's), skipping the build when the exact key is already cached. Tag runs restore that cache and no longer save one. Dropping the Intel Mac job, which took 28 minutes, and the warm cache shorten a release from about 32 minutes. See `docs/install.md` "Build cache".
