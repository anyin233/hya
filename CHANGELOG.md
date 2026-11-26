# 0.50.0

- Editors can launch `hya acp` to stream conversations, display tool calls, answer permissions and cancel turns over Agent Client Protocol v1. Each editor session can attach its own stdio MCP servers, which stop when the editor sends `session/close`.
- A turn that fails because the provider rejected its credentials (expired login, HTTP 401 or 403) now reports error code `provider_auth` instead of `provider_error`.
