//! Durable restore prompts for sessions interrupted by a process stop.

use hya_proto::{
    Envelope, Event, FinishReason, MessageId, OwnerRunId, PartId, Role, SessionId, now_millis,
};
use sqlx::Row;

use crate::{SessionStore, StoreError, append_event_in_transaction, decode_session_key};

/// A restore prompt waiting for the next runtime owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingResume {
    /// Interrupted session.
    pub session: SessionId,
    /// Explains the interruption and names unfinished tool calls.
    pub prompt: String,
}

impl SessionStore {
    /// Queue one restore prompt per interrupted session. Requires the runtime owner.
    pub async fn queue_restart_resume(
        &self,
        session: SessionId,
        owner: OwnerRunId,
        prompt: &str,
    ) -> Result<(), StoreError> {
        self.require_runtime_owner(owner)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("INSERT INTO pending_resume (session_id, prompt, created_at) VALUES (?, ?, ?)")
            .bind(session.storage_key())
            .bind(prompt)
            .bind(now_millis())
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE pending_interaction SET resolved_at = ? WHERE session_id = ? AND resolved_at IS NULL")
            .bind(now_millis()).bind(session.storage_key()).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
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

    /// Consume a pending restore and append its user prompt in one transaction.
    /// A failed append rolls back the consume; a second call appends nothing.
    pub async fn take_restart_resume(
        &self,
        session: SessionId,
        owner: OwnerRunId,
    ) -> Result<Vec<Envelope>, StoreError> {
        self.require_runtime_owner(owner)?;
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let row = sqlx::query("DELETE FROM pending_resume WHERE session_id = ? RETURNING prompt")
            .bind(session.storage_key())
            .fetch_optional(&mut *tx)
            .await?;
        let Some(row) = row else {
            return Ok(Vec::new());
        };
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
        tx.commit().await?;
        Ok(envelopes)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn failed_prompt_append_does_not_consume_the_restore() {
        let store = SessionStore::connect_memory().await.unwrap();
        let owner = OwnerRunId::new();
        store.claim_runtime_owner(owner).unwrap();
        let session = SessionId::new();
        store
            .queue_restart_resume(session, owner, "restore")
            .await
            .unwrap();
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
                .is_empty()
        );
    }
}
