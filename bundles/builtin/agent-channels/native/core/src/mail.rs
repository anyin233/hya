//! Mail address, channel id, preview, and channel history types for
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
