# 0.37.15

## Extended native tool bundle

- Move all nine extended built-in tool implementations into the extended bundle's Rust dynamic library.
- Keep host-owned LSP, skill, spawn, workflow, mailbox, and lifecycle service planes in `hya-tool`, exposing their narrow methods to the bundle.
- Package and load the extended library beside the backend in release assets.
