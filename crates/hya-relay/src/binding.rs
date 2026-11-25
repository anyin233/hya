//! Shared web binding markers for the relay client and server.

/// WebSocket close code base for relay errors: a stream that fails with
/// `RelayErrorCode` `c` closes with `4000 + c` (for example `4005` for
/// `NOT_FOUND`, `4014` for `UNAVAILABLE`).
pub const WS_CLOSE_ERROR_BASE: u16 = 4000;

/// WebSocket close code for a text frame (the relay speaks binary only).
pub const WS_CLOSE_UNSUPPORTED_DATA: u16 = 1003;

/// Body of the `404` answer to any non-relay request. `hya relay doctor`
/// uses it to recognize a relay server behind an intermediary.
pub const NOT_FOUND_BODY: &str = "hya relay";
