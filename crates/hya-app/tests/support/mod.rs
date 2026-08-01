#![allow(dead_code, clippy::expect_used)]

use std::sync::Arc;

use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy,
    PreparedAgent, PreparedBundle, ResourceView, SpawnLifecycle,
};
use hya_core::RuntimeRegistry;
use hya_proto::AgentName;
use hya_tool::ToolRegistry;

pub fn test_runtime(
    tools: Arc<ToolRegistry>,
    agents: &[(&str, AgentRole, &[&str])],
) -> Arc<RuntimeRegistry> {
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/app-tests".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents: agents
            .iter()
            .map(|(stable_id, role, can_spawn)| PreparedAgent {
                local_id: (*stable_id).to_string(),
                stable_id: AgentName::new(*stable_id),
                description: None,
                role: *role,
                color: None,
                prompt: Some(format!("{stable_id} prompt")),
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                spawn_lifecycle: SpawnLifecycle::Transient,
                harness_access: HarnessAccess::Full,
                resource_view: ResourceView::default(),
                can_spawn: can_spawn
                    .iter()
                    .map(|agent| AgentName::new(*agent))
                    .collect(),
                hook_refs: Vec::new(),
            })
            .collect(),
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    };
    let catalog = BundleCatalog::from_prepared(&[bundle]);
    let Ok(catalog) = catalog else {
        panic!("test catalog must be valid: {catalog:?}");
    };
    Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(catalog),
    ))
}
