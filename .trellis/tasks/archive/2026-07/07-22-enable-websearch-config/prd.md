# Enable configurable websearch for all providers

## Goal

Make the built-in `websearch` tool configurable and available to every model
provider instead of tying exposure to one model-provider ID.

## Requirements

- Add an optional `tools.websearch` configuration section with `provider`,
  `endpoint`, `key`, and `enabled` fields.
- Default `enabled` to `true` and `provider` to Exa.
- With no websearch configuration, preserve unauthenticated requests to the
  built-in Exa endpoint.
- When configured, apply the endpoint override and add the key to every search
  request using the selected search provider's authentication convention.
- Expose `websearch` independently of the selected model provider.
- Reuse the existing `WebSearchConfig`, `WebSearchPlane`, and configuration
  loading paths; do not add another search abstraction.

## Acceptance Criteria

- [x] Omitted `tools.websearch` configuration enables unauthenticated Exa
      search at the existing default endpoint.
- [x] Explicit provider, endpoint, key, and enabled values reach the runtime
      websearch plane.
- [x] `enabled: false` prevents the built-in websearch tool from being exposed.
- [x] Every model provider receives the websearch schema when websearch is
      enabled.
- [x] Focused config, request, and tool-filtering regressions pass.
- [x] The Rust workspace verification gate passes.

## Notes

- This is a lightweight, PRD-only task per the user's request to implement
  directly without a separate planning phase.
