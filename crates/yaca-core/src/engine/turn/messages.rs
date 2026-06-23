use serde_json::Value;
use yaca_proto::{
    Message, MessageProjection, ModelRef, Part, PartId, PartProjection, Projection, Role,
};
use yaca_provider::CompletionRequest;
use yaca_tool::ToolRegistry;

use crate::engine::AgentSpec;

pub(super) fn projection_to_messages(agent: &AgentSpec, projection: &Projection) -> Vec<Message> {
    let model = active_model(agent, projection);
    projection
        .session
        .messages
        .iter()
        .filter(|m| !(m.role == Role::Assistant && m.parts.is_empty()))
        .map(|m| match m.role {
            Role::User => Message::User {
                id: m.id,
                parts: user_parts(m),
            },
            Role::Assistant => Message::Assistant {
                id: m.id,
                agent: agent.name.clone(),
                model: model.clone(),
                parts: map_parts(&m.parts),
                finish: m.finish,
                tokens: None,
            },
            Role::System => Message::System {
                id: m.id,
                content: collect_text(&m.parts),
            },
        })
        .collect()
}

pub(super) fn request_from_messages(
    agent: &AgentSpec,
    projection: &Projection,
    messages: Vec<Message>,
    tools: &ToolRegistry,
) -> CompletionRequest {
    CompletionRequest {
        model: active_model(agent, projection),
        system: Some(agent.system_prompt.clone()),
        messages,
        tools: tools.schemas(),
        temperature: None,
        max_output_tokens: None,
        reasoning: agent.reasoning,
        headers: Default::default(),
    }
}

fn active_model(agent: &AgentSpec, projection: &Projection) -> ModelRef {
    projection
        .session
        .model
        .clone()
        .unwrap_or_else(|| agent.model.clone())
}

fn collect_text(parts: &[PartProjection]) -> String {
    let mut s = String::new();
    for p in parts {
        if let PartProjection::Text { text, .. } = p {
            s.push_str(text);
        }
    }
    s
}

fn user_parts(message: &MessageProjection) -> Vec<Part> {
    let mut parts = map_parts(&message.parts);
    parts.extend(message.files.iter().filter_map(media_part));
    parts
}

fn media_part(file: &Value) -> Option<Part> {
    let media_type = file.get("mime").and_then(Value::as_str)?;
    let data = file.get("uri").and_then(Value::as_str)?;
    Some(Part::Media {
        id: PartId::new(),
        media_type: media_type.to_string(),
        data: data.to_string(),
        filename: file.get("name").and_then(Value::as_str).map(str::to_string),
    })
}

fn map_parts(parts: &[PartProjection]) -> Vec<Part> {
    parts
        .iter()
        .filter_map(|p| match p {
            PartProjection::Text { id, text } => Some(Part::Text {
                id: *id,
                text: text.clone(),
            }),
            PartProjection::Tool {
                id,
                call,
                name,
                state,
            } => Some(Part::Tool {
                id: *id,
                call_id: *call,
                name: name.clone(),
                state: state.clone(),
            }),
            PartProjection::Reasoning { .. } => None,
        })
        .collect()
}
