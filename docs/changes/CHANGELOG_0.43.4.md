# 0.43.4

## Anthropic routes keep the model's thinking between tool steps

- The Anthropic decoder now keeps each thinking block's `signature` (and `redacted_thinking` data) as the reasoning part's provider data.
- Within the current turn, the Anthropic encoder replays signed thinking blocks before the text and `tool_use` blocks of the same step, so the model sees its own earlier reasoning in a tool loop instead of re-deriving it every step. Reasoning from earlier turns, or without provider data, is not replayed.
- Assistant parts are grouped per step as `[thinking*, text?, tool_use+]`. For histories recorded before the text-ordering fix, trailing text in the final assistant message is folded into its step instead of becoming a trailing assistant message the model could mistake for user input.
