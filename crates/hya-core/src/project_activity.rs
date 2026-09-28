//! Host implementation of the project activity read plane.
use hya_proto::now_millis;
use hya_store::SessionStore;
use hya_tool::{
    ProjectActivityFile, ProjectActivityRequestMsg, ProjectActivityResult, ProjectActivitySession,
};
use std::sync::Arc;

pub(crate) async fn serve(
    store: SessionStore,
    active: Arc<dyn Fn(hya_proto::SessionId) -> bool + Send + Sync>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<ProjectActivityRequestMsg>,
) {
    while let Some(ProjectActivityRequestMsg::Query {
        session,
        request,
        reply,
    }) = rx.recv().await
    {
        let result = query(&store, &active, session, request)
            .await
            .map_err(|error| error.to_string());
        let _ = reply.send(result);
    }
}

async fn query(
    store: &SessionStore,
    active: &Arc<dyn Fn(hya_proto::SessionId) -> bool + Send + Sync>,
    caller: hya_proto::SessionId,
    request: hya_tool::ProjectActivityRequest,
) -> Result<ProjectActivityResult, hya_store::StoreError> {
    let caller_projection = store.read_projection(caller).await?;
    let Some(project) = caller_projection.session.project else {
        return Ok(ProjectActivityResult {
            sessions: Vec::new(),
            files: Vec::new(),
            note: Some("This session has no Project; activity is empty.".to_string()),
        });
    };
    let since = request
        .since_ms
        .unwrap_or_else(|| now_millis().saturating_sub(2 * 60 * 60 * 1000));
    let limit = request.limit.unwrap_or(50).clamp(1, 200);
    let exclude = (!request.include_self).then_some(caller);
    let rows = store
        .list_sessions_in_since(project, since, limit, exclude)
        .await?;
    let mut sessions = Vec::with_capacity(rows.len());
    for row in rows {
        let root = lineage_root(store, row.session).await?;
        let projection = store.read_projection(row.session).await?;
        let relation = if row.session == caller {
            "self"
        } else if caller_projection.session.parent == Some(row.session) {
            "parent"
        } else if projection.session.parent == Some(caller) {
            "child"
        } else if projection.session.parent == caller_projection.session.parent
            && projection.session.parent.is_some()
        {
            "sibling"
        } else {
            "unrelated"
        };
        sessions.push(ProjectActivitySession {
            id: row.session.to_string(),
            agent: projection
                .session
                .agent
                .map_or_else(|| "unknown".to_string(), |v| v.to_string()),
            title: projection.session.title.unwrap_or_default(),
            status: if active(row.session) { "busy" } else { "idle" }.to_string(),
            last_activity_ms: row.updated_millis,
            workdir: projection.session.workdir.unwrap_or_default(),
            parent: projection.session.parent.map(|v| v.to_string()),
            lineage_root: root.to_string(),
            relation: relation.to_string(),
        });
    }
    let files = store
        .project_activity_files(project, since, limit, exclude)
        .await?
        .into_iter()
        .map(|file: hya_store::ProjectActivityFile| ProjectActivityFile {
            path: file.path,
            session: file.session.to_string(),
            last_changed_ms: file.changed_millis,
            change_kind: if file.created { "created" } else { "changed" }.to_string(),
        })
        .collect();
    Ok(ProjectActivityResult {
        sessions,
        files,
        note: Some("busy/idle reflects this daemon's live turn state.".to_string()),
    })
}

async fn lineage_root(
    store: &SessionStore,
    mut session: hya_proto::SessionId,
) -> Result<hya_proto::SessionId, hya_store::StoreError> {
    for _ in 0..64 {
        let projection = store.read_projection(session).await?;
        let Some(parent) = projection.session.parent else {
            return Ok(session);
        };
        session = parent;
    }
    Ok(session)
}
