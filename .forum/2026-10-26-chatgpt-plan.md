# How does Hya connect official ChatGPT plan usage while retaining its own agent loop?

Use the new `openai-chatgpt` provider and OAuth type, backed by the public Responses API and OpenAI's dynamic registration flow. This is separate from existing API-key Responses and ChatGPT Codex backend routes. The implementation was based on origin/main 90bd9e47 after PR #33 merged.

`hya oauth login --provider chatgpt --type openai-chatgpt` and `l` in `/key` share the same registration, verified OIDC identity, scope checks, protected credentials and refresh. The existing v1 OAuth methods now delegate through ProviderControl; their default trait methods remain unavailable for runtimes without a provider manager. Manual completion takes the entire callback URL so a new issued client_id can be validated; sending a bare code loses that information.

Never use dynamic_agent_client for token exchange or refresh. Save the issued registration before exchanging the code, preserve host ID and account mapping through sign-out, and reject returning sign-ins that change identity. State, nonce, PKCE, signed issuer/audience/expiry validation, plan-use scope checks and refresh serialization are required. Do not reuse Codex's hard-coded client ID or another tool's token store.

Plan requests need stream:true/store:false, developer history, namespaced tools and full local context; temperature, max_output_tokens and hosted compaction are unsupported. Preserve function-call namespace on replay. Discover account-visible models using models[].slug/display_name/visibility, and do not persist guessed models when discovery fails. A saved ChatGPT grant must not follow a provider edit to an arbitrary host.

Verification uses fake token/JWKS servers and signed offline JWT fixtures. The only live OpenAI request during development read the public OIDC discovery document; no live login or inference was performed. Interactive consent and account entitlement remain a manual check.
