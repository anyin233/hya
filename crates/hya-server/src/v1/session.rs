//! `/v1` session domain: lifecycle, fork, compact, summarize, revert.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::routing::{get, post};

use super::Json;

use crate::ServerState;
use hya_api::v1 as pb;
use hya_core::CreateSession;
use hya_proto::SessionId;

use super::V1Error;
use super::convert::session_info;

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/sessions", get(list_sessions).post(create_session))
        .route(
            "/v1/sessions/:id",
            get(get_session)
                .patch(axum::routing::patch(update_session))
                .delete(delete_session),
        )
        .route("/v1/sessions/:id/fork", post(fork_session))
        .route("/v1/sessions/:id/compact", post(compact_session))
        .route("/v1/sessions/:id/summarize", post(summarize_session))
        .route("/v1/sessions/:id/revert", post(revert_session))
}

pub(crate) fn parse_session(id: &str) -> Result<SessionId, V1Error> {
    id.parse::<SessionId>()
        .map_err(|_| V1Error::invalid_argument(format!("invalid session id: {id}")))
}

async fn session_exists(st: &ServerState, session: SessionId) -> Result<(), V1Error> {
    if st.engine.session_exists(session).await? {
        Ok(())
    } else {
        Err(V1Error::session_not_found(&session.to_string()))
    }
}

async fn create_session(
    State(st): State<ServerState>,
    Json(request): Json<pb::CreateSessionRequest>,
) -> Result<Json<pb::CreateSessionResponse>, V1Error> {
    if request.agent.trim().is_empty() {
        return Err(V1Error::invalid_argument("agent is required"));
    }
    if request.model.trim().is_empty() {
        return Err(V1Error::invalid_argument("model is required"));
    }
    let placement = session_placement(&st, &request).await?;
    let agent = crate::support::bound_agent_metadata::resolve_session_agent(
        &st,
        std::path::Path::new(&placement.workdir),
        Some(request.agent.as_str()),
    )
    .await
    .map_err(|error| V1Error::new(hya_api::error::Code::Internal, error.text().to_owned()))?;
    let session = st
        .engine
        .create_with_id(
            Some(placement.session),
            CreateSession {
                parent: placement.parent,
                agent,
                model: hya_proto::ModelRef::new(request.model.clone()),
                workdir: placement.workdir,
                project: placement.project,
                kind: placement.kind,
            },
        )
        .await?;
    if !request.title.is_empty() {
        st.engine.set_title(session, request.title.clone()).await?;
    }
    if placement.parent.is_none() && placement.project.is_some() {
        st.notify_projects_updated();
    }
    let info = projection_info(&st, session).await?;
    Ok(Json(pb::CreateSessionResponse {
        session: Some(info),
    }))
}

/// Where a new session works: its id (chosen up front so a temporary
/// session's scratch directory can be named after it), parent, workdir,
/// Project, and kind.
struct Placement {
    session: SessionId,
    parent: Option<SessionId>,
    workdir: String,
    project: Option<hya_proto::ProjectId>,
    kind: hya_proto::SessionKind,
}

