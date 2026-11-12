//! Durable restore prompts for sessions interrupted by a process stop.

use hya_proto::{
    Envelope, Event, FinishCause, FinishReason, MessageId, OwnerRunId, PartId, PartProjection,
    Role, SessionId, ToolPartState,
};
use sqlx::Row;

use crate::{
    SessionStore, StoreError, append_event_in_transaction, decode_session_key, replay_projection,
};

/// A shutdown-interrupted message waiting for the next runtime owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingResume {
    /// Interrupted session.
    pub session: SessionId,
    /// Assistant message cancelled by shutdown.
    pub message: MessageId,
}

impl SessionStore {
    /// Discover pending restores in creation order without consuming them.
    pub async fn list_pending_resumes(&self) -> Result<Vec<PendingResume>, StoreError> {
        let rows = sqlx::query(
            "SELECT session_id, message_id FROM pending_resume ORDER BY created_at, session_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let key: Vec<u8> = row.try_get("session_id")?;
                Ok(PendingResume {
                    session: decode_session_key(&key)?,
                    message: row
                        .try_get::<String, _>("message_id")?
                        .parse()
                        .map_err(|error| sqlx::Error::Decode(Box::new(error)))?,
                })
            })
            .collect()
    }

    /// Append a pending prompt once, retaining the row until an assistant starts.
    /// Returns the prompt's id and the appended events (none when an earlier
    /// process already injected it). Missing messages, missing/archived/child
    /// sessions, and an injected prompt that is no longer visible (reverted)
    /// lose their restore.
    pub async fn take_restart_resume(
        &self,
        session: SessionId,
        owner: OwnerRunId,
    ) -> Result<Option<(MessageId, Vec<Envelope>)>, StoreError> {
        self.require_runtime_owner(owner)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT message_id, prompt_message_id FROM pending_resume WHERE session_id = ?",
        )
        .bind(session.storage_key())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let projection = replay_projection(&self.projections, &mut tx, session).await?;
        let interrupted: String = row.try_get("message_id")?;
        let injected = row
            .try_get::<Option<Vec<u8>>, _>("prompt_message_id")?
            .map(|bytes| {
                uuid::Uuid::from_slice(&bytes)
                    .map(MessageId::from_uuid)
                    .map_err(|error| sqlx::Error::Decode(Box::new(error)))
            })
            .transpose()?;
        let visible = |id: MessageId| projection.session.messages.iter().any(|m| m.id == id);
        let interrupted = projection
            .session
            .messages
            .iter()
            .find(|message| message.id.to_string() == interrupted)
            .filter(|_| {
                projection.session.id.is_some()
                    && projection.session.parent.is_none()
                    && !projection.session.is_archived()
                    && injected.is_none_or(visible)
            });
        let Some(interrupted) = interrupted else {
            sqlx::query("DELETE FROM pending_resume WHERE session_id = ?")
                .bind(session.storage_key())
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(None);
        };
        if let Some(prompt) = injected {
            tx.commit().await?;
            return Ok(Some((prompt, Vec::new())));
        }
        let unfinished = interrupted
            .parts
            .iter()
            .filter_map(|part| match part {
                PartProjection::Tool {
                    call, name, state, ..
                } if !matches!(state, ToolPartState::Completed { .. }) => {
                    Some(format!("- {name} (call {call})"))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let tools = if unfinished.is_empty() {
            "None.".to_string()
        } else {
            unfinished.join("\n")
        };
        let mut text = format!(
            "Your previous turn was interrupted by a restart.\n\nTool calls that did not complete successfully:\n{tools}\n\n\
             Unanswered permission and question requests were cancelled. Unfinished calls are not \
             successes; side effects may have happened before interruption. Review the existing \
             transcript and decide what to redo, then continue the task."
        );
        let candidates = projection
            .team
            .roster
            .values()
            .map(|entry| (&entry.handle, entry.session))
            .chain(
                projection
                    .team
                    .archived
                    .values()
                    .filter(|entry| entry.reason == hya_proto::ArchiveReason::Shutdown)
                    .map(|entry| (&entry.handle, entry.session)),
            );
        let mut children = Vec::new();
        for (handle, child) in candidates {
            if child == session {
                continue;
            }
            let child = replay_projection(&self.projections, &mut tx, child).await?;
            if child
                .session
                .messages
                .iter()
                .rev()
                .find(|message| message.role == Role::Assistant)
                .is_some_and(|message| {
                    message.finish == Some(FinishReason::Cancelled)
                        && message.cause == Some(FinishCause::Shutdown)
                })
            {
                children.push(handle.as_str());
            }
        }
        if !children.is_empty() {
            text.push_str(&format!(
                "\n\nInterrupted subagents: {}. Their recovery is managed by the supervisor.",
                children.join(", ")
            ));
        }
        let message = MessageId::new();
        let part = PartId::new();
        let events = [
            Event::MessageStarted {
                session,
                message,
                role: Role::User,
                agent: None,
                model: None,
            },
            Event::TextStart {
                session,
                message,
                part,
            },
            Event::TextDelta {
                session,
                message,
                part,
                delta: text,
            },
            Event::TextEnd {
                session,
                message,
                part,
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::User,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        ];
        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            envelopes.push(append_event_in_transaction(&mut tx, session, event).await?);
        }
        sqlx::query("UPDATE pending_resume SET prompt_message_id = ? WHERE session_id = ?")
            .bind(message.as_uuid().as_bytes().as_slice())
            .bind(session.storage_key())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some((message, envelopes)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    async fn interrupted(store: &SessionStore) -> (SessionId, MessageId) {
        let session = SessionId::new();
        store
            .append_event(
                session,
                &Event::SessionCreated {
                    session,
                    parent: None,
                    project: None,
                    kind: hya_proto::SessionKind::Project,
                    agent: hya_proto::AgentName::new("test"),
                    model: hya_proto::ModelRef::new("test/model"),
                    workdir: "/tmp".into(),
                },
            )
            .await
            .unwrap();
        let message = MessageId::new();
        store
            .append_event(
                session,
                &Event::MessageStarted {
                    session,
                    message,
                    role: Role::Assistant,
                    agent: None,
                    model: None,
                },
            )
            .await
            .unwrap();
        store
            .close_open_turns(session, FinishCause::Shutdown, "stopping")
            .await
            .unwrap();
        (session, message)
    }
    #[tokio::test]
    async fn failed_prompt_append_does_not_consume_the_restore() {
        let store = SessionStore::connect_memory().await.unwrap();
        let owner = OwnerRunId::new();
        store.claim_runtime_owner(owner).unwrap();
        let (session, _) = interrupted(&store).await;
        sqlx::query("CREATE TRIGGER fail_restore BEFORE INSERT ON event_log BEGIN SELECT RAISE(ABORT, 'injected append failure'); END")
        .execute(&store.pool).await.unwrap();
        assert!(store.take_restart_resume(session, owner).await.is_err());
        assert_eq!(store.list_pending_resumes().await.unwrap().len(), 1);
        sqlx::query("DROP TRIGGER fail_restore")
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            !store
                .take_restart_resume(session, owner)
                .await
                .unwrap()
                .unwrap()
                .1
                .is_empty()
        );
    }

    #[tokio::test]
    async fn missing_session_restore_does_not_create_a_ghost() {
        let store = SessionStore::connect_memory().await.unwrap();
        let owner = OwnerRunId::new();
        store.claim_runtime_owner(owner).unwrap();
        let (session, _) = interrupted(&store).await;
        sqlx::query("DELETE FROM event_log WHERE session_id = ?")
            .bind(session.storage_key())
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            store
                .take_restart_resume(session, owner)
                .await
                .unwrap()
                .is_none()
        );
        assert!(!store.session_exists(session).await.unwrap());
        assert!(store.list_pending_resumes().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn failed_restore_write_rolls_back_the_shutdown_finish() {
        let store = SessionStore::connect_memory().await.unwrap();
        let owner = OwnerRunId::new();
        store.claim_runtime_owner(owner).unwrap();
        let (session, _) = interrupted(&store).await;
        let message = MessageId::new();
        store
            .append_event(
                session,
                &Event::MessageStarted {
                    session,
                    message,
                    role: Role::Assistant,
                    agent: None,
                    model: None,
                },
            )
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER fail_resume BEFORE INSERT ON pending_resume BEGIN SELECT RAISE(ABORT, 'injected resume failure'); END").execute(&store.pool).await.unwrap();
        assert!(
            store
                .append_event(
                    session,
                    &Event::MessageFinished {
                        session,
                        message,
                        role: Role::Assistant,
                        finish: FinishReason::Cancelled,
                        tokens: None,
                        cause: Some(FinishCause::Shutdown),
                    }
                )
                .await
                .is_err()
        );
        let projection = store.read_projection(session).await.unwrap();
        assert_eq!(projection.session.messages.last().unwrap().finish, None);
    }
}
