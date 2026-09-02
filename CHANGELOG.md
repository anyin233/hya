# 0.36.9

## Session tool reliability and native coding tools

- Fix Read duplicate-key failures from captured `filePath` plus `path` requests; canonicalize `path` while retaining hidden legacy compatibility, and normalize empty Task inline descriptions so schema and execution agree.
- Replace fuzzy filesystem coding tools with native hashline Read/Edit/Grep, with bounded process-local recovery state, typed errors, and filesystem-safe locking and atomicity.
- Align Write, Bash, and tool-result envelopes with host contracts, including hidden `shell` compatibility, shape-aware result caps, bounded output/artifacts, and semantic metadata.
- Add hya-owned coding-tool TUI blocks for Read/Edit/Write/Grep/Bash with syntax-aware rendering, 80-column and wide-terminal layouts, and Session replay.
- Harden external-path privacy, atomic inode revalidation, cancellation and PTY descendant cleanup, bounded Grep/hashline/result processing, post-hook caps, private Bash artifacts, single-owner TUI synchronization, and recursive notice-complete release packaging.
- Update tests, specifications, user documentation, and source/license notices. Historical Events remain unchanged; an already-running backend must restart before future calls use this release.
