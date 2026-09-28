# 0.43.7

## Reasoning is stored once per part

- Reasoning deltas are now live-only (streamed to connected clients as before). The durable log keeps `ReasoningStart` in stream order, then one `ReasoningReplace` with the full text and the `ReasoningEnd` (with provider data) when the part closes. A long thinking block is a handful of rows instead of one row per token; one traced session had 631k reasoning rows.
- If the provider stream fails, or ends without closing a reasoning part, the thinking streamed so far is still stored.
- Logs written by earlier versions (durable per-token deltas) replay unchanged.
