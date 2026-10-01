# 0.43.42

## Fixes

- `hya update` compares a release's `min_updater_version` against the backend
  release version. Before, it compared against the updater crate's placeholder
  version `0.0.0`, so it rejected every signed release with `UpdaterTooOld`.
  `hya update version` now prints the backend release version.

## Maintenance

- Removed dead code with no change in behavior: Rust APIs that nothing calls or
  that only their own tests call, the legacy git diff and model-parser helpers
  in `hya-server`, an unread `workspace_adapters` server field, the unused
  Compat model-import code in `hya-app`, and the placeholder
  `hya-plugin-example` crate.
- Removed `docs/development-history/`, old tracked `.planning/` folders, and
  unreferenced assets and test fixtures.
