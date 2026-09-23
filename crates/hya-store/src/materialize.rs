//! Write-through materialization of team/mail side tables.
//!
//! `event_log` stays the single source of truth; these tables are queryable
//! projections maintained inside the same transaction as the event append:
//! `session_created` → `session`, `agent_registered` → `team_run` +
//! `team_member`, `mail_sent` → `mail`, `member_spawned`/`subagent_reported`
//! → `task_board`, assistant `message_started` / `message_finished` /
//! `message_deleted` → `open_assistant_message` (the crash-recovery index).
//! Reads keep folding the event log; nothing derives state from these rows.

use hya_proto::{Event, MailEndpoint, MailKind, MessageId, ReportOutcome, Role, SessionId};
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
        } => {
            sqlx::query(
                "INSERT OR IGNORE INTO session \
                 (id, parent_id, agent, model, workdir, title, permission, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, NULL, '{}', ?, ?)",
            )
            .bind(session.storage_key())
            .bind(parent.as_ref().map(SessionId::storage_key))
            .bind(agent.as_str())
            .bind(model.as_str())
            .bind(workdir)
            .bind(ts_millis)
            .bind(ts_millis)
            .execute(&mut **tx)
            .await?;
        }
        Event::AgentRegistered {
            session,
            agent_session,
            agent_type,
            ..
        } => {
            ensure_team_run(tx, root, root_key, ts_millis).await?;
            ensure_session_stub(tx, *agent_session, agent_type.as_str(), ts_millis).await?;
            sqlx::query(
                "INSERT OR IGNORE INTO team_member \
                 (id, team_id, session_id, background_task_id, role, state, created_at) \
                 VALUES (?, ?, ?, NULL, ?, 'active', ?)",
            )
            .bind(agent_session.storage_key())
            .bind(root_key)
            .bind(agent_session.storage_key())
            .bind(agent_type.as_str())
            .bind(ts_millis)
            .execute(&mut **tx)
            .await?;
            let _ = session;
        }
        Event::MailSent {
            from,
            to,
            kind,
            body,
            ..
        } => {
            ensure_team_run(tx, root, root_key, ts_millis).await?;
            let to_ep = match to {
                MailEndpoint::Channel(channel) => format!("#{channel}"),
                MailEndpoint::Handle(handle) => handle.clone(),
            };
            let kind_label = match kind {
                MailKind::Announcement => "announcement",
                MailKind::Message => "message",
            };
            let body_json = serde_json::to_string(body).map_err(|e| StoreError::Json(e.into()))?;
            let id = uuid::Uuid::now_v7().as_bytes().to_vec();
            sqlx::query(
                "INSERT INTO mail \
                 (id, team_id, from_ep, to_ep, kind, body_json, delivered_at, acked_at, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?)",
            )
            .bind(id)
            .bind(root_key)
            .bind(from)
            .bind(to_ep)
            .bind(kind_label)
            .bind(body_json)
            .bind(ts_millis)
            .bind(ts_millis)
            .execute(&mut **tx)
            .await?;
        }
        Event::MemberSpawned {
            member,
            child,
            description,
            ..
        } => {
            ensure_team_run(tx, root, root_key, ts_millis).await?;
            sqlx::query(
                "INSERT OR REPLACE INTO task_board \
                 (id, team_id, title, body, status, assignee, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, 'pending', ?, ?, ?)",
            )
            .bind(member.as_uuid().as_bytes().to_vec())
            .bind(root_key)
            .bind(description)
            .bind(description)
            .bind(child.as_ref().map(SessionId::storage_key))
            .bind(ts_millis)
            .bind(ts_millis)
            .execute(&mut **tx)
            .await?;
        }
        Event::SubagentReported {
            member, outcome, ..
        } => {
            let status = match outcome {
                ReportOutcome::Done => "done",
                ReportOutcome::Failed => "failed",
            };
            sqlx::query(
                "UPDATE task_board SET status = ?, updated_at = ? \
                 WHERE id = ? AND team_id = ?",
            )
            .bind(status)
            .bind(ts_millis)
            .bind(member.as_uuid().as_bytes().to_vec())
            .bind(root_key)
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
        Event::MessageFinished { message, .. } | Event::MessageDeleted { message, .. } => {
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

/// The `open_assistant_message.message_id` text: the id's JSON string form,
/// identical to what the migration backfill reads with `json_extract`.
pub(crate) fn open_message_key(message: MessageId) -> Result<String, StoreError> {
    match serde_json::to_value(message)? {
        serde_json::Value::String(key) => Ok(key),
        other => Ok(other.to_string()),
    }
}

/// The orchestration root's log session doubles as the team-run row; member,
/// task, and mail rows all reference it.
async fn ensure_team_run(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    root: SessionId,
    root_key: &[u8],
    ts_millis: i64,
) -> Result<(), StoreError> {
    // `session` rows are the FK anchor; a registration may arrive on a log
    // whose session_created predates this materialization (or never happened
    // in a synthetic sequence), so anchor a placeholder first. A later
    // authoritative `session_created` keeps its own row (INSERT OR IGNORE).
    ensure_session_stub(tx, root, "", ts_millis).await?;
    sqlx::query(
        "INSERT OR IGNORE INTO team_run (id, lead_session, spec_json, state, created_at) \
         VALUES (?, ?, '{}', 'active', ?)",
    )
    .bind(root_key)
    .bind(root_key)
    .bind(ts_millis)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Anchor a minimal `session` row so FK-dependent materializations succeed.
async fn ensure_session_stub(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    session: SessionId,
    agent_hint: &str,
    ts_millis: i64,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT OR IGNORE INTO session \
         (id, parent_id, agent, model, workdir, title, permission, created_at, updated_at) \
         VALUES (?, NULL, ?, '', '.', NULL, '{}', ?, ?)",
    )
    .bind(session.storage_key())
    .bind(agent_hint)
    .bind(ts_millis)
    .bind(ts_millis)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::{SessionStore, append_event_in_transaction};
    use hya_proto::{
        AgentName, Event, MailEndpoint, MailKind, MemberId, ModelRef, ReportOutcome, SubagentMode,
    };
    use sqlx::{Executor as _, Row};

    #[tokio::test]
    async fn team_mail_and_session_events_materialize_side_tables() {
        let store = SessionStore::connect_memory().await.unwrap();
        let root = SessionId::new();
        let child = SessionId::new();

        store
            .append_event(
                root,
                &Event::SessionCreated {
                    session: root,
                    parent: None,
                    agent: AgentName::new("build"),
                    model: ModelRef::new("fake"),
                    workdir: ".".into(),
                },
            )
            .await
            .unwrap();
        store
            .append_event(
                child,
                &Event::SessionCreated {
                    session: child,
                    parent: Some(root),
                    agent: AgentName::new("general"),
                    model: ModelRef::new("fake"),
                    workdir: ".".into(),
                },
            )
            .await
            .unwrap();

        let sessions: i64 = store
            .pool
            .fetch_one("SELECT count(*) AS n FROM session")
            .await
            .unwrap()
            .try_get("n")
            .unwrap();
        assert_eq!(sessions, 2, "session_created materializes session rows");

        let member = MemberId::new();
        let mut tx = store.pool.begin().await.unwrap();
        append_event_in_transaction(
            &mut tx,
            root,
            Event::MemberSpawned {
                session: root,
                member,
                child: Some(child),
                subagent_type: AgentName::new("general"),
                description: "research task".into(),
                depth: 1,
                directive: "go research".into(),
                tool_call: None,
            },
        )
        .await
        .unwrap();
        append_event_in_transaction(
            &mut tx,
            root,
            Event::AgentRegistered {
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
        append_event_in_transaction(
            &mut tx,
            root,
            Event::MailSent {
                session: root,
                from: "main/general-1".into(),
                to: MailEndpoint::Handle("main".into()),
                kind: MailKind::Message,
                body: "report from the field".into(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let team_runs: i64 = store
            .pool
            .fetch_one("SELECT count(*) AS n FROM team_run")
            .await
            .unwrap()
            .try_get("n")
            .unwrap();
        assert_eq!(team_runs, 1, "agent_registered ensures one team_run");
        let members: i64 = store
            .pool
            .fetch_one("SELECT count(*) AS n FROM team_member")
            .await
            .unwrap()
            .try_get("n")
            .unwrap();
        assert_eq!(members, 1, "agent_registered materializes team_member");
        let mail = store
            .pool
            .fetch_one(
                "SELECT from_ep, to_ep, kind, body_json FROM mail ORDER BY created_at LIMIT 1",
            )
            .await
            .unwrap();
        assert_eq!(
            mail.try_get::<String, _>("from_ep").unwrap(),
            "main/general-1"
        );
        assert_eq!(mail.try_get::<String, _>("to_ep").unwrap(), "main");
        assert_eq!(mail.try_get::<String, _>("kind").unwrap(), "message");
        assert!(
            mail.try_get::<String, _>("body_json")
                .unwrap()
                .contains("report from the field")
        );
        let task = store
            .pool
            .fetch_one("SELECT title, status FROM task_board")
            .await
            .unwrap();
        assert_eq!(task.try_get::<String, _>("title").unwrap(), "research task");
        assert_eq!(task.try_get::<String, _>("status").unwrap(), "pending");

        store
            .append_event(
                root,
                &Event::SubagentReported {
                    session: root,
                    member,
                    child,
                    handle: "main/general-1".into(),
                    outcome: ReportOutcome::Done,
                    report: "all done".into(),
                },
            )
            .await
            .unwrap();
        let status: String = store
            .pool
            .fetch_one("SELECT status FROM task_board")
            .await
            .unwrap()
            .try_get("status")
            .unwrap();
        assert_eq!(status, "done", "subagent_reported advances the task status");
    }
}
