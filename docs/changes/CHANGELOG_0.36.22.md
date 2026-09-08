# 0.36.22

## Steer and queue

- While a turn is running, Enter queues a follow-up instead of returning session-busy.
- `ctrl+alt+return` aborts the current turn and sends the composer text now.
- Wire `session.queued_prompts` (`<leader>q` when the queue is not empty) to inspect and delete waiting follow-ups.