/// Apply the `CreateSessionRequest` placement rules (ADR-0024; see the
/// message comment in `session.proto`).
async fn session_placement(
    st: &ServerState,
    request: &pb::CreateSessionRequest,
) -> Result<Placement, V1Error> {
    let session = SessionId::new();
    let kind = pb::SessionKind::try_from(request.kind).map_err(|_| {
        V1Error::invalid_argument(format!("unknown session kind: {}", request.kind))
    })?;
    let workdir = request
        .workdir
        .as_deref()
        .map(str::trim)
        .filter(|workdir| !workdir.is_empty());
    let project_id = request.project_id.trim();
    if !request.parent.trim().is_empty() {
        let parent = parse_session(request.parent.trim())?;
        if !project_id.is_empty() || kind != pb::SessionKind::Unspecified {
            return Err(V1Error::invalid_argument(
                "a child session joins its parent's project: leave projectId and kind unset",
            ));
        }
        let workdir = match workdir {
            Some(workdir) => hya_store::normalize_project_path(workdir)?,
            None => st
                .engine
                .read_projection_shared(parent)
                .await?
                .session
                .workdir
                .clone()
                .ok_or_else(|| V1Error::session_not_found(&parent.to_string()))?,
        };
        // The engine records the parent's Project and kind on the child.
        return Ok(Placement {
            session,
            parent: Some(parent),
            workdir,
            project: None,
            kind: hya_proto::SessionKind::Project,
        });
    }
    if kind == pb::SessionKind::Temporary {
        if !project_id.is_empty() || workdir.is_some() {
            return Err(V1Error::invalid_argument(
                "a temporary session has no project and no workdir: the server creates its scratch directory",
            ));
        }
        let workdir = create_scratch_dir(st, session)?;
        return Ok(Placement {
            session,
            parent: None,
            workdir,
            project: None,
            kind: hya_proto::SessionKind::Temporary,
        });
    }
    if !project_id.is_empty() {
        let project =
            super::project::load_project(st, super::project::parse_project(project_id)?).await?;
        let workdir = match workdir {
            Some(workdir) => {
                let workdir = hya_store::normalize_project_path(workdir)?;
                let inside = project
                    .roots
                    .iter()
                    .any(|root| std::path::Path::new(&workdir).starts_with(root));
                if !inside {
                    return Err(V1Error::invalid_argument(format!(
                        "workdir {workdir} is outside the roots of project {}",
                        project.id
                    )));
                }
                workdir
            }
            None => project
                .roots
                .first()
                .cloned()
                .ok_or_else(|| V1Error::internal("project has no roots"))?,
        };
        return Ok(Placement {
            session,
            parent: None,
            workdir,
            project: Some(project.id),
            kind: hya_proto::SessionKind::Project,
        });
    }
    let Some(workdir) = workdir else {
        return Err(V1Error::invalid_argument(
            "a session needs a projectId, a workdir, or kind SESSION_KIND_TEMPORARY",
        ));
    };
    let workdir = hya_store::normalize_project_path(workdir)?;
    let (project, _created) = super::project::ensure_project_for_path(st, &workdir).await?;
    Ok(Placement {
        session,
        parent: None,
        workdir,
        project: Some(project.id),
        kind: hya_proto::SessionKind::Project,
    })
}

/// Create a temporary session's scratch directory `<scratch root>/<id>`,
/// private to the user (0700 on Unix). hya never deletes it (ADR-0024).
fn create_scratch_dir(st: &ServerState, session: SessionId) -> Result<String, V1Error> {
    let Some(root) = &st.scratch_root else {
        return Err(V1Error::unavailable(
            "temporary sessions need XDG_CACHE_HOME or HOME for their scratch directory",
        ));
    };
    let dir = root.join(session.to_string());
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(&dir).map_err(|error| {
        V1Error::internal(format!(
            "create scratch directory {}: {error}",
            dir.display()
        ))
    })?;
    dir.to_str().map(str::to_owned).ok_or_else(|| {
        V1Error::internal(format!(
            "scratch directory {} is not valid UTF-8",
            dir.display()
        ))
    })
}

/// Read one session's projection summary with store timestamps.
async fn projection_info(st: &ServerState, session: SessionId) -> Result<pb::SessionInfo, V1Error> {
    let (started, updated) = st
        .engine
        .store()
        .session_info(session)
        .await?
        .map(|row| (row.started_millis, row.updated_millis))
        .unwrap_or((0, 0));
    projection_info_at(st, session, started, updated).await
}

/// `SessionInfo` from the cached projection and already-known log bounds.
async fn projection_info_at(
    st: &ServerState,
    session: SessionId,
    started: i64,
    updated: i64,
) -> Result<pb::SessionInfo, V1Error> {
    let projection = st.engine.read_projection_shared(session).await?;
    let mut info = session_info(&projection, started, updated);
    info.busy = st.is_busy(session);
    info.permission_mode = st.engine.permission_mode(session).await?;
    Ok(info)
}

