//! Shared Workflow HTTP adapters and slash-command decision logic.
//!
//! All native, legacy Compat, and Compat v2 command routes call the parser in
//! this module before they reserve a parent-model run. Typed endpoints call
//! the same server-owned control port and serialize the shared proto DTOs.

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use hya_proto::api::CommandRequest;
use hya_proto::{SessionId, WorkflowCommand, WorkflowCommandResult, WorkflowDelivery};

use crate::workflow_control::WorkflowControlError;
use crate::{ApiError, ServerState};

/// Build native `/sessions/:id/workflow` routes.
pub(crate) fn native_router() -> Router<ServerState> {
    Router::new().route("/sessions/:id/workflow", get(state).post(command_endpoint))
}

/// Build legacy Compat and Compat v2 `/session/:id/workflow` routes.
pub(crate) fn compat_router() -> Router<ServerState> {
    Router::new()
        .route("/session/:id/workflow", get(state).post(command_endpoint))
        .route(
            "/api/session/:id/workflow",
            get(state).post(command_endpoint),
        )
}

/// Parse and, when applicable, execute one `/workflow` command.
//
/// `None` means the command belongs to the existing parent-model path. A
/// recognized Workflow command is admitted as a normal command transcript row,
/// dispatched through the control port, and completed with a visible assistant result row;
/// no parent-model run is started.
//
/// # Errors
/// Returns a syntax, control, persistence, or serialization error.
pub(crate) async fn intercept_slash(
    st: &ServerState,
    session: SessionId,
    request: &CommandRequest,
) -> Result<Option<WorkflowCommandResult>, ApiError> {
    let Some(command) = parse_slash(&request.command, &request.arguments)? else {
        return Ok(None);
    };
    if st.engine.replay(session).await?.is_empty() {
        return Err(ApiError::workflow(WorkflowControlError::new(
            "SESSION_NOT_FOUND",
            format!("Session `{session}` was not found"),
        )));
    }
    let _reservation = reserve_workflow_command(st, session, &command)?;
    let arguments = request.arguments.clone();
    let text = request
        .text
        .clone()
        .unwrap_or_else(|| crate::command_prompt_text(&request.command, &arguments));
    st.engine
        .admit_command_prompt(session, request.command.clone(), arguments, text)
        .await?;
    let result = match execute_reserved(st, session, command, WorkflowDelivery::Started).await {
        Ok(result) => result,
        Err(error) => {
            let message = error.code.as_deref().map_or_else(
                || error.message.clone(),
                |code| format!("{code}: {}", error.message),
            );
            st.engine.inject_assistant_message(session, message).await?;
            return Err(error);
        }
    };
    let result_text = serde_json::to_string(&result)
        .map_err(|error| ApiError::internal(format!("encode Workflow result: {error}")))?;
    st.engine
        .inject_assistant_message(session, result_text)
        .await?;
    Ok(Some(result))
}

/// Parse one command name and its raw argument string into the shared command.
///
/// The parser deliberately owns only the `/workflow` grammar. Other slash
/// commands return `None` so their existing route behavior remains unchanged.
///
/// # Errors
/// Returns a structured 400 error for malformed Workflow syntax.
pub(crate) fn parse_slash(
    command: &str,
    arguments: &str,
) -> Result<Option<WorkflowCommand>, ApiError> {
    let command = command.trim().trim_start_matches('/');
    if command != "workflow" {
        return Ok(None);
    }
    let tokens: Vec<&str> = arguments.split_whitespace().collect();
    let Some(operation) = tokens.first().copied() else {
        return Err(syntax("missing Workflow operation"));
    };
    let command = match operation {
        "list" if tokens.len() == 1 => WorkflowCommand::List,
        "state" if tokens.len() == 1 => WorkflowCommand::State,
        "info" if tokens.len() == 2 => WorkflowCommand::Info {
            name: required_name(tokens[1])?,
        },
        "use" if tokens.len() == 2 => WorkflowCommand::Select {
            name: required_name(tokens[1])?,
            expected_revision: None,
        },
        "run" => parse_run(&tokens[1..])?,
        _ => return Err(syntax("invalid Workflow syntax")),
    };
    Ok(Some(command))
}

fn parse_run(tokens: &[&str]) -> Result<WorkflowCommand, ApiError> {
    let mut name = None;
    let mut inputs = BTreeMap::new();
    for token in tokens {
        if let Some((key, value)) = token.split_once('=') {
            if key.is_empty()
                || value.is_empty()
                || inputs.insert(key.to_string(), value.to_string()).is_some()
            {
                return Err(syntax("invalid Workflow input assignment"));
            }
        } else if name.is_none() {
            name = Some(required_name(token)?);
        } else {
            return Err(syntax("invalid Workflow run argument"));
        }
    }
    Ok(WorkflowCommand::Run {
        name,
        expected_revision: None,
        inputs,
        run: None,
    })
}

fn required_name(name: &str) -> Result<String, ApiError> {
    (!name.is_empty())
        .then(|| name.to_string())
        .ok_or_else(|| syntax("Workflow name must not be empty"))
}

