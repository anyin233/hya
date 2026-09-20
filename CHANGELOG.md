# 0.36.53

## todo__ namespaced tool group replaces todowrite

The single full-replace `todowrite` tool is replaced by a three-tool
group under the first builtin `todo` namespace:

- `todo__read` — the session's current items as `{id, content, status}`
  with stable plane-assigned ids ("1", "2", …) that are never reused.
- `todo__update_status` — batch `{id, status}` updates.
- `todo__update_content` — batch `add` / `remove` / `edit` operations,
  validated whole before anything is applied; a bad id fails the batch
  with the current ids listed and leaves the list untouched.

Statuses are now a typed set — `pending`, `in_progress`, `blocked`,
`completed` — replacing opaque strings; the v1 wire enum gains
`TODO_STATUS_BLOCKED`, and `GET /v1/sessions/{id}/todo` returns the
plane's stable ids (replayed pre-0.36.53 rows keep synthesized
`todo-{index}` ids via a lenient fold). The optional `priority` field
is dropped (the wire never carried it). `todowrite` and its `todo`
alias are removed from dispatch; canonical advertised tool count is
28.

Also fixes a pre-existing flaky hya-app `workflow_control` fixture:
parallel tests could collide on a nanosecond temp-dir nonce and delete
each other's catalog roots (now unique via an atomic counter).
