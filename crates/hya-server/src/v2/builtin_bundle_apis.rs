//! In-tree `NativeBundleApiHandler` adapters for `hya/workflow` and
//! `hya/multi-agent` during Phase 0 until Phases 3–4 extract them into
//! standalone native bundles under `bundles/builtin/`.

use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use hya_engine::{ApiMethod, ApiScope, BundleApiCall, BundleApiError, BundleApiOutcome, SourceApis};
use hya_runtime_abi::{
    HostError, NativeApiEndpoint, NativeBundleApiHandler, NativeBundleApiReply,
    NativeBundleApiRequest,
};
use hya_tool::{WorkflowCommand, WorkflowCommandResult, WorkflowDelivery};
use serde_json::{Value, json};

use crate::{ApiError, ServerState};

use super::request_scope;

const WORKFLOW_BUNDLE_ID: &str = "hya/workflow";
const MULTI_AGENT_BUNDLE_ID: &str = "hya/multi-agent";

struct WorkflowBundleApiHandler {
    state: ServerState,
}

impl WorkflowBundleApiHandler {
    fn endpoints_list() -> &'static [NativeApiEndpoint] {
        static ENDPOINTS: OnceLock<Vec<NativeApiEndpoint>> = OnceLock::new();
        ENDPOINTS.get_or_init(|| {
            vec![
                NativeApiEndpoint {
                    id: "list_workflows".to_owned(),
                    method: ApiMethod::Get,
                    scope: ApiScope::Global,
                    path: "/workflows".to_owned(),
                    description: "List discovered workflow sources for a directory.".to_owned(),
                },
                NativeApiEndpoint {
                    id: "get_workflow_state".to_owned(),
                    method: ApiMethod::Get,
                    scope: ApiScope::Session,
                    path: "/state".to_owned(),
                    description: "Read the projected workflow state of one session.".to_owned(),
                },
                NativeApiEndpoint {
                    id: "submit_workflow_command".to_owned(),
                    method: ApiMethod::Post,
                    scope: ApiScope::Session,
                    path: "/command".to_owned(),
                    description: "Submit one typed workflow command against a session.".to_owned(),
                },
            ]
        })
    }
}

#[async_trait]
impl NativeBundleApiHandler for WorkflowBundleApiHandler {
    fn id(&self) -> &str {
        WORKFLOW_BUNDLE_ID
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        Self::endpoints_list()
    }

    async fn handle(&self, request: NativeBundleApiRequest) -> Result<NativeBundleApiReply, HostError> {
        match request.api.as_str() {
            "list_workflows" => {
                let directory = request
                    .query
                    .get("directory")
                    .map(String::as_str)
                    .unwrap_or_default();
                let scope = match request_scope(directory) {
                    Ok(scope) => scope,
                    Err(error) => return Ok(api_error_reply(error)),
                };
                match self.state.workflow_control.list(scope).await {
                    Ok(workflows) => json_reply(200, json!({ "workflows": workflows })),
                    Err(error) => Ok(api_error_reply(ApiError::workflow(error))),
                }
            }
            "get_workflow_state" => {
                let Some(session) = request.session else {
                    return Ok(api_error_reply(ApiError::invalid_argument(
                        "session is required",
                    )));
                };
                match crate::workflow::execute(
                    &self.state,
                    session,
                    WorkflowCommand::State,
                    WorkflowDelivery::Started,
                )
                .await
                {
                    Ok(WorkflowCommandResult::State { state }) => json_reply(200, json!(state)),
                    Ok(other) => json_reply(200, json!(other)),
                    Err(error) => Ok(api_error_reply(error)),
                }
            }
            "submit_workflow_command" => {
                let Some(session) = request.session else {
                    return Ok(api_error_reply(ApiError::invalid_argument(
                        "session is required",
                    )));
                };
                let command = match parse_workflow_command(&request.body) {
                    Ok(command) => command,
                    Err(error) => return Ok(api_error_reply(error)),
                };
                match crate::workflow::execute(
                    &self.state,
                    session,
                    command,
                    WorkflowDelivery::Started,
                )
                .await
                {
                    Ok(WorkflowCommandResult::List { workflows }) => {
                        json_reply(200, json!({ "list": { "workflows": workflows } }))
                    }
                    Ok(WorkflowCommandResult::Info { workflow }) => {
                        json_reply(200, json!({ "info": workflow }))
                    }
                    Ok(WorkflowCommandResult::Selected { state }) => {
                        json_reply(200, json!({ "selected": state }))
                    }
                    Ok(WorkflowCommandResult::State { state }) => {
                        json_reply(200, json!({ "state": state }))
                    }
                    Ok(WorkflowCommandResult::Run { result }) => {
                        json_reply(200, json!({ "started": result }))
                    }
                    Err(error) => Ok(api_error_reply(error)),
                }
            }
            other => Err(HostError::NotFound(format!(
                "unknown workflow endpoint `{other}`"
            ))),
        }
    }
}

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

    async fn handle(&self, request: NativeBundleApiRequest) -> Result<NativeBundleApiReply, HostError> {
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
        WORKFLOW_BUNDLE_ID => Arc::new(WorkflowBundleApiHandler { state: st.clone() }),
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

fn parse_workflow_command(body: &Value) -> Result<WorkflowCommand, ApiError> {
    if body.get("list").is_some() {
        return Ok(WorkflowCommand::List);
    }
    if let Some(info) = body.get("info") {
        let name = info
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return Ok(WorkflowCommand::Info { name });
    }
    if let Some(select) = body.get("select") {
        let name = select
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let expected_raw = select
            .get("expectedRevision")
            .or_else(|| select.get("expected_revision"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let expected_revision = if expected_raw.is_empty() {
            None
        } else {
            Some(
                hya_proto::workflow::WorkflowRevision::from_str(expected_raw)
                    .map_err(|error| ApiError::invalid_argument(format!("invalid revision: {error}")))?,
            )
        };
        return Ok(WorkflowCommand::Select {
            name,
            expected_revision,
        });
    }
    if let Some(run) = body.get("run") {
        let name = run
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let inputs: BTreeMap<String, String> = run
            .get("inputs")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        return Ok(WorkflowCommand::Run {
            name,
            expected_revision: None,
            inputs,
            run: None,
        });
    }
    Err(ApiError::invalid_argument("missing workflow command"))
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

fn json_reply(status: u16, body: Value) -> Result<NativeBundleApiReply, HostError> {
    Ok(NativeBundleApiReply { status, body })
}
