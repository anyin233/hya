use std::path::PathBuf;

use hya_proto::{
    AgentName, Event, MessageId, ModelRef, PartId, ProjectId, Projection, SessionId, SessionKind,
};

use super::{AgentSpec, DRAIN_DEADLINE, SessionEngine};
use crate::error::CoreError;
use crate::hooks::SessionLifecycleInput;

impl SessionEngine {
    /// Record an agent switch event for the session.
    pub async fn switch_agent(
        &self,
        session: SessionId,
        agent: AgentName,
    ) -> Result<(), CoreError> {
        self.ensure_external_turn_allowed(session).await?;
        self.emit(session, Event::AgentSwitched { session, agent })
            .await
    }

    /// Record a model switch event for the session.
    pub async fn switch_model(&self, session: SessionId, model: ModelRef) -> Result<(), CoreError> {
        self.ensure_external_turn_allowed(session).await?;
        self.emit(session, Event::ModelSwitched { session, model })
            .await
    }

    /// Set the session title.
    pub async fn set_title(&self, session: SessionId, title: String) -> Result<(), CoreError> {
        self.emit(session, Event::SessionTitled { session, title })
            .await
    }

    /// Discard `session`'s recorded initial context (`/reload-context`):
    /// appends `InitialContextInvalidated`, so its next full-mode turn renders
    /// the environment, `AGENTS.md` files, and directory skill index from
    /// the files as they are then. Every round of a turn reads the recorded
    /// text, so this is refused while one runs. Returns `false` without
    /// appending when nothing is recorded yet (the first turn reads the
    /// files anyway).
    ///
    /// # Errors
    /// `CoreError::Invalid("session not found")` for an unknown session,
    /// `CoreError::Invalid("session busy")` while a turn of it runs, plus
    /// store failures.
    pub async fn refresh_initial_context(&self, session: SessionId) -> Result<bool, CoreError> {
        let projection = self.read_projection_shared(session).await?;
        if projection.session.id.is_none() {
            return Err(CoreError::Invalid("session not found".to_owned()));
        }
        if self.turn_active(session) {
            return Err(CoreError::Invalid("session busy".to_owned()));
        }
        if projection.session.initial_context.is_none() {
            return Ok(false);
        }
        self.emit(session, Event::InitialContextInvalidated { session })
            .await?;
        Ok(true)
    }

    /// Set arbitrary session metadata key/value.
    pub async fn set_metadata(
        &self,
        session: SessionId,
        metadata: serde_json::Value,
    ) -> Result<(), CoreError> {
        self.emit(session, Event::SessionMetadataSet { session, metadata })
            .await
    }

    /// Archive a root session: hide it from default session lists.
    ///
    /// Appends `SessionArchived` stamped with the current time and returns
    /// `true`, or returns `false` without appending when the session is
    /// already archived (the first stamp is kept). A running turn is not
    /// cancelled; it finishes on the archived session. Archiving is the
    /// session-close signal, so it notifies the `session.end` hook
    /// best-effort.
    ///
    /// # Errors
    /// `CoreError::Invalid` for an unknown session (`"session not found"`) or
    /// a subagent child session, plus store failures.
    pub async fn archive_session(&self, session: SessionId) -> Result<bool, CoreError> {
        if !self.set_session_archived(session, true).await? {
            return Ok(false);
        }
        if self.turn_active(session) {
            // The running turn still reads the session's captured bundle
            // hooks and channel policy: fire `session.end` without the
            // teardown that drops them.
            self.notify_session_end_keeping_state(session).await;
        } else {
            self.notify_session_lifecycle(session, false).await;
        }
        Ok(true)
    }

    /// Unarchive a session; returns whether `SessionUnarchived` was
    /// appended (`false` when it was not archived, including every subagent
    /// child). Notifies the `session.start` hook best-effort when it was.
    ///
    /// # Errors
    /// `CoreError::Invalid("session not found")` for an unknown session, plus
    /// store failures.
    pub async fn unarchive_session(&self, session: SessionId) -> Result<bool, CoreError> {
        let changed = self.set_session_archived(session, false).await?;
        if changed {
            self.notify_session_lifecycle(session, true).await;
        }
        Ok(changed)
    }

