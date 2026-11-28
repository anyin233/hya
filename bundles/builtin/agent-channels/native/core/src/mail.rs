//! Mail address, channel id, preview, channel row, and channel history types for
//! `hya/agent-channels` (ADR-0001, ADR-0016, ADR-0039).

use hya_proto::Projection;
use hya_tool::extension::team_projection;

pub use hya_proto::mail::{
    CHANNEL_RANDOM_LEN, ChannelKind, ChannelPolicySnapshot, MailEndpoint, MailKind,
    is_minted_channel_id, mint_channel_id,
};

/// Maximum preview character length for mail bodies in steer notices and `wait`.
pub const MAIL_PREVIEW_CHARS: usize = 600;

/// Stable canonical handle for a team's root/main agent.
pub const MAIN_HANDLE: &str = hya_proto::scope::ROOT_HANDLE;

/// One `channel://` read result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelHistoryRead {
    /// Channel id that was read (after normalization).
    pub channel: String,
    /// Newest-first `(from, body)` pairs.
    pub messages: Vec<(String, String)>,
    /// Unread messages remaining in the caller's inbox after this read.
    pub unread_remaining: usize,
    /// Set when the caller's id spelling was normalized or resolved via a
    /// member-handle DM alias.
    pub warning: Option<String>,
}

/// One row of `list_channel`: what the acting agent can see. Group channels
/// never expose a member list (ADR-0016); DM channels always name their peer.
#[derive(Clone, Debug)]
pub struct ChannelRow {
    /// Channel id (`announce-{8}` / `DM-{8}`).
    pub id: String,
    /// Group broadcast pipe or DM pair.
    pub kind: ChannelKind,
    /// Whether the acting agent may post (group: unit leader only; dm: yes).
    pub can_speak: bool,
    /// DM peer identity; `None` for group channels.
    pub peer: Option<String>,
    /// Unread message count for the acting agent.
    pub unread: usize,
}

/// One team row of `list_channel`: a direct child's live status and harness
/// heartbeat freshness, so the parent can tell a busy child that is really
/// progressing (fresh heartbeat) from one that has stalled.
#[derive(Clone, Debug)]
pub struct MemberStatusRow {
    /// Canonical handle of the direct child.
    pub handle: String,
    /// Live roster status: `idle` / `busy` / `done` / `failed`.
    pub status: String,
    /// Seconds since the child's last harness-observed activity; `None` when
    /// no heartbeat was ever observed.
    pub last_active_seconds: Option<u64>,
}

/// One steered unread message: sender, body, and the channel it arrived through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SteeredMail {
    /// Sender canonical handle.
    pub from: String,
    /// Message body.
    pub body: String,
    /// Channel id the message was delivered through (`DM-{8}` / `announce-{8}`);
    /// `None` for harness notices.
    pub channel: Option<String>,
}

/// Bound a mail body to [`MAIL_PREVIEW_CHARS`] characters with a trailing `…`.
#[must_use]
pub fn mail_preview(text: &str) -> String {
    match text.char_indices().nth(MAIL_PREVIEW_CHARS) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// Find the minted DM channel id between `from` and `parent` in `projection`, if any.
#[must_use]
pub fn dm_channel_between(projection: &Projection, from: &str, parent: &str) -> Option<String> {
    team_projection(projection)
        .channels
        .iter()
        .find(|(_, channel)| {
            channel.kind == ChannelKind::Dm
                && channel.members.iter().any(|member| member == from)
                && channel.members.iter().any(|member| member == parent)
        })
        .map(|(channel, _)| channel.clone())
}
