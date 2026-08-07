# Findings

- `ProviderKind::GrokBuild` routes to `/responses` with Bearer authentication.
- `GrokBuildProtocol` reuses the Responses encoder/decoder, adds encrypted
  reasoning inclusion, and requires a typed terminal event.
- The shared decoder now accepts `response.reasoning_text.delta`; ordinary
  `openai-response` terminal behavior remains permissive.
- Grok reasoning variants are `low`, `medium`, and `high`, defaulting to `high`.
- Provider/config files share a dirty worktree with unrelated active tasks, so
  review and staging must be hunk-scoped.
- The current Grok diff is an unstaged, localized extension in five provider
  files plus config/docs/spec tests; no new dependency or transport branch was
  added.
- Release metadata is shared with a concurrent websearch task. `CHANGELOG.md`
  currently contains that task's entry plus an unstaged Grok entry, while the
  version files had no Grok-only unstaged diff when reviewed.
- The concurrent websearch task was committed at `4f803641`/`bfcefef9`; its
  `0.33.17` version and changelog entry are now the repository baseline. The
  Grok changelog line remains the task-owned addition.
- No Grok/XAI key or base-URL environment variable is available in this shell,
  so the live acceptance gate cannot be retried without runtime input.
- With runtime input supplied, exact implemented Responses probes for `low`,
  `medium`, and `high` each returned HTTP 503 `api_error`, with no typed
  terminal event or output text. This matches the earlier minimal Responses
  and Chat controls and leaves acceptance externally blocked.
- Differential live probes rule out the only remaining local header concern:
  adding `Accept: text/event-stream` does not change the 503. The model catalog
  is healthy, while valid and invalid non-streaming model requests both fail
  identically, locating the fault before model validation.
- Current xAI docs do not define 503 as validation, authentication, rate-limit,
  or retry behavior. A sanitized custom-gateway trace is recorded in the API
  research; only origin logs can distinguish route provisioning, entitlement,
  dispatch, and upstream network/capacity failure.
