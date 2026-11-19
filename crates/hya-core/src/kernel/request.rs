use hya_proto::{
    Message, MessageProjection, ModelRef, Part, PartId, PartProjection, Projection, Role,
    ToolPartState,
};
use hya_provider::{COMPACT_CONTEXT_MARKER, CompletionRequest, ReasoningEffort};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::View;
use super::rewrite::is_goal_checkpoint;
use crate::CoreError;

/// Prompt image data by blob hash (`data:` URLs), loaded from the session's
/// blob table for the messages a request sends.
pub type AttachmentData = std::collections::HashMap<String, String>;

/// Build a model request from a view and resolved harness inputs.
///
/// Evicted tool outputs carry the notice recorded in the projection.
pub fn request(
    view: &View<'_>,
    system: &str,
    tools: Vec<hya_proto::ToolSchema>,
    model: &ModelRef,
    agent: &hya_proto::AgentName,
    attachments: &AttachmentData,
    reasoning: Option<ReasoningEffort>,
) -> CompletionRequest {
    CompletionRequest {
        tools,
        model: model.clone(),
        system: Some(system.to_string()),
        messages: projection_to_messages(agent, view.projection, model, attachments),
        temperature: None,
        max_output_tokens: None,
        reasoning,
        headers: Default::default(),
    }
}

/// Lowercase hex SHA-256 of the compact JSON array `[system, tools, messages]`
/// with object keys sorted recursively and array order preserved.
pub fn request_sha256(request: &CompletionRequest) -> Result<String, CoreError> {
    let mut content = serde_json::to_value((&request.system, &request.tools, &request.messages))
        .map_err(|error| CoreError::Invalid(format!("request content: {error}")))?;
    content.sort_all_objects();
    Ok(format!("{:x}", Sha256::digest(content.to_string())))
}

/// Render visible messages with the resolved agent, model, and image data.
pub fn projection_to_messages(
    agent: &hya_proto::AgentName,
    projection: &Projection,
    model: &ModelRef,
    attachments: &AttachmentData,
) -> Vec<Message> {
    let messages = compacted_messages(projection);
    let assistant = messages.iter().rfind(|row| row.role == Role::Assistant);
    let marker = messages.first().filter(|row| is_marker(row));
    let bare = agent.as_str() == super::AGENT
        && assistant.is_some_and(|row| row.config_generation.is_none());
    let deadline_stage = |row: &MessageProjection| {
        if !bare || row.role != Role::System {
            return false;
        }
        let text = collect_text(&row.parts);
        ["TIME: ", "FINAL ", "clock: "].iter().any(|kind| {
            text.strip_prefix("HEADLESS RUN — ")
                .is_some_and(|rest| rest.starts_with(kind))
        })
    };
    messages
        .iter()
        .filter(|row| {
            !(is_goal_checkpoint(row) || deadline_stage(row))
                || (marker.is_none_or(|marker| marker.id < row.id)
                    && assistant.is_some_and(|assistant| {
                        // The marker can move in the projection. These IDs are
                        // minted in creation order by the running turn.
                        assistant.id < row.id
                            && sent_parts(assistant)
                                .iter()
                                .all(|part| part.id().as_uuid() < row.id.as_uuid())
                    }))
        })
        .filter(|m| !(m.role == Role::Assistant && sent_parts(m).is_empty()))
        .map(|m| match m.role {
            Role::User => Message::User {
                id: m.id,
                parts: user_parts(m, attachments),
            },
            Role::Assistant => Message::Assistant {
                id: m.id,
                agent: agent.clone(),
                model: model.clone(),
                parts: map_parts(sent_parts(m)),
                finish: m.finish,
                tokens: m.tokens,
                last_round: m.usage.as_ref().map(|usage| usage.last_round),
            },
            // Keep the changing clock in the conversation on providers that
            // hoist system messages into the top-level instructions.
            Role::System if deadline_stage(m) => Message::User {
                id: m.id,
                parts: map_parts(&m.parts),
            },
            Role::System => Message::System {
                id: m.id,
                content: collect_text(&m.parts),
            },
        })
        .collect()
}

fn sent_parts(message: &MessageProjection) -> &[PartProjection] {
    &message.parts[message.folded_parts..]
}

/// Messages beginning at the latest context-compaction marker.
pub fn compacted_messages(projection: &Projection) -> &[MessageProjection] {
    let start = projection
        .session
        .messages
        .iter()
        .rposition(is_marker)
        .unwrap_or(0);
    &projection.session.messages[start..]
}

/// Whether `message` is a context-compaction marker.
pub(super) fn is_marker(message: &MessageProjection) -> bool {
    message.role == Role::System && collect_text(&message.parts).starts_with(COMPACT_CONTEXT_MARKER)
}

pub(super) fn collect_text(parts: &[PartProjection]) -> String {
    let mut s = String::new();
    for p in parts {
        if let PartProjection::Text { text, .. } = p {
            s.push_str(text);
        }
    }
    s
}

