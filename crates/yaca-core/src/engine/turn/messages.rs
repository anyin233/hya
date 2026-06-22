use yaca_proto::{Message, ModelRef, Part, PartProjection, Projection, Role};
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
                parts: map_parts(&m.parts),
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
