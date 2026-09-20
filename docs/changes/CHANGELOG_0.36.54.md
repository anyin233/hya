# 0.36.54

## send unifies dm and broadcast into one channel-addressed tool

`dm` and `broadcast` collapse into a single `send` tool: send a message
on one channel, and the channel's own nature decides the delivery.

- `{ "channel"?, "body" }` — `#channel` (or a bare `DM-…`/`announce-…`
  id from `list_channel`) posts on that channel; a bare handle sends
  private vertical mail (`^parent` for the upward peer; archived
  children still revive); the legacy `to` field spelling still parses.
- Omitted `channel` routes by role: the unit group channel when the
  sender leads one (broadcast), else the parent DM pair, else a typed
  error.
- Delivery kind is derived engine-side from the channel: group posts
  are stamped as announcements, everything else stays 1:1 chatter.
  The group-channel write gate (leader-only) is unchanged.
- `dm` and `broadcast` are removed from dispatch; canonical advertised
  tool count is 27. The mailbox plane loses the tool-only `dm()`/
  `announce()` helpers and the `Announce` request variant (replaced by
  `SendDefault`); engine `mail_send`/`mail_announce*` are unchanged.
- Docs realigned: the tools-and-permissions inventory drops the
  long-drifted `roster`/`channels`/`join`/`leave`/`announce` rows in
  favor of the real `send`/`list_channel`/`search_agent` contract.
