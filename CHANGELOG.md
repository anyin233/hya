# 0.33.14

- Added before/after content snapshots to edit-tool result events.
- Made Compat session revert and unrevert restore edit snapshots on disk with workspace path and symlink containment checks.
- Preserved the earliest before and latest after content across repeated edits to the same file, with integration and unit coverage.
- Restored formatted content and UTF-8 BOMs exactly from edit snapshots.
- Made session revert remove files created by the reverted edit and support relative session work directories.
