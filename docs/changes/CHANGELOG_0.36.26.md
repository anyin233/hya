# 0.36.26

## Repository cleanup (no behavior change)

- `hya-sdk`: removed the Rust-TUI-era `PendingClient`/`PendingSlot`, the Bun stdio `NativeBridge`/`NativeClient` (superseded by `hya-native`), `Session::revert_message_id`, and the unused `WorkflowActivity` family; the `native_spike` example is gone.
- `hya-proto`: removed the never-written `CostBreakdown` type.
- `hya-app`: dropped the test-only spawn-intent batch encoder and the file-level `allow(dead_code)`.
- `hya-server`: the six identical Compat `millis` helpers now live in `compat/time.rs`; `hya-provider::ModelCatalogSource::as_str` replaces two copies of the source-label mapper.
- TUI: removed unused `DialogTag`, the legacy session `Footer`, `toolDisplayMetadata`, `startupTraceEnabled`, keybind `Descriptions`, `win32InstallCtrlCGuard`, `isZedTerminal`, and `offsetToPosition`; `getRelativeTime` and `isRecord` are shared from `util/`.
- Restored the CI gates that were red on `main`: rustfmt drift, two clippy lints in the provider-cache code, the README workspace version, and the TUI boundary allowlist for `boot.tsx`.
