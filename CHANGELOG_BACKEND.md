# 0.45.9

## Runtime

- Fixed a redundant `TEAM QUIESCED` synthesis after the parent had already consumed subagent results through `wait` or `read channel://…`. Both delivery paths now acknowledge the resident supervisor after committing the inbox cursor, as automatic in-turn steering already does. Genuinely late unread mail still wakes the parent.
