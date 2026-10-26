# 0.45.11

## ChatGPT plan usage

- Add official Sign in with ChatGPT with dynamic registration, verified identity, protected per-provider accounts, plan-permission checks, refresh serialization, and logout revocation. `hya oauth login --provider chatgpt --type openai-chatgpt` connects without a Platform API key.
- Implement the existing v1 OAuth start/callback routes through live provider control. The new `openai-chatgpt` route uses the public Responses API, account model discovery, stateless streaming and namespaced local tools while retaining Hya's agent loop. Credential removal reports a warning when remote revocation cannot be confirmed.
