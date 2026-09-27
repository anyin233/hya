# 0.42.1

## Session tree deletion

- Deleting a session now deletes its entire descendant subagent session tree, including event logs, token ledgers, projection snapshots, open-assistant indexes, and file snapshots. Unrelated sessions remain available, and conditional deletion still refuses to remove a tree when the root log has changed.
