//! Registered file tools honour every Project root (ADR-0026): a path in any
//! root runs without an `ExternalDirectory` ask, a path outside every root
//! (including through a symlink) asks, and yolo (`danger`) never asks.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_tool::{
    Action, Decision, InteractionPlane, LspPlane, Mode, PermissionModel, PermissionPlane,
    PermissionRules, Resource, Rule, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx, ToolError,
    ToolRegistry, WebSearchPlane, handle::ArtifactPlane,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn tempdir() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-project-roots-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(dir).unwrap()
}

/// Two roots with a file in each, an outside directory with a file, and a
/// symlink in root one that points at the outside directory.
struct Layout {
    one: PathBuf,
    two: PathBuf,
    outside: PathBuf,
}

fn layout() -> Layout {
    let one = tempdir();
    let two = tempdir();
    let outside = tempdir();
    for dir in [&one, &two, &outside] {
        std::fs::write(dir.join("file.txt"), "alpha\nneedle\n").unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, one.join("escape")).unwrap();
    Layout { one, two, outside }
}

/// Every resource ask the call raised, as `(action, resource path)`.
type Asks = Arc<Mutex<Vec<(Action, String)>>>;

async fn run(
    layout: &Layout,
    tool: &str,
    input: Value,
    model: Option<PermissionModel>,
) -> (Result<Value, ToolError>, Vec<(Action, String)>) {
    let rules = PermissionRules::new(
        [Action::Read, Action::Edit, Action::Glob, Action::Grep]
            .into_iter()
            .map(|action| Rule::new(action, "*", Mode::Allow))
            .collect(),
    );
    let (permission, mut rx) = PermissionPlane::new(rules);
    let permission = match model {
        Some(model) => permission.with_invocation_model(model),
        None => permission,
    };
    let asks: Asks = Arc::default();
    let recorder = Arc::clone(&asks);
    let drain = tokio::spawn(async move {
        while let Some(ask) = rx.recv().await {
            let resource = match &ask.resource {
                Resource::Path(path) => path.clone(),
                other => format!("{other:?}"),
            };
            recorder.lock().unwrap().push((ask.action, resource));
            let _ = ask.reply.send(Decision::Reject { feedback: None });
        }
    });
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    let ctx = ToolCtx {
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction,
        spawner,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        agents: Default::default(),
        roots: vec![layout.one.clone(), layout.two.clone()],
        workdir: layout.one.clone(),
        cancel: CancellationToken::new(),
    };
    let tool = ToolRegistry::builtins().get(tool).unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), tool.execute(&ctx, input))
        .await
        .expect("tool call exceeded the test guard");
    drop(ctx);
    drain.abort();
    let asks = asks.lock().unwrap().clone();
    (result, asks)
}

