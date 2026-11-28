//! User control of one subagent from a client (the TUI's Subagents selector):
//! archive it, or steer it with a message. Both act on the child's session id
//! and go through the same paths the parent agent uses — the `archive` tool
//! and a downward DM — so the supervisor's lifecycle rules apply unchanged.
//! Interrupting a subagent needs nothing here: `CancelTurn` on its session
//! is [`SessionEngine::cancel_user_turn`], which stops its subtree.

use std::sync::Arc;

use hya_proto::{MailEndpoint, MailKind, SessionId, scope};
use hya_tool::{ArchiveReceipt, MailReceipt};

use super::SessionEngine;
use crate::error::CoreError;
use crate::resident::resolve_member_target;

/// Prefix on a steer message, so the subagent (and its parent, reading the
/// DM history) can tell the user wrote it, not the parent agent.
pub use hya_multi_agent::USER_STEER_PREFIX;

impl SessionEngine {
    /// Archive the subagent running in `child`, as its team lead would with
    /// the `archive` tool: its in-flight turn is cancelled, live descendants
    /// archive first, and mail on its parent DM wakes it again later.
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
            .archive_member_for_user(root, &child.to_string())
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
        let projection = self.read_projection_shared(root).await?;
        let target =
            resolve_member_target(&projection, root, scope::ROOT_HANDLE, &child.to_string())?;
        let parent = scope::parent_path(&target.handle).unwrap_or(scope::ROOT_HANDLE);
        let parent_session = if parent == scope::ROOT_HANDLE {
            root
        } else {
            hya_tool::extension::team_projection(&projection)
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
        drop(projection);
        // The shared send boundary resolves this handle to the persistent DM,
        // including when the child is archived. The receipt always names it.
        let receipt = self
            .mail_send_checked(
                parent_session,
                MailEndpoint::Handle(target.handle),
                MailKind::Message,
                format!("{USER_STEER_PREFIX}{body}"),
                None,
                None,
                true,
            )
            .await?;
        // The user acts on this subagent again: only a delivered steer lifts
        // a cancel's stop, which then lets the steer (and queued mail) wake it.
        self.resume_after_user_stop(child);
        Ok(receipt)
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

/// Episode lifecycle control for the engine's resident supervisor.
/// Mail revival calls the supervisor's transactional install path directly.
#[async_trait::async_trait]
pub trait ArchiveReviver: Send + Sync {
    /// End every episode under `root`, await its result, then reconcile orphan
    /// claims. The episode owners publish their own terminal events.
    async fn teardown_root(&self, root: SessionId) -> Result<(), CoreError>;

    /// Whether an episode owns this member's turns. Clients steer it with mail.
    fn has_episode(&self, _session: SessionId) -> bool {
        false
    }

    /// Serialize an orphan terminal write with registration and revival.
    /// A live episode must run its own ending instead.
    async fn reconcile_orphan(
        &self,
        root: SessionId,
        handle: &str,
        archive: crate::store_bridge::ResidentArchive,
    ) -> Result<crate::store_bridge::ResidentArchiveOutcome, CoreError> {
        let _ = (root, handle, archive);
        Err(CoreError::Invalid(
            "resident orphan has no supervisor".into(),
        ))
    }

    /// Archive `target` (a handle or session id) on behalf of `caller`, as
    /// the `archive` tool does: cancel its turn, archive live descendants
    /// first, then the target itself.
    ///
    /// # Errors
    /// [`CoreError::Invalid`] when the target cannot be archived by `caller`,
    /// or when this reviver does not supervise residents.
    async fn archive_member(
        &self,
        caller: SessionId,
        target: &str,
        reason: &str,
    ) -> Result<hya_tool::ArchiveReceipt, CoreError> {
        let _ = (caller, target, reason);
        Err(CoreError::Invalid(
            "subagents are not supervised by this process".to_string(),
        ))
    }

    /// Archive `target` (a handle or session id) on team `root` for the user,
    /// as [`archive_member`](Self::archive_member) does for the lead, but
    /// recorded as archived by the user, not by its parent.
    ///
    /// # Errors
    /// As [`archive_member`](Self::archive_member).
    async fn archive_member_for_user(
        &self,
        root: SessionId,
        target: &str,
    ) -> Result<hya_tool::ArchiveReceipt, CoreError> {
        self.archive_member(root, target, "requested by the user")
            .await
    }

    /// The user cancelled `session`'s turn: stop `session` and every live
    /// subagent below it, so the harness starts none of their turns (mail
    /// wakes, quiescence synthesis) until the user acts on them again.
    /// Returns the stopped sessions, `session` included when it is
    /// supervised; the caller cancels their in-flight turns.
    fn stop_for_user(&self, session: SessionId) -> Vec<SessionId> {
        let _ = session;
        Vec::new()
    }

    /// The user acted on `session` again (a prompt or a steer): lift a
    /// [`stop_for_user`](Self::stop_for_user) from it and its subtree.
    fn resume_for_user(&self, session: SessionId) {
        let _ = session;
    }

    /// Whether the user stopped the live member at `handle` on team `root`.
    fn stopped_by_user(&self, root: SessionId, handle: &str) -> bool {
        let _ = (root, handle);
        false
    }

    /// Whether the user stopped `session`'s own slot (see
    /// [`stop_for_user`](Self::stop_for_user)).
    fn session_stopped_by_user(&self, session: SessionId) -> bool {
        let _ = session;
        false
    }
}

impl SessionEngine {
    /// Wire the archive-revival seam on a shared engine. Idempotent; the
    /// resident supervisor calls this from its own start.
    pub fn set_reviver(&self, reviver: std::sync::Weak<crate::resident::ResidentSupervisor>) {
        match self.reviver.write() {
            Ok(mut slot) => *slot = Some(reviver),
            Err(poisoned) => *poisoned.into_inner() = Some(reviver),
        }
    }

    /// The installed archive reviver, if any.
    #[must_use]
    pub fn archive_reviver(&self) -> Option<Arc<dyn ArchiveReviver>> {
        self.resident_supervisor()
            .map(|supervisor| supervisor as Arc<dyn ArchiveReviver>)
    }

    pub(crate) fn resident_supervisor(&self) -> Option<Arc<crate::resident::ResidentSupervisor>> {
        self.reviver
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
    }
}
