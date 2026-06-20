//! Newtype ids. A mis-passed id is a compile error, not a runtime bug.

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
    };
}

uuid_id!(SessionId, "ses");
uuid_id!(MessageId, "msg");
uuid_id!(PartId, "part");
uuid_id!(ToolCallId, "tc");
uuid_id!(TeamRunId, "team");
uuid_id!(MemberId, "mbr");
uuid_id!(GoalId, "goal");
uuid_id!(LoopRunId, "loop");
uuid_id!(PermissionRequestId, "perm");

/// Monotonic per-session event sequence (the `event_log.seq` rowid).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventSeq(pub u64);