async fn get_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::SessionInfo>, V1Error> {
    let session = parse_session(&id)?;
    session_exists(&st, session).await?;
    Ok(Json(projection_info(&st, session).await?))
}

async fn list_sessions(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Result<Json<pb::ListSessionsResponse>, V1Error> {
    let request: pb::ListSessionsRequest = super::query_request(&[], &query)?;
    let project = match request.project_id.trim() {
        "" => None,
        id => Some(super::project::parse_project(id)?),
    };
    let rows = st.engine.store().list_sessions_in(project).await?;
    let mut infos = Vec::with_capacity(rows.len());
    for row in rows {
        if !request.parent.is_empty()
            && let Ok(parent) = parse_session(&request.parent)
        {
            let projection = st.engine.read_projection_shared(row.session).await?;
            let is_child = projection.session.parent == Some(parent);
            if !is_child {
                continue;
            }
        }
        // The list query already carries each log's bounds; re-listing every
        // session per row made this O(sessions x events).
        let info =
            projection_info_at(&st, row.session, row.started_millis, row.updated_millis).await?;
        // Archived root sessions are hidden unless asked for.
        let listed = if request.archived_only {
            info.archived
        } else {
            request.include_archived || !info.archived
        };
        if listed {
            infos.push(info);
        }
    }
    infos.reverse();
    let (sessions, page) = super::catalog::paginate(infos, &request.page);
    Ok(Json(pb::ListSessionsResponse {
        sessions,
        page: Some(page),
    }))
}

async fn update_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<pb::UpdateSessionRequest>,
) -> Result<Json<pb::SessionInfo>, V1Error> {
    let session = parse_session(&id)?;
    session_exists(&st, session).await?;
    // Validate the mode before applying any other field, so an unknown mode
    // leaves the session unchanged.
    if let Some(mode) = request.permission_mode.as_deref()
        && hya_core::SessionPermissionMode::parse(mode).is_none()
    {
        return Err(V1Error::invalid_argument(format!(
            "unknown permission mode: {mode:?}"
        )));
    }
    // Only root sessions are archived: reject a child before any write.
    if request.archived == Some(true)
        && st
            .engine
            .read_projection_shared(session)
            .await?
            .session
            .parent
            .is_some()
    {
        return Err(V1Error::invalid_argument(
            hya_proto::SessionArchiveError::NotRoot.to_string(),
        ));
    }
    if let Some(title) = request.title
        && !title.is_empty()
    {
        st.engine.set_title(session, title).await?;
    }
    if let Some(model) = request.model
        && !model.is_empty()
    {
        st.engine
            .switch_model(session, hya_proto::ModelRef::new(model))
            .await?;
    }
    if let Some(agent) = request.agent
        && !agent.is_empty()
    {
        st.engine
            .switch_agent(session, hya_proto::AgentName::new(agent))
            .await?;
    }
    if let Some(mode) = request.permission_mode {
        let root = st
            .engine
            .set_permission_mode(session, &mode)
            .await
            .map_err(|error| match error {
                hya_core::CoreError::Invalid(message) => V1Error::invalid_argument(message),
                other => V1Error::from(other),
            })?;
        if mode == hya_core::permission_mode::YOLO {
            // The tree now bypasses every check: let the calls already
            // waiting for an answer continue as if allowed once.
            st.permission_requests.allow_tree_once(root).await;
        }
    }
    match request.archived {
        Some(true) => {
            st.engine
                .archive_session(session)
                .await
                .map_err(archive_error)?;
        }
        Some(false) => {
            st.engine
                .unarchive_session(session)
                .await
                .map_err(archive_error)?;
        }
        None => {}
    }
    if request.archived.is_some() {
        // A Project's `busy` counts only non-archived sessions.
        st.notify_projects_updated();
    }
    Ok(Json(projection_info(&st, session).await?))
}

