# 0.37.13

## Continue DeepSeek tool turns

OpenAI Chat streaming now preserves DeepSeek thinking content and sends it back
with assistant history after a tool call. Durable turn replay also keeps
assistant text before its tool calls. Together, these changes let a DeepSeek
turn continue after the user approves a pending tool request.
