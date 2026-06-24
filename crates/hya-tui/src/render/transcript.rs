use std::collections::HashMap;

use hya_sdk::{Message, MessageStore, Part, Session, StoredPart};
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub(crate) struct TranscriptData<'a> {
    pub(crate) session: &'a Session,
    pub(crate) messages: &'a [Message],
    pub(crate) parts: &'a HashMap<String, Vec<StoredPart>>,
}

#[derive(Clone, Copy)]
pub(crate) struct TranscriptOptions {
    pub(crate) thinking: bool,
    pub(crate) tool_details: bool,
    pub(crate) assistant_metadata: bool,
}

impl Default for TranscriptOptions {
    fn default() -> Self {
        Self {
            thinking: true,
            tool_details: true,
            assistant_metadata: true,
        }
    }
}

pub(crate) fn format_transcript(data: TranscriptData<'_>, options: TranscriptOptions) -> String {
    let title = data
        .session
        .title
        .as_deref()
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(data.session.id.as_str());
    let mut transcript = format!("# {title}\n\n");
    transcript.push_str(&format!("**Session ID:** {}\n", data.session.id));
    transcript.push_str(&format!(
        "**Created:** {}\n",
        format_timestamp(session_time(data.session, "created"))
    ));
    transcript.push_str(&format!(
        "**Updated:** {}\n\n",
        format_timestamp(session_time(data.session, "updated"))
    ));
    transcript.push_str("---\n\n");

    for message in data.messages {
        transcript.push_str(&format_message(
            message,
            data.parts.get(&message.id).map_or(&[][..], Vec::as_slice),
            options,
        ));
        transcript.push_str("---\n\n");
    }

    transcript
}

pub(crate) fn format_store_transcript(
    store: &MessageStore,
    session_id: &str,
    options: TranscriptOptions,
) -> Option<String> {
    let session = store.session(session_id)?;
    Some(format_transcript(
        TranscriptData {
            session,
            messages: store
                .messages
                .get(session_id)
                .map_or(&[][..], Vec::as_slice),
            parts: &store.parts,
        },
        options,
    ))
}

pub(crate) fn format_message(
    message: &Message,
    parts: &[StoredPart],
    options: TranscriptOptions,
) -> String {
    let mut result = match message.role.as_deref() {
        Some("user") => "## User\n\n".to_owned(),
        Some("assistant") => format_assistant_header(message, options.assistant_metadata),
        _ => String::new(),
    };

    for part in parts {
        result.push_str(&format_part(part, options));
    }

    result
}

fn format_assistant_header(message: &Message, include_metadata: bool) -> String {
    if !include_metadata {
        return "## Assistant\n\n".to_owned();
    }

    let agent = titlecase(
        message
            .rest
            .get("agent")
            .and_then(Value::as_str)
            .unwrap_or("assistant"),
    );
    let Some(model) = message.rest.get("modelID").and_then(Value::as_str) else {
        return "## Assistant\n\n".to_owned();
    };
    let duration = match (message.time.completed, message.time.created) {
        (Some(completed), Some(created)) if completed > created => {
            format!(" · {:.1}s", (completed - created) as f64 / 1000.0)
        }
        _ => String::new(),
    };

    format!("## Assistant ({agent} · {model}{duration})\n\n")
}

fn format_part(part: &StoredPart, options: TranscriptOptions) -> String {
    match &part.inner {
        Part::Text { text, rest } => {
            if rest
                .get("synthetic")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return String::new();
            }
            format!("{text}\n\n")
        }
        Part::Reasoning { text, .. } if options.thinking => {
            format!("_Thinking:_\n\n{text}\n\n")
        }
        Part::Tool(tool) => format_tool_part(tool, options.tool_details),
        _ => String::new(),
    }
}

