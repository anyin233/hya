# Where does the disk inspector plugin live, and what works initially?

**Answer:** `plugins/disk-inspector/` owns its manifest, explicit Bun stdio
provider, shared contracts, TUI SDK entry, response schema, docs and tests.
It packages through the existing xtask and installs as one optional bundle,
`hya-extra/disk-inspector`; it is not automatically included in release assets.
Manifest version follows `version_ref: backend` with identity `0.0.0`.

The scaffold implements global `GET /info` through the existing bundle API
router and reports backend hostname/platform and false volume/scan/cancel
capabilities. Its `disk` pane is opt-in and truthfully disconnected: no generic
own-bundle request bridge exists in the SDK yet. Do not bypass this with local
frontend filesystem reads, arbitrary fetch or disk-specific host routes.
`InspectorClient.info()` is only a plugin-local interface, not an SDK method.

Next phases are the generic bridge, plugin-owned bounded background scanner,
interactive view model, and generic independent pane instances. Exact implemented
contracts are in the plugin README, proposed foundations in FOUNDATION.md.
Builds, package and browser artifacts live under `~/data/hya-plugins/disk-inspector`.
Browser execution on this machine uses
`PLAYWRIGHT_BROWSERS_PATH=~/data/hya-rust/playwright-browsers`.
