# 0.45.4

## Subagents

- Make `task` batch-first with concurrent `tasks[]`, shared `context`, and compatible single-task/`members[]` inputs. Reject empty batches and blank member prompts before dispatch.
- Expose running handles and per-member failures accurately; batch launch no longer claims completed work. Align task, discovery, wait and agent prompts around launching independent siblings together and collecting reports afterward.
