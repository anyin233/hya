# 0.45.4

## Subagents

- Make `task` batch-first with concurrent `tasks[]`, shared `context`, and compatible single-task/`members[]` inputs. Reject empty batches and blank member prompts before dispatch.
- Expose running handles and per-member failures accurately; batch launch no longer claims completed work. Align task, discovery, wait and agent prompts around launching independent siblings together and collecting reports afterward.

- Prepare first-party native tool policies concurrently and reuse verified native-library cache files across daemon starts.
- Probe new daemons promptly and support cross-process startup trace files.
- Run VCS snapshots outside async RPC workers so Git subprocesses do not stall startup requests.
- Supervise the terminal TUI directly using its existing reload protocol, preserving sessions and drafts while removing an extra Bun process at startup.
