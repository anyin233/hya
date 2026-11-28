//! Native [`NativeBundleApiHandler`] for `hya/workflow`.
//!
//! Exposes the canonical v2 workflow endpoints:
//! - `GET /workflows` (global scope, optional `?directory=<abs>`)
//! - `GET /state` (session scope)
//! - `POST /command` (session scope)
//! - `POST /cli/{list,info}` (global scope) and `POST /cli/{use,run,state}`
//!   (session scope): the `hya workflow` command endpoints ([`crate::cli`])

use std::collections::BTreeMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use hya_bundle::{ApiMethod, ApiScope};
use hya_runtime_abi::{
    HostError, NativeApiEndpoint, NativeBundleApiHandler, NativeBundleApiReply,
    NativeBundleApiRequest,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::error::WorkflowControlError;
use crate::model::{
    WorkflowCommand, WorkflowCommandResult, WorkflowDelivery, WorkflowRevision, WorkflowRunId,
};
use crate::service::{WORKFLOW_BUNDLE_ID, WorkflowInvocation, WorkflowService};

/// Native HTTP/gRPC bundle API handler for `hya/workflow`.
pub struct WorkflowBundleApiHandler {
    service: Arc<WorkflowService>,
}

impl WorkflowBundleApiHandler {
    /// Construct a [`WorkflowBundleApiHandler`] backed by `service`.
    #[must_use]
    pub fn new(service: Arc<WorkflowService>) -> Self {
        Self { service }
    }

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
                cli_endpoint("cli_list", ApiScope::Global, "/cli/list", "list"),
                cli_endpoint("cli_info", ApiScope::Global, "/cli/info", "info"),
                cli_endpoint("cli_use", ApiScope::Session, "/cli/use", "use"),
                cli_endpoint("cli_run", ApiScope::Session, "/cli/run", "run"),
                cli_endpoint("cli_state", ApiScope::Session, "/cli/state", "state"),
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

    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        match request.api.as_str() {
            "list_workflows" => {
                let directory = request
                    .query
                    .get("directory")
                    .map(String::as_str)
                    .unwrap_or_default();
                let scope = match parse_directory_scope(directory) {
                    Ok(scope) => scope,
                    Err(reply) => return Ok(reply),
                };
                match self.service.list(scope).await {
                    Ok(workflows) => Ok(json_reply(200, json!({ "workflows": workflows }))),
                    Err(error) => Ok(control_error_reply(&error)),
                }
            }
            "get_workflow_state" => {
                let Some(session) = request.session else {
                    return Ok(error_reply(400, "invalid_argument", "session is required"));
                };
                let invocation = WorkflowInvocation {
                    caller: None,
                    operation: None,
                    delivery: WorkflowDelivery::Started,
                };
                match self
                    .service
                    .execute(
                        session,
                        invocation,
                        WorkflowCommand::State,
                        CancellationToken::new(),
                    )
                    .await
                {
                    Ok(WorkflowCommandResult::State { state }) => Ok(json_reply(200, json!(state))),
                    Ok(other) => Ok(json_reply(200, json!(other))),
                    Err(error) => Ok(control_error_reply(&error)),
                }
            }
            "submit_workflow_command" => {
                let Some(session) = request.session else {
                    return Ok(error_reply(400, "invalid_argument", "session is required"));
                };
                let (command, delivery) = match parse_workflow_command(&request.body) {
                    Ok(parsed) => parsed,
                    Err(reply) => return Ok(reply),
                };
                let invocation = WorkflowInvocation {
                    caller: None,
                    operation: None,
                    delivery,
                };
                match self
                    .service
                    .execute(session, invocation, command, CancellationToken::new())
                    .await
                {
                    Ok(WorkflowCommandResult::List { workflows }) => Ok(json_reply(
                        200,
                        json!({ "list": { "workflows": workflows } }),
                    )),
                    Ok(WorkflowCommandResult::Info { workflow }) => {
                        Ok(json_reply(200, json!({ "info": workflow })))
                    }
                    Ok(WorkflowCommandResult::Selected { state }) => {
                        Ok(json_reply(200, json!({ "selected": state })))
                    }
                    Ok(WorkflowCommandResult::State { state }) => {
                        Ok(json_reply(200, json!({ "state": state })))
                    }
                    Ok(WorkflowCommandResult::Run { result }) => {
                        Ok(json_reply(200, json!({ "started": result })))
                    }
                    Err(error) => Ok(control_error_reply(&error)),
                }
            }
            endpoint @ ("cli_list" | "cli_info" | "cli_use" | "cli_run" | "cli_state") => {
                Ok(crate::cli::handle(&self.service, endpoint, &request).await)
            }
            other => Err(HostError::NotFound(format!(
                "unknown workflow endpoint `{other}`"
            ))),
        }
    }
}

