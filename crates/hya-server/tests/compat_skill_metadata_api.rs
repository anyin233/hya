//! Integration tests for `hya-server`: compat skill metadata api.

#![allow(clippy::unwrap_used)]

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use hya_core::{AgentSpec, EventBus, SessionEngine};
use hya_proto::{AgentName, ModelRef};
use hya_provider::{FakeProvider, ProviderRouter};
use hya_server::{AppState, router};
use hya_store::SessionStore;
use hya_tool::{PermissionPlane, PermissionRules, ToolRegistry};
use serde_json::Value;
use tower::ServiceExt;

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-server-skill-metadata-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn state(workdir: PathBuf) -> AppState {
    let providers = Arc::new(ProviderRouter::new().with(Arc::new(FakeProvider::scripted(vec![]))));
    let tools = Arc::new(ToolRegistry::builtins());
    let (permission, _rx) = PermissionPlane::new(PermissionRules::default());
    let store = SessionStore::connect_memory().await.unwrap();
    let engine = SessionEngine::new(
        store,
        providers,
        support::test_runtime(tools),
        permission,
        EventBus::default(),
    );
    AppState::new(
        Arc::new(engine),
        Arc::new(AgentSpec {
            name: AgentName::new("build"),
            model: ModelRef::new("fake-model"),
            system_prompt: "system prompt".to_string(),
            workdir,
            reasoning: None,
        }),
    )
}

async fn get_json(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn find_named<'a>(items: &'a Value, name: &str) -> &'a Value {
    items
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["name"] == name)
        .unwrap_or_else(|| panic!("missing {name}: {items}"))
}

static ENV_LOCK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct HomeGuard {
    previous: Option<std::ffi::OsString>,
}

impl HomeGuard {
    fn set(home: &std::path::Path) -> Self {
        while ENV_LOCK
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::Acquire,
                std::sync::atomic::Ordering::Relaxed,
            )
            .is_err()
        {
            std::thread::yield_now();
        }
        let previous = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", home);
        }
        Self { previous }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var("HOME", previous);
            } else {
                std::env::remove_var("HOME");
            }
        }
        ENV_LOCK.store(false, std::sync::atomic::Ordering::Release);
    }
}

fn write_skill(root: &std::path::Path, rel: &str, name: &str, description: &str, body: &str) {
    let dir = root.join(rel);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .unwrap();
}

#[tokio::test]
async fn compat_skill_and_command_routes_keep_project_hya_before_home_duplicate() {
    let workdir = tempdir();
    let home = tempdir();
    let _home = HomeGuard::set(&home);
    write_skill(
        &workdir,
        ".hya/skills/project-home-dupe",
        "project-home-dupe",
        "Project duplicate",
        "Project duplicate body\n",
    );
    write_skill(
        &home,
        ".config/hya/skills/project-home-dupe",
        "project-home-dupe",
        "Home duplicate",
        "Home duplicate body\n",
    );
    let app = router(state(workdir.clone()).await);

    let (skill_status, skills) = get_json(
        app.clone(),
        &format!("/skill?directory={}", workdir.display()),
    )
    .await;
    let (command_status, commands) =
        get_json(app, &format!("/command?directory={}", workdir.display())).await;

    assert_eq!(skill_status, StatusCode::OK);
    let skill = find_named(&skills, "project-home-dupe");
    assert_eq!(skill["description"], "Project duplicate");
    assert_eq!(skill["content"], "Project duplicate body\n");
    assert!(
        !skills
            .as_array()
            .unwrap()
            .iter()
            .any(|skill| skill["content"] == "Home duplicate body\n")
    );
    assert_eq!(command_status, StatusCode::OK);
    let command = find_named(&commands, "project-home-dupe");
    assert_eq!(command["source"], "skill");
    assert_eq!(command["template"], "Project duplicate body\n");
}

