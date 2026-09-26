use hya_proto::{
    Message, MessageProjection, ModelRef, Part, PartId, PartProjection, Projection, Role,
};
use hya_provider::{CompletionRequest, ReasoningEffort};
use serde_json::Value;

use crate::engine::AgentSpec;
use crate::runtime_registry::CompiledResourceView;

use hya_provider::COMPACT_CONTEXT_MARKER;

/// Prompt image data by blob hash (`data:` URLs), loaded from the session's
/// blob table for the messages a request sends.
pub(super) type AttachmentData = std::collections::HashMap<String, String>;

/// Blob hashes (with their image type) of the prompt images in the messages
/// a request sends (after the latest compaction).
pub(super) fn attachment_blobs(projection: &Projection) -> Vec<(String, String)> {
    compacted_messages(projection)
        .iter()
        .filter(|message| message.role == Role::User)
        .flat_map(|message| crate::attachments::recorded_attachments(&message.files))
        .map(|attachment| (attachment.blob, attachment.mime))
        .collect()
}

pub(super) fn projection_to_messages(
    agent: &AgentSpec,
    projection: &Projection,
    model: &ModelRef,
    attachments: &AttachmentData,
) -> Vec<Message> {
    compacted_messages(projection)
        .iter()
        .filter(|m| !(m.role == Role::Assistant && m.parts.is_empty()))
        .map(|m| match m.role {
            Role::User => Message::User {
                id: m.id,
                parts: user_parts(m, attachments),
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
    messages: Vec<Message>,
    resources: &CompiledResourceView,
    model: &ModelRef,
    depth: u32,
) -> CompletionRequest {
    let reasoning = reasoning_for_model(model, agent.reasoning);
    let tools = filtered_tool_schemas(resources, model, depth);
    // The team quick reference teaches exactly the coordination tools this
    // request advertises (harness-allocated per agent and depth).
    let reference = crate::prompt::team_quick_reference(
        |name| tools.iter().any(|schema| schema.name.as_str() == name),
        depth,
    );
    let system = match reference {
        Some(reference) if agent.system_prompt.trim().is_empty() => reference,
        Some(reference) => format!("{}\n\n{reference}", agent.system_prompt.trim_end()),
        None => agent.system_prompt.clone(),
    };
    CompletionRequest {
        tools,
        model: model.clone(),
        system: Some(system),
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
    _model: &ModelRef,
    depth: u32,
) -> Vec<hya_proto::ToolSchema> {
    resources
        .tool_schemas()
        .into_iter()
        .filter(|schema| advertise_tool_at_depth(schema.name.as_str(), depth))
        .collect()
}

/// Whether a canonical builtin belongs in model-facing schemas.
///
/// Hashline `write` and `edit` are advertised to every model. `apply_patch`
/// stays registered for hidden `patch` dispatch but is never advertised.
pub fn advertise_tool(name: &str) -> bool {
    name != "apply_patch"
}

/// The orchestration plane (ADR-0015): spawn, discovery, workflow control,
/// archive search, and `archive`. Unadvertised for sessions at the hardcoded
/// depth cap — the bottom layer communicates, it does not orchestrate.
pub const ORCHESTRATION_TOOLS: &[&str] =
    &["task", "list_agents", "workflow", "search_agent", "archive"];

/// Depth-aware advertisement, two rules (ADR-0015 follow-up):
/// - depth 0 (the main agent) never sees `report`: reporting ends a
///   subagent's episode, and the main agent must deliver its answer as text.
/// - at [`crate::MAX_SUBAGENT_DEPTH`] the orchestration plane disappears from
///   the model-facing schema list.
pub fn advertise_tool_at_depth(name: &str, depth: u32) -> bool {
    advertise_tool(name)
        && !(depth == 0 && name == "report")
        && !(depth >= crate::MAX_SUBAGENT_DEPTH && ORCHESTRATION_TOOLS.contains(&name))
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
    if let Some(attachment) = crate::attachments::RecordedAttachment::from_entry(file) {
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
    fn orchestration_tools_disappear_at_the_depth_cap() {
        // Above the cap everything normal is advertised.
        assert!(advertise_tool_at_depth("task", 1));
        assert!(advertise_tool_at_depth("archive", 0));
        // At the hardcoded cap (ADR-0015) the orchestration plane vanishes…
        for name in ORCHESTRATION_TOOLS {
            assert!(
                !advertise_tool_at_depth(name, crate::MAX_SUBAGENT_DEPTH),
                "{name} must not be advertised at depth {}",
                crate::MAX_SUBAGENT_DEPTH
            );
        }
        // …while communication and coding tools stay.
        assert!(advertise_tool_at_depth("send", crate::MAX_SUBAGENT_DEPTH));
        assert!(advertise_tool_at_depth("bash", crate::MAX_SUBAGENT_DEPTH));
        // report belongs to subagents only: hidden at depth 0, present at
        // every subagent depth including the cap.
        assert!(
            !advertise_tool_at_depth("report", 0),
            "the main agent must never see the report schema"
        );
        assert!(advertise_tool_at_depth("report", 1));
        assert!(
            advertise_tool_at_depth("report", crate::MAX_SUBAGENT_DEPTH),
            "subagents at the depth cap still report to finish their episode"
        );
    }

    #[test]
    fn advertised_builtins_are_hashline_write_edit_without_apply_patch() {
        let builtins = ToolRegistry::builtins()
            .schemas()
            .into_iter()
            .map(|schema| schema.name.as_str().to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(builtins.len(), 28);

        let advertised = builtins
            .iter()
            .filter(|name| advertise_tool(name))
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(advertised.len(), 27);
        assert!(advertised.contains("write"));
        assert!(advertised.contains("edit"));
        assert!(!advertised.contains("apply_patch"));
    }
}
