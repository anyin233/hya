# 0.37.10

## Call-scoped native tool host RPC

- Add optional `host_capability` on native `tool/call` requests and a child-to-host `host/capability` request on the same plugin connection.
- Bind each capability to its process connection, session, and tool call; revoke it after completion or cancellation, with typed denial for invalid and expired requests.
