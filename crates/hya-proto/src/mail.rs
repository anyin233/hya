//! Event-sourced mailbox address + message-kind types (ADR-0001).
//!
//! These are the serializable, dependency-light successors to the in-memory
//! `MailEndpoint`/`MailKind` that used to live in `hya-core::team`. Direct 1:1
//! mail is addressed by a stable **handle**; a **channel** (`#name`) is a
//! multi-subscriber endpoint. Broadcast is modelled as a well-known channel
//! rather than a distinct variant, keeping one delivery primitive.

use serde::{Deserialize, Serialize};

/// A mail address: either a single agent's stable handle or a named channel.
///
/// Adjacently tagged so the wire form is unambiguous and self-describing:
/// `{"kind":"handle","id":"reviewer-3"}` / `{"kind":"channel","id":"build"}`.
/// Channel ids are stored without the leading `#`; the `#` is a UI/address
/// convention parsed at the tool boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum MailEndpoint {
    /// A single agent, addressed by its team-scoped handle (e.g. `reviewer-3`).
    Handle(String),
    /// A named channel; every current subscriber receives the message.
    Channel(String),
}

impl MailEndpoint {
    /// Parse an address string as written by a model: a leading `#` marks a
    /// channel, anything else is a handle. Surrounding whitespace is trimmed and
    /// the `#` prefix is stripped from the stored channel id.
    #[must_use]
    pub fn parse(addr: &str) -> Self {
        let addr = addr.trim();
        match addr.strip_prefix('#') {
            Some(channel) => MailEndpoint::Channel(channel.to_string()),
            None => MailEndpoint::Handle(addr.to_string()),
        }
    }

    /// The channel id if this address is a channel, else `None`.
    #[must_use]
    pub fn channel(&self) -> Option<&str> {
        match self {
            MailEndpoint::Channel(name) => Some(name),
            MailEndpoint::Handle(_) => None,
        }
    }

    /// The handle if this address is a direct handle, else `None`.
    #[must_use]
    pub fn handle(&self) -> Option<&str> {
        match self {
            MailEndpoint::Handle(name) => Some(name),
            MailEndpoint::Channel(_) => None,
        }
    }
}

/// The intent of a message, carried for rendering/policy (parity with the old
/// `MailKind`). `Announcement` marks channel-wide notices; `Message` is the
/// default 1:1/channel chatter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailKind {
    #[default]
    Message,
    Announcement,
}

impl MailKind {
    /// Parse a model-supplied kind string, defaulting to [`MailKind::Message`]
    /// for empty/unknown input so a missing `kind` is never an error.
    #[must_use]
    pub fn parse(kind: &str) -> Self {
        match kind.trim().to_ascii_lowercase().as_str() {
            "announcement" | "announce" => MailKind::Announcement,
            _ => MailKind::Message,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn endpoint_parse_distinguishes_handle_and_channel() {
        assert_eq!(
            MailEndpoint::parse("reviewer-3"),
            MailEndpoint::Handle("reviewer-3".to_string())
        );
        assert_eq!(
            MailEndpoint::parse(" #build "),
            MailEndpoint::Channel("build".to_string())
        );
    }

    #[test]
    fn endpoint_round_trips_through_json() {
        for endpoint in [
            MailEndpoint::Handle("main".to_string()),
            MailEndpoint::Channel("build".to_string()),
        ] {
            let json = serde_json::to_string(&endpoint).unwrap();
            let back: MailEndpoint = serde_json::from_str(&json).unwrap();
            assert_eq!(endpoint, back);
        }
    }

    #[test]
    fn kind_parse_defaults_to_message() {
        assert_eq!(MailKind::parse("announcement"), MailKind::Announcement);
        assert_eq!(MailKind::parse(""), MailKind::Message);
        assert_eq!(MailKind::parse("whatever"), MailKind::Message);
    }
}
