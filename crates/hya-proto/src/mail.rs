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
/// `{"kind":"handle","id":"reviewer-lappland"}` / `{"kind":"channel","id":"build"}`.
/// Channel ids are stored without the leading `#`; the `#` is a UI/address
/// convention parsed at the tool boundary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum MailEndpoint {
    /// A single agent, addressed by its team-scoped handle (e.g. `reviewer-lappland`).
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
    /// Ordinary 1:1 or channel chatter (default when kind is omitted).
    #[default]
    Message,
    /// Channel-wide notice; render/policy may treat it as higher priority than chatter.
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

/// Scheduling-plane channel kinds (ADR-0016).
///
/// A minted id is its own canonical key (globally unique within a team root),
/// unlike legacy unit-qualified channel names.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    /// Leader-only broadcast pipe for one unit (`announce-{8}`).
    #[default]
    Group,
    /// Persistent two-member vertical pair channel (`DM-{8}`).
    Dm,
}

/// Length of the random suffix in a minted channel id.
pub const CHANNEL_RANDOM_LEN: usize = 8;

const GROUP_CHANNEL_PREFIX: &str = "announce-";
const DM_CHANNEL_PREFIX: &str = "DM-";

/// Alphabet for minted channel ids: 62 ASCII letters+digits, sampled without
/// bias by rejecting `248..=255` (same scheme as `hysec_` session suffixes).
const CHANNEL_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

/// Mint one channel id as an event fact (ADR-0016): `announce-{8}` for a unit
/// group channel, `DM-{8}` for a vertical pair channel. The id is stored
/// without the leading `#`; collisions are the caller's to re-mint against the
/// team-root channel table.
#[must_use]
pub fn mint_channel_id(kind: ChannelKind) -> String {
    let prefix = match kind {
        ChannelKind::Group => GROUP_CHANNEL_PREFIX,
        ChannelKind::Dm => DM_CHANNEL_PREFIX,
    };
    let mut id = String::with_capacity(prefix.len() + CHANNEL_RANDOM_LEN);
    id.push_str(prefix);
    let mut entropy = [0u8; 16];
    fill_os_random(&mut entropy);
    let mut idx = 0usize;
    while id.len() < prefix.len() + CHANNEL_RANDOM_LEN {
        if idx >= entropy.len() {
            fill_os_random(&mut entropy);
            idx = 0;
        }
        let byte = entropy[idx];
        idx += 1;
        if byte < 248 {
            id.push(CHANNEL_ALPHABET[(byte as usize) % CHANNEL_ALPHABET.len()] as char);
        }
    }
    id
}

/// Whether `key` is a minted channel id (`announce-{8}` / `DM-{8}`) and hence
/// its own canonical channel key — never unit-qualified, never joinable.
#[must_use]
pub fn is_minted_channel_id(key: &str) -> bool {
    let Some((prefix, suffix)) = key
        .strip_prefix(GROUP_CHANNEL_PREFIX)
        .map(|suffix| (GROUP_CHANNEL_PREFIX, suffix))
        .or_else(|| {
            key.strip_prefix(DM_CHANNEL_PREFIX)
                .map(|suffix| (DM_CHANNEL_PREFIX, suffix))
        })
    else {
        return false;
    };
    let _ = prefix;
    suffix.len() == CHANNEL_RANDOM_LEN && suffix.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn fill_os_random(dest: &mut [u8]) {
    use rand_core::{OsRng, RngCore, TryRngCore};
    OsRng.unwrap_err().fill_bytes(dest);
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

    /// Minted channel ids follow the plane spec (ADR-0016): `announce-{8}` /
    /// `DM-{8}` over `[A-Za-z0-9]`, effectively unique, and recognized as their
    /// own canonical keys.
    #[test]
    fn minted_channel_ids_match_the_plane_spec() {
        use std::collections::BTreeSet;
        for kind in [ChannelKind::Group, ChannelKind::Dm] {
            let prefix = match kind {
                ChannelKind::Group => "announce-",
                ChannelKind::Dm => "DM-",
            };
            let mut seen = BTreeSet::new();
            for _ in 0..64 {
                let id = mint_channel_id(kind);
                let suffix = id
                    .strip_prefix(prefix)
                    .unwrap_or_else(|| panic!("minted id {id} must carry the {prefix} prefix"));
                assert_eq!(suffix.len(), CHANNEL_RANDOM_LEN, "suffix length in {id}");
                assert!(
                    suffix.bytes().all(|b| b.is_ascii_alphanumeric()),
                    "suffix charset in {id}"
                );
                assert!(is_minted_channel_id(&id), "self-recognition of {id}");
                seen.insert(id);
            }
            assert_eq!(seen.len(), 64, "minted ids must be effectively unique");
        }
        assert!(!is_minted_channel_id("announce-short"), "too short");
        assert!(!is_minted_channel_id("announce-aB12Cd34x"), "too long");
        assert!(!is_minted_channel_id("announce-aB1!Cd34"), "bad charset");
        assert!(!is_minted_channel_id("build"), "legacy name");
        assert!(
            !is_minted_channel_id("main#announce-aB12Cd34"),
            "qualified key"
        );
    }

    /// Wire form is snake_case; a missing `kind` on an old log defaults to the
    /// group interpretation, which is what legacy unit channels were.
    #[test]
    fn channel_kind_round_trips_snake_case_and_defaults_to_group() {
        assert_eq!(
            serde_json::to_string(&ChannelKind::Group).unwrap(),
            "\"group\""
        );
        assert_eq!(serde_json::to_string(&ChannelKind::Dm).unwrap(), "\"dm\"");
        let back: ChannelKind = serde_json::from_str("\"dm\"").unwrap();
        assert_eq!(back, ChannelKind::Dm);
        assert_eq!(ChannelKind::default(), ChannelKind::Group);
    }
}
