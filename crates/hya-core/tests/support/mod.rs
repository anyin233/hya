#![allow(dead_code, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, ModelPolicy, PreparedAgent, PreparedAgentBundle,
    PreparedInstallableBundle, ResourceView, SpawnLifecycle,
};
use hya_core::{AgentCatalog, RuntimeRegistry};
use hya_proto::{AgentName, ToolName, ToolSchema};
use hya_tool::{Tool, ToolCtx, ToolError, ToolRegistry};
use serde_json::{Value, json};

static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

/// One installed bundle per requested agent, over the compiled-in built-ins.
///
/// A bundle defines exactly one agent, so a fixture asking for N agents becomes
/// N single-agent bundles.
pub fn test_catalog(agents: &[(&str, AgentRole, &[&str])]) -> Arc<AgentCatalog> {
    let with_lifecycle = agents
        .iter()
        .map(|(stable_id, role, can_spawn)| {
            (*stable_id, *role, SpawnLifecycle::Transient, *can_spawn)
        })
        .collect::<Vec<_>>();
    test_catalog_with_lifecycles(&with_lifecycle)
}

/// Build one installed test bundle per Agent with an explicit spawn lifecycle.
pub fn test_catalog_with_lifecycles(
    agents: &[(&str, AgentRole, SpawnLifecycle, &[&str])],
) -> Arc<AgentCatalog> {
    let bundles = agents
        .iter()
        .filter(|(stable_id, _, _, _)| !hya_core::is_builtin_id(stable_id))
        .map(
            |(stable_id, role, lifecycle, can_spawn)| PreparedAgentBundle {
                format_version: 2,
                identity: BundleIdentity {
                    id: format!("hya/test-{stable_id}"),
                    version: "0.0.0".to_string(),
                    publisher: "hya-tests".to_string(),
                },
                digest: format!("test-only-{stable_id}"),
                agent: PreparedAgent {
                    id: AgentName::new(*stable_id),
                    description: None,
                    role: *role,
                    color: None,
                    prompt: Some(format!("{stable_id} prompt")),
                    prompt_source: None,
                    prompt_digest: None,
                    model_policy: ModelPolicy::default(),
                    workdir: None,
                    spawn_lifecycle: *lifecycle,
                    resource_view: ResourceView::default(),
                    can_spawn: can_spawn
                        .iter()
                        .map(|agent| AgentName::new(*agent))
                        .collect(),
                    hook_refs: Vec::new(),
                },
                tools: Vec::new(),
                skills: Vec::new(),
                mcp: Vec::new(),
                hooks: Vec::new(),
                extensions: Vec::new(),
            },
        )
        .collect::<Vec<_>>();
    let bundles = bundles
        .into_iter()
        .map(|bundle| PreparedInstallableBundle::Agent(Box::new(bundle)))
        .collect::<Vec<_>>();
    let catalog = BundleCatalog::from_prepared(&bundles);
    let Ok(catalog) = catalog else {
        panic!("test catalog must be valid: {catalog:?}");
    };
    let catalog = AgentCatalog::new(Arc::new(catalog));
    let Ok(catalog) = catalog else {
        panic!("test agent catalog must be valid: {catalog:?}");
    };
    Arc::new(catalog)
}

/// Agent catalog with no installed bundles: the compiled-in built-ins only.
pub fn builtin_only_catalog() -> Arc<AgentCatalog> {
    let bundles = BundleCatalog::from_verified_catalogs(&[]).expect("empty bundle catalog");
    Arc::new(AgentCatalog::new(Arc::new(bundles)).expect("builtin-only agent catalog"))
}

/// Default runtime for root/main turn integration fixtures.
///
/// Uses the compiled-in built-in roster only: `build` / `plan` / `general` keep
/// `prompt = None` so root turns preserve the already composed base system
/// prompt (AGENTS/context), and the reserved system agents stay available for
/// exact lookup without entering any ordinary spawn roster.
pub fn test_runtime(tools: Arc<ToolRegistry>) -> Arc<RuntimeRegistry> {
    Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        builtin_only_catalog(),
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
