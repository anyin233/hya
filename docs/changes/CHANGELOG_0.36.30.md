# 0.36.30

## Token accounting is visible on the wire and in the TUI (core, server, TUI)

- Every streaming round now emits a `ContextStatus` event: the window
  occupancy the request actually carries, whether the figure was
  provider-anchored or locally estimated, the accounting mode in force
  (`auto` / `provider` / `estimate`), and the resolved compaction threshold it
  was judged against. The event is recorded after the compaction ladder, so
  the reported number matches what was sent.
- The session projection folds the latest report and the session JSON carries
  it as a `context` block (`tokens`, `source`, `mode`, `threshold`), so any
  client can see what the engine believed — including the fallback to a local
  estimate when a route's reported usage is absent or implausible.
- The TUI sidebar's Context panel now prefers the accounting report and shows
  occupancy against the resolved threshold — `4 / 150,000 tokens`, `0% used` —
  with an `estimated` badge whenever the figure is a local estimate instead of
  a provider-reported one. Sessions on older backends fall back to the previous
  last-message computation.
- `TokenAccountingMode` and `TokenSource` moved into `hya-proto` as wire types
  (`hya_core` re-exports both), so events, projections, and clients share one
  spelling.
