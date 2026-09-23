# 0.37.17

## First-party bundles load at runtime

- Load every first-party tool policy, agent, Skill, command, channel preset and workflow bundle at runtime through `hya_bundle::first_party_bundle`. Installed backends read the trusted `bundles/hya-<name>.hyabundle` packages beside `bin/`; Cargo builds read the in-tree sources, so edits apply on restart. The `hya-core`, `hya-tool` and `hya-app` build scripts no longer embed bundle content.
- Add the `hya/core-commands` Plugin, which owns the `/init` and `/review` prompt templates. The trusted preset inventory and `bundle list` now show nine presets.
- Replace the `hya_core::BUILTIN_AGENTS` constant with `hya_core::builtin_agents()`. Core agent reserved ids come from the loaded preset policy.
- Release assets include all twelve first-party packages, and the release smoke test lists them through the packaged backend.
- Run the edit and read serialization tests on the real clock. Native tool I/O runs on the bundle's Tokio runtime, which a paused host test clock cannot observe.
