//! `/v1/relay/*`: the loopback-only `RelayControl` service (ADR-0025 D5;
//! docs/relay.md "Hosting a backend on a relay").
//!
//! Every rpc is refused with `permission_denied` when the request arrived
//! through the relay ([`Origin::Relay`]), came from a TCP peer that is not a
//! loopback address, or was sent by a browser (`Origin` / `Sec-Fetch-Site`
//! header): a link holder must not be able to change the relay, read the
//! link, or rotate it away from the local owner, and a web page must not
//! read the link through the mirrored CORS policy.

use std::net::SocketAddr;

use axum::Router;
use axum::extract::{ConnectInfo, State};
use axum::http::{Extensions, HeaderMap};
use axum::routing::{get, post};
use hya_api::error::Code;
use hya_api::v1 as pb;

use super::{Json, V1Error};
use crate::relay_host::{RelayHostError, RelaySettings, RelayState, RelayStatus};
use crate::{Origin, ServerState};

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/relay/connect", post(connect))
        .route("/v1/relay/disconnect", post(disconnect))
        .route("/v1/relay/status", get(status))
        .route("/v1/relay/link", get(link))
        .route("/v1/relay/rotate", post(rotate))
}

/// The TCP peer of a gRPC call, set by the gRPC binding on the request it
/// dispatches through the router (the in-process dispatch has no
/// `ConnectInfo`). The server sets it; it is never parsed from the wire.
#[derive(Clone, Copy, Debug)]
pub(crate) struct GrpcPeer(pub(crate) SocketAddr);

/// Refuse a request that did not come from the local owner.
pub(crate) fn require_local_owner(
    extensions: &Extensions,
    headers: &HeaderMap,
) -> Result<(), V1Error> {
    if Origin::of(extensions) == Origin::Relay {
        return Err(V1Error::new(
            Code::PermissionDenied,
            "relay control is loopback-only: refused for a request that arrived through the relay",
        ));
    }
    // Fail closed: a request whose peer is unknown (no TCP `ConnectInfo`,
    // no gRPC peer) is refused, whatever listener it came from.
    let peer = extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| *peer)
        .or_else(|| extensions.get::<GrpcPeer>().map(|GrpcPeer(peer)| *peer));
    match peer {
        None => {
            return Err(V1Error::new(
                Code::PermissionDenied,
                "relay control is loopback-only: refused for a request whose client address is unknown",
            ));
        }
        Some(peer) if !peer.ip().is_loopback() => {
            return Err(V1Error::new(
                Code::PermissionDenied,
                "relay control is loopback-only: refused for a non-loopback client",
            ));
        }
        Some(_) => {}
    }
    if headers.contains_key("origin") || headers.contains_key("sec-fetch-site") {
        return Err(V1Error::new(
            Code::PermissionDenied,
            "relay control is refused for browser requests",
        ));
    }
    Ok(())
}

/// Refuse a request that arrived through the relay (process stop/upgrade).
pub(crate) fn refuse_relay_origin(extensions: &Extensions, what: &str) -> Result<(), V1Error> {
    if Origin::of(extensions) == Origin::Relay {
        return Err(V1Error::new(
            Code::PermissionDenied,
            format!("{what} is refused for a request that arrived through the relay"),
        ));
    }
    Ok(())
}

impl From<RelayHostError> for V1Error {
    fn from(error: RelayHostError) -> Self {
        let code = match &error {
            RelayHostError::InvalidArgument(_) => Code::InvalidArgument,
            RelayHostError::FailedPrecondition(_) => Code::FailedPrecondition,
            RelayHostError::Identity(_) => Code::Internal,
            RelayHostError::Stopping => Code::Unavailable,
        };
        V1Error::new(code, error.to_string())
    }
}

fn state_pb(state: RelayState) -> pb::RelayState {
    match state {
        RelayState::Disconnected => pb::RelayState::Disconnected,
        RelayState::Connecting => pb::RelayState::Connecting,
        RelayState::Connected => pb::RelayState::Connected,
        RelayState::Backoff => pb::RelayState::Backoff,
    }
}

