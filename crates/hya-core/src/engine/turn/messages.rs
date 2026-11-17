use hya_proto::{Message, ModelRef, Projection, Role};
use hya_provider::ReasoningEffort;

use crate::engine::AgentSpec;
use crate::runtime_registry::CompiledResourceView;

pub(super) use crate::kernel::request::AttachmentData;
use crate::kernel::request::compacted_messages;
pub(crate) use crate::kernel::request::map_parts;

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
    crate::kernel::request::projection_to_messages(&agent.name, projection, model, attachments)
}

/// The agent's system prompt plus the team quick reference for exactly the
/// coordination tools in `tools` (harness-allocated per agent and depth).
pub(super) fn system_prompt(
    agent: &AgentSpec,
    tools: &[hya_proto::ToolSchema],
    depth: u32,
) -> String {
    let reference = crate::prompt::team_quick_reference(
        |name| tools.iter().any(|schema| schema.name.as_str() == name),
        depth,
    );
    match reference {
        Some(reference) if agent.system_prompt.trim().is_empty() => reference,
        Some(reference) => format!("{}\n\n{reference}", agent.system_prompt.trim_end()),
        None => agent.system_prompt.clone(),
    }
}

/// Effort for a cross-model fallback attempt: the candidate's own `#variant`
/// wins; otherwise the effort resolved for the preferred model carries over.
///
/// A `#suffix` is an explicit choice: a valid variant wins, and an invalid one
/// sends no effort rather than silently inheriting the default.
pub(super) fn reasoning_for_model(
    model: &ModelRef,
    fallback: Option<ReasoningEffort>,
) -> Option<ReasoningEffort> {
    match model.as_str().rsplit_once('#') {
        Some((_, variant)) => ReasoningEffort::parse(variant),
        None => fallback,
    }
}

pub(super) fn filtered_tool_schemas(
    resources: &CompiledResourceView,
    interaction_available: bool,
    depth: u32,
    exclusive: Option<&str>,
) -> Vec<hya_proto::ToolSchema> {
    let offered = offered_tool_schemas(resources, depth, interaction_available);
    let Some(exclusive) = exclusive_in_effect(&offered, exclusive) else {
        return offered;
    };
    let (mut shown, hidden): (Vec<_>, Vec<_>) = offered
        .into_iter()
        .partition(|schema| [exclusive, "report"].contains(&schema.name.as_str()));
    if let Some(tool) = shown
        .iter_mut()
        .find(|schema| schema.name.as_str() == exclusive)
    {
        tool.description.push_str(&hidden_tools_section(&hidden));
    }
    shown
}

fn offered_tool_schemas(
    resources: &CompiledResourceView,
    depth: u32,
    interaction_available: bool,
) -> Vec<hya_proto::ToolSchema> {
    resources
        .tool_schemas()
        .into_iter()
        .filter(|schema| {
            let name = schema.name.as_str();
            advertise_tool_at_depth(name, depth)
                && resources.resolve_tool(name).is_some_and(|resolved| {
                    super::tool_available(resolved.tool.name(), interaction_available)
                })
                && (resources.advertises(name) || (depth >= 1 && name == "report"))
        })
        .collect()
}

fn exclusive_in_effect<'a>(
    offered: &[hya_proto::ToolSchema],
    exclusive: Option<&'a str>,
) -> Option<&'a str> {
    exclusive.filter(|name| offered.iter().any(|schema| schema.name.as_str() == *name))
}

/// The tool a model call of `name` has to go through instead, when the
/// agent's exclusive tool hides `name` from the model.
pub(super) fn hidden_behind<'a>(
    resources: &CompiledResourceView,
    depth: u32,
    exclusive: Option<&'a str>,
    interaction_available: bool,
    name: &str,
) -> Option<&'a str> {
    let offered = offered_tool_schemas(resources, depth, interaction_available);
    exclusive_in_effect(&offered, exclusive).filter(|tool| ![*tool, "report"].contains(&name))
}

