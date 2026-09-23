# 0.40.0

## `hya bundle search` and `hya bundle schema` cover every scope

- `hya bundle search` now covers every bundle `hya bundle list` shows: builtin, user-installed, and project (`./.hya/bundles`) bundles, with the same SCOPE column and `shadowed` state. Before, it covered only builtin and user-installed bundles. The new `--user` and `--project` flags narrow a search to one scope. `list` and `search` now share one catalog builder, so they can't drift apart.
- **Breaking:** `hya bundle schemas` (every scheme across the catalog) is replaced by `hya bundle schema <BUNDLE_ID|PACKAGE>`. It prints a `SCHEME TOOL WRITABLE` header and one row per scheme the named bundle declares, and only the header when the bundle declares none. A bundle id resolves like `info` (preset, project, user, then first-party), or `--user`/`--project` narrows it. A `.hyabundle` path is inspected without installing it. An unknown id exits 1 with `BUNDLE_NOT_FOUND`. For the merged, live scheme table across bundles, use `GET /v1/runtime/schemas`.
