//! In-tree `NativeBundleApiHandler` adapter for `hya/multi-agent` until Phase 4
//! extracts it into a standalone native bundle under `bundles/builtin/`.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use hya_engine::{
    ApiMethod, ApiScope, BundleApiCall, BundleApiError, BundleApiOutcome, SourceApis,
};
use hya_runtime_abi::{
    HostError, NativeApiEndpoint, NativeBundleApiHandler, NativeBundleApiReply,
    NativeBundleApiRequest,
};
use serde_json::{Value, json};

use crate::{ApiError, ServerState};

const MULTI_AGENT_BUNDLE_ID: &str = "hya/multi-agent";

struct MultiAgentBundleApiHandler {
    state: ServerState,
}

impl MultiAgentBundleApiHandler {
    fn endpoints_list() -> &'static [NativeApiEndpoint] {
        static ENDPOINTS: OnceLock<Vec<NativeApiEndpoint>> = OnceLock::new();
        ENDPOINTS.get_or_init(|| {
            vec![
                NativeApiEndpoint {
                    id: "archive_subagent".to_owned(),
                    method: ApiMethod::Post,
                    scope: ApiScope::Session,
                    path: "/archive".to_owned(),
                    description: "Archive a subagent session on the user's behalf.".to_owned(),
                },
                NativeApiEndpoint {
                    id: "steer_subagent".to_owned(),
                    method: ApiMethod::Post,
                    scope: ApiScope::Session,
                    path: "/steer".to_owned(),
                    description: "Steer a subagent session with a message from the user."
                        .to_owned(),
                },
            ]
        })
    }
}

#[async_trait]
impl NativeBundleApiHandler for MultiAgentBundleApiHandler {
    fn id(&self) -> &str {
        MULTI_AGENT_BUNDLE_ID
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        Self::endpoints_list()
    }

    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        let Some(session) = request.session else {
            return Ok(api_error_reply(ApiError::invalid_argument(
                "session is required",
            )));
        };
        match request.api.as_str() {
            "archive_subagent" => match self.state.engine.archive_subagent(session).await {
                Ok(receipt) => Ok(NativeBundleApiReply {
                    status: 200,
                    body: json!({
                        "handle": receipt.handle,
                        "cancelledTurn": receipt.cancelled_turn,
                        "descendants": receipt.descendants,
                    }),
                }),
                Err(error) => Ok(api_error_reply(subagent_error(error))),
            },
            "steer_subagent" => {
                let text = request
                    .body
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match self.state.engine.steer_subagent(session, text).await {
                    Ok(receipt) => Ok(NativeBundleApiReply {
                        status: 200,
                        body: json!({ "from": receipt.from }),
                    }),
                    Err(error) => Ok(api_error_reply(subagent_error(error))),
                }
            }
            other => Err(HostError::NotFound(format!(
                "unknown multi-agent endpoint `{other}`"
            ))),
        }
    }
}

pub(crate) async fn invoke_builtin(
    st: &ServerState,
    call: BundleApiCall,
) -> Result<BundleApiOutcome, BundleApiError> {
    let handler: Arc<dyn NativeBundleApiHandler> = match call.bundle.as_str() {
        MULTI_AGENT_BUNDLE_ID => Arc::new(MultiAgentBundleApiHandler { state: st.clone() }),
        _ => {
            return Err(BundleApiError::NotFound {
                bundle: call.bundle.clone(),
                scope: call.scope(),
                path: call.path,
            });
        }
    };
    let apis = SourceApis::from_native_handlers(&call.bundle, vec![handler])?;
    apis.invoke(call).await
}

fn subagent_error(error: hya_engine::CoreError) -> ApiError {
    match error {
        hya_engine::CoreError::Invalid(message) => ApiError::invalid_argument(message),
        other => other.into(),
    }
}

fn api_error_reply(error: ApiError) -> NativeBundleApiReply {
    NativeBundleApiReply {
        status: error.code.http_status(),
        body: json!({
            "error": {
                "code": error.code.as_str(),
                "message": error.message,
            }
        }),
    }
}
