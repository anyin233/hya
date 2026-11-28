use std::sync::Arc;

use hya_bundle::{
    AgentRole, Bundle, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, ResourceView,
};
use hya_proto::AgentName;
use hya_tool::ToolRegistry;

use crate::RuntimeRegistry;
use crate::agent_catalog::AgentCatalog;

pub(crate) fn runtime(tools: ToolRegistry) -> Arc<RuntimeRegistry> {
    // `hya-main`, `hya-title`, and the multi-agent workers come from their
    // first-party bundles, so the fixture only has to add the ids these unit
    // tests spawn.
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
                private: false,
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
                handoff: hya_bundle::HandoffPolicy::Model,
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
            commands: Vec::new(),
            runtime: None,
            permission_modes: vec![],
            cli: Vec::new(),
            requires: Vec::new(),
            default_agent: None,
            overrides: None,
        })
        .collect::<Vec<_>>();
    let mut bundles = bundles.into_iter().collect::<Vec<_>>();
    bundles.extend(first_party(&["hya/multi-agent", "hya/core-agents"]));
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

/// The prepared bundles of first-party packages (`hya/core-agents` gives the
/// `hya-main` roster).
pub(crate) fn first_party(ids: &[&str]) -> Vec<Bundle> {
    ids.iter()
        .flat_map(|id| {
            let Ok(catalog) = hya_bundle::first_party_bundle(id) else {
                panic!("first-party {id} must load");
            };
            catalog.bundles().to_vec()
        })
        .collect()
}
