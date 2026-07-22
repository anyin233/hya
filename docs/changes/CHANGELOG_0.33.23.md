# 0.33.23

- Add OAuth login for `openai-codex` and `grok-build` via `hya-backend oauth login`.
- Store OAuth credential bundles under `~/.config/hya/auth/<provider>.yaml` with automatic access-token refresh and re-login hints on expiry.
- Add provider kind `openai-codex` (ChatGPT Codex Responses backend + `ChatGPT-Account-Id`).
- Upsert provider routes into `config.yaml` after a successful OAuth login.