    /// Mark a root session ephemeral (`true`: the server deletes it once
    /// it is still unused and no client watches it) or keep it (`false`).
    /// Appends `SessionEphemeralSet` only when the mark changes; returns
    /// whether it did. Marking a subagent child, or a session that is already
    /// used (a message, a title, archived), ephemeral is a no-op: only unused
    /// root sessions are ephemeral.
    ///
    /// # Errors
    /// `CoreError::Invalid("session not found")` for an unknown session, plus
    /// store failures.
    pub async fn set_session_ephemeral(
        &self,
        session: SessionId,
        ephemeral: bool,
    ) -> Result<bool, CoreError> {
        let projection = self.read_projection_shared(session).await?;
        if projection.session.id.is_none() {
            return Err(CoreError::Invalid("session not found".to_owned()));
        }
        let used = !projection.session.messages.is_empty()
            || projection.session.revert.is_some()
            || projection.session.title.is_some()
            || projection.session.archived.is_some();
        if projection.session.ephemeral == ephemeral
            || (ephemeral && (used || projection.session.parent.is_some()))
        {
            return Ok(false);
        }
        self.emit(session, Event::SessionEphemeralSet { session, ephemeral })
            .await?;
        Ok(true)
    }

    async fn set_session_archived(
        &self,
        session: SessionId,
        archived: bool,
    ) -> Result<bool, CoreError> {
        let projection = self.read_projection_shared(session).await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            });
        let event = hya_proto::session_archive_event(&projection, archived, now)
            .map_err(|error| CoreError::Invalid(error.to_string()))?;
        match event {
            Some(event) => {
                self.emit(session, event).await?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Delete a part within a message.
    pub async fn delete_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::PartDeleted {
                session,
                message,
                part,
            },
        )
        .await
    }
}

/// Parameters for creating a new session event log.
pub struct CreateSession {
    /// Parent session for subagents; `None` for a root/interactive session.
    pub parent: Option<SessionId>,
    /// Agent name recorded on `SessionCreated`.
    pub agent: AgentName,
    /// Initial model for the session.
    pub model: ModelRef,
    /// Working directory string stored on the session.
    pub workdir: String,
    /// Project the root session belongs to (ADR-0024). Ignored for a
    /// subagent session, which always carries its parent's Project.
    pub project: Option<ProjectId>,
    /// Kind of the root session (ADR-0024). Ignored for a subagent session,
    /// which always carries its parent's kind. A temporary session never
    /// belongs to a Project.
    pub kind: SessionKind,
}

impl SessionEngine {
    /// Walk the `SessionCreated{parent}` chain to the top ancestor, returning the
    /// root session and this session's depth (0 = no parent / interactive lead,
    /// 1 = a direct subagent, and so on). Depth is derived from the replayed
    /// projection so there is no separate stored value that can drift. Bounded by a
    /// generous iteration cap as a cycle/runaway guard.
    pub async fn session_lineage(&self, session: SessionId) -> Result<(SessionId, u32), CoreError> {
        let mut current = session;
        let mut depth = 0u32;
        for _ in 0..1024 {
            let parent = self
                .store
                .with_projection(current, |projection| projection.session.parent)
                .await?;
            match parent {
                Some(parent) => {
                    current = parent;
                    depth = depth.saturating_add(1);
                }
                None => break,
            }
        }
        Ok((current, depth))
    }

    /// Project and kind recorded on a new session's `SessionCreated`
    /// (ADR-0024): a subagent session carries its parent's, whatever the
    /// spec says; a root session records the spec's own. A temporary root
    /// session naming a Project is refused.
    async fn session_project(
        &self,
        spec: &CreateSession,
    ) -> Result<(Option<ProjectId>, SessionKind), CoreError> {
        let Some(parent) = spec.parent else {
            if spec.kind == SessionKind::Temporary && spec.project.is_some() {
                return Err(CoreError::Invalid(
                    "a temporary session cannot belong to a project".to_string(),
                ));
            }
            return Ok((spec.project, spec.kind));
        };
        Ok(self
            .store
            .with_projection(parent, |projection| {
                (projection.session.project, projection.session.kind)
            })
            .await?)
    }