fn user_parts(message: &MessageProjection, attachments: &AttachmentData) -> Vec<Part> {
    let mut parts = map_parts(&message.parts);
    parts.extend(
        message
            .files
            .iter()
            .filter_map(|file| media_part(file, attachments)),
    );
    parts
}

fn media_part(file: &Value, attachments: &AttachmentData) -> Option<Part> {
    if let Some(attachment) = RecordedAttachment::from_entry(file) {
        // A prompt image: its bytes live in the session blob table.
        let Some(data) = attachments.get(&attachment.blob) else {
            tracing::warn!(
                blob = %attachment.blob,
                name = %attachment.name,
                "prompt image blob missing; the image is left out of the request"
            );
            return None;
        };
        return Some(Part::Media {
            id: attachment.part.parse().unwrap_or_else(|_| PartId::new()),
            media_type: attachment.mime,
            data: data.clone(),
            filename: Some(attachment.name),
        });
    }
    let media_type = file.get("mime").and_then(Value::as_str)?;
    let data = file
        .get("uri")
        .and_then(Value::as_str)
        .or_else(|| file.get("url").and_then(Value::as_str))?;
    if is_context_reference(file, media_type) {
        return None;
    }
    Some(Part::Media {
        id: PartId::new(),
        media_type: media_type.to_string(),
        data: data.to_string(),
        filename: file
            .get("name")
            .and_then(Value::as_str)
            .or_else(|| file.get("filename").and_then(Value::as_str))
            .map(str::to_string),
    })
}

fn is_context_reference(file: &Value, media_type: &str) -> bool {
    media_type.starts_with("text/")
        || media_type == "application/x-directory"
        || file.pointer("/source/type").and_then(Value::as_str) == Some("resource")
}

/// Convert stored parts without changing their provider payloads.
pub fn map_parts(parts: &[PartProjection]) -> Vec<Part> {
    parts
        .iter()
        .map(|p| match p {
            PartProjection::Text { id, text } => Part::Text {
                id: *id,
                text: text.clone(),
            },
            PartProjection::Reasoning {
                id,
                text,
                provider_data,
                ..
            } => Part::Reasoning {
                id: *id,
                text: text.clone(),
                provider_data: provider_data.clone(),
            },
            PartProjection::Tool {
                id,
                call,
                name,
                state,
                eviction_notice,
            } => Part::Tool {
                id: *id,
                call_id: *call,
                name: name.clone(),
                state: match (state, eviction_notice) {
                    (ToolPartState::Completed { input, time_ms, .. }, Some(notice)) => {
                        ToolPartState::Completed {
                            input: input.clone(),
                            output: Value::String(notice.clone()),
                            time_ms: *time_ms,
                        }
                    }
                    _ => state.clone(),
                },
            },
        })
        .collect()
}

/// A prompt image as recorded on a user message (no bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedAttachment {
    /// Transcript part id.
    pub part: String,
    /// File name.
    pub name: String,
    /// Image type.
    pub mime: String,
    /// Size in bytes.
    pub size: u64,
    /// sha256 of the bytes (the session blob key).
    pub blob: String,
    /// Client-side path, when sent.
    pub path: Option<String>,
}

