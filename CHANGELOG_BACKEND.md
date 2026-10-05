# 0.45.3

## Bundles

- Bundles may declare an executable TUI extension with a top-level `tui:` section (`api_version`, `entry`, `sdk`, `permissions`). Preparation validates the entry, the independent SDK version, and the permission allowlist (including `fs.read`), and covers the declaration in the bundle digest.
- Added the trusted first-party `hya/basic-tui-components` bundle, which supplies the built-in TUI Sessions, Todos, Projects, and Context surfaces.
- Added `hya bundle enable|disable <id>`: a disabled bundle stays installed but publishes nothing in any scope (agents, skills, tools, MCP servers, workflows, APIs, permission modes, TUI extension); `hya bundle list` shows it `disabled`. `remove`, `enable`, and `disable` refresh a running backend at once.

## API

- Added `ListTuiExtensions` (`GET /v1/tui-extensions`): the scope's bundle TUI extensions with their verified source files (path, sha256, content), so a TUI can run them locally without access to the bundle store.
- Added the catalog `first_party` field (`first_party = 9`) so clients can distinguish bundles from the trusted first-party inventory.
- `ListTuiExtensions` accepts `known=<digest>,…` (at most 64 prepared digests): those entries come back with `cached = 10` set and no files.
- Added bundle management: `ListBundles` (`GET /v1/bundles`: every bundle with scope, state, components, and its TUI extension), `InstallBundle` (`POST /v1/bundles:install`), `UninstallBundle` (`POST /v1/bundles:uninstall`), and `SetBundleEnabled` (`POST /v1/bundles:set-enabled`); each change refreshes the scope and returns the refresh.

## Runtime and tooling

- Fixed `hya serve restart` run by one of the daemon's own turns (a shell tool call): since it started waiting for the successor, the turn could not reach its handoff boundary and every such restart was rejected at the drain deadline. Inside the daemon's session it now returns at the `queued` acknowledgement again, and the successor continues the turn.