fn format_tool_part(tool: &hya_sdk::ToolPart, include_details: bool) -> String {
    let mut result = format!("**Tool: {}**\n", tool.tool.as_deref().unwrap_or_default());
    let Some(state) = &tool.state else {
        result.push('\n');
        return result;
    };
    if include_details {
        if let Some(input) = state.get("input").filter(|value| json_truthy(value)) {
            result.push_str(&format!(
                "\n**Input:**\n```json\n{}\n```\n",
                serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string())
            ));
        }
        if state.get("status").and_then(Value::as_str) == Some("completed") {
            if let Some(output) = state.get("output").filter(|value| json_truthy(value)) {
                let output = output
                    .as_str()
                    .map_or_else(|| output.to_string(), str::to_owned);
                result.push_str(&format!("\n**Output:**\n```\n{output}\n```\n"));
            }
        }
        if state.get("status").and_then(Value::as_str) == Some("error") {
            if let Some(error) = state.get("error").filter(|value| json_truthy(value)) {
                let error = error
                    .as_str()
                    .map_or_else(|| error.to_string(), str::to_owned);
                result.push_str(&format!("\n**Error:**\n```\n{error}\n```\n"));
            }
        }
    }
    result.push('\n');
    result
}

fn json_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn session_time(session: &Session, key: &str) -> Option<i64> {
    session
        .rest
        .get("time")
        .and_then(|time| time.get(key))
        .and_then(Value::as_i64)
}

fn format_timestamp(timestamp_ms: Option<i64>) -> String {
    let Some(timestamp_ms) = timestamp_ms else {
        return String::new();
    };
    let seconds = timestamp_ms.div_euclid(1000);
    match OffsetDateTime::from_unix_timestamp(seconds) {
        Ok(time) => time
            .format(&Rfc3339)
            .unwrap_or_else(|_| timestamp_ms.to_string()),
        Err(_) => timestamp_ms.to_string(),
    }
}

fn titlecase(value: &str) -> String {
    value
        .split_inclusive(|ch: char| !ch.is_alphanumeric())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{format_transcript, TranscriptData, TranscriptOptions};
    use hya_sdk::{Message, MessageStore, StoredPart};

    #[test]
    fn transcript_contains_session_and_message_text_when_user_and_assistant_exist() {
        let mut store = MessageStore::default();
        store.apply_event(&event(
            "session.updated",
            serde_json::json!({
                "info": {
                    "id": "ses_1234567890",
                    "title": "Export Demo",
                    "time": { "created": 1_700_000_000_000i64, "updated": 1_700_000_060_000i64 }
                }
            }),
        ));
        let messages = vec![message("msg_1", "user"), message("msg_2", "assistant")];
        store.messages.insert("ses_1234567890".to_owned(), messages);
        store.parts.insert(
            "msg_1".to_owned(),
            vec![part("prt_1", "msg_1", "What happened?")],
        );
        store.parts.insert(
            "msg_2".to_owned(),
            vec![part("prt_2", "msg_2", "Everything worked.")],
        );

        let session = store.session("ses_1234567890").expect("session exists");
        let messages = store
            .messages
            .get("ses_1234567890")
            .expect("messages exist");
        let transcript = format_transcript(
            TranscriptData {
                session,
                messages,
                parts: &store.parts,
            },
            TranscriptOptions::default(),
        );

        assert!(transcript.contains("# Export Demo"), "{transcript}");
        assert!(
            transcript.contains("**Session ID:** ses_1234567890"),
            "{transcript}"
        );
        assert!(transcript.contains("## User"), "{transcript}");
        assert!(transcript.contains("What happened?"), "{transcript}");
        assert!(transcript.contains("## Assistant"), "{transcript}");
        assert!(transcript.contains("Everything worked."), "{transcript}");
    }

    fn event(kind: &str, properties: serde_json::Value) -> hya_sdk::GlobalEvent {
        serde_json::from_value(serde_json::json!({
            "payload": { "type": kind, "properties": properties }
        }))
        .expect("decode event")
    }

    fn message(id: &str, role: &str) -> Message {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "sessionID": "ses_1234567890",
            "role": role,
            "time": { "created": 1_700_000_000_000i64, "completed": 1_700_000_001_000i64 },
            "agent": "build",
            "providerID": "anthropic",
            "modelID": "claude-sonnet-4"
        }))
        .expect("decode message")
    }

    fn part(id: &str, message_id: &str, text: &str) -> StoredPart {
        StoredPart::from_value(&serde_json::json!({
            "id": id,
            "messageID": message_id,
            "type": "text",
            "text": text
        }))
        .expect("decode part")
    }
}
