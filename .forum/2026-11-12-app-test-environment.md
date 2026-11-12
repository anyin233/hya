# Why did a registry install test fail with BundleRegistryBusy only in parallel?

A temporary transaction/path/thread trace at fbc017e4 caught
`engine_build_fences_running_resident_and_resumes_queued_mail_before_readiness`
opening the registry in the temporary XDG_DATA_HOME of
`built_engine_lazily_refreshes_installed_catalog_at_root_binding`.
Startup recovery bound a catalog through `InstalledBundleRefresh`, whose lazy
`BundleRegistry::connect` reserved `BEGIN IMMEDIATE` for migration while the
other test installed. The sweeper was not the writer.

The runtime's environment writers had a private RwLock but its readers were
unguarded; config writers had a second, independent mutex. The missing reader
guard already existed at 5d7536b7, before d525b646. Unit tests now share the
`hya-app/src/test_env.rs` reader/writer gate across modules, acquired before the
first environment-dependent operation and retained for the test's duration.
Config mutators must acquire before their initial default-resolution assertions,
not only before the first set_var. Tests with explicit file paths and no
process-environment/cwd-dependent calls need no guard. Never add a module-local
environment lock or a mutation retry to address this failure.

The resident Bun reuse test also bounds each whole wait-for-idle loop. A
sidecar/turn failure does not promise an Idle event, and the held engine keeps
the bus open. Forcing Bun to exit (`BUN=/bin/false`) left the unchanged exact
test waiting until an external eight-second kill; the fixed test reports the
first missing Idle after its five-second deadline. This confirms the loop's
hang mechanism independently of environment leakage; the originally reported
load-triggered failure of the resident was not captured.
