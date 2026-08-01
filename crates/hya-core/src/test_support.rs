use std::sync::Arc;

use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy,
    PreparedAgent, PreparedBundle, ResourceView, SpawnLifecycle,
};
use hya_proto::AgentName;
use hya_tool::ToolRegistry;

use crate::RuntimeRegistry;

pub(crate) fn runtime(tools: ToolRegistry) -> Arc<RuntimeRegistry> {
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/core-unit-tests".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents: {
            let ordinary = ["build", "general", "resident"].map(|stable_id| PreparedAgent {
                local_id: stable_id.to_string(),
                stable_id: AgentName::new(stable_id),
                description: None,
                role: AgentRole::Main,
                color: None,
                prompt: None,
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                harness_access: HarnessAccess::Full,
                resource_view: ResourceView::default(),
                can_spawn: Vec::new(),
                hook_refs: Vec::new(),
            });
            let system = ["compaction", "title", "summary"].map(|stable_id| PreparedAgent {
                local_id: stable_id.to_string(),
                stable_id: AgentName::new(stable_id),
                description: None,
                role: AgentRole::Subagent,
                color: None,
                prompt: Some(format!("{stable_id} prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                harness_access: HarnessAccess::Full,
                resource_view: ResourceView::default(),
                can_spawn: Vec::new(),
                hook_refs: Vec::new(),
            });
            ordinary.into_iter().chain(system).collect()
        },
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    };
    let catalog = BundleCatalog::from_prepared(&[bundle]);
    let Ok(catalog) = catalog else {
        panic!("unit-test catalog must be valid: {catalog:?}");
    };
    Arc::new(RuntimeRegistry::new(tools, Arc::new(catalog)))
}
