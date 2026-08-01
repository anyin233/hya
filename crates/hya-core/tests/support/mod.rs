#![allow(dead_code, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy,
    PreparedAgent, PreparedBundle, ResourceView, SpawnLifecycle,
};
use hya_core::RuntimeRegistry;
use hya_proto::{AgentName, ToolName, ToolSchema};
use hya_tool::{Tool, ToolCtx, ToolError, ToolRegistry};
use serde_json::{Value, json};

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

pub fn test_catalog(agents: &[(&str, AgentRole, &[&str])]) -> Arc<BundleCatalog> {
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/test".to_string(),
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
    Arc::new(catalog)
}

/// Default runtime for root/main turn integration fixtures.
///
/// Includes the common main stable IDs with `prompt = None` so root turns keep
/// the already composed base system prompt (AGENTS/context), matching build/
/// plan/general native Bundle semantics.
pub fn test_runtime(tools: Arc<ToolRegistry>) -> Arc<RuntimeRegistry> {
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/test-runtime".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents: {
            let ordinary = ["build", "plan", "general"].map(|stable_id| PreparedAgent {
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
                can_spawn: vec![
                    AgentName::new("build"),
                    AgentName::new("plan"),
                    AgentName::new("general"),
                ],
                hook_refs: Vec::new(),
            });
            // Fixed Harness system agents: present for exact lookup, not in can_spawn.
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
        panic!("test runtime catalog must be valid: {catalog:?}");
    };
    Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(catalog),
    ))
}

pub struct TestDir {
    path: PathBuf,
}

impl TestDir {
    pub fn new(label: &str) -> Self {
        let sequence = NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "hya-runtime-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create test workdir");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn write_skill(&self, name: &str) {
        let dir = self.path.join(".hya/skills").join(name);
        std::fs::create_dir_all(&dir).expect("create skill directory");
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {name} skill\n---\n{name} body\n"),
        )
        .expect("write skill");
    }

    pub fn remove_skill(&self, name: &str) {
        std::fs::remove_dir_all(self.path.join(".hya/skills").join(name))
            .expect("remove skill directory");
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

pub struct MarkerTool {
    name: String,
}

impl MarkerTool {
    pub fn new(name: impl Into<String>) -> Arc<Self> {
        Arc::new(Self { name: name.into() })
    }
}

#[async_trait]
impl Tool for MarkerTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name.clone()),
            description: format!("{} marker", self.name),
            input_schema: json!({ "type": "object" }),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        Ok(json!({ "ok": true }))
    }
}