impl RecordedAttachment {
    /// Parse one `UserPromptContextRecorded.files` entry; `None` for entries
    /// that are not prompt images (for example `@file` context).
    #[must_use]
    pub fn from_entry(entry: &Value) -> Option<Self> {
        if entry.get("type").and_then(Value::as_str) != Some("image_attachment") {
            return None;
        }
        let text = |key: &str| entry.get(key).and_then(Value::as_str).map(str::to_string);
        Some(Self {
            part: text("part")?,
            name: text("name")?,
            mime: text("mime")?,
            size: entry.get("size").and_then(Value::as_u64).unwrap_or(0),
            blob: text("blob")?,
            path: text("path"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_keeps_in_flight_message_after_compaction_marker() {
        use hya_proto::{AgentName, CompactionStrategy, Envelope, Event, EventSeq, SessionKind};
        let session = hya_proto::SessionId::new();
        let user = hya_proto::MessageId::new();
        let assistant = hya_proto::MessageId::new();
        let marker = hya_proto::MessageId::new();
        let marker_part = hya_proto::PartId::new();
        let assistant_part = hya_proto::PartId::new();
        let env = |seq, event| Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event,
        };
        let projection = Projection::from_events(&[
            env(
                1,
                Event::SessionCreated {
                    session,
                    parent: None,
                    agent: AgentName::new("build"),
                    model: ModelRef::new("fake"),
                    workdir: "/tmp".to_string(),
                    project: None,
                    kind: SessionKind::Project,
                },
            ),
            env(
                2,
                Event::MessageStarted {
                    session,
                    message: user,
                    role: Role::User,
                    agent: None,
                    model: None,
                },
            ),
            env(
                3,
                Event::MessageStarted {
                    session,
                    message: assistant,
                    role: Role::Assistant,
                    agent: Some(AgentName::new("build")),
                    model: Some(ModelRef::new("fake")),
                },
            ),
            env(
                4,
                Event::MessageStarted {
                    session,
                    message: marker,
                    role: Role::System,
                    agent: None,
                    model: None,
                },
            ),
            env(
                5,
                Event::TextStart {
                    session,
                    message: marker,
                    part: marker_part,
                },
            ),
            env(
                6,
                Event::TextDelta {
                    session,
                    message: marker,
                    part: marker_part,
                    delta: "HYA_COMPACTED_CONTEXT\nsummary".to_string(),
                },
            ),
            env(
                7,
                Event::ContextCompacted {
                    session,
                    message: marker,
                    strategy: CompactionStrategy::LocalSummarizer,
                    from_message: user,
                    to_message: user,
                    folded_count: 1,
                    input_tokens_est: 100,
                    threshold: 75,
                    step_cut: None,
                },
            ),
            env(
                8,
                Event::TextStart {
                    session,
                    message: assistant,
                    part: assistant_part,
                },
            ),
            env(
                9,
                Event::TextDelta {
                    session,
                    message: assistant,
                    part: assistant_part,
                    delta: "retained in-flight work".to_string(),
                },
            ),
        ]);
        let agent = AgentName::new("build");

        let messages = projection_to_messages(
            &agent,
            &projection,
            &ModelRef::new("fake"),
            &AttachmentData::new(),
        );
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            &messages[1],
            Message::Assistant { parts, .. }
                if parts.iter().any(|part| matches!(
                    part,
                    Part::Text { text, .. } if text == "retained in-flight work"
                ))
        ));
    }

    /// After a step cut the request is `[marker, prompt, kept steps]`: folded
    /// parts are left out whole, and every kept reasoning part goes out exactly
    /// as stored, `provider_data` included.
    #[test]
    fn request_skips_step_folded_parts_and_replays_kept_reasoning() {
        use hya_proto::{
            AgentName, CompactionStrategy, Envelope, Event, EventSeq, MessageId, PartId,
            SessionKind, StepCut,
        };
        let session = hya_proto::SessionId::new();
        let (user, assistant, marker) = (MessageId::new(), MessageId::new(), MessageId::new());
        let user_part = PartId::new();
        let marker_part = PartId::new();
        let steps: Vec<PartId> = (0..3).map(|_| PartId::new()).collect();
        let started = |message, role| Event::MessageStarted {
            session,
            message,
            role,
            agent: None,
            model: None,
        };
        let text = |message, part, delta: &str| {
            [
                Event::TextStart {
                    session,
                    message,
                    part,
                },
                Event::TextDelta {
                    session,
                    message,
                    part,
                    delta: delta.to_string(),
                },
            ]
        };
        let mut events = vec![Event::SessionCreated {
            session,
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("fake"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: SessionKind::Project,
        }];
        events.push(started(user, Role::User));
        events.extend(text(user, user_part, "do the task"));
        events.push(started(assistant, Role::Assistant));
        for (step, part) in steps.iter().enumerate() {
            events.extend([
                Event::ReasoningStart {
                    session,
                    message: assistant,
                    part: *part,
                    reason: None,
                },
                Event::ReasoningDelta {
                    session,
                    message: assistant,
                    part: *part,
                    delta: format!("thinking {step}"),
                },
                Event::ReasoningEnd {
                    session,
                    message: assistant,
                    part: *part,
                    provider_data: Some(serde_json::json!({
                        "type": "reasoning",
                        "encrypted_content": format!("opaque {step}"),
                    })),
                },
            ]);
        }
        events.push(started(marker, Role::System));
        events.extend(text(marker, marker_part, "HYA_COMPACTED_CONTEXT\nsummary"));
        events.push(Event::ContextCompacted {
            session,
            message: marker,
            strategy: CompactionStrategy::LocalSummarizer,
            from_message: assistant,
            to_message: assistant,
            folded_count: 1,
            input_tokens_est: 100,
            threshold: 75,
            step_cut: Some(StepCut {
                through_part: steps[0],
                kept_head: user,
            }),
        });
        let envelopes: Vec<_> = events
            .into_iter()
            .enumerate()
            .map(|(seq, event)| Envelope {
                seq: EventSeq(seq as u64 + 1),
                ts_millis: 0,
                event,
            })
            .collect();
        let projection = Projection::from_events(&envelopes);
        let agent = AgentName::new("build");

        let messages = projection_to_messages(
            &agent,
            &projection,
            &ModelRef::new("fake"),
            &AttachmentData::new(),
        );
        let ids: Vec<_> = messages.iter().map(Message::id).collect();
        assert_eq!(ids, vec![marker, user, assistant]);
        let Message::Assistant { parts, .. } = &messages[2] else {
            panic!("expected the in-flight assistant");
        };
        let stored = &projection.session.messages[2].parts;
        assert_eq!(stored.len(), 3, "the projection keeps the folded step");
        assert_eq!(parts, &map_parts(&stored[1..]));
        assert!(parts.iter().all(|part| matches!(
            part,
            Part::Reasoning { provider_data: Some(data), .. } if data["encrypted_content"] != "opaque 0"
        )));
    }
}