#[tokio::test]
async fn compat_skill_and_command_routes_include_builtin_customize_skill() {
    // Given: a server with no workspace skills on disk.
    let app = router(state(tempdir()).await);

    // When: the Compat skill and command metadata routes are listed.
    let (skill_status, skills) = get_json(app.clone(), "/skill").await;
    let (command_status, commands) = get_json(app, "/command").await;

    // Then: Compat's built-in customize-compat skill is present in both surfaces.
    assert_eq!(skill_status, StatusCode::OK);
    let skill = find_named(&skills, "customize-compat");
    assert_eq!(skill["location"], "<built-in>");
    let customize_description = skill["description"].as_str().unwrap_or("");
    let customize_content = skill["content"].as_str().unwrap_or("");
    assert!(customize_description.starts_with("Use ONLY"));
    assert!(customize_content.contains("# Customizing compat"));

    // And: customize-compat must not advertise unsupported legacy agent surfaces
    // (Markdown agent paths, opencode.json agent definitions, write-agent-file
    // persistence, or "create/fix agents/subagents" as this skill's job).
    let customize_surface = format!("{customize_description}\n{customize_content}");
    assert!(
        !customize_surface.contains(".opencode/agent")
            && !customize_surface.contains(".opencode/agents")
            && !customize_surface.contains("~/.config/opencode/agent"),
        "customize-compat must not advertise legacy .opencode/agent(s) Markdown paths: {customize_surface}"
    );
    assert!(
        !customize_content.contains("## Agents")
            && !customize_content.contains("\"agent\": {")
            && !customize_content.contains("Two ways to define an agent")
            && !customize_content.contains("Inline (in `opencode.json`)"),
        "customize-compat must not advertise an opencode JSON agent definition surface: {customize_content}"
    );
    assert!(
        !customize_content.contains("an agent file")
            && !customize_content.contains("For agent, command, skill, and plugin")
            && !customize_content.contains("writing agent files"),
        "customize-compat must not teach persistence by writing agent files: {customize_content}"
    );
    assert!(
        !customize_description.contains("creating or fixing compat agents")
            && !customize_description.contains("compat agents, subagents")
            && !customize_surface.contains("creating or fixing compat agents, subagents"),
        "customize-compat must not say this skill creates/fixes compat agents or subagents: {customize_description}"
    );
    // Native-only agent authority note: 0.34.11 does not parse, discover, or
    // migrate legacy agent JSON/JSONC/Markdown; external public packages can
    // remain process-free or use a Bun Compat sidecar and use the bundle
    // info/install commands, while authoring remains delegated to
    // agent-bundle-authoring.
    assert!(
        customize_surface.contains("agent-bundle-authoring")
            && customize_surface.contains("0.34.11")
            && customize_surface.contains("does not parse, discover")
            && customize_surface.contains("JSON/JSONC/Markdown")
            && customize_surface.contains("public")
            && customize_surface.contains("Bun Compat")
            && customize_surface.contains("process-free")
            && customize_surface.contains(".hyabundle")
            && customize_surface.contains("hya bundle info -f")
            && customize_surface.contains("hya bundle install"),
        "customize-compat must state 0.34.11 native-only agent boundaries and public bundle inspection/install: {customize_surface}"
    );
    assert!(
        !customize_surface.contains("external bundle distribution is later scope"),
        "customize-compat must not claim external bundle distribution is later scope: {customize_surface}"
    );
    assert!(
        !customize_surface.contains("0.34.8"),
        "customize-compat must not advertise stale 0.34.8 native-agent behavior: {customize_surface}"
    );
    assert!(
        customize_surface.contains("AgentBundle") || customize_surface.contains("embedded native"),
        "customize-compat must state built-ins come from embedded native AgentBundles: {customize_surface}"
    );

    assert_eq!(command_status, StatusCode::OK);
    let command = find_named(&commands, "customize-compat");
    assert_eq!(command["source"], "skill");
    assert!(
        command["template"]
            .as_str()
            .unwrap()
            .contains("opencode.json")
    );

    // And: the built-in agent-bundle-authoring skill is registered exactly once
    // with built-in location and nonempty truthful content.
    let authoring_matches = skills
        .as_array()
        .unwrap()
        .iter()
        .filter(|skill| skill["name"] == "agent-bundle-authoring")
        .collect::<Vec<_>>();
    assert_eq!(
        authoring_matches.len(),
        1,
        "agent-bundle-authoring must appear exactly once: {skills}"
    );
    let authoring = authoring_matches[0];
    assert_eq!(authoring["location"], "<built-in>");
    let authoring_description = authoring["description"].as_str().unwrap_or("");
    assert!(
        !authoring_description.trim().is_empty(),
        "agent-bundle-authoring description must be nonempty"
    );
    let authoring_content = authoring["content"].as_str().unwrap_or("");
    assert!(
        !authoring_content.trim().is_empty(),
        "agent-bundle-authoring content must be nonempty"
    );
    let authoring_surface = format!("{authoring_description}\n{authoring_content}");
    let required_markers = [
        ("0.36.0", "the release"),
        ("AgentBundle", "the bundle format"),
        ("Bun Compat", "the executable sidecar implementation"),
        ("hya bundle install", "the install command"),
        ("hya bundle info -f", "the inspect command"),
        (
            "Harness remains the agent runtime",
            "the Harness runtime authority",
        ),
        ("one sidecar per activation", "activation ownership"),
        (
            "Static-only Bundles remain process-free",
            "the static-only boundary",
        ),
        ("activation_id", "activation identity"),
        ("lifecycle", "activation lifecycle"),
        ("newline-delimited JSON-RPC", "the wire protocol"),
        ("tool/call", "tool request/reply"),
        ("one-way", "one-way events"),
        ("stdout is protocol-only", "stdout handling"),
        ("stderr is diagnostic-only", "stderr handling"),
        ("referenced", "referenced archive entries"),
        ("closure", "archive closure validation"),
        (
            "The public JS profile admits only self-contained selected Extension entrypoint files; no separate Bundle-local helper file kind or transitive JS source closure exists.",
            "the self-contained public JS profile",
        ),
        (
            "external single-file bundling",
            "external single-file packaging",
        ),
        (
            "activation never executes the authoring tree",
            "authoring-tree isolation",
        ),
        (
            "undeclared directory files are ignored",
            "undeclared directory-file omission",
        ),
        (
            "unreferenced archive files are rejected",
            "unreferenced archive rejection",
        ),
        (
            "missing relative helper import fails before ACK",
            "pre-ACK relative-import failure",
        ),
        (
            "`hook_refs` select Bundle-local Hook resources only",
            "Bundle-local hook refs",
        ),
        (
            "all `harness:hook/*` spellings reject",
            "harness hook rejection",
        ),
        (
            "Harness host hooks stay outside AgentBundle metadata",
            "Harness host-hook ownership",
        ),
        ("volatile", "resident state"),
        ("explicit stop", "resident stop semantics"),
        ("authentication=unverified", "private authentication status"),
        ("payload=opaque", "private payload status"),
        (
            "private activation is unsupported",
            "private activation rejection",
        ),
        ("raw Rust", "raw Rust rejection"),
        ("Bundle-declared MCP", "Bundle MCP rejection"),
        ("unsupported", "unsupported feature handling"),
        ("no sandbox", "the sandbox boundary"),
        ("no permission expansion", "the permission boundary"),
    ];
    for (marker, requirement) in required_markers {
        assert!(
            authoring_surface.contains(marker),
            "agent-bundle-authoring must state {requirement} (`{marker}`): {authoring_surface}"
        );
    }
    assert!(
        !authoring_surface.contains("validated transitive referenced closure"),
        "agent-bundle-authoring must not advertise stale transitive-closure wording: {authoring_surface}"
    );
    let forbidden_warnings = [
        (
            "sidecar never runs the agent/model loop",
            "the agent/model loop",
        ),
        ("no `agent/invoke`", "agent/invoke"),
        ("no sidecar send/wait", "sidecar send/wait"),
        ("no terminal/artifact result", "terminal/artifact results"),
    ];
    for (warning, subject) in forbidden_warnings {
        assert!(
            authoring_surface.contains(warning),
            "agent-bundle-authoring must explicitly warn against {subject} (`{warning}`): {authoring_surface}"
        );
    }
    // Role controls TUI direct-selector visibility only: main is selectable;
    // subagent is hidden from direct selection — never a subagent selector placement.
    // Roster/spawn come from can_spawn, never from role.
    assert!(
        !authoring_content.contains("subagent selector placement"),
        "agent-bundle-authoring must not imply role subagent has a subagent selector placement: {authoring_content}"
    );
    assert!(
        authoring_content.contains("hidden from direct TUI selection"),
        "agent-bundle-authoring must state role subagent is hidden from direct TUI selection: {authoring_content}"
    );
    assert!(
        authoring_content.contains("can_spawn") && authoring_content.contains("never from `role`"),
        "agent-bundle-authoring must distinguish can_spawn-derived roster/spawn from role: {authoring_content}"
    );
}

