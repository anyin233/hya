use serde::Deserialize;
use serde_json::{Map, Number, Value};
use yaca_proto::SessionId;

use super::projection::OpenCodeSessionInfo;
use crate::{ApiError, ServerState};

#[derive(Deserialize)]
pub(in crate::opencode) struct UpdateSessionPayload {
    title: Option<String>,
    metadata: Option<Map<String, Value>>,
    permission: Option<Vec<Value>>,
    time: Option<UpdateSessionTime>,
}

#[derive(Deserialize)]
struct UpdateSessionTime {
    archived: Option<Number>,
}

pub(in crate::opencode) async fn apply(
    st: &ServerState,
    session: SessionId,
    payload: UpdateSessionPayload,
) -> Result<OpenCodeSessionInfo, ApiError> {
    let snapshot = super::session_legacy::load_session(st, session, None).await?;
    if let Some(title) = payload.title {
        st.engine.set_title(session, title).await?;
    }
    if let Some(metadata) = payload.metadata {
        st.engine
            .set_metadata(session, Value::Object(metadata))
            .await?;
    }
    if let Some(permission) = payload.permission {
        let mut merged = snapshot
            .info
            .permission()
            .map(|permission| permission.to_vec())
            .unwrap_or_default();
        merged.extend(permission);
        st.engine.set_permission(session, merged).await?;
    }
    if let Some(archived) = payload.time.and_then(|time| time.archived) {
        st.engine.set_archived(session, archived).await?;
    }
    Ok(super::session_legacy::load_session(st, session, None)
        .await?
        .info)
}
