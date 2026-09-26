//! `/v1` workflow domain: catalog list, projected state, and typed
//! commands over the app-owned control handle.

use std::collections::BTreeMap;
use std::str::FromStr;

use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::HeaderMap;
use axum::routing::get;

use super::Json;
use serde_json::Value;

use crate::{ApiError, ServerState};
use hya_api::v1 as pb;
use hya_api::v1::submit_workflow_command_request::Command;
use hya_proto::workflow::{WorkflowCommand, WorkflowCommandResult};

use super::V1Error;
use super::request_scope;
use super::session::parse_session;

pub(crate) fn router() -> Router<ServerState> {
    Router::new()
        .route("/v1/workflows", get(list_workflows))
        .route(
            "/v1/sessions/:id/workflow",
            get(get_state).post(submit_command),
        )
}

async fn list_workflows(
    State(st): State<ServerState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
) -> Result<Json<pb::ListWorkflowsResponse>, V1Error> {
    let request: pb::ListWorkflowsRequest = super::query_request(&[], &query)?;
    let scope = request_scope(&headers, &request.directory)?;
    let rows = st
        .workflow_control
        .list(scope)
        .await
        .map_err(ApiError::workflow)
        .map_err(V1Error::from)?;
    let workflows = rows
        .iter()
        .map(|row| {
            let value = serde_json::to_value(row).unwrap_or(Value::Null);
            pb::WorkflowSummary {
                name: field(&value, "name"),
                revision: field(&value, "revision"),
                description: field(&value, "description"),
                stage_count: value
                    .get("stages")
                    .and_then(Value::as_array)
                    .map_or(0, |stages| stages.len() as u32),
            }
        })
        .collect();
    Ok(Json(pb::ListWorkflowsResponse {
        workflows,
        page: Some(pb::PageInfo::default()),
    }))
}

async fn get_state(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<pb::WorkflowState>, V1Error> {
    let session = parse_session(&id)?;
    let result = crate::workflow::execute(
        &st,
        session,
        WorkflowCommand::State,
        hya_proto::WorkflowDelivery::Started,
    )
    .await
    .map_err(V1Error::from)?;
    Ok(Json(map_state(&result)))
}

async fn submit_command(
    State(st): State<ServerState>,
    AxumPath(id): AxumPath<String>,
    Json(request): Json<pb::SubmitWorkflowCommandRequest>,
) -> Result<Json<pb::SubmitWorkflowCommandResponse>, V1Error> {
    let session = parse_session(&id)?;
    if !st.engine.session_exists(session).await? {
        return Err(V1Error::session_not_found(&id));
    }
    let command = match request.command {
        Some(Command::List(_)) => WorkflowCommand::List,
        Some(Command::Info(info)) => WorkflowCommand::Info { name: info.name },
        Some(Command::Select(select)) => WorkflowCommand::Select {
            name: select.name,
            expected_revision: revision_opt(&select.expected_revision),
        },
        Some(Command::Run(run)) => WorkflowCommand::Run {
            name: if run.name.is_empty() {
                None
            } else {
                Some(run.name.clone())
            },
            expected_revision: None,
            inputs: run
                .inputs
                .as_ref()
                .map(super::convert::from_struct)
                .and_then(|value| serde_json::from_value(value).ok())
                .unwrap_or_default(),
            run: None,
        },
        None => return Err(V1Error::invalid_argument("missing workflow command")),
    };
    let result =
        crate::workflow::execute(&st, session, command, hya_proto::WorkflowDelivery::Started)
            .await
            .map_err(V1Error::from)?;
    let response = match result {
        WorkflowCommandResult::List { .. } => {
            pb::submit_workflow_command_response::Result::List(pb::ListWorkflowsResponse {
                workflows: Vec::new(),
                page: Some(pb::PageInfo::default()),
            })
        }
        WorkflowCommandResult::Info { workflow } => {
            let value = serde_json::to_value(&workflow).unwrap_or(Value::Null);
            let stages_json = value
                .get("stages")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let stage_names: Vec<String> =
                stages_json.iter().map(|stage| field(stage, "id")).collect();
            let stages: Vec<pb::WorkflowStageInfo> = stages_json
                .iter()
                .map(|stage| pb::WorkflowStageInfo {
                    name: field(stage, "id"),
                    agent: field(stage, "agent"),
                    level: stage.get("level").and_then(Value::as_u64).unwrap_or(0) as u32,
                    worker_model: stage.get("worker_model").map(model_assignment),
                    verifier_model: stage.get("verifier_model").map(model_assignment),
                })
                .collect();
            pb::submit_workflow_command_response::Result::Info(pb::WorkflowInfoResult {
                name: value
                    .get("identity")
                    .map(|identity| field(identity, "name"))
                    .unwrap_or_default(),
                revision: value
                    .get("identity")
                    .map(|identity| field(identity, "revision"))
                    .unwrap_or_default(),
                stage_names,
                stages,
            })
        }
        WorkflowCommandResult::Selected { .. }
        | WorkflowCommandResult::State { .. }
        | WorkflowCommandResult::Run { .. } => {
            pb::submit_workflow_command_response::Result::Started(map_state(&result))
        }
    };
    Ok(Json(pb::SubmitWorkflowCommandResponse {
        result: Some(response),
    }))
}

/// Map a workflow result onto the wire state view via its serde shape.
fn map_state(result: &WorkflowCommandResult) -> pb::WorkflowState {
    let value = match result {
        WorkflowCommandResult::Selected { state } | WorkflowCommandResult::State { state } => {
            serde_json::to_value(state).unwrap_or(Value::Null)
        }
        WorkflowCommandResult::Run { result } => {
            serde_json::to_value(result).unwrap_or(Value::Null)
        }
        _ => Value::Null,
    };
    let selection = value.get("selection").cloned().unwrap_or(Value::Null);
    let run = value.get("run").cloned().unwrap_or(Value::Null);
    let raw_json = value.to_string();
    pb::WorkflowState {
        session: field(&run, "session"),
        workflow: field(&selection, "name"),
        revision: field(&selection, "revision"),
        status: run_status(&run),
        stages: Vec::new(),
        error_code: String::new(),
        raw_json,
    }
}

/// Parse an optional optimistic revision string.
fn revision_opt(revision: &str) -> Option<hya_proto::workflow::WorkflowRevision> {
    if revision.is_empty() {
        return None;
    }
    hya_proto::workflow::WorkflowRevision::from_str(revision).ok()
}

/// Map an authored model assignment JSON object onto the wire type.
fn model_assignment(model: &Value) -> pb::WorkflowModelAssignment {
    pb::WorkflowModelAssignment {
        id: field(model, "id"),
        reasoning: field(model, "reasoning"),
        fallback: model
            .get("fallback")
            .and_then(Value::as_array)
            .map(|rows| {
                rows.iter()
                    .map(|row| pb::WorkflowModelCandidate {
                        id: field(row, "id"),
                        reasoning: field(row, "reasoning"),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn run_status(run: &Value) -> i32 {
    match run.get("status").and_then(Value::as_str) {
        Some("running") => pb::WorkflowRunStatus::Running as i32,
        Some("finished") => pb::WorkflowRunStatus::Finished as i32,
        Some("failed") => pb::WorkflowRunStatus::Failed as i32,
        Some("cancelled") => pb::WorkflowRunStatus::Cancelled as i32,
        Some("selected") => pb::WorkflowRunStatus::Selected as i32,
        _ => pb::WorkflowRunStatus::Unspecified as i32,
    }
}

fn field(value: &Value, name: &str) -> String {
    value
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_default()
}
