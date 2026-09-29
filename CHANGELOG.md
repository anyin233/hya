# 0.43.28

## Build

- **CI and release workflows run their actions on Node.js 24.** GitHub warned that `actions/checkout` targets the deprecated Node.js 20 and is forced onto Node.js 24. Every pinned action that ran on Node.js 20 now points at its Node.js 24 release: `actions/checkout` v7.0.1, `Swatinem/rust-cache` v2.9.2 (CI and release now share one pin), `actions/upload-artifact` v7.0.1, `actions/download-artifact` v8.0.1, `actions/attest-build-provenance` v4.2.2 (its bundled `actions/attest` was also Node.js 20), and `softprops/action-gh-release` v3.0.3. `dtolnay/rust-toolchain` is a composite action and is unchanged. rust-cache v2.9 adds every installed toolchain to its cache key, so the first run after this change rebuilds the Rust dependency caches once.
