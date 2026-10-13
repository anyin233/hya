# 0.45.7

## API

- Expose subagent actor archival and its reason independently of task outcomes in member snapshots and durable lifecycle streams, including revival and archived handles for nested agents.
- Fold actor lifecycle updates in the v1 SDK session mirror; deliver them on the lightweight global stream as well as the team root stream.
