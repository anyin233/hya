# 0.43.6

## Separate command input

- `/` opens a dedicated command pane while the message composer is empty; Ctrl+X then `/` opens it without losing a message draft.
- Command suggestions, argument completion, and command history now live in that pane. Esc returns to the previous input, and slash characters inside a message remain literal.
- A pasted command-looking message is sent as message text; relay links in message text are still refused.
