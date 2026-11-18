# 0.48.0-dev.5

- `bash` now stamps every file the session knows after each call, with or without Git, so a shell edit outside a Git work tree is recorded as the command's own observation instead of an outside change.
- The kernel shows the goal checkpoint when context reaches 60% before a rewrite, asking the model to record in the todo list every result it still needs. It leaves later requests once the assistant produces another part or a rewrite occurs. A rewrite marker that carries a todo list states that results recorded there are still valid. The `todo` tool asks for checkable end states and results worth keeping, accepts ids as rendered (`#1`), and can change an item’s content and status in one operation or set its status on creation.
- Headless `hya exec` no longer continues a run because todos are open; live subagents, unread mail and an announced next step still continue it.
