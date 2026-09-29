# 0.43.2

## Continue DeepSeek tool turns

- OpenAI Chat streaming preserves reasoning content and sends it back with
  assistant history after tool calls, as required by DeepSeek thinking mode.
- DeepSeek assistant history includes an empty reasoning field when a model
  produced no reasoning chunks. Durable replay keeps assistant text before
  its tool calls so the follow-up request retains the original order.
- The TUI and WebUI package versions now match the 0.43.2 backend, so a fresh
  frontend no longer reports a version mismatch at connection.