fn syntax(message: impl Into<String>) -> ApiError {
    ApiError::structured(
        axum::http::StatusCode::BAD_REQUEST,
        "WORKFLOW_SYNTAX",
        message,
    )
}

async fn command_endpoint(
    State(st): State<ServerState>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    let session = crate::parse_session(&id)?;
    let command = serde_json::from_slice::<WorkflowCommand>(&body)
        .map_err(|error| syntax(format!("invalid Workflow command: {error}")))?;
    let result = execute(&st, session, command, WorkflowDelivery::Started).await?;
    Ok(Json(result).into_response())
}

async fn state(
    State(st): State<ServerState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let session = crate::parse_session(&id)?;
    let result = execute(
        &st,
        session,
        WorkflowCommand::State,
        WorkflowDelivery::Started,
    )
    .await?;
    Ok(Json(result).into_response())
}

/// Reserve a Session for commands that can change durable Workflow state.
///
/// Read-only commands deliberately skip the server registry, so catalog and
/// projected state reads remain available while a parent-model turn runs.
fn reserve_workflow_command(
    st: &ServerState,
    session: SessionId,
    command: &WorkflowCommand,
) -> Result<Option<crate::runs::RunGuard>, ApiError> {
    if !matches!(
        command,
        WorkflowCommand::Select { .. } | WorkflowCommand::Run { .. }
    ) {
        return Ok(None);
    }
    st.reserve_workflow_run(session).map(Some).ok_or_else(|| {
        ApiError::workflow(WorkflowControlError::new(
            "WORKFLOW_BUSY",
            "another run is active",
        ))
    })
}

async fn execute(
    st: &ServerState,
    session: SessionId,
    command: WorkflowCommand,
    delivery: WorkflowDelivery,
) -> Result<WorkflowCommandResult, ApiError> {
    let _reservation = reserve_workflow_command(st, session, &command)?;
    execute_reserved(st, session, command, delivery).await
}

async fn execute_reserved(
    st: &ServerState,
    session: SessionId,
    command: WorkflowCommand,
    delivery: WorkflowDelivery,
) -> Result<WorkflowCommandResult, ApiError> {
    st.workflow_control
        .execute(session, command, delivery)
        .await
        .map_err(ApiError::workflow)
}

/// Convert a server-owned control failure into the stable HTTP boundary.
pub(crate) fn error_status(error: &WorkflowControlError) -> axum::http::StatusCode {
    match error.code.as_str() {
        "WORKFLOW_INVALID_INPUT" | "WORKFLOW_INVALID_SOURCE" => {
            axum::http::StatusCode::UNPROCESSABLE_ENTITY
        }
        "WORKFLOW_NOT_FOUND" | "SESSION_NOT_FOUND" | "WORKFLOW_NOT_SELECTED" => {
            axum::http::StatusCode::NOT_FOUND
        }
        "WORKFLOW_UNAUTHORIZED" => axum::http::StatusCode::FORBIDDEN,
        "WORKFLOW_BUSY" | "WORKFLOW_OPERATION_CONFLICT" | "WORKFLOW_STALE_REVISION" => {
            axum::http::StatusCode::CONFLICT
        }
        "WORKFLOW_RUNTIME_UNAVAILABLE" => axum::http::StatusCode::SERVICE_UNAVAILABLE,
        _ => axum::http::StatusCode::INTERNAL_SERVER_ERROR,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_handles_all_workflow_slash_forms() {
        assert!(matches!(
            parse_slash("workflow", "list"),
            Ok(Some(WorkflowCommand::List))
        ));
        assert!(matches!(
            parse_slash("workflow", "state"),
            Ok(Some(WorkflowCommand::State))
        ));
        assert!(matches!(
            parse_slash("workflow", "info plan"),
            Ok(Some(WorkflowCommand::Info { name })) if name == "plan"
        ));
        assert!(matches!(
            parse_slash("workflow", "use plan"),
            Ok(Some(WorkflowCommand::Select { name, expected_revision: None })) if name == "plan"
        ));
        assert!(matches!(
            parse_slash("workflow", "run plan request=build mode=fast"),
            Ok(Some(WorkflowCommand::Run { name: Some(name), inputs, .. }))
                if name == "plan"
                    && inputs.get("request") == Some(&"build".to_string())
                    && inputs.get("mode") == Some(&"fast".to_string())
        ));
    }

    #[test]
    fn parser_leaves_unrelated_commands_untouched() {
        assert!(matches!(parse_slash("compact", ""), Ok(None)));
        assert!(matches!(
            parse_slash("/workflow", "list"),
            Ok(Some(WorkflowCommand::List))
        ));
    }

    #[test]
    fn parser_rejects_invalid_workflow_syntax() {
        assert!(parse_slash("workflow", "").is_err());
        assert!(parse_slash("workflow", "list extra").is_err());
        assert!(parse_slash("workflow", "run plan request=").is_err());
        assert!(parse_slash("workflow", "run plan request=one request=two").is_err());
    }
}
