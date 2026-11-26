//! Connection-owned MCP exports for local editor sessions.
use std::{
    collections::HashSet,
    convert::Infallible,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    sync::Arc,
};

use axum::{
    Router,
    extract::State,
    http::{Extensions, HeaderMap},
    response::sse::{Event, KeepAlive, Sse},
    routing::post,
};
use futures::{Stream, StreamExt, stream};
use hya_api::v1 as pb;
use hya_engine::{RuntimeSource, RuntimeSourceExport, RuntimeSourceId, RuntimeSourceKind};
use tokio::net::UnixStream;

use super::{Json, V1Error};
use crate::ServerState;

pub(super) fn router() -> Router<ServerState> {
    Router::new().route("/v1/mcp/session", post(http_open))
}

async fn http_open(
    State(state): State<ServerState>,
    extensions: Extensions,
    headers: HeaderMap,
    Json(request): Json<pb::OpenMcpSessionRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, V1Error> {
    super::relay::require_local_owner(&extensions, &headers)?;
    Ok(Sse::new(open(state, request).await?.map(|frame| {
        Ok(Event::default().data(serde_json::json!({"session":frame.session}).to_string()))
    }))
    .keep_alive(KeepAlive::default()))
}

struct Lease {
    engine: Arc<hya_engine::SessionEngine>,
    id: hya_proto::SessionId,
    // Dropping the prepared peers closes even the clients held by in-flight tools.
    _peers: Vec<hya_mcp::PreparedMcpServer>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.engine.set_session_mcp_sources(self.id, Vec::new());
    }
}

fn invalid() -> V1Error {
    V1Error::invalid_argument("expected a private hya ACP socket owned by the daemon user")
}

async fn peer_socket(path: &Path) -> Result<UnixStream, V1Error> {
    // Only the reserved /tmp/hya-acp-*/peer-* namespace is a dial target.
    // Reject relative components, links and paths through other directories.
    let dir = path.parent().ok_or_else(invalid)?;
    if dir.parent() != Some(Path::new("/tmp"))
        || !dir
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("hya-acp-"))
        || !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("peer-"))
    {
        return Err(invalid());
    }
    // SAFETY: geteuid takes no arguments and cannot fail.
    let uid = unsafe { libc::geteuid() };
    let directory = tokio::fs::symlink_metadata(dir)
        .await
        .map_err(|_| invalid())?;
    let socket = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| invalid())?;
    if !directory.is_dir()
        || directory.mode() & 0o077 != 0
        || directory.uid() != uid
        || !socket.file_type().is_socket()
        || socket.uid() != uid
    {
        return Err(invalid());
    }
    let stream = UnixStream::connect(path).await.map_err(|_| invalid())?;
    if stream.peer_cred().map_err(|_| invalid())?.uid() != uid {
        return Err(invalid());
    }
    Ok(stream)
}

pub(super) async fn open(
    state: ServerState,
    request: pb::OpenMcpSessionRequest,
) -> Result<impl Stream<Item = pb::OpenMcpSessionResponse> + Send + use<>, V1Error> {
    if request.peers.is_empty()
        || !Path::new(&request.workdir).is_absolute()
        || !tokio::fs::metadata(&request.workdir)
            .await
            .is_ok_and(|m| m.is_dir())
    {
        return Err(V1Error::invalid_argument(
            "absolute workdir and MCP peers required",
        ));
    }
    let mut names = HashSet::new();
    let mut peers = Vec::new();
    let mut sources = Vec::new();
    for peer in request.peers {
        if !hya_mcp::bridge::valid_server_name(&peer.name) || !names.insert(peer.name.clone()) {
            return Err(V1Error::invalid_argument(
                "MCP names must be distinct namespace segments (letters, digits, hyphens or underscores, without double underscores)",
            ));
        }
        let (reader, writer) = peer_socket(Path::new(&peer.socket)).await?.into_split();
        let prepared = hya_mcp::manager::prepare_peer(
            peer.name.clone(),
            hya_mcp::McpClient::new(reader, writer),
        )
        .await
        .map_err(|_| V1Error::invalid_argument("MCP discovery failed"))?;
        let exports = prepared
            .tools()
            .into_iter()
            .map(|tool| {
                let name = tool.name().to_owned();
                RuntimeSourceExport::tool(
                    &name,
                    &name,
                    Vec::new(),
                    tool,
                    hya_tool::ToolPermission::Mcp,
                )
            })
            .collect();
        sources.push(
            RuntimeSource::new(
                RuntimeSourceId::new(RuntimeSourceKind::Mcp, peer.name),
                [0; 32],
                Arc::new(()),
                exports,
            )
            .with_resources(prepared.resources()),
        );
        peers.push(prepared);
    }
    let Json(created) = super::session::create_session(
        State(state.clone()),
        Json(pb::CreateSessionRequest {
            workdir: Some(request.workdir),
            model: request.model,
            ..Default::default()
        }),
    )
    .await?;
    let session = created
        .session
        .ok_or_else(|| V1Error::invalid_argument("missing created session"))?
        .id;
    let id = super::session::parse_session(&session)?;
    state.engine.set_session_mcp_sources(id, sources);
    let lease = Lease {
        engine: state.engine.clone(),
        id,
        _peers: peers,
    };
    let closed = state.streams.closed();
    Ok(
        stream::once(async move { pb::OpenMcpSessionResponse { session } }).chain(
            stream::once(async move {
                closed.await;
                drop(lease);
                None
            })
            .filter_map(futures::future::ready),
        ),
    )
}