pub(crate) fn status_pb(status: RelayStatus) -> pb::RelayStatus {
    pb::RelayStatus {
        state: state_pb(status.state) as i32,
        proxy: status.proxy.unwrap_or_default(),
        room_id: status.room_id.unwrap_or_default(),
        redacted_link: status.redacted_link.unwrap_or_default(),
        transport: status
            .transport
            .map(|transport| transport.as_str().to_owned())
            .unwrap_or_default(),
        binding: status
            .binding
            .as_ref()
            .map(|choice| choice.binding.as_str().to_owned())
            .unwrap_or_default(),
        binding_reason: status
            .binding
            .as_ref()
            .map(|choice| choice.reason.to_string())
            .unwrap_or_default(),
        last_error: status.last_error.unwrap_or_default(),
        connected_since: status.connected_since.and_then(|at| {
            at.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .and_then(|since| i64::try_from(since.as_millis()).ok())
                .and_then(super::convert::timestamp)
        }),
        active_streams: u32::try_from(status.active_streams).unwrap_or(u32::MAX),
        ephemeral: status.ephemeral,
    }
}

/// `ConnectRelayRequest` → settings.
fn settings_of(request: pb::ConnectRelayRequest) -> Result<RelaySettings, V1Error> {
    if request.proxy_url.trim().is_empty() {
        return Err(V1Error::invalid_argument("proxy_url is required"));
    }
    let transport = match request.transport.trim() {
        "" => hya_relay::link::Transport::Auto,
        other => other.parse().map_err(|_| {
            V1Error::invalid_argument(format!(
                "unknown relay transport `{other}` (expected auto, grpc, or ws)"
            ))
        })?,
    };
    let extra_ca = Some(request.extra_ca_path.trim())
        .filter(|path| !path.is_empty())
        .map(std::path::PathBuf::from);
    if let Some(path) = &extra_ca
        && !path.is_absolute()
    {
        return Err(V1Error::invalid_argument(format!(
            "the relay CA path must be absolute, got `{}`",
            path.display()
        )));
    }
    Ok(RelaySettings {
        proxy_url: request.proxy_url.trim().to_owned(),
        transport,
        extra_ca,
        ephemeral: request.ephemeral,
    })
}

async fn connect(
    State(st): State<ServerState>,
    extensions: Extensions,
    headers: HeaderMap,
    Json(request): Json<pb::ConnectRelayRequest>,
) -> Result<Json<pb::ConnectRelayResponse>, V1Error> {
    require_local_owner(&extensions, &headers)?;
    let settings = settings_of(request)?;
    let link = st.relay.connect(settings).await?;
    Ok(Json(pb::ConnectRelayResponse {
        status: Some(status_pb(st.relay.status())),
        link: link.to_secret_string(),
    }))
}

async fn disconnect(
    State(st): State<ServerState>,
    extensions: Extensions,
    headers: HeaderMap,
    Json(_request): Json<pb::DisconnectRelayRequest>,
) -> Result<Json<pb::RelayStatus>, V1Error> {
    require_local_owner(&extensions, &headers)?;
    st.relay.disconnect().await;
    Ok(Json(status_pb(st.relay.status())))
}

async fn status(
    State(st): State<ServerState>,
    extensions: Extensions,
    headers: HeaderMap,
) -> Result<Json<pb::RelayStatus>, V1Error> {
    require_local_owner(&extensions, &headers)?;
    Ok(Json(status_pb(st.relay.status())))
}

async fn link(
    State(st): State<ServerState>,
    extensions: Extensions,
    headers: HeaderMap,
) -> Result<Json<pb::RelayLinkResponse>, V1Error> {
    require_local_owner(&extensions, &headers)?;
    let Some(link) = st.relay.link() else {
        return Err(V1Error::new(
            Code::FailedPrecondition,
            "not connected to a relay (hya serve relay connect <url>)",
        ));
    };
    Ok(Json(pb::RelayLinkResponse {
        link: link.to_secret_string(),
        status: Some(status_pb(st.relay.status())),
    }))
}

async fn rotate(
    State(st): State<ServerState>,
    extensions: Extensions,
    headers: HeaderMap,
    Json(_request): Json<pb::RotateRelayKeyRequest>,
) -> Result<Json<pb::RelayLinkResponse>, V1Error> {
    require_local_owner(&extensions, &headers)?;
    let link = st.relay.rotate().await?;
    Ok(Json(pb::RelayLinkResponse {
        link: link.map(|link| link.to_secret_string()).unwrap_or_default(),
        status: Some(status_pb(st.relay.status())),
    }))
}