fn external(asks: &[(Action, String)]) -> Vec<String> {
    asks.iter()
        .filter(|(action, _)| *action == Action::ExternalDirectory)
        .map(|(_, resource)| resource.clone())
        .collect()
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Build the input for `tool` aimed at `target` (a file for file tools, its
/// directory for directory tools).
fn input_for(tool: &str, target: &Path) -> Value {
    let file = target.join("file.txt");
    match tool {
        "read" => json!({ "path": text(&file) }),
        "write" => json!({ "path": text(&target.join("new.txt")), "content": "x\n" }),
        "edit" => json!({
            "path": text(&file),
            "edits": [{ "op": "append", "lines": ["omega"] }]
        }),
        "glob" => json!({ "pattern": "*.txt", "path": text(target) }),
        "find" => json!({ "pattern": "*.txt", "path": text(target) }),
        "grep" => json!({ "pattern": "needle", "path": text(target) }),
        "ls" => json!({ "path": text(target) }),
        other => panic!("no input for {other}"),
    }
}

const FILE_TOOLS: [&str; 7] = ["read", "write", "edit", "glob", "find", "grep", "ls"];

#[tokio::test]
async fn file_tools_use_the_second_root_without_an_external_directory_ask() {
    for tool in FILE_TOOLS {
        let layout = layout();
        let (result, asks) = run(&layout, tool, input_for(tool, &layout.two), None).await;
        assert!(result.is_ok(), "{tool}: {result:?}");
        assert!(external(&asks).is_empty(), "{tool}: {asks:?}");
    }
}

#[tokio::test]
async fn file_tools_ask_external_directory_outside_every_root() {
    for tool in FILE_TOOLS {
        let layout = layout();
        let (result, asks) = run(&layout, tool, input_for(tool, &layout.outside), None).await;
        let expected = match tool {
            // Directory tools name the directory itself; file tools its parent.
            "find" | "ls" => format!("{}/*", text(&layout.outside)),
            "glob" | "grep" => format!("{}/*", text(layout.outside.parent().unwrap())),
            _ => format!("{}/*", text(&layout.outside)),
        };
        assert_eq!(external(&asks), vec![expected], "{tool}");
        assert!(
            matches!(result, Err(ToolError::Permission(_))),
            "{tool}: {result:?}"
        );
    }
}

#[tokio::test]
async fn file_tools_in_yolo_mode_run_outside_every_root_without_asking() {
    for tool in FILE_TOOLS {
        let layout = layout();
        let (result, asks) = run(
            &layout,
            tool,
            input_for(tool, &layout.outside),
            Some(PermissionModel::Danger),
        )
        .await;
        assert!(result.is_ok(), "{tool}: {result:?}");
        assert!(asks.is_empty(), "{tool}: {asks:?}");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn file_tools_ask_when_a_symlink_in_a_root_escapes_it() {
    for tool in FILE_TOOLS {
        let layout = layout();
        let through_link = layout.one.join("escape");
        let (result, asks) = run(&layout, tool, input_for(tool, &through_link), None).await;
        // The ask names the canonical directory the link lands in, never
        // the link's own lexical spelling.
        let expected = match tool {
            "glob" | "grep" => format!("{}/*", text(layout.outside.parent().unwrap())),
            _ => format!("{}/*", text(&layout.outside)),
        };
        assert_eq!(external(&asks), vec![expected], "{tool}");
        assert!(
            matches!(result, Err(ToolError::Permission(_))),
            "{tool}: {result:?}"
        );
        assert_eq!(
            std::fs::read_to_string(layout.outside.join("file.txt")).unwrap(),
            "alpha\nneedle\n",
            "{tool} must not touch the outside file"
        );
        assert!(!layout.outside.join("new.txt").exists(), "{tool}");
    }
}

fn add_file_patch(path: &Path) -> Value {
    json!({
        "patchText": format!(
            "*** Begin Patch\n*** Add File: {}\n+added\n*** End Patch",
            text(path)
        )
    })
}

#[tokio::test]
async fn apply_patch_accepts_an_absolute_path_in_the_second_root() {
    let layout = layout();
    let target = layout.two.join("added.txt");
    let (result, asks) = run(&layout, "apply_patch", add_file_patch(&target), None).await;
    assert!(result.is_ok(), "{result:?}");
    assert!(external(&asks).is_empty(), "{asks:?}");
    assert_eq!(std::fs::read_to_string(target).unwrap(), "added\n");
}

#[tokio::test]
async fn apply_patch_rejects_paths_outside_every_root() {
    let layout = layout();
    let target = layout.outside.join("added.txt");
    let (result, _) = run(&layout, "apply_patch", add_file_patch(&target), None).await;
    assert!(matches!(result, Err(ToolError::Input(_))), "{result:?}");
    assert!(!target.exists());

    let escape = layout.one.join("../escaped.txt");
    let (result, _) = run(&layout, "apply_patch", add_file_patch(&escape), None).await;
    assert!(matches!(result, Err(ToolError::Input(_))), "{result:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn apply_patch_rejects_a_symlink_escape() {
    let layout = layout();
    let patch = json!({
        "patchText": "*** Begin Patch\n*** Add File: escape/added.txt\n+added\n*** End Patch"
    });
    let (result, _) = run(&layout, "apply_patch", patch, None).await;
    assert!(matches!(result, Err(ToolError::Input(_))), "{result:?}");
    assert!(!layout.outside.join("added.txt").exists());
}
