# 0.37.18

## First-party bundles in the release

- Publish all twelve first-party bundles with every release. The `hya-<version>-<target>.tar.gz` archive keeps them under `bundles/`, and each is also a standalone asset: `hya-<name>-<version>-<target>.hyabundle` for the five native tool families and `hya-<name>-<version>.hyabundle` for the others. `SHA256SUMS` and the build provenance attestations cover every asset.
- Release first-party bundles at the hya version. Every first-party `bundle.yaml` identity version now equals the workspace version, and staging refuses a mismatch.
- Add `cargo run -p xtask -- stage-first-party-bundles`, which the release workflow, `release-rehearsal` and `install.sh` share. The release smoke test compares each asset with the archive and lists all twelve bundles through the packaged backend.
- `install.sh` installs the release layout: `bin/hya-backend`, the twelve first-party bundles in `bundles/`, and the Bun adapter in `lib/hya/bun-adapter`. It verifies the install with `bundle list` and rolls back the backend, adapter and bundles on failure. `--bin-dir` must name a `bin` directory. The script no longer references the removed Compat adapter.
- Delete the temporary copy of each native tool library once an installed backend has loaded it. Previously every start left the extracted libraries in the temp directory.
- `release-rehearsal` builds the tool libraries and checks the bundle packages, assets and checksums like the workflow.
