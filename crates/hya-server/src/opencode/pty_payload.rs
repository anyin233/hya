use serde_json::Value;

use crate::ApiError;

use super::pty_state::{CreatePayload, UpdatePayload};

pub(super) fn create(payload: Value, cwd: String) -> Result<CreatePayload, ApiError> {
    let object = payload
        .as_object()
        .ok_or_else(|| ApiError::bad_request("pty payload must be an object"))?;
    let command = optional_string(object, "command")?.unwrap_or_else(default_shell);
    let args = optional_string_array(object, "args")?.unwrap_or_default();
    let cwd = optional_string(object, "cwd")?.unwrap_or(cwd);
    let title = optional_string(object, "title")?.unwrap_or_else(|| command.clone());
    Ok(CreatePayload {
        command,
        args,
        cwd,
        title,
    })
}

pub(super) fn update(payload: Value) -> Result<UpdatePayload, ApiError> {
    let object = payload
        .as_object()
        .ok_or_else(|| ApiError::bad_request("pty payload must be an object"))?;
    if let Some(size) = object.get("size") {
        validate_size(size)?;
    }
    Ok(UpdatePayload {
        title: optional_string(object, "title")?,
    })
}

fn validate_size(size: &Value) -> Result<(), ApiError> {
    let object = size
        .as_object()
        .ok_or_else(|| ApiError::bad_request("pty size must be an object"))?;
    for field in ["rows", "cols"] {
        let value = object
            .get(field)
            .and_then(Value::as_u64)
            .ok_or_else(|| ApiError::bad_request(format!("pty size {field} must be positive")))?;
        if value == 0 {
            return Err(ApiError::bad_request(format!(
                "pty size {field} must be positive"
            )));
        }
    }
    Ok(())
}

fn optional_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<String>, ApiError> {
    match object.get(field) {
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ApiError::bad_request(format!(
            "pty {field} must be a string"
        ))),
        None => Ok(None),
    }
}

fn optional_string_array(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<Option<Vec<String>>, ApiError> {
    match object.get(field) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str().map(ToString::to_string).ok_or_else(|| {
                    ApiError::bad_request(format!("pty {field} must contain strings"))
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(ApiError::bad_request(format!(
            "pty {field} must be an array"
        ))),
        None => Ok(None),
    }
}

fn default_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}