fn cli_endpoint(id: &str, scope: ApiScope, path: &str, command: &str) -> NativeApiEndpoint {
    NativeApiEndpoint {
        id: id.to_owned(),
        method: ApiMethod::Post,
        scope,
        path: path.to_owned(),
        description: format!("Run `hya workflow {command}` and reply what it prints."),
    }
}

fn parse_directory_scope(raw: &str) -> Result<Option<&Path>, NativeBundleApiReply> {
    if raw.is_empty() {
        return Ok(None);
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(error_reply(
            400,
            "invalid_argument",
            format!("directory must be an absolute path: {raw}"),
        ));
    }
    Ok(Some(path))
}

fn parse_workflow_command(
    body: &Value,
) -> Result<(WorkflowCommand, WorkflowDelivery), NativeBundleApiReply> {
    if body.get("list").is_some() {
        return Ok((WorkflowCommand::List, WorkflowDelivery::Started));
    }
    if body.get("state").is_some() {
        return Ok((WorkflowCommand::State, WorkflowDelivery::Started));
    }
    if let Some(info) = body.get("info") {
        let name = info
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return Ok((WorkflowCommand::Info { name }, WorkflowDelivery::Started));
    }
    if let Some(select) = body.get("select") {
        let name = select
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let expected_revision = parse_optional_revision(select)?;
        return Ok((
            WorkflowCommand::Select {
                name,
                expected_revision,
            },
            WorkflowDelivery::Started,
        ));
    }
    if let Some(run) = body.get("run") {
        let name = run
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
        let expected_revision = parse_optional_revision(run)?;
        let inputs: BTreeMap<String, String> = run
            .get("inputs")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default();
        let requested_run = match run
            .get("run")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            Some(raw) => Some(WorkflowRunId::from_str(raw).map_err(|error| {
                error_reply(400, "invalid_argument", format!("invalid run id: {error}"))
            })?),
            None => None,
        };
        let delivery = match run.get("delivery").and_then(Value::as_str) {
            Some("finished") => WorkflowDelivery::Finished,
            Some("started") | None => WorkflowDelivery::Started,
            Some(other) => {
                return Err(error_reply(
                    400,
                    "invalid_argument",
                    format!("invalid workflow delivery `{other}` (expected started|finished)"),
                ));
            }
        };
        return Ok((
            WorkflowCommand::Run {
                name,
                expected_revision,
                inputs,
                run: requested_run,
            },
            delivery,
        ));
    }
    Err(error_reply(
        400,
        "invalid_argument",
        "missing workflow command",
    ))
}

fn parse_optional_revision(
    value: &Value,
) -> Result<Option<WorkflowRevision>, NativeBundleApiReply> {
    let raw = value
        .get("expectedRevision")
        .or_else(|| value.get("expected_revision"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if raw.is_empty() {
        return Ok(None);
    }
    WorkflowRevision::from_str(raw).map(Some).map_err(|error| {
        error_reply(
            400,
            "invalid_argument",
            format!("invalid revision: {error}"),
        )
    })
}

fn json_reply(status: u16, body: Value) -> NativeBundleApiReply {
    NativeBundleApiReply { status, body }
}

fn error_reply(status: u16, code: &str, message: impl Into<String>) -> NativeBundleApiReply {
    NativeBundleApiReply {
        status,
        body: json!({
            "error": {
                "code": code,
                "message": message.into(),
            }
        }),
    }
}

fn control_error_reply(error: &WorkflowControlError) -> NativeBundleApiReply {
    let (status, v2_code) = error.http_status_and_v2_code();
    error_reply(status, v2_code, error.to_string())
}
