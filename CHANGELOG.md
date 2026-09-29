# 0.43.26

## Fixes

- **DeepSeek and other thinking-mode chat models can finish a tool round over `openai-compatible`.** An `openai-compatible` (Chat Completions) route used to discard the streamed `reasoning_content`. The next request after a tool call then failed with `http status 400: The reasoning_content in the thinking mode must be passed back to the API`. hya now stores that reasoning as a reasoning part and sends it back as the assistant message's `reasoning_content` on every later request. Once a session holds such reasoning, every assistant message carries the field, as an empty string when a message had none. Reasoning from other protocols, such as Anthropic thinking before a model switch, is never sent. The context estimate counts the resent reasoning.
- **A preamble streamed before a tool call stays in front of it on `openai-compatible` routes.** Text such as "I'll start by finding your slides draft." was stored after the tool call. The next request then sent it as a separate assistant message after the tool result. The chat decoder now ends an open reasoning or text part before the next part starts.
