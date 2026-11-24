use std::path::PathBuf;
use std::sync::Arc;

use hya_proto::{AgentName, ModelRef};
use hya_provider::ReasoningEffort;
use hya_tool::{AgentDef, ResolvedTool};

use super::SessionEngine;
use crate::agent_catalog::AgentDefinition;
use crate::runtime_registry::CompiledResourceView;
use crate::{AgentResourcePolicy, CoreError, TurnBinding};

/// Turn-time agent identity: name, model, prompt, workdir, and reasoning effort.
#[derive(Clone)]
pub struct AgentSpec {
    /// Agent display / catalog name.
    pub name: AgentName,
    /// Model route for completions.
    pub model: ModelRef,
    /// System prompt base before guidance/skills composition.
    pub system_prompt: String,
    /// Filesystem workdir for tools and path resolution.
    pub workdir: PathBuf,
    /// Optional reasoning effort for capable models.
    pub reasoning: Option<ReasoningEffort>,
}

impl SessionEngine {
    /// Resolve a catalog agent into an [`AgentSpec`] using `binding`.
    ///
    /// # Errors
    /// Returns [`CoreError::AgentDefinitionMissing`] or bundle errors.
    pub fn agent_spec_for_binding(
        &self,
        binding: &TurnBinding,
        base: &AgentSpec,
        stable_id: &str,
    ) -> Result<AgentSpec, CoreError> {
        agent_from_definition(base, stable_id, binding)
    }

    /// Resolve the exact recorded resident identity and all its capabilities
    /// from one binding. Recovery and revival keep the recorded member identity.
    pub fn resolve_resident_runtime(
        &self,
        binding: TurnBinding,
        base: &AgentSpec,
        recorded: &AgentName,
    ) -> Result<(AgentSpec, crate::resident::ResolvedResidentRuntime), CoreError> {
        let stable = recorded.as_str();
        let agent = self.agent_spec_for_binding(&binding, base, stable)?;
        let agents = self.agent_roster_for_binding(&binding, stable)?;
        let resources = self.agent_resource_policy_for_binding(&binding, stable)?;
        let factory = self
            .sidecar_environment()
            .map(|environment| environment.factory_for(&binding, stable))
            .transpose()?
            .flatten();
        Ok((agent, (binding, agents, resources, factory)))
    }

    /// Build the caller's authorized spawn roster for tools.
    ///
    /// # Errors
    /// Returns catalog/resolution errors.
    pub fn agent_roster_for_binding(
        &self,
        binding: &TurnBinding,
        caller: &str,
    ) -> Result<Arc<[AgentDef]>, CoreError> {
        agent_roster(binding, caller)
    }

    /// Resource/tool policy for `stable_id` under `binding`.
    ///
    /// # Errors
    /// Returns catalog/resolution errors.
    pub fn agent_resource_policy_for_binding(
        &self,
        binding: &TurnBinding,
        stable_id: &str,
    ) -> Result<AgentResourcePolicy, CoreError> {
        binding
            .agent_resource_policy(stable_id)
            .map_err(CoreError::bundle)
    }
}

pub(crate) fn effective_agent_for_binding(
    agent: &AgentSpec,
    stable_id: &str,
    binding: &TurnBinding,
    guidance: Option<&str>,
) -> Result<(AgentSpec, Arc<CompiledResourceView>), CoreError> {
    effective_agent_for_binding_with_sidecar_tools(agent, stable_id, binding, guidance, &[])
}

pub(crate) fn effective_agent_for_binding_with_sidecar_tools(
    agent: &AgentSpec,
    stable_id: &str,
    binding: &TurnBinding,
    guidance: Option<&str>,
    sidecar_tools: &[ResolvedTool],
) -> Result<(AgentSpec, Arc<CompiledResourceView>), CoreError> {
    // One composition seam: agent_base (Bundle Some replaces / None keeps
    // Harness base) → nonempty guidance → skill prompt material.
    let effective = agent_from_definition(agent, stable_id, binding)?;
    let effective = agent_with_guidance_layer(effective, guidance);
    let policy = binding
        .agent_resource_policy(stable_id)
        .map_err(CoreError::bundle)?;
    let resources = if sidecar_tools.is_empty() {
        binding
            .compile_agent_resources(&policy)
            .map_err(CoreError::bundle)?
    } else {
        binding
            .compile_agent_resources_with_sidecar_tools(&policy, sidecar_tools)
            .map_err(CoreError::bundle)?
    };
    Ok((
        agent_with_bound_skills(effective, resources.as_ref()),
        resources,
    ))
}

fn agent_from_definition(
    agent: &AgentSpec,
    stable_id: &str,
    binding: &TurnBinding,
) -> Result<AgentSpec, CoreError> {
    let definition =
        binding
            .resolve_agent(stable_id)
            .ok_or_else(|| CoreError::AgentDefinitionMissing {
                agent_id: stable_id.to_string(),
            })?;
    let mut effective = agent.clone();
    effective.name = AgentName::new(definition.stable_id);
    effective.workdir = binding.workdir().to_path_buf();
    // Agent prompt Some replaces only agent_base; None preserves Harness base.
    if let Some(prompt) = definition.prompt {
        effective.system_prompt = prompt.to_string();
    }
    if let Some(reasoning) = definition
        .model_policy
        .reasoning
        .as_deref()
        .and_then(hya_provider::ReasoningEffort::parse)
    {
        effective.reasoning = Some(reasoning);
    }
    Ok(effective)
}

/// Append nonempty request-scoped guidance after agent_base resolution.
///
/// Absence or empty text is an empty layer (no error). Callers pre-render once
/// per turn; this does not discover files.
pub(crate) fn agent_with_guidance_layer(mut agent: AgentSpec, guidance: Option<&str>) -> AgentSpec {
    let Some(guidance) = guidance.map(str::trim).filter(|text| !text.is_empty()) else {
        return agent;
    };
    let base = agent.system_prompt.trim_end();
    agent.system_prompt = if base.is_empty() {
        guidance.to_string()
    } else {
        format!("{base}\n\n{guidance}")
    };
    agent
}

pub(crate) fn agent_with_bound_skills(
    mut effective: AgentSpec,
    resources: &CompiledResourceView,
) -> AgentSpec {
    if let Some(section) = resources.skills_prompt_section() {
        let prompt = effective.system_prompt.trim_end();
        effective.system_prompt = if prompt.is_empty() {
            section
        } else {
            format!("{prompt}\n\n{section}")
        };
    }
    effective
}

pub(super) fn agent_roster(
    binding: &TurnBinding,
    caller: &str,
) -> Result<Arc<[AgentDef]>, CoreError> {
    Ok(binding
        .spawnable_agents(caller)
        .map_err(CoreError::bundle)?
        .iter()
        .map(agent_def)
        .collect::<Vec<_>>()
        .into())
}

/// A roster row with the Agent's authored effort; the user's runtime and
/// configured choices are layered on by
/// [`SessionEngine::annotate_agent_efforts`] where the roster reaches a model.
fn agent_def(agent: &AgentDefinition<'_>) -> AgentDef {
    let authored = agent.model_policy.reasoning.clone();
    AgentDef {
        name: agent.stable_id.to_string(),
        description: agent.description.map(str::to_string),
        category: agent.model_policy.category.clone(),
        mode: agent.selector_mode().to_string(),
        effort_source: authored.as_ref().map(|_| "authored".to_string()),
        effort: authored,
    }
}
