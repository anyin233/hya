//! A native hya plugin used as a deterministic test/QA fixture.
//!
//! Phase 0 ships a no-op stub; Phase 7 makes it speak the plugin protocol on
//! stdin/stdout (message.user.before marker, chat.params temperature, a
//! tool.execute.before veto sentinel, and event logging to stderr).

fn main() {}
