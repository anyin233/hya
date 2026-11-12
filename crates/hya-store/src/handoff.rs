//! Durable restore prompts for sessions interrupted by a process stop.

use hya_proto::{
    Envelope, Event, FinishCause, FinishReason, MessageId, OwnerRunId, PartId, Role, SessionId,
    now_millis,
};
use sqlx::Row;

use crate::{
    SessionStore, StoreError, append_event_in_transaction, decode_session_key, replay_projection,
};

/// A restore prompt waiting for the next runtime owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingResume {
    /// Interrupted session.
    pub session: SessionId,
    /// Explains the interruption and names unfinished tool calls.
    pub prompt: String,
}

impl SessionStore {
    /// Cancel an open turn and queue its root's restore in the same transaction.
    /// A finished turn is left alone; children and archived roots get no restore.
    pub async fn cancel_turn_for_restart(
        &self,
        session: SessionId,
        message: MessageId,
        owner: OwnerRunId,
        prompt: &str,
    ) -> Result<Vec<Envelope>, StoreError> {
        self.require_runtime_owner(owner)?;
        self.warm_projection(session).await?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let mut projection = replay_projection(&self.projections, &mut tx, session).await?;
        projection
            .session
            .messages
            .retain(|entry| entry.id == message && entry.finish.is_none());
        if projection.session.messages.is_empty() {
            return Ok(Vec::new());
        }
        let events = crate::recovery::open_turn_terminal_events(
            session,
            &projection,
            "cancelled: the backend is stopping",
            "CANCELLED",
            Some(FinishCause::Shutdown),
        );
        let mut envelopes = Vec::with_capacity(events.len());
        for event in events {
            envelopes.push(append_event_in_transaction(&mut tx, session, event).await?);
        }
        if projection.session.parent.is_none() && !projection.session.is_archived() {
            sqlx::query("INSERT INTO pending_resume (session_id, prompt, created_at) VALUES (?, ?, ?) ON CONFLICT(session_id) DO UPDATE SET prompt = excluded.prompt, prompt_message_id = NULL, created_at = excluded.created_at")
                .bind(session.storage_key()).bind(prompt).bind(now_millis()).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(envelopes)
    }

    /// Discover pending restores in creation order without consuming them.
    pub async fn list_pending_resumes(&self) -> Result<Vec<PendingResume>, StoreError> {
        let rows = sqlx::query(
            "SELECT session_id, prompt FROM pending_resume ORDER BY created_at, session_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let key: Vec<u8> = row.try_get("session_id")?;
                Ok(PendingResume {
                    session: decode_session_key(&key)?,
                    prompt: row.try_get("prompt")?,
                })
            })
            .collect()
    }

    /// Append a pending prompt once, retaining the row until an assistant starts.
    /// Missing, archived and child sessions lose their restore without an append.
    pub async fn take_restart_resume(
        &self,
        session: SessionId,
        owner: OwnerRunId,
    ) -> Result<Option<Vec<Envelope>>, StoreError> {
        self.require_runtime_owner(owner)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query(
            "SELECT prompt, prompt_message_id FROM pending_resume WHERE session_id = ?",
        )
        .bind(session.storage_key())
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let projection = replay_projection(&self.projections, &mut tx, session).await?;
        if projection.session.id.is_none()
            || projection.session.parent.is_some()
            || projection.session.is_archived()
        {
            sqlx::query("DELETE FROM pending_resume WHERE session_id = ?")
                .bind(session.storage_key())
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            return Ok(None);
        }
        if row
            .try_get::<Option<Vec<u8>>, _>("prompt_message_id")?
            .is_some()
        {
            tx.commit().await?;
            return Ok(Some(Vec::new()));
        }
        let text: String = row.try_get("prompt")?;
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
        Ok(Some(envelopes))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    async fn interrupted(store: &SessionStore, owner: OwnerRunId) -> (SessionId, MessageId) {
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
            .cancel_turn_for_restart(session, message, owner, "restore")
            .await
            .unwrap();
        (session, message)
    }
    #[tokio::test]
    async fn failed_prompt_append_does_not_consume_the_restore() {
        let store = SessionStore::connect_memory().await.unwrap();
        let owner = OwnerRunId::new();
        store.claim_runtime_owner(owner).unwrap();
        let (session, _) = interrupted(&store, owner).await;
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
                .is_empty()
        );
    }

    #[tokio::test]
    async fn missing_session_restore_does_not_create_a_ghost() {
        let store = SessionStore::connect_memory().await.unwrap();
        let owner = OwnerRunId::new();
        store.claim_runtime_owner(owner).unwrap();
        let (session, _) = interrupted(&store, owner).await;
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
        let (session, _) = interrupted(&store, owner).await;
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
                .cancel_turn_for_restart(session, message, owner, "restore")
                .await
                .is_err()
        );
        let projection = store.read_projection(session).await.unwrap();
        assert_eq!(projection.session.messages.last().unwrap().finish, None);
    }
}
