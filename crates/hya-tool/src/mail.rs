//! Receipt of a delivered team mail send (ADR-0001).

use hya_proto::MailEndpoint;

/// Outcome of a delivered send: the resolved sender handle, the address, and how
/// many inboxes it reached (1 for a handle, the subscriber count for a channel).
#[derive(Clone, Debug)]
pub struct MailReceipt {
    /// Resolved sender handle.
    pub from: String,
    /// Normalized recipient endpoint.
    pub to: MailEndpoint,
    /// Number of inboxes that received the mail.
    pub recipients: usize,
}