/// What the exclusive tool's description says about the tools the model is
/// not offered: they are its only way to learn their names and arguments.
fn hidden_tools_section(hidden: &[hya_proto::ToolSchema]) -> String {
    let mut section = String::from(
        "\n\nYou are offered no other tool. These tools exist only inside this one; each is listed with its description and the JSON Schema of its arguments.",
    );
    for schema in hidden {
        section.push_str(&format!(
            "\n\n### {}\n{}\nArguments: {}",
            schema.name.as_str(),
            schema.description.trim_end(),
            schema.input_schema
        ));
    }
    section
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

/// The parts a request sends: those a step cut folded stay in the
/// projection but are represented by the compaction summary.
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    use hya_tool::ToolRegistry;

    fn bundle_advertise_view(
        fields: &str,
    ) -> Result<std::sync::Arc<CompiledResourceView>, Box<dyn std::error::Error>> {
        use crate::{AgentCatalog, RuntimeRegistry};
        use hya_bundle::{BundleCatalog, BundleSource, SourceFile, prepare_package};

        let prepared = prepare_package(BundleSource::new(
            "advertise",
            vec![SourceFile::new(
                "bundle.yaml",
                format!(
                    "kind: Bundle\nidentity: {{ id: acme/advertise, version: 1.0.0, publisher: acme }}\nagents:\n  - id: selective-agent\n    role: main\n    can_spawn: [hya-scout]\n    {fields}\n"
                ),
            )],
        ))?;
        let catalog = AgentCatalog::new(std::sync::Arc::new(
            BundleCatalog::from_verified_catalogs(&[&prepared])?,
        ))?;
        let registry = RuntimeRegistry::new(ToolRegistry::builtins(), std::sync::Arc::new(catalog));
        let binding = registry.bind_turn(std::path::Path::new("/tmp/hya-advertise"))?;
        let policy = binding.agent_resource_policy("selective-agent")?;
        Ok(binding.compile_agent_resources(&policy)?)
    }

    #[test]
    fn advertise_limits_model_schemas_and_keeps_the_executable_view()
    -> Result<(), Box<dyn std::error::Error>> {
        let resources = bundle_advertise_view(
            "advertise: [bash]\n    resource_view: { allow: ['harness:tool/bash', 'harness:tool/read'] }",
        )?;
        for (depth, expected) in [
            (0, vec!["bash"]),
            (1, vec!["bash", "report"]),
            (crate::MAX_SUBAGENT_DEPTH, vec!["bash", "report"]),
        ] {
            let mut names = filtered_tool_schemas(&resources, true, depth, None)
                .into_iter()
                .map(|schema| schema.name.to_string())
                .collect::<Vec<_>>();
            names.sort();
            assert_eq!(names, expected, "depth {depth}");
        }
        for name in ["bash", "read", "wait", "task", "archive", "report"] {
            assert!(
                resources.resolve_tool(name).is_some(),
                "{name} stays executable"
            );
        }
        for schema in resources.tool_schemas() {
            assert!(resources.resolve_tool(schema.name.as_str()).is_some());
        }
        Ok(())
    }

    #[test]
    fn advertise_uses_the_views_public_tool_names() -> Result<(), Box<dyn std::error::Error>> {
        let resources = bundle_advertise_view(
            "advertise: [run]\n    resource_view: { allow: ['harness:tool/bash'], aliases: { run: 'harness:tool/bash' } }",
        )?;
        let names = filtered_tool_schemas(&resources, true, 0, None)
            .into_iter()
            .map(|schema| schema.name.to_string())
            .collect::<Vec<_>>();
        assert_eq!(names, ["run"]);
        assert!(resources.resolve_tool("run").is_some());
        assert!(resources.resolve_tool("wait").is_some());
        Ok(())
    }

    #[test]
    fn exclusive_tool_is_offered_alone_and_describes_the_hidden_tools()
    -> Result<(), Box<dyn std::error::Error>> {
        let resources = bundle_advertise_view(
            "resource_view: { allow: ['harness:tool/bash', 'harness:tool/read'] }",
        )?;
        for (depth, expected) in [(0, vec!["bash"]), (1, vec!["bash", "report"])] {
            let offered = filtered_tool_schemas(&resources, true, depth, Some("bash"));
            let mut names = offered
                .iter()
                .map(|schema| schema.name.as_str())
                .collect::<Vec<_>>();
            names.sort_unstable();
            assert_eq!(names, expected, "depth {depth}");
            let bash = offered
                .iter()
                .find(|schema| schema.name.as_str() == "bash")
                .ok_or("bash is offered")?;
            let read = resources
                .resolve_tool("read")
                .ok_or("read stays executable")?
                .tool
                .schema();
            assert!(bash.description.contains("\n\n### read\n"));
            assert!(bash.description.contains(&read.input_schema.to_string()));
            assert!(!bash.description.contains("### bash"));
            assert!(!bash.description.contains("### report"));
        }
        Ok(())
    }

    #[test]
    fn headless_surface_omits_human_tools_and_their_exclusive_descriptions()
    -> Result<(), Box<dyn std::error::Error>> {
        for aliases in [
            "",
            ", aliases: { ask: 'harness:tool/ask_user', approve: 'harness:tool/plan_exit' }",
        ] {
            let resources = bundle_advertise_view(&format!(
                "resource_view: {{ allow: ['harness:tool/bash', 'harness:tool/ask_user', 'harness:tool/plan_exit']{aliases} }}"
            ))?;
            let human_tools = if aliases.is_empty() {
                ["ask_user", "plan_exit"]
            } else {
                ["ask", "approve"]
            };
            for depth in [0, 1, crate::MAX_SUBAGENT_DEPTH] {
                for interactive in [false, true] {
                    let offered = filtered_tool_schemas(&resources, interactive, depth, None);
                    for name in human_tools {
                        assert_eq!(
                            offered.iter().any(|tool| tool.name.as_str() == name),
                            interactive
                        );
                        assert!(
                            resources.resolve_tool(name).is_some(),
                            "dispatch is unchanged"
                        );
                    }
                    let exclusive =
                        filtered_tool_schemas(&resources, interactive, depth, Some("bash"));
                    let bash = exclusive
                        .iter()
                        .find(|tool| tool.name.as_str() == "bash")
                        .ok_or("bash is offered")?;
                    for name in human_tools {
                        assert_eq!(
                            bash.description.contains(&format!("### {name}\n")),
                            interactive
                        );
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn a_hidden_tool_names_the_exclusive_tool_it_is_behind()
    -> Result<(), Box<dyn std::error::Error>> {
        let resources = bundle_advertise_view(
            "resource_view: { allow: ['harness:tool/bash', 'harness:tool/read'] }",
        )?;
        assert_eq!(
            hidden_behind(&resources, 0, Some("bash"), true, "read"),
            Some("bash")
        );
        assert_eq!(
            hidden_behind(&resources, 0, Some("bash"), true, "bash"),
            None
        );
        assert_eq!(
            hidden_behind(&resources, 1, Some("bash"), true, "report"),
            None
        );
        assert_eq!(hidden_behind(&resources, 0, None, true, "read"), None);
        assert_eq!(
            hidden_behind(&resources, 0, Some("absent"), true, "read"),
            None
        );
        Ok(())
    }

    #[test]
    fn exclusive_tool_outside_the_offered_set_changes_nothing()
    -> Result<(), Box<dyn std::error::Error>> {
        for (fields, exclusive) in [
            ("resource_view: { allow: ['harness:tool/read'] }", "bash"),
            (
                "advertise: [read]\n    resource_view: { allow: ['harness:tool/bash', 'harness:tool/read'] }",
                "bash",
            ),
        ] {
            let resources = bundle_advertise_view(fields)?;
            assert_eq!(
                filtered_tool_schemas(&resources, true, 0, Some(exclusive)),
                filtered_tool_schemas(&resources, true, 0, None),
                "{fields}"
            );
        }
        Ok(())
    }

    #[test]
    fn missing_and_empty_advertise_preserve_depth_filtering()
    -> Result<(), Box<dyn std::error::Error>> {
        for fields in ["", "advertise: []"] {
            let resources = bundle_advertise_view(fields)?;
            for depth in [0, 1, crate::MAX_SUBAGENT_DEPTH] {
                let expected = resources
                    .tool_schemas()
                    .into_iter()
                    .filter(|schema| advertise_tool_at_depth(schema.name.as_str(), depth))
                    .map(|schema| schema.name.to_string())
                    .collect::<BTreeSet<_>>();
                let actual = filtered_tool_schemas(&resources, true, depth, None)
                    .into_iter()
                    .map(|schema| schema.name.to_string())
                    .collect::<BTreeSet<_>>();
                assert_eq!(actual, expected);
            }
        }
        Ok(())
    }

    #[test]
    fn advertise_rejects_unknown_or_excluded_tools() {
        for name in ["missing-tool", "write"] {
            let result = bundle_advertise_view(&format!(
                "advertise: [{name}]\n    resource_view: {{ allow: ['harness:tool/bash'] }}"
            ));
            let Err(error) = result else {
                panic!("{name} must not compile");
            };
            assert!(
                error
                    .downcast_ref::<hya_bundle::BundleError>()
                    .is_some_and(|error| matches!(
                        error,
                        hya_bundle::BundleError::InvalidManifest { .. }
                    ))
            );
            let message = error.to_string();
            assert!(
                message.contains("selective-agent") && message.contains(name),
                "{message}"
            );
        }
    }

    /// Pin explicit overrides, the unsuffixed default, and no silent
    /// inheritance for an invalid variant.
    #[test]
    fn model_variant_overrides_preserves_default_and_rejects_invalid_variants() {
        let default = Some(ReasoningEffort::Low);

        assert_eq!(
            reasoning_for_model(&ModelRef::new("fallback#high"), default),
            Some(ReasoningEffort::High),
        );
        assert_eq!(
            reasoning_for_model(&ModelRef::new("fallback"), default),
            default,
        );
        assert_eq!(
            reasoning_for_model(&ModelRef::new("fallback#unknown"), default),
            None,
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
