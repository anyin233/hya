//! Write-through session placement and open-assistant-message recovery indexes.
//! Maintained in the event-append transaction; transcript/team/mail reads fold events.

use hya_proto::{Event, MessageId, Role, SessionId};
use sqlx::Sqlite;

use crate::StoreError;

/// Materialize the side-table rows for one appended event, on the event's own
/// transaction. Non-matching events are a cheap match away.
pub(crate) async fn materialize_event_side_tables(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    root: SessionId,
    event: &Event,
    ts_millis: i64,
) -> Result<(), StoreError> {
    let root_key = root.storage_key();
    let root_key = root_key.as_slice();
    match event {
        Event::SessionCreated {
            session,
            parent,
            agent,
            model,
            workdir,
            project,
            kind,
        } => {
            let project = project.as_ref().map(ToString::to_string);
            sqlx::query(
                "INSERT OR IGNORE INTO session \
                 (id, parent_id, agent, model, workdir, title, permission, created_at, updated_at, \
                  project_id, kind) \
                 VALUES (?, ?, ?, ?, ?, NULL, '{}', ?, ?, ?, ?)",
            )
            .bind(session.storage_key())
            .bind(parent.as_ref().map(SessionId::storage_key))
            .bind(agent.as_str())
            .bind(model.as_str())
            .bind(workdir)
            .bind(ts_millis)
            .bind(ts_millis)
            .bind(&project)
            .bind(kind.as_str())
            .execute(&mut **tx)
            .await?;
        }
        Event::MessageStarted {
            message,
            role: Role::Assistant,
            ..
        } => {
            sqlx::query(
                "INSERT OR IGNORE INTO open_assistant_message (session_id, message_id) \
                 VALUES (?, ?)",
            )
            .bind(root_key)
            .bind(open_message_key(*message)?)
            .execute(&mut **tx)
            .await?;
        }
        Event::MessageFinished { message, .. } => {
            sqlx::query(
                "DELETE FROM open_assistant_message WHERE session_id = ? AND message_id = ?",
            )
            .bind(root_key)
            .bind(open_message_key(*message)?)
            .execute(&mut **tx)
            .await?;
        }
        _ => {}
    }
    Ok(())
}

/// The `open_assistant_message.message_id` text: the id's JSON string form.
pub(crate) fn open_message_key(message: MessageId) -> Result<String, StoreError> {
    match serde_json::to_value(message)? {
        serde_json::Value::String(key) => Ok(key),
        other => Ok(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::SessionStore;
    use hya_proto::{AgentName, Event, ModelRef, SubagentMode};
    use sqlx::Row;

    /// Only `session_created` creates placement rows; registration creates no stubs.
    #[tokio::test]
    async fn session_created_materializes_project_and_kind() {
        let store = SessionStore::connect_memory().await.unwrap();
        let root = SessionId::new();
        let child = SessionId::new();
        let project = hya_proto::ProjectId::new();
        store
            .append_event(
                root,
                &Event::AgentRegistered {
                    session: root,
                    agent_session: child,
                    handle: "general-1".into(),
                    parent: Some("main".into()),
                    agent_type: AgentName::new("general"),
                    mode: SubagentMode::Resident,
                },
            )
            .await
            .unwrap();
        let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM session")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        assert_eq!(
            sessions, 0,
            "registration must not create placeholder sessions"
        );
        for (session, parent, kind) in [
            (root, None, hya_proto::SessionKind::Project),
            (child, Some(root), hya_proto::SessionKind::Project),
        ] {
            store
                .append_event(
                    session,
                    &Event::SessionCreated {
                        session,
                        parent,
                        agent: AgentName::new("build"),
                        model: ModelRef::new("fake"),
                        workdir: "/repo".into(),
                        project: Some(project),
                        kind,
                    },
                )
                .await
                .unwrap();
        }
        let temp = SessionId::new();
        store
            .append_event(
                temp,
                &Event::SessionCreated {
                    session: temp,
                    parent: None,
                    agent: AgentName::new("build"),
                    model: ModelRef::new("fake"),
                    workdir: "/scratch".into(),
                    project: None,
                    kind: hya_proto::SessionKind::Temporary,
                },
            )
            .await
            .unwrap();

        for (session, expected_project, expected_kind) in [
            (root, Some(project.to_string()), "project"),
            (child, Some(project.to_string()), "project"),
            (temp, None, "temporary"),
        ] {
            let row = sqlx::query("SELECT project_id, kind FROM session WHERE id = ?")
                .bind(session.storage_key())
                .fetch_one(&store.pool)
                .await
                .unwrap();
            assert_eq!(
                row.try_get::<Option<String>, _>("project_id").unwrap(),
                expected_project
            );
            assert_eq!(row.try_get::<String, _>("kind").unwrap(), expected_kind);
        }
    }
}
