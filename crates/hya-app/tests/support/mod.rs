#![allow(dead_code, clippy::expect_used)]

use std::sync::Arc;

use hya_bundle::{AgentRole, BundleCatalog, BundleSource, SourceFile};
use hya_core::RuntimeRegistry;
use hya_tool::ToolRegistry;

pub fn test_runtime(
    tools: Arc<ToolRegistry>,
    agents: &[(&str, AgentRole, &[&str])],
) -> Arc<RuntimeRegistry> {
    let mut manifest = String::from(
        "api_version: hya.agent-bundle/v1\nkind: AgentBundle\nidentity:\n  id: hya/app-tests\n  version: 0.0.0\n  publisher: hya-tests\nagents:\n",
    );
    let mut files = Vec::with_capacity(agents.len() + 1);
    for (stable_id, role, can_spawn) in agents {
        let role = match role {
            AgentRole::Main => "main",
            AgentRole::Subagent => "subagent",
        };
        manifest.push_str(&format!(
            "  - local_id: {stable_id}\n    stable_id: {stable_id}\n    role: {role}\n    prompt: prompts/{stable_id}.md\n    spawn_lifecycle: transient\n    harness_access: full\n"
        ));
        if !can_spawn.is_empty() {
            manifest.push_str("    can_spawn: [");
            manifest.push_str(&can_spawn.join(", "));
            manifest.push_str("]\n");
        }
        files.push(SourceFile::new(
            format!("prompts/{stable_id}.md"),
            format!("{stable_id} prompt").into_bytes(),
        ));
    }
    files.push(SourceFile::new("bundle.yaml", manifest.into_bytes()));
    let prepared = hya_bundle::prepare_builtins(vec![BundleSource::new("hya/app-tests", files)])
        .expect("test bundle must prepare");
    let catalog = BundleCatalog::from_verified_catalogs(&[&prepared])
        .expect("test bundle must retain verified identity");
    Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(catalog),
    ))
}
