use hya_proto::{
    Message, MessageProjection, ModelRef, Part, PartId, PartProjection, Projection, Role,
};
use hya_provider::{CompletionRequest, ReasoningEffort};
use serde_json::Value;

use crate::engine::AgentSpec;
use crate::runtime_registry::CompiledResourceView;

const COMPACT_CONTEXT_MARKER: &str = "HYA_COMPACTED_CONTEXT";

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
    resources: &CompiledResourceView,
) -> CompletionRequest {
    let model = active_model(agent, projection);
    let reasoning = reasoning_for_model(&model, agent.reasoning);
    CompletionRequest {
        tools: filtered_tool_schemas(resources, &model),
        model,
        system: Some(agent.system_prompt.clone()),
        messages,
        temperature: None,
        max_output_tokens: None,
        reasoning,
        headers: Default::default(),
    }
}

/// Resolve an explicit model-ref variant before the Agent's configured default.
pub(super) fn reasoning_for_model(
    model: &ModelRef,
    fallback: Option<ReasoningEffort>,
) -> Option<ReasoningEffort> {
    model
        .as_str()
        .rsplit_once('#')
        .and_then(|(_, variant)| ReasoningEffort::parse(variant))
        .or(fallback)
}

fn filtered_tool_schemas(
    resources: &CompiledResourceView,
    model: &ModelRef,
) -> Vec<hya_proto::ToolSchema> {
    resources
        .tool_schemas()
        .into_iter()
        .filter(|schema| include_tool(schema.name.as_str(), model.as_str()))
        .collect()
}

fn include_tool(id: &str, model: &str) -> bool {
    let gpt_model = model.contains("gpt-");
    let legacy_gpt = gpt_model && (model.contains("oss") || model.contains("gpt-4"));
    let patch_only = gpt_model && !legacy_gpt;
    match id {
        "apply_patch" => !legacy_gpt,
        "edit" | "write" => !patch_only,
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
            } => Part::Tool {
                id: *id,
                call_id: *call,
                name: name.clone(),
                state: state.clone(),
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use hya_tool::ToolRegistry;

    /// Pin explicit overrides and unchanged request effort for missing or invalid variants.
    #[test]
    fn model_variant_overrides_or_preserves_request_reasoning() {
        let original = Some(ReasoningEffort::Low);

        assert_eq!(
            reasoning_for_model(&ModelRef::new("fallback#high"), original),
            Some(ReasoningEffort::High),
        );
        assert_eq!(
            reasoning_for_model(&ModelRef::new("fallback"), original),
            original,
        );
        assert_eq!(
            reasoning_for_model(&ModelRef::new("fallback#unknown"), original),
            original,
        );
    }

    #[test]
    fn gpt_and_glm_advertise_exact_builtin_schema_sets() {
        let builtins = ToolRegistry::builtins()
            .schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(builtins.len(), 28);

        let gpt = builtins
            .iter()
            .filter(|name| include_tool(name, "12th-oai/gpt-5.6-sol"))
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(gpt.len(), 26);
        assert!(gpt.contains("apply_patch"));
        assert!(!gpt.contains("write"));
        assert!(!gpt.contains("edit"));

        let glm = builtins
            .iter()
            .filter(|name| include_tool(name, "12th-oai/glm-5.3"))
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(glm, builtins);
    }
}
