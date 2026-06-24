use serde_json::Value;
use yaca_proto::{
    Message, MessageProjection, ModelRef, Part, PartId, PartProjection, Projection, Role,
};
use yaca_provider::{CompletionRequest, ProviderRouter};
use yaca_tool::ToolRegistry;

use crate::engine::AgentSpec;

const COMPACT_CONTEXT_MARKER: &str = "YACA_COMPACTED_CONTEXT";

pub(super) fn projection_to_messages(agent: &AgentSpec, projection: &Projection) -> Vec<Message> {
    let model = active_model(agent, projection);
    compacted_messages(projection)
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
                tokens: m.tokens,
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
    providers: &ProviderRouter,
) -> CompletionRequest {
    let model = active_model(agent, projection);
    let provider = providers.resolve(&model);
    CompletionRequest {
        tools: filtered_tool_schemas(
            tools,
            provider.as_deref().map(yaca_provider::Provider::id),
            &model,
        ),
        model,
        system: Some(agent.system_prompt.clone()),
        messages,
        temperature: None,
        max_output_tokens: None,
        reasoning: agent.reasoning,
        headers: Default::default(),
    }
}

fn filtered_tool_schemas(
    tools: &ToolRegistry,
    provider_id: Option<&str>,
    model: &ModelRef,
) -> Vec<yaca_proto::ToolSchema> {
    tools
        .schemas()
        .into_iter()
        .filter(|schema| include_tool(schema.name.as_str(), provider_id, model.as_str()))
        .collect()
}

fn include_tool(id: &str, provider_id: Option<&str>, model: &str) -> bool {
    let use_patch = model.contains("gpt-") && !model.contains("oss") && !model.contains("gpt-4");
    match id {
        "apply_patch" => use_patch,
        "edit" | "write" => !use_patch,
        "websearch" => provider_id == Some("opencode"),
        _ => true,
    }
}

fn active_model(agent: &AgentSpec, projection: &Projection) -> ModelRef {
    projection
        .session
        .model
        .clone()
        .unwrap_or_else(|| agent.model.clone())
}

fn compacted_messages(projection: &Projection) -> &[MessageProjection] {
    let start = projection
        .session
        .messages
        .iter()
        .rposition(|message| {
            message.role == Role::System
                && collect_text(&message.parts).starts_with(COMPACT_CONTEXT_MARKER)
        })
        .unwrap_or(0);
    &projection.session.messages[start..]
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
