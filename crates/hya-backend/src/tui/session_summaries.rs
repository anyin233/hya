use hya_proto::SessionId;
use hya_provider::ReasoningEffort;
use hya_store::SessionStore;

use super::controller::SessionSummary;
use super::history::{HistoryStore, SessionMeta, SessionModelSnapshot};
use crate::config::ModelEntry;

pub(super) async fn session_summaries_from_store(
    history: &HistoryStore,
    store: &SessionStore,
) -> Vec<SessionSummary> {
    let Ok(mut infos) = store.list_sessions().await else {
        return session_summaries(history);
    };
    if hydrate_missing_history_sessions(history, store, &infos).await
        && let Ok(refreshed) = store.list_sessions().await
    {
        infos = refreshed;
    }
    let mut summaries = Vec::new();
    for info in infos {
        let Ok(projection) = store.read_projection(info.session).await else {
            continue;
        };
        if hya_core::title::is_empty_unnamed_session(&projection) {
            continue;
        }
        let title = projection
            .session
            .title
            .unwrap_or_else(|| hya_core::title::fallback_title(info.started_millis));
        let model = projection
            .session
            .model
            .map(|model| model.to_string())
            .unwrap_or_default();
        let workdir = projection.session.workdir.unwrap_or_default();
        summaries.push(SessionSummary {
            id: info.session.to_string(),
            title,
            detail: format!("{model} · {workdir}"),
        });
    }
    if summaries.is_empty() {
        session_summaries(history)
    } else {
        summaries
    }
}

async fn hydrate_missing_history_sessions(
    history: &HistoryStore,
    store: &SessionStore,
    infos: &[hya_store::SessionInfo],
) -> bool {
    let Ok(history_sessions) = history.list_sessions() else {
        return false;
    };
    let existing = infos.iter().map(|info| info.session).collect::<Vec<_>>();
    let mut hydrated = false;
    for meta in history_sessions {
        let Ok(session) = meta.id.parse::<SessionId>() else {
            continue;
        };
        if existing.contains(&session) {
            continue;
        }
        if history.hydrate_store(store, session).await.is_ok() {
            hydrated = true;
        }
    }
    hydrated
}

pub(super) fn resume_session_id(id: &str) -> Option<SessionId> {
    id.parse().ok()
}

pub(super) fn session_summaries(history: &HistoryStore) -> Vec<SessionSummary> {
    history
        .list_sessions()
        .unwrap_or_default()
        .into_iter()
        .map(|meta| SessionSummary {
            id: meta.id,
            title: if meta.title == "Untitled session" && !meta.last_user_message.is_empty() {
                meta.last_user_message
            } else {
                meta.title
            },
            detail: format!("{} · {}", meta.model, meta.workdir),
        })
        .collect()
}

pub(super) fn meta_for(history: &HistoryStore, id: &str) -> Option<SessionMeta> {
    history
        .list_sessions()
        .ok()?
        .into_iter()
        .find(|meta| meta.id == id)
}

pub(super) fn update_session_model_snapshot(
    history: &HistoryStore,
    session: SessionId,
    entry: Option<&ModelEntry>,
    model: &str,
    reasoning: Option<ReasoningEffort>,
) {
    let _ = history.update_session_model_snapshot(
        session,
        SessionModelSnapshot {
            provider: entry.map(|entry| entry.provider.as_str()),
            model,
            reasoning,
        },
    );
}

#[cfg(test)]
#[path = "session_summaries_tests.rs"]
mod tests;