    /// Create a new session id and append `SessionCreated`.
    ///
    /// # Errors
    /// Returns store/append failures.
    pub async fn create(&self, spec: CreateSession) -> Result<SessionId, CoreError> {
        self.create_with_id(None, spec).await
    }

    /// Create a session under a resident actor claim (fenced append).
    ///
    /// # Errors
    /// Returns claim validation or store failures.
    /// Create or re-open a session with an optional fixed id (idempotent if log non-empty).
    ///
    /// # Errors
    /// Returns store/append failures.
    pub async fn create_with_id(
        &self,
        id: Option<SessionId>,
        spec: CreateSession,
    ) -> Result<SessionId, CoreError> {
        let id = id.unwrap_or_default();
        if !self.replay(id).await?.is_empty() {
            return Ok(id);
        }
        let is_root = spec.parent.is_none();
        let stable_agent_id = spec.agent.as_str().to_string();
        let workdir = PathBuf::from(&spec.workdir);
        let (project, kind) = self.session_project(&spec).await?;
        self.emit(
            id,
            Event::SessionCreated {
                session: id,
                parent: spec.parent,
                agent: spec.agent,
                model: spec.model,
                workdir: spec.workdir,
                project,
                kind,
            },
        )
        .await?;
        if is_root {
            match self.bind_session_runtime(id, &workdir).await {
                Ok(binding) => {
                    self.capture_session_bundle_hooks(id, &binding, &stable_agent_id)
                        .await;
                }
                Err(error) => {
                    tracing::warn!(session = %id, %error, "session.start bundle hook binding failed");
                }
            }
        }
        for hooks in self
            .hooks
            .iter()
            .cloned()
            .chain(self.runtime.global_plugin_hooks())
        {
            hooks
                .session_start(SessionLifecycleInput { session: id })
                .await;
        }
        Ok(id)
    }

    /// Refuse deletion of a registered member, whose identity remains revivable
    /// on its root even after archival. Delete the root to remove the whole team.
    ///
    /// # Errors
    /// Returns an invalid-operation error for members, or store failures.
    pub async fn ensure_session_delete_allowed(&self, session: SessionId) -> Result<(), CoreError> {
        let (root, depth) = self.session_lineage(session).await?;
        if depth > 0 && self.replay(root).await?.iter().any(|envelope| {
            matches!(envelope.event, Event::AgentRegistered { agent_session, .. } if agent_session == session)
        }) {
            return Err(CoreError::Invalid(
                "cannot delete a team member session; archive the subagent or delete the root".into(),
            ));
        }
        Ok(())
    }

    /// Delete a session log from the store.
    ///
    /// # Errors
    /// Returns store failures.
    pub async fn delete_session(&self, session: SessionId) -> Result<bool, CoreError> {
        self.ensure_session_delete_allowed(session).await?;
        // Root-turn teardown (ADR-0015 §1): deleting a team root force-archives
        // every live descendant (claims release, roster exits) before the log
        // goes away. A failed ending must not leave tasks writing a deleted log.
        if let Ok((root, 0)) = self.session_lineage(session).await {
            tokio::time::timeout(DRAIN_DEADLINE, self.force_archive_team(root)).await
                .map_err(|_| CoreError::Invalid("session deletion timed out waiting for resident endings; no sessions were deleted".into()))??;
        }
        let deleted = self.store.delete_session(session).await?;
        if deleted {
            if let Some(supervisor) = self.resident_supervisor() {
                supervisor.forget_wait_stalls(session);
            }
            self.notify_session_lifecycle(session, false).await;
        }
        Ok(deleted)
    }
}

pub(crate) fn projection_workdir(projection: &Projection) -> Option<PathBuf> {
    projection.session.workdir.as_ref().map(PathBuf::from)
}

/// The session's recorded workdir.
///
/// Every `SessionCreated` carries a workdir, so a projection of an existing
/// session always has one. `agent.workdir` is reached only for a projection
/// with no session at all (callers reject those first); it is the turn's
/// AgentSpec, never a process working directory (ADR-0024).
pub(crate) fn session_workdir(agent: &AgentSpec, projection: &Projection) -> PathBuf {
    projection
        .session
        .workdir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| agent.workdir.clone())
}
