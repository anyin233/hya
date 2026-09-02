# 0.36.8

## Model catalog endpoint discovery

- Resolve one immutable model catalog at each backend startup. Explicit Hya model lists remain authoritative and network-free; empty lists use bounded provider-kind discovery with optional Hya authentication.
- Publish non-secret configured, discovered, authentication-required, authentication-rejected, unavailable, empty, unsupported, and offline provider states across the server bootstrap and catalog APIs.
- Use exactly `hya/offline` when no live row resolves. The local provider echoes the request and explains that a live provider must be configured; the row is absent when any live model exists.
- Keep normal startup Hya-owned: foreign agent configuration is read only by the explicit Compat import command.
- Make the CLI, server, Rust SDK, and TypeScript TUI consume the same snapshot. Stale Session, Recent, and Favorite values cannot create catalog rows.
- Keep OAuth model fetches as non-persistent login previews, preserve user-authored model lists, and leave empty lists eligible for startup discovery.
- Remove the redundant `models --refresh` option.
