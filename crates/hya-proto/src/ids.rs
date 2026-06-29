//! Newtype ids. A mis-passed id is a compile error, not a runtime bug.

use rand_core::{OsRng, RngCore, TryRngCore};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident, $prefix:literal) => {
        #[derive(
            Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Mint a fresh, time-ordered (v7) id.
            #[must_use]
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
            #[must_use]
            pub fn from_uuid(u: Uuid) -> Self {
                Self(u)
            }
            #[must_use]
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}_{}", $prefix, self.0.simple())
            }
        }

        impl std::str::FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let raw = s.strip_prefix(concat!($prefix, "_")).unwrap_or(s);
                Uuid::parse_str(raw).map(Self)
            }
        }
    };
}

/// Primary hya session id. New sessions use the `hysec_` grammar; legacy
/// UUID-backed ids (parsed from `ses_<uuid-simple>`, raw hyphenated UUIDs, or
/// UUID-simple strings) are preserved for read/decode compatibility.
///
/// `as_uuid()` returns `Option<Uuid>` (`None` for `hysec_` ids) so callers that
/// still assume a UUID are forced by the compiler — not a runtime panic — to
/// migrate to `storage_key()` / `Display` during the session-naming rollout.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SessionId(SessionIdRepr);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
enum SessionIdRepr {
    Hysec([u8; HYSEC_SUFFIX_LEN]),
    Uuid(Uuid),
}

pub const HYSEC_PREFIX: &str = "hysec_";
pub const HYSEC_SUFFIX_LEN: usize = 20;
const HYSEC_CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

impl SessionId {
    /// Mint a fresh `hysec_` session id backed by OS-provided randomness. The
    /// 20-char suffix is sampled uniformly over `[A-Za-z0-9]` using rejection
    /// sampling.
    #[must_use]
    pub fn new() -> Self {
        Self(SessionIdRepr::Hysec(generate_hysec_suffix()))
    }

    /// Construct a legacy UUID-backed session id. Kept for compatibility with
    /// existing callers; new sessions should go through [`SessionId::new`].
    #[must_use]
    pub fn from_uuid(u: Uuid) -> Self {
        Self(SessionIdRepr::Uuid(u))
    }

    /// Legacy UUID accessor. Returns `None` for `hysec_` ids because they have
    /// no UUID representation. Migrate call sites to `storage_key()` or
    /// `to_string()` instead of assuming a UUID is always present.
    #[must_use]
    pub fn as_uuid(&self) -> Option<Uuid> {
        match self.0 {
            SessionIdRepr::Uuid(u) => Some(u),
            SessionIdRepr::Hysec(_) => None,
        }
    }

    /// Durable storage key for the session. `hysec_` ids encode as their ASCII
    /// display bytes (including the prefix); legacy ids keep the 16 UUID bytes
    /// so existing SQLite rows remain addressable.
    #[must_use]
    pub fn storage_key(&self) -> Vec<u8> {
        match self.0 {
            SessionIdRepr::Hysec(suffix) => {
                let mut key = Vec::with_capacity(HYSEC_PREFIX.len() + HYSEC_SUFFIX_LEN);
                key.extend_from_slice(HYSEC_PREFIX.as_bytes());
                key.extend_from_slice(&suffix);
                key
            }
            SessionIdRepr::Uuid(u) => u.as_bytes().to_vec(),
        }
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            SessionIdRepr::Hysec(suffix) => {
                f.write_str(HYSEC_PREFIX)?;
                // Suffix bytes are validated ASCII-alphanumeric at construction.
                for &byte in &suffix {
                    write!(f, "{}", char::from(byte))?;
                }
                Ok(())
            }
            SessionIdRepr::Uuid(u) => write!(f, "ses_{}", u.simple()),
        }
    }
}

impl std::str::FromStr for SessionId {
    type Err = SessionIdParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(suffix) = s.strip_prefix(HYSEC_PREFIX) {
            if suffix.len() != HYSEC_SUFFIX_LEN {
                return Err(SessionIdParseError(format!(
                    "hysec_ session id suffix must be exactly {HYSEC_SUFFIX_LEN} chars, got {}",
                    suffix.len()
                )));
            }
            if !suffix.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return Err(SessionIdParseError(
                    "hysec_ session id suffix must be ASCII alphanumeric".to_string(),
                ));
            }
            let mut buf = [0u8; HYSEC_SUFFIX_LEN];
            buf.copy_from_slice(suffix.as_bytes());
            Ok(Self(SessionIdRepr::Hysec(buf)))
        } else if let Some(rest) = s.strip_prefix("ses_") {
            let u = Uuid::parse_str(rest)
                .map_err(|e| SessionIdParseError(format!("invalid legacy session id: {e}")))?;
            Ok(Self(SessionIdRepr::Uuid(u)))
        } else {
            let u = Uuid::parse_str(s)
                .map_err(|e| SessionIdParseError(format!("invalid session id: {e}")))?;
            Ok(Self(SessionIdRepr::Uuid(u)))
        }
    }
}

