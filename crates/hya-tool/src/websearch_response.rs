use serde_json::Value;

use crate::tool::ToolError;

pub(crate) fn parse_response(body: &str) -> Result<Option<String>, ToolError> {
    let trimmed = body.trim();
    if let Some(text) = parse_payload(trimmed)? {
        return Ok(Some(text));
    }
    for line in body.lines() {
        if let Some(payload) = line.strip_prefix("data: ")
            && let Some(text) = parse_payload(payload)?
        {
            return Ok(Some(text));
        }
    }
    Ok(None)
}

fn parse_payload(payload: &str) -> Result<Option<String>, ToolError> {
    let trimmed = payload.trim();
    if !trimmed.starts_with('{') {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(trimmed)?;
    Ok(value["result"]["content"]
        .as_array()
        .and_then(|items| items.iter().find_map(|item| item["text"].as_str()))
        .map(ToString::to_string))
}
