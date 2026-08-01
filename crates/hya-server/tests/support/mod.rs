#![allow(clippy::expect_used, clippy::unwrap_used, dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use hya_bundle::{
    AgentRole, BundleCatalog, BundleIdentity, BundleOrigin, HarnessAccess, ModelPolicy,
    PreparedAgent, PreparedBundle, ResourceView, SpawnLifecycle,
};
use hya_core::RuntimeRegistry;
use hya_proto::AgentName;
use hya_tool::ToolRegistry;

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// One prepared agent for explicit server test catalogs.
pub struct AgentFixture {
    pub stable_id: &'static str,
    pub role: AgentRole,
    pub description: Option<&'static str>,
    pub can_spawn: Vec<&'static str>,
    pub prompt: Option<&'static str>,
}

impl AgentFixture {
    pub fn main(stable_id: &'static str) -> Self {
        Self {
            stable_id,
            role: AgentRole::Main,
            description: None,
            can_spawn: Vec::new(),
            prompt: None,
        }
    }

    pub fn subagent(stable_id: &'static str) -> Self {
        Self {
            stable_id,
            role: AgentRole::Subagent,
            description: None,
            can_spawn: Vec::new(),
            prompt: None,
        }
    }

    pub fn can_spawn(mut self, ids: &[&'static str]) -> Self {
        self.can_spawn = ids.to_vec();
        self
    }

    pub fn description(mut self, description: &'static str) -> Self {
        self.description = Some(description);
        self
    }

    pub fn prompt(mut self, prompt: &'static str) -> Self {
        self.prompt = Some(prompt);
        self
    }
}

/// Build a runtime from an explicit prepared-agent catalog (single authority).
pub fn runtime_with_catalog(
    tools: Arc<ToolRegistry>,
    agents: &[AgentFixture],
) -> Arc<RuntimeRegistry> {
    let prepared: Vec<PreparedAgent> = agents
        .iter()
        .map(|agent| PreparedAgent {
            local_id: agent.stable_id.to_string(),
            stable_id: AgentName::new(agent.stable_id),
            description: agent.description.map(str::to_string),
            role: agent.role,
            color: None,
            prompt: agent.prompt.map(str::to_string),
            prompt_source: None,
            prompt_digest: None,
            model_policy: ModelPolicy::default(),
            workdir: None,
            spawn_lifecycle: SpawnLifecycle::Transient,
            harness_access: HarnessAccess::Full,
            resource_view: ResourceView::default(),
            can_spawn: agent
                .can_spawn
                .iter()
                .map(|id| AgentName::new(*id))
                .collect(),
            hook_refs: Vec::new(),
        })
        .collect();
    let bundle = PreparedBundle {
        format_version: 1,
        identity: BundleIdentity {
            id: "hya/server-tests".to_string(),
            version: "0.0.0".to_string(),
            publisher: "hya-tests".to_string(),
        },
        origin: BundleOrigin::Builtin,
        immutable: true,
        digest: "test-only".to_string(),
        agents: prepared,
        tools: Vec::new(),
        skills: Vec::new(),
        mcp: Vec::new(),
        hooks: Vec::new(),
        extensions: Vec::new(),
    };
    let catalog = BundleCatalog::from_prepared(&[bundle]).expect("server test catalog");
    Arc::new(RuntimeRegistry::from_snapshot(
        tools.snapshot(),
        Arc::new(catalog),
    ))
}

/// Minimal prepared catalog for server engine fixtures (Commit 2 RuntimeRegistry).
///
/// Matches builtin Bundle roles: `build`/`plan` are main; `general` is subagent.
/// Compaction/title/summary are fixed system subagents (ordinarily unreachable).
pub fn test_runtime(tools: Arc<ToolRegistry>) -> Arc<RuntimeRegistry> {
    runtime_with_catalog(
        tools,
        &[
            AgentFixture::main("build").can_spawn(&["build", "plan", "general"]),
            AgentFixture::main("plan").can_spawn(&["build", "plan", "general"]),
            AgentFixture::subagent("general"),
            AgentFixture::subagent("compaction").prompt("compaction prompt"),
            AgentFixture::subagent("title").prompt("title prompt"),
            AgentFixture::subagent("summary").prompt("summary prompt"),
        ],
    )
}

pub fn tempdir(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let serial = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-server-{label}-{nanos}-{serial}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

pub fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

pub fn init_git_repo(label: &str) -> PathBuf {
    let repo = tempdir(label);
    git(&repo, &["init"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}