fn archive_error(error: hya_core::CoreError) -> V1Error {
    match error {
        hya_core::CoreError::Invalid(message) => V1Error::invalid_argument(message),
        other => V1Error::from(other),
    }
}

/// Unarchive `session` because a new prompt, command, or shell turn was
/// admitted on it (appends `SessionUnarchived` only when it was archived).
pub(crate) async fn unarchive_for_turn(
    st: &ServerState,
    session: SessionId,
) -> Result<(), V1Error> {
    st.engine.unarchive_session(session).await?;
    Ok(())
}

async fn delete_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::DeleteSessionResponse>, V1Error> {
    let session = parse_session(&id)?;
    let in_project = st
        .engine
        .read_projection_shared(session)
        .await?
        .session
        .project
        .is_some();
    let deleted = st.engine.store().delete_session(session).await?;
    if !deleted {
        return Err(V1Error::session_not_found(&id));
    }
    if in_project {
        st.notify_projects_updated();
    }
    Ok(Json(pb::DeleteSessionResponse {}))
}

async fn fork_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<pb::ForkSessionRequest>,
) -> Result<Json<pb::ForkSessionResponse>, V1Error> {
    let source = parse_session(&id)?;
    let envs = st.engine.replay(source).await?;
    if envs.is_empty() {
        return Err(V1Error::session_not_found(&id));
    }
    let projection = hya_proto::Projection::from_events(&envs);
    let at = if !request.message_id.is_empty() {
        hya_core::ForkAt::Message(parse_message(&request.message_id)?)
    } else if request.until_seq > 0 {
        hya_core::ForkAt::UntilSeq(request.until_seq)
    } else {
        hya_core::ForkAt::Head
    };
    let before = hya_core::fork_cut(&envs, &projection, at).map_err(|error| match error {
        hya_core::ForkError::MessageNotFound(_) => {
            V1Error::new(hya_api::error::Code::NotFound, error.to_string())
        }
        hya_core::ForkError::NotUserMessage(_) => V1Error::invalid_argument(error.to_string()),
    })?;
    let prompt_text = match at {
        hya_core::ForkAt::Message(cut) => projection
            .session
            .messages
            .iter()
            .find(|message| message.id == cut)
            .map(|message| super::convert::message_text(&message.parts))
            .unwrap_or_default(),
        _ => String::new(),
    };
    let target = st
        .engine
        .create(CreateSession {
            parent: None,
            agent: projection
                .session
                .agent
                .clone()
                .unwrap_or_else(|| st.agent.name.clone()),
            model: projection
                .session
                .model
                .clone()
                .unwrap_or_else(|| st.agent.model.clone()),
            // Every session records its workdir; the fork works there too.
            workdir: projection
                .session
                .workdir
                .clone()
                .ok_or_else(|| V1Error::session_not_found(&source.to_string()))?,
            // A fork stays in its source's Project and keeps its kind.
            project: projection.session.project,
            kind: projection.session.kind,
        })
        .await?;
    st.engine
        .record_session_forked(target, source, before)
        .await?;
    st.engine
        .set_title(target, fork_title(&projection, source))
        .await?;
    if let Some(metadata) = projection.session.metadata.clone() {
        st.engine.set_metadata(target, metadata).await?;
    }
    st.engine
        .copy_messages_to_session(target, &projection, before)
        .await?;
    Ok(Json(pb::ForkSessionResponse {
        session: Some(projection_info(&st, target).await?),
        prompt_text,
    }))
}

/// Title of a fork: `<source title> (fork)`, the source id standing in for
/// a missing or default title. The suffix is not stacked on a fork of a
/// fork. Not a default title, so automatic titling never renames the fork.
fn fork_title(source: &hya_proto::Projection, id: hya_proto::SessionId) -> String {
    const SUFFIX: &str = " (fork)";
    let title = source
        .session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|title| !title.is_empty() && !hya_core::title::is_default_or_fallback_title(title));
    match title {
        Some(title) if title.ends_with(SUFFIX) => title.to_owned(),
        Some(title) => format!("{title}{SUFFIX}"),
        None => format!("{id}{SUFFIX}"),
    }
}

