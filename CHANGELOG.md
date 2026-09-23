# 0.37.16

## Built-in tool bundle completion

- Move the base, network, and channel tool implementations and their source assets into their owning native bundles. All 27 canonical built-in tools now load from five trusted `.hyabundle` packages.
- Poll native tool futures on their bundle's Tokio runtime while preserving host task-local admission context; support calls without an existing runtime through a temporary joined worker.
- Load the lockstep library a Cargo build just linked before any package staged under `target/debug/bundles/`. Stale staged packages no longer shadow fresh builds or add package verification to local backend startup.
- Keep `hya-tool` as the tool trait, registry, permission, session-plane, and native-loader crate. Release assets now include all five native tool bundles.
