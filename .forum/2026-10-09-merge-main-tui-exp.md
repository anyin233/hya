# What must survive merging startup and batch-task main into the PTY branch?

Keep the required `tui` job on `bun run test:tui-exp`, with seven tagged
browser-only cases and two generic host cases excluded. Main adds three
startup/reload scenarios and two batch-task pane cases: 299 terminal scenarios
now share the same driver, in addition to nine protocol checks.

The precompiled frontend can return to Conversation during asynchronous
startup after Status first appears. Launch checks must read related status
facts from one snapshot and reopen Status when needed, rather than wait for
separate fields on a view that may have disappeared. Use the shared session-id
reader for startup session admission.

When validating in a worktree, required extension sandbox checks need SDK
packages inside that worktree's allowed roots. A node_modules symlink outside
the worktree may resolve on the host but fail in the sandbox. Hard-linked
SDK dependencies preserve local paths without duplicating package data.
