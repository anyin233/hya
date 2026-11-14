use std::sync::Arc;

use hya_bundle::{
    AgentRole, Bundle, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, ResourceView,
};
use hya_proto::AgentName;
use hya_tool::ToolRegistry;

use crate::RuntimeRegistry;
use crate::agent_catalog::AgentCatalog;

pub(crate) fn runtime(tools: ToolRegistry) -> Arc<RuntimeRegistry> {
    // `build`, `general`, and the reserved system agents are compiled-in
    // built-ins now, so the fixture only has to add the non-builtin ids these
    // unit tests spawn.
    let bundles = ["resident", "reviewer"]
        .into_iter()
        .map(|stable_id| Bundle {
            check: None,
            format_version: 4,
            identity: BundleIdentity {
                id: format!("hya/core-unit-tests-{stable_id}"),
                version: "0.0.0".to_string(),
                publisher: "hya-tests".to_string(),
            },
            namespace: None,
            digest: format!("test-only-{stable_id}"),
            agents: vec![PreparedAgent {
                id: AgentName::new(stable_id),
                description: None,
                role: AgentRole::Main,
                color: None,
                prompt: None,
                prompt_source: None,
                prompt_digest: None,
                model_policy: ModelPolicy::default(),
                workdir: None,
                resource_view: ResourceView::default(),
                can_spawn: Vec::new(),
                hook_refs: Vec::new(),
                advertise: Vec::new(),
            }],
            tools: Vec::new(),
            skills: Vec::new(),
            mcp: Vec::new(),
            hooks: Vec::new(),
            extensions: Vec::new(),
            tui: None,

            workflow: None,
            channels: vec![],
            schemas: vec![],
            process: None,
            apis: vec![],
            permission_modes: vec![],
        })
        .collect::<Vec<_>>();
    let bundles = bundles.into_iter().collect::<Vec<_>>();
    let catalog = BundleCatalog::from_prepared(&bundles);
    let Ok(catalog) = catalog else {
        panic!("unit-test catalog must be valid: {catalog:?}");
    };
    let catalog = AgentCatalog::new(Arc::new(catalog));
    let Ok(catalog) = catalog else {
        panic!("unit-test agent catalog must be valid: {catalog:?}");
    };
    Arc::new(RuntimeRegistry::new(tools, Arc::new(catalog)))
}
