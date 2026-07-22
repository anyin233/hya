# 0.33.21

- Grok Build auth is self-contained: credentials come from config `api_key` or `hya login` only (no `~/.grok/auth.json` reads); `grok-build` always sends CLI chat-proxy session headers.
