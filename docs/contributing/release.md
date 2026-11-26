# Versioning & Release Guide

Backend, first-party bundles, TUI, and WebUI share **one unified release version** tracked in root [`versions.toml`](../../versions.toml).

---

## 1. Where versions live

- **`versions.toml` (`version`):** The aggregate release version (e.g. `"0.48.0"` or `"0.48.0-dev.1"`). Mirrored in:
  - `Cargo.toml` (`[workspace.package].version` and `hya-backend` version),
  - `packages/hya-tui/frontend-version.ts` (`frontendVersion`),
  - `README.md`, and
  - `# <version>` (exact first line of `CHANGELOG.md`, **no** leading `v`).
- **`versions.toml` (`minimum_backend_version`):** Mirrored as `minimumBackendVersion` in `packages/hya-tui/frontend-version.ts`. Declares the oldest backend daemon or remote server this TUI accepts. Bump it **only** when a frontend change requires a newer `/v1` backend contract.
- **Placeholder `0.0.0` everywhere else:** All other Rust crates, frontend/adapter `package.json` files (`@hya/tui-sdk` keeps its own independent `1.0.0` SDK version), and bundle source manifests use `0.0.0` with `version-reference = "hya"` / `version_ref: hya` (resolved automatically at package time).

### When to bump the version

- **Bump once** whenever a change modifies **shipped behavior** (`crates/`, `proto/`, `bundles/`, `packages/hya-tui*`, adapters, or installer). Archive the previous `CHANGELOG.md` to `docs/changes/CHANGELOG_<old-version>.md` and write fresh notes under `# <new-version>` in `CHANGELOG.md`.
- **Do NOT bump the version or touch `CHANGELOG.md`** for documentation-only changes (`docs/`, `*.md`, `.agents/`, `.planning/`), CI-only changes (`.github/`), or test-only changes.

---

## 2. Git Tag Rules (Strict Contract)

Releases are published exclusively by `.github/workflows/release.yml` when a canonical `v<version>` tag is pushed:

- **Allowed grammar** (`^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$`):
  - **Stable release:** `v0.48.0` → publishes a GitHub Release (`Latest`, picked up by default `install.sh` and `hya update`).
  - **Pre-release:** `v0.48.0-dev.1` or `v0.48.0-rc.1` → publishes a GitHub Pre-release (installed only when pinned with `--version v0.48.0-dev.1`).
- **Forbidden tags:** Split prefixes (`backend/*`, `frontend/*`), milestone tags (`phase*`), bare versions without `v` (`0.48.0`), and `+` build metadata are rejected by `xtask` and CI and must never exist locally or on `origin`.

---

## 3. Release Checklist

1. **Run pre-tag verification:**
   ```sh
   cargo test -p xtask -p hya-bundle
   actionlint .github/workflows/release.yml   # if installed
   bash tests/hya_install_script.sh
   cargo run -p xtask -- release-rehearsal \
     --version <semver> \
     --target "$(rustc -vV | sed -n 's/^host: //p')" \
     --no-publish
   ```
2. **Push the version + `CHANGELOG.md` commit to `main` first, then tag and push:**
   ```sh
   git push origin main
   git tag -a v0.48.0 -m "release v0.48.0"
   git push origin v0.48.0
   ```
3. **What CI publishes:** Target archives `hya-<version>-<target>.tar.gz` (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin`) containing `bin/hya`, the 13 first-party bundles, the Argus example bundle, pinned Bun, Bun & Claude adapters, TUI, WebUI, and TUI SDK—plus standalone `.hyabundle` assets, `SHA256SUMS`, and provenance attestations. *(The installer script itself is never attached to releases; `https://hya.ed-aisys.com/install.sh` serves `scripts/hya-install.sh` from `main`.)*

### Retracting a bad release tag cleanly

If you ever have to retract a tag that already created a GitHub Release, **never delete only the Git ref** (which leaves an orphaned `untagged-*` draft Release on GitHub). Run:

```sh
gh release delete v0.48.0-dev.1 --cleanup-tag --yes
git tag -d v0.48.0-dev.1
git fetch --prune --prune-tags origin
```
