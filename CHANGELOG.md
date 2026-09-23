# 0.37.19

## Multi-platform releases

- Build and publish every release for `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, and `aarch64-apple-darwin`. Each target gets its own `hya-<version>-<target>.tar.gz` and five native tool-family bundle assets.
- Publish the seven platform-independent bundles once. The release job fails if any target built them with different bytes.
- Each target job writes an attested `SHA256SUMS-<target>`. The release job checks them all, then writes and attests a combined `SHA256SUMS`. Checksums use the portable `shasum -a 256`.
- `release-rehearsal` rehearses any target in the release matrix on a host of that target, including macOS. It requires the workflow matrix to list exactly the supported targets and fails early on a foreign host.
- Require Bun 1.4.2 for releases and the rehearsal and drop support for older Bun versions. The rehearsal now fails before building if the Bun adapter `bun.lock` uses a lockfile version the pinned Bun cannot read. The previous 1.3.14 pin could not read the checked-in lockfile, which would have failed every release at `bun install`.
- `tests/install_script.sh` now covers the current installer (twelve bundles, Bun adapter, rollback) and runs in the workspace tests. It had been failing since the Compat adapter was removed.
- Fix a flaky LSP transport test: parallel tests could create the same temporary workspace because the clock repeats within a tick, and one test deleted it while another started its language server there. Workspace names now include a per-process counter.
