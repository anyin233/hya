# 0.43.4

## Reopen chats with waiting requests

- A plain local TUI launch reopens the active Project's latest saved chat, including chats archived on exit. A waiting permission or question takes priority over a newer chat, and the transcript is restored immediately.
- The `/sessions` picker includes archived chats by default. Ctrl+A toggles their visibility.
- Pending requests in other chats show F4 as the direct route to the conversation and its numbered approval choices, even when the chat is archived or absent from the sidebar.
