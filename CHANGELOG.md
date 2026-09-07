# 0.36.13

## Agent configuration and Session model overrides

- Load built-in Agent model defaults from Hya configuration and bundle Agent defaults from origin-owned `agents/<encoded-bundle-id>/config.yml` files, above remembered TUI defaults.
- Treat ordinary choices for configured Agents as durable root-Session overrides shared with descendants, without leaking into unrelated Sessions or already captured work.
- Add **Ctrl+S — Save configured default** to model pickers. Save only the owning YAML model leaf, preserve unrelated settings and permissions, and retain distinct Session overrides.
- Keep explicit request, inline spawn, and Workflow model choices authoritative at the actual provider-request boundary.
- Preserve draft selections and reasoning when the first Session starts, retain legacy-backend behavior, and display effective provenance and configuration destinations.
- Expose each executable bundle's own configuration directory and file through `HYA_BUNDLE_CONFIG_DIR` and `HYA_BUNDLE_CONFIG_FILE`.
