# Add Grok Build provider compatibility

## Goal

Allow hya users to select a `grok-build` provider and use Grok Build-style endpoints through the existing model runtime.

## Background

- The supplied test route uses model `grok-4.5` at an OpenAI Responses-style base URL.
- The route advertises a 1,000,000-token context window and backend search support.
- The supplied API credential is test-only sensitive data and must not be committed, copied into task artifacts, or printed by tests.

## Requirements

- Recognize `grok-build` as a native YAML provider `kind` in configuration and provider routing.
- Determine the Grok API and Grok Build request/streaming contracts from primary evidence before implementation.
- Exercise the supplied Grok 4.5 endpoint with the compatible request format and every supported reasoning-effort value.
- Preserve hya's normalized event-stream behavior and existing provider compatibility.
- Keep the implementation to the smallest extension of existing provider code that the protocol evidence supports.
- Add the required project version and changelog update for the feature.

## Acceptance Criteria

- [x] A native hya provider configured with `kind: grok-build` is accepted and routes requests through the correct provider behavior.
- [x] A focused automated test fails before the implementation and passes afterward at the agreed public provider seam.
- [ ] The Grok 4.5 test endpoint returns a successful response for each reasoning effort that research establishes as supported; sanitized observations are recorded without credentials or response secrets.
- [x] Streaming/tool/search behavior needed by the existing runtime remains normalized into existing hya events.
- [x] Existing provider tests remain green.
- [x] Rust formatting, clippy, workspace tests, and a local executable build pass.
- [x] The workspace version and newest-only root changelog follow the repository release rules.

## Out Of Scope

- A general provider abstraction rewrite.
- Parsing the supplied `[models]` / `[model.*]` TOML metadata as a new hya configuration format.
- Persisting endpoint credentials or adding a checked-in endpoint-specific configuration.
- Compatibility shims unsupported by observed Grok Build behavior.

## Notes

- Supported reasoning-effort values and exact request/streaming differences remain research questions until validated against primary documentation and the supplied endpoint.
