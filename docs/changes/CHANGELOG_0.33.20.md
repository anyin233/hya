# 0.33.20

- Grok Build OAuth: `kind: grok-build` prefers `~/.grok/auth.json` session tokens from `grok login`, sends CLI chat-proxy session headers, and falls back to hya login / inline API keys.