#[tokio::test]
async fn compat_skill_and_command_routes_discover_compat_project_skills() {
    // Given: a workspace with an Compat project skill on disk.
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".opencode/skills/release")).unwrap();
    std::fs::write(
        workdir.join(".opencode/skills/release/SKILL.md"),
        "---\nname: release\ndescription: Prepare a release\n---\nCheck version, changelog, and tag.\n",
    )
    .unwrap();
    let app = router(state(workdir.clone()).await);

    // When: metadata is requested for that workspace.
    let uri = format!("/skill?directory={}", workdir.display());
    let (skill_status, skills) = get_json(app.clone(), &uri).await;
    let uri = format!("/command?directory={}", workdir.display());
    let (command_status, commands) = get_json(app, &uri).await;

    // Then: the Compat project skill is available as both a skill and command.
    assert_eq!(skill_status, StatusCode::OK);
    let skill = find_named(&skills, "release");
    assert_eq!(skill["description"], "Prepare a release");
    assert_eq!(skill["content"], "Check version, changelog, and tag.\n");

    assert_eq!(command_status, StatusCode::OK);
    let command = find_named(&commands, "release");
    assert_eq!(command["source"], "skill");
    assert_eq!(command["template"], "Check version, changelog, and tag.\n");
}

#[tokio::test]
async fn compat_skill_command_does_not_override_existing_command_name() {
    // Given: a workspace skill collides with the built-in help command.
    let workdir = tempdir();
    std::fs::create_dir_all(workdir.join(".opencode/skills/help")).unwrap();
    std::fs::write(
        workdir.join(".opencode/skills/help/SKILL.md"),
        "---\nname: help\ndescription: Workspace help skill\n---\nDisk help skill body.\n",
    )
    .unwrap();
    let app = router(state(workdir.clone()).await);

    // When: skill and command metadata are requested for that workspace.
    let uri = format!("/skill?directory={}", workdir.display());
    let (skill_status, skills) = get_json(app.clone(), &uri).await;
    let uri = format!("/command?directory={}", workdir.display());
    let (command_status, commands) = get_json(app, &uri).await;

    // Then: the disk skill is listed without overriding the built-in help command.
    assert_eq!(skill_status, StatusCode::OK);
    let skill = find_named(&skills, "help");
    assert_eq!(skill["description"], "Workspace help skill");
    assert_eq!(skill["content"], "Disk help skill body.\n");

    assert_eq!(command_status, StatusCode::OK);
    let command = find_named(&commands, "help");
    assert_eq!(command["source"], "command");
    assert_eq!(command["template"], "/help");
}
