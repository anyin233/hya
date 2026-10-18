//! User control of one subagent from a client (the TUI's Subagents selector):
//! archive it, or steer it with a message. Both act on the child's session id
//! and go through the same paths the parent agent uses — the `archive` tool
//! and a downward DM — so the supervisor's lifecycle rules apply unchanged.
//! Interrupting a subagent needs nothing here: `CancelTurn` on its session
//! cancels its in-flight turn through the turn gate.

use hya_proto::{MailEndpoint, MailKind, SessionId, scope};
use hya_tool::{ArchiveReceipt, MailReceipt};

use super::SessionEngine;
use crate::error::CoreError;
use crate::resident::resolve_member_target;

/// Prefix on a steer message, so the subagent (and its parent, reading the
/// DM history) can tell the user wrote it, not the parent agent.
pub const USER_STEER_PREFIX: &str = "[user] ";

impl SessionEngine {
    /// Archive the subagent running in `child`, as its team lead would with
    /// the `archive` tool: its in-flight turn is cancelled, live descendants
    /// archive first, and mail to its handle wakes it again later.
    ///
    /// # Errors
    /// [`CoreError::Invalid`] when `child` is not a subagent, is already
    /// archived, or this process does not supervise the team.
    pub async fn archive_subagent(&self, child: SessionId) -> Result<ArchiveReceipt, CoreError> {
        let root = self.subagent_root(child).await?;
        let reviver = self.archive_reviver().ok_or_else(|| {
            CoreError::Invalid("subagents are not supervised by this process".to_string())
        })?;
        reviver
            .archive_member(root, &child.to_string(), "requested by the user")
            .await
    }

    /// Steer the subagent running in `child` with `body`: delivered on its
    /// parent's DM, marked [`USER_STEER_PREFIX`]. A running subagent sees it
    /// at its next tool step, an idle one wakes for it, and an archived one is
    /// revived by it.
    ///
    /// # Errors
    /// [`CoreError::Invalid`] when `body` is empty, `child` is not a subagent
    /// of a live parent, or the mailbox rejects the delivery.
    pub async fn steer_subagent(
        &self,
        child: SessionId,
        body: &str,
    ) -> Result<MailReceipt, CoreError> {
        let body = body.trim();
        if body.is_empty() {
            return Err(CoreError::Invalid("a steer message needs text".to_string()));
        }
        let root = self.subagent_root(child).await?;
        let projection = self.read_projection(root).await?;
        let target =
            resolve_member_target(&projection, root, scope::ROOT_HANDLE, &child.to_string())?;
        let parent = scope::parent_path(&target.handle).unwrap_or(scope::ROOT_HANDLE);
        let parent_session = if parent == scope::ROOT_HANDLE {
            root
        } else {
            projection
                .team
                .roster
                .get(parent)
                .map(|entry| entry.session)
                .ok_or_else(|| {
                    CoreError::Invalid(format!(
                        "`{}`'s parent `{parent}` is archived; steer `{parent}` first",
                        target.handle
                    ))
                })?
        };
        self.mail_send_for_actor_with_policy(
            parent_session,
            MailEndpoint::Handle(target.handle),
            MailKind::Message,
            format!("{USER_STEER_PREFIX}{body}"),
            None,
            None,
        )
        .await
    }

    /// The team root of `child`, refusing a root session (it is no subagent).
    async fn subagent_root(&self, child: SessionId) -> Result<SessionId, CoreError> {
        let (root, depth) = self.session_lineage(child).await?;
        if depth == 0 {
            return Err(CoreError::Invalid(
                "this session is not a subagent; use CancelTurn or CreateTurn on it directly"
                    .to_string(),
            ));
        }
        Ok(root)
    }
}
