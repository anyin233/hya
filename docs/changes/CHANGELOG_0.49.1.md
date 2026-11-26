# 0.49.1

- **Rust API change for embedders.** Skill catalog discovery (`discover`, `parse_skill`, `builtin_skills`, `skill_dirs_for` and the rest of the module) moved from `hya_app::skill_catalog` to `hya_engine::skill_catalog`. No runtime, wire or CLI behaviour changes.
- **Smaller test build graph.** `hya-engine` and `hya-server` no longer take `hya-app` as a dev-dependency, so their test binaries no longer compile the runtime composition crate.
