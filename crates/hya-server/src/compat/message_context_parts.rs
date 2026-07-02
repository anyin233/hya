use hya_proto::{MessageId, MessageProjection, PartId, SessionId};
use serde_json::{Value, json};

pub(super) fn prompt_attachment_parts(
    session: SessionId,
    message: &MessageProjection,
) -> Vec<Value> {
    let files = message
        .files
        .iter()
        .enumerate()
        .filter_map(|(index, file)| {
            file_part(
                session,
                message.id,
                message.id.as_uuid(),
                "file",
                index,
                file,
            )
        });
    let agents = message
        .agents
        .iter()
        .enumerate()
        .filter_map(|(index, agent)| agent_part(session, message.id, index, agent));
    files.chain(agents).collect()
}

pub(super) fn tool_attachment_parts(
    session: SessionId,
    message: MessageId,
    part: PartId,
    output: &Value,
) -> Option<Vec<Value>> {
    let attachments = output.get("attachments")?.as_array()?;
    let parts = attachments
        .iter()
        .enumerate()
        .filter_map(|(index, file)| {
            file_part(session, message, part.as_uuid(), "tool_file", index, file)
        })
        .collect::<Vec<_>>();
    (!parts.is_empty()).then_some(parts)
}

fn file_part(
    session: SessionId,
    message: MessageId,
    seed: uuid::Uuid,
    kind: &str,
    index: usize,
    file: &Value,
) -> Option<Value> {
    let url = file
        .get("uri")
        .or_else(|| file.get("url"))
        .and_then(Value::as_str)?;
    let mime = file.get("mime").and_then(Value::as_str)?;
    let filename = file
        .get("name")
        .or_else(|| file.get("filename"))
        .and_then(Value::as_str);
    let mut part = json!({
        "id": derived_part_id(seed, kind, index),
        "sessionID": session.to_string(),
        "messageID": message.to_string(),
        "type": "file",
        "mime": mime,
        "url": url,
    });
    if let Some(filename) = filename {
        part["filename"] = json!(filename);
    }
    if let Some(source) = file_source(file, filename.unwrap_or(url)) {
        part["source"] = source;
    }
    Some(part)
}

fn agent_part(
    session: SessionId,
    message: MessageId,
    index: usize,
    agent: &Value,
) -> Option<Value> {
    let name = agent.get("name").and_then(Value::as_str)?;
    let mut part = json!({
        "id": derived_part_id(message.as_uuid(), "agent", index),
        "sessionID": session.to_string(),
        "messageID": message.to_string(),
        "type": "agent",
        "name": name,
    });
    if let Some(source) = source_span(agent) {
        part["source"] = source;
    }
    Some(part)
}

fn file_source(file: &Value, path: &str) -> Option<Value> {
    Some(json!({
        "type": "file",
        "path": path,
        "text": source_span(file)?,
    }))
}

fn source_span(value: &Value) -> Option<Value> {
    let source = value.get("source")?;
    let text = source.get("text").and_then(Value::as_str)?;
    let start = source.get("start").filter(|value| value.is_number())?;
    let end = source.get("end").filter(|value| value.is_number())?;
    Some(json!({
        "value": text,
        "start": start.clone(),
        "end": end.clone(),
    }))
}

fn derived_part_id(seed: uuid::Uuid, kind: &str, index: usize) -> String {
    let mut bytes = *seed.as_bytes();
    bytes[0] ^= match kind {
        "file" => 0xf1,
        "agent" => 0xa6,
        "tool_file" => 0x7c,
        _ => 0x1d,
    };
    for (offset, byte) in index.to_le_bytes().iter().enumerate().take(8) {
        bytes[8 + offset] ^= *byte;
    }
    format!("part_{}", uuid::Uuid::from_bytes(bytes).simple())
}