impl Serialize for SessionId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SessionId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        raw.parse::<Self>().map_err(serde::de::Error::custom)
    }
}

/// Error returned when parsing a [`SessionId`] from a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionIdParseError(pub String);

impl std::fmt::Display for SessionIdParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SessionIdParseError {}

/// Generate a uniformly-distributed 20-char `hysec_` suffix from OS-backed
/// randomness. Rejection sampling (rejecting `248..=255`) keeps the 62-symbol
/// charset unbiased.
fn generate_hysec_suffix() -> [u8; HYSEC_SUFFIX_LEN] {
    let mut out = [0u8; HYSEC_SUFFIX_LEN];
    let mut filled = 0usize;
    let mut entropy = [0u8; 32];
    let mut idx = entropy.len();
    while filled < HYSEC_SUFFIX_LEN {
        if idx >= entropy.len() {
            fill_os_random(&mut entropy);
            idx = 0;
        }
        let byte = entropy[idx];
        idx += 1;
        // Reject 248..=255 so the 62-symbol charset is sampled without bias.
        if byte < 248 {
            out[filled] = HYSEC_CHARSET[(byte as usize) % HYSEC_CHARSET.len()];
            filled += 1;
        }
    }
    out
}

fn fill_os_random(dest: &mut [u8]) {
    OsRng.unwrap_err().fill_bytes(dest);
}

uuid_id!(MessageId, "msg");
uuid_id!(PartId, "part");
uuid_id!(ToolCallId, "tc");
uuid_id!(TeamRunId, "team");
uuid_id!(MemberId, "mbr");
uuid_id!(GoalId, "goal");
uuid_id!(LoopRunId, "loop");
uuid_id!(PermissionRequestId, "perm");
uuid_id!(QuestionRequestId, "q");

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn session_id_parses_display_format() {
        let id = SessionId::new();
        let parsed: SessionId = id.to_string().parse().unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn session_id_new_uses_hysec_shape() {
        let id = SessionId::new().to_string();

        assert!(id.starts_with("hysec_"));
        let suffix = &id["hysec_".len()..];
        assert_eq!(suffix.len(), 20);
        assert!(suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    }

    #[test]
    fn session_id_default_uses_hysec_shape() {
        let id = SessionId::default().to_string();

        assert!(id.starts_with("hysec_"));
        let suffix = &id["hysec_".len()..];
        assert_eq!(suffix.len(), 20);
        assert!(suffix.bytes().all(|byte| byte.is_ascii_alphanumeric()));
    }

    #[test]
    fn session_id_parses_hysec_and_legacy_formats() {
        let parsed: SessionId = "hysec_ABCDEFGHIJKLMNOPQRST".parse().unwrap();
        assert_eq!(parsed.to_string(), "hysec_ABCDEFGHIJKLMNOPQRST");

        let uuid = Uuid::parse_str("018f032a-3d2f-7a21-a05c-2e61fc57dced").unwrap();
        let prefixed: SessionId = "ses_018f032a3d2f7a21a05c2e61fc57dced".parse().unwrap();
        let raw: SessionId = "018f032a-3d2f-7a21-a05c-2e61fc57dced".parse().unwrap();

        assert_eq!(prefixed, SessionId::from_uuid(uuid));
        assert_eq!(raw, SessionId::from_uuid(uuid));
    }

    #[test]
    fn session_id_rejects_invalid_hysec_formats() {
        assert!("ses_ABCDEFGHIJKLMNOPQRST".parse::<SessionId>().is_err());
        assert!("hysec_ABCDEFGHIJKLMNOPQRS".parse::<SessionId>().is_err());
        assert!("hysec_ABCDEFGHIJKLMNOPQRSTU".parse::<SessionId>().is_err());
        assert!("hysec_ABCDEFGHIJKLMNO!!!!".parse::<SessionId>().is_err());
    }

    #[test]
    fn session_id_serde_round_trips_hysec_display() {
        let id: SessionId = "hysec_ABCDEFGHIJKLMNOPQRST".parse().unwrap();

        let encoded = serde_json::to_string(&id).unwrap();
        let decoded: SessionId = serde_json::from_str(&encoded).unwrap();

        assert_eq!(encoded, "\"hysec_ABCDEFGHIJKLMNOPQRST\"");
        assert_eq!(decoded, id);
    }

    #[test]
    fn session_id_storage_key_uses_display_bytes_for_hysec() {
        let id: SessionId = "hysec_ABCDEFGHIJKLMNOPQRST".parse().unwrap();

        assert_eq!(id.storage_key(), b"hysec_ABCDEFGHIJKLMNOPQRST".to_vec());
    }
}

/// Monotonic per-session event sequence (the `event_log.seq` rowid).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventSeq(pub u64);
