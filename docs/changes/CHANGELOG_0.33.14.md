# 0.33.14

- Added `openai-response` providers with Responses API streaming for text, reasoning summaries, parallel function calls, usage, and errors.
- Added validated per-model reasoning variants and startup defaults, including all seven reasoning effort levels supported by `gpt-5.6-sol`.
- Preserved completed opaque reasoning items through event replay so stateless tool continuations can send them back unchanged.
- Kept `openai`, `openai-compatible`, and `openai-completion` on Chat Completions with request, stream, replay, and startup regression coverage.
