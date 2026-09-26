use hya_proto::{AgentName, Event, MessageId, ModelRef, PartId, SessionId, ToolPartState};

use super::SessionEngine;
use crate::error::CoreError;

impl SessionEngine {
    /// Record an agent switch event for the session.
    pub async fn switch_agent(
        &self,
        session: SessionId,
        agent: AgentName,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::AgentSwitched {
                session,
                message: Some(MessageId::new()),
                agent,
            },
        )
        .await
    }

    /// Record a model switch event for the session.
    pub async fn switch_model(&self, session: SessionId, model: ModelRef) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::ModelSwitched {
                session,
                message: Some(MessageId::new()),
                model,
            },
        )
        .await
    }

    /// Set the session title.
    pub async fn set_title(&self, session: SessionId, title: String) -> Result<(), CoreError> {
        self.emit(session, Event::SessionTitled { session, title })
            .await
    }

    /// Update the session working directory.
    pub async fn set_workdir(&self, session: SessionId, workdir: String) -> Result<(), CoreError> {
        self.emit(session, Event::SessionMoved { session, workdir })
            .await
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

    /// Update session permission snapshot rules.
    pub async fn set_permission(
        &self,
        session: SessionId,
        permission: Vec<serde_json::Value>,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::SessionPermissionSet {
                session,
                permission,
            },
        )
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

    /// Record a share URL for the session.
    pub async fn set_share(&self, session: SessionId, url: String) -> Result<(), CoreError> {
        self.emit(session, Event::SessionShareSet { session, url })
            .await
    }

    /// Clear any share URL on the session.
    pub async fn clear_share(&self, session: SessionId) -> Result<(), CoreError> {
        self.emit(session, Event::SessionShareCleared { session })
            .await
    }

    /// Delete a message from the session projection path.
    pub async fn delete_message(
        &self,
        session: SessionId,
        message: MessageId,
    ) -> Result<(), CoreError> {
        self.emit(session, Event::MessageDeleted { session, message })
            .await
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

    /// Replace text content of a message part.
    pub async fn replace_text_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::TextReplace {
                session,
                message,
                part,
                text,
            },
        )
        .await
    }

    /// Replace reasoning content on a message part.
    pub async fn replace_reasoning_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        text: String,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::ReasoningReplace {
                session,
                message,
                part,
                text,
            },
        )
        .await
    }

    /// Update a tool part state (input/output/error) in the session.
    pub async fn update_tool_part(
        &self,
        session: SessionId,
        message: MessageId,
        part: PartId,
        state: ToolPartState,
    ) -> Result<(), CoreError> {
        self.emit(
            session,
            Event::ToolPartUpdated {
                session,
                message,
                part,
                state,
            },
        )
        .await
    }
}
