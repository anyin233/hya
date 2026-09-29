# 0.43.5

## Text parts are stored in the order the model wrote them

- Each assistant text part now becomes durable at its own `TextEnd` (after any `text_complete` rewrite), instead of after the whole provider round. Text the model wrote before a tool call is stored before that tool call, so replay, the TUI transcript, and later model requests see the real order.
- Previously the stored order was reasoning, tools, then text; rebuilt requests placed the step's text after its tool results, and on Anthropic routes the latest step's text became a trailing assistant message that models read as a new user instruction.
- If a provider stream fails mid-part, the text streamed so far is stored (without the `text_complete` hook) instead of being lost from replay.