fn parse_message(id: &str) -> Result<hya_proto::MessageId, V1Error> {
    id.parse::<hya_proto::MessageId>()
        .map_err(|_| V1Error::invalid_argument(format!("invalid message id: {id}")))
}

async fn compact_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    // `until_seq` is deprecated and ignored: a manual compaction always folds
    // the whole transcript at the head (see `CompactSessionRequest`).
    Json(_request): Json<pb::CompactSessionRequest>,
) -> Result<Json<pb::CompactSessionResponse>, V1Error> {
    let session = parse_session(&id)?;
    match st.engine.summarize_session(session).await {
        Ok(_) => {
            let projection = st.engine.read_projection(session).await?;
            Ok(Json(pb::CompactSessionResponse {
                compacted_until_seq: projection.last_seq,
                // `summarize_session` records a local-summarizer fold.
                strategy: hya_proto::CompactionStrategy::LocalSummarizer
                    .as_str()
                    .to_owned(),
            }))
        }
        Err(hya_core::CoreError::Invalid(message)) if message == "summarizer not configured" => {
            Err(V1Error::unavailable(
                "compaction summarizer is not configured",
            ))
        }
        Err(hya_core::CoreError::Invalid(message)) if message == "session not found" => {
            Err(V1Error::session_not_found(&id))
        }
        Err(other) => Err(V1Error::from(other)),
    }
}

async fn summarize_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::SummarizeSessionResponse>, V1Error> {
    let session = parse_session(&id)?;
    match st.engine.summarize_session(session).await {
        Ok(summary) => Ok(Json(pb::SummarizeSessionResponse {
            summary_message: summary.to_string(),
        })),
        Err(hya_core::CoreError::Invalid(message)) if message == "session not found" => {
            Err(V1Error::session_not_found(&id))
        }
        Err(hya_core::CoreError::Invalid(message)) if message == "summarizer not configured" => {
            Err(V1Error::unavailable("summarizer is not configured"))
        }
        Err(other) => Err(V1Error::from(other)),
    }
}

async fn revert_session(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<pb::RevertSessionRequest>,
) -> Result<Json<pb::RevertSessionResponse>, V1Error> {
    let session = parse_session(&id)?;
    session_exists(&st, session).await?;
    if !request.undo && request.until_seq != 0 {
        return Err(V1Error::invalid_argument(
            "untilSeq is not supported by RevertSession; pass messageId",
        ));
    }
    let target = if request.message_id.is_empty() {
        hya_core::RevertTarget::LastUserMessage
    } else {
        hya_core::RevertTarget::Message(parse_message(&request.message_id)?)
    };
    if st.is_busy(session) {
        return Err(V1Error::session_busy());
    }
    // Hold the admission slot so no prompt is admitted while files and the
    // transcript change; the engine also holds the turn lease.
    let run = st.start_run(session).ok_or_else(V1Error::session_busy)?;
    let files = if request.undo {
        st.engine.unrevert_session(session).await
    } else {
        st.engine
            .revert_session(session, target)
            .await
            .map(|outcome| outcome.files)
    }
    .map_err(revert_error)?;
    drop(run);
    Ok(Json(pb::RevertSessionResponse {
        session: Some(projection_info(&st, session).await?),
        files: files.iter().map(super::convert::reverted_file).collect(),
    }))
}

fn revert_error(error: hya_core::RevertError) -> V1Error {
    use hya_core::RevertError as E;
    match error {
        E::SessionNotFound => {
            V1Error::new(hya_api::error::Code::SessionNotFound, error.to_string())
        }
        E::Busy => V1Error::session_busy(),
        E::MessageNotFound(_) => V1Error::new(hya_api::error::Code::NotFound, error.to_string()),
        E::NothingToRevert | E::NotUserMessage(_) | E::AlreadyReverted(_) | E::NoRevertPending => {
            V1Error::invalid_argument(error.to_string())
        }
        E::Core(error) => V1Error::from(error),
    }
}
