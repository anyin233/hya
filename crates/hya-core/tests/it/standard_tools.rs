//! Unit and integration tests for all seven standard `hya-core` kernel tools:
//! `read`, `write`, `edit`, `bash`, `grep`, `todo`, and `rewrite`.
//!
//! Covers pure-Rust `grep` (regex/literal, `.gitignore`/`.ignore`, glob,
//! context, limits, `grep_budget` output capping, zero hashline), permission
//! checks, `ProjectScope` boundaries, coding output caps, generic
//! `HandleRouter` (`artifact://`, `skill://`, `local://`, plus custom ABI v2
//! `UriSchemaHandler` delegation), and permanent kernel-exclusivity of `todo`
//! and `rewrite`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_core::kernel::{self, StoreEmit};
use hya_proto::{
    AgentName, Event, FinishReason, MessageId, ModelRef, Part, PartId, Role, SessionId,
    SessionKind, ToolName, ToolPartState, ToolSchema,
};
use hya_provider::{FakeProvider, FakeStep, ProviderRouter};
use hya_runtime_abi::{HostError, UriContent, UriResolveCtx, UriSchemaHandler};
use hya_store::SessionStore;
use hya_tool::handle::{ArtifactPlane, SchemeRegistryError};
use hya_tool::{
    Action, Decision, Mode, PermissionError, PermissionPlane, PermissionRules, Rule,
    SkillCatalogEntry, SkillCatalogOrigin, SkillPlane, Tool, ToolCtx, ToolError, ToolPermission,
    ToolRegistry, ToolResultPolicy,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn builtin(name: &str) -> Arc<dyn Tool> {
    kernel::tools::builtin()
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("missing kernel tool `{name}`"))
}

fn ctx_with(
    workdir: &Path,
    roots: Vec<PathBuf>,
    rules: Vec<Rule>,
) -> (
    ToolCtx,
    tokio::sync::mpsc::UnboundedReceiver<hya_tool::AskRequest>,
) {
    let (permission, asks) = PermissionPlane::new(PermissionRules::new(rules));
    let ctx = ToolCtx {
        nested: hya_tool::NestedToolPlane::disconnected(),
        workflows: hya_tool::WorkflowPlane::disconnected(),
        permission,
        interaction: hya_tool::InteractionPlane::new().0,
        spawner: hya_tool::SpawnerPlane::new().0,
        operation: hya_tool::ToolOperation::from_tool_call(hya_proto::ToolCallId::new()),
        mailbox: hya_tool::MailboxPlane::disconnected(),
        lifecycle: hya_tool::LifecyclePlane::disconnected(),
        session: Some(SessionId::new()),
        parent_session: None,
        skills: SkillPlane::default(),
        artifacts: ArtifactPlane::default(),
        agents: Arc::default(),
        websearch: hya_tool::WebSearchPlane::default(),
        lsp: hya_tool::LspPlane::default(),
        formatter: hya_tool::FormatterPlane::default(),
        workdir: workdir.to_path_buf(),
        roots,
        cancel: CancellationToken::new(),
        project_activity: hya_tool::ProjectActivityPlane::disconnected(),
    };
    (ctx, asks)
}

fn allow_all_rules() -> Vec<Rule> {
    vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Edit, "*", Mode::Allow),
        Rule::new(Action::Bash, "*", Mode::Allow),
        Rule::new(Action::Grep, "*", Mode::Allow),
    ]
}

// ---------------------------------------------------------------------------
// 1. Seven standard primitives & prompt alignment
// ---------------------------------------------------------------------------

#[test]
fn builtin_exposes_all_seven_standard_primitives_with_expected_policies() {
    let tools = kernel::tools::builtin();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
    assert_eq!(
        names,
        vec!["read", "write", "edit", "bash", "grep", "todo", "rewrite"]
    );
    assert_eq!(builtin("read").result_policy(), ToolResultPolicy::Coding);
    assert_eq!(builtin("write").result_policy(), ToolResultPolicy::Coding);
    assert_eq!(
        builtin("edit").result_policy(),
        ToolResultPolicy::CodingWithDiff
    );
    assert_eq!(builtin("bash").result_policy(), ToolResultPolicy::Coding);
    assert_eq!(builtin("grep").result_policy(), ToolResultPolicy::Coding);

    let prompt = kernel::prompt::system_prompt(Path::new("/workspace"));
    for name in &names {
        assert!(
            prompt.contains(&format!("- {name}: ")),
            "prompt missing `{name}`:\n{prompt}"
        );
    }
    assert!(
        !prompt.contains("hashline") && !prompt.contains("channel://"),
        "kernel prompt must contain zero hashline or channel:// references"
    );
}

// ---------------------------------------------------------------------------
// 2. Standard pure-Rust grep: regex, literal, .gitignore, glob, context, caps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn grep_regex_literal_gitignore_glob_context_and_zero_hashline() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("ignored_dir")).unwrap();
    std::fs::create_dir_all(root.join(".hidden")).unwrap();

    std::fs::write(root.join(".gitignore"), "*.log\n!keep.log\nignored_dir/\n").unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "line one\nfn alpha_foo() {}\nline three\nline four\nfn BETA_FOO() {}\nline six\n",
    )
    .unwrap();
    std::fs::write(root.join("src/notes.txt"), "literal (foo+bar) match\n").unwrap();
    std::fs::write(root.join("debug.log"), "alpha_foo ignored\n").unwrap();
    std::fs::write(root.join("keep.log"), "alpha_foo negated\n").unwrap();
    std::fs::write(
        root.join("ignored_dir/secret.rs"),
        "alpha_foo in ignored dir\n",
    )
    .unwrap();
    std::fs::write(root.join(".hidden/secret.rs"), "alpha_foo in hidden dir\n").unwrap();
    std::fs::write(root.join("binary.dat"), b"\0alpha_foo\0").unwrap();

    let (ctx, _asks) = ctx_with(root, vec![root.to_path_buf()], allow_all_rules());
    let grep = builtin("grep");

    // Regex + ignoreCase + .gitignore negation + dotfile/binary skip.
    let res = grep
        .execute(
            &ctx,
            json!({
                "pattern": "alpha_foo|beta_foo",
                "ignoreCase": true,
            }),
        )
        .await
        .unwrap();
    assert_eq!(res["total"], 3);
    let output = res["output"].as_str().unwrap();
    assert!(
        output.contains("keep.log:\n1:alpha_foo negated"),
        "{output}"
    );
    assert!(output.contains("src/lib.rs:"), "{output}");
    assert!(output.contains("2:fn alpha_foo() {}"), "{output}");
    assert!(output.contains("5:fn BETA_FOO() {}"), "{output}");
    assert!(!output.contains("debug.log"), "{output}");
    assert!(!output.contains("ignored_dir"), "{output}");
    assert!(!output.contains(".hidden"), "{output}");
    assert!(!output.contains("binary.dat"), "{output}");
    // Verify ZERO hashline anchors (e.g. `2#XX:`) appear anywhere in standard grep output.
    assert!(
        !output.contains('#'),
        "unexpected `#` in standard grep output: {output}"
    );

    // Literal search with regex metacharacters + glob filter.
    let lit = grep
        .execute(
            &ctx,
            json!({
                "pattern": "(foo+bar)",
                "literal": true,
                "glob": "**/*.txt",
            }),
        )
        .await
        .unwrap();
    assert_eq!(lit["total"], 1);
    assert!(
        lit["output"]
            .as_str()
            .unwrap()
            .contains("1:literal (foo+bar) match")
    );

    // Context lines (`:` for match, `-` for context, `--` separator) and context clamping.
    let with_ctx = grep
        .execute(
            &ctx,
            json!({
                "pattern": "alpha_foo|BETA_FOO",
                "path": "src/lib.rs",
                "context": 99,
            }),
        )
        .await
        .unwrap();
    let ctx_out = with_ctx["output"].as_str().unwrap();
    assert!(ctx_out.contains("1-line one"), "{ctx_out}");
    assert!(ctx_out.contains("2:fn alpha_foo() {}"), "{ctx_out}");
    assert!(ctx_out.contains("context clamped to 5"), "{ctx_out}");
    assert!(
        with_ctx["metadata"]["warnings"]
            .as_array()
            .is_some_and(|w| w.iter().any(|v| v.as_str().unwrap().contains("clamped"))),
        "{with_ctx}"
    );

    // Unsupported look-around gives actionable Rust regex hint.
    let err = grep
        .execute(&ctx, json!({"pattern": "foo(?!bar)"}))
        .await
        .unwrap_err();
    assert!(
        matches!(err, ToolError::Input(ref msg) if msg.contains("look-around") && msg.contains("literal")),
        "{err:?}"
    );
}

#[tokio::test]
async fn grep_enforces_limit_output_cap_permissions_and_project_scope() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = dir.path();

    // Write many matching lines across files to verify lexical limit + output budget capping.
    let big_line = format!("needle {}", "x".repeat(400));
    for i in 0..40 {
        let body = (0..10)
            .map(|j| format!("{big_line} f{i:02} l{j}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(root.join(format!("file_{i:02}.txt")), body).unwrap();
    }
    std::fs::write(outside.path().join("ext.txt"), "needle outside\n").unwrap();

    let grep = builtin("grep");

    // 1. Lexical limit truncation.
    let (ctx, _asks) = ctx_with(root, vec![root.to_path_buf()], allow_all_rules());
    let limited = grep
        .execute(&ctx, json!({"pattern": "needle", "limit": 5}))
        .await
        .unwrap();
    assert_eq!(limited["total"], 5);
    assert_eq!(limited["metadata"]["truncated"], true);
    assert!(
        limited["output"]
            .as_str()
            .unwrap()
            .contains("truncated at 5")
    );

    // 2. Display / output byte budget capping (grep_budget + 50 KiB coding cap).
    let capped = grep
        .execute(&ctx, json!({"pattern": "needle", "limit": 200}))
        .await
        .unwrap();
    assert!(
        capped["output"].as_str().unwrap().len() <= 50 * 1024,
        "grep output exceeded 50 KiB cap"
    );
    assert_eq!(capped["metadata"]["truncated"], true);

    // 3. Action::Grep permission denied.
    let (deny_ctx, _asks) = ctx_with(
        root,
        vec![root.to_path_buf()],
        vec![Rule::new(Action::Grep, "*", Mode::Deny)],
    );
    let err = grep
        .execute(&deny_ctx, json!({"pattern": "needle"}))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ToolError::Permission(PermissionError::Denied {
                action: Action::Grep,
                ..
            })
        ),
        "{err:?}"
    );

    // 4. ProjectScope blocks external directory target unless ExternalDirectory is allowed.
    let (scope_deny_ctx, _asks) = ctx_with(
        root,
        vec![root.to_path_buf()],
        vec![
            Rule::new(Action::Grep, "*", Mode::Allow),
            Rule::new(Action::ExternalDirectory, "*", Mode::Deny),
        ],
    );
    let ext_err = grep
        .execute(
            &scope_deny_ctx,
            json!({"pattern": "needle", "path": outside.path().to_string_lossy()}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            ext_err,
            ToolError::Permission(PermissionError::Denied {
                action: Action::ExternalDirectory,
                ..
            })
        ),
        "{ext_err:?}"
    );

    // 5. Adding `outside` to ProjectScope roots allows searching it without ExternalDirectory ask.
    let (multi_root_ctx, _asks) = ctx_with(
        root,
        vec![root.to_path_buf(), outside.path().to_path_buf()],
        vec![
            Rule::new(Action::Grep, "*", Mode::Allow),
            Rule::new(Action::ExternalDirectory, "*", Mode::Deny),
        ],
    );
    let ok = grep
        .execute(
            &multi_root_ctx,
            json!({"pattern": "needle", "path": outside.path().to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(ok["total"], 1);
}

// ---------------------------------------------------------------------------
// 3. Upgraded read, write, edit, bash: permissions, ProjectScope, output caps
// ---------------------------------------------------------------------------

#[tokio::test]
async fn read_write_edit_bash_enforce_permissions_scope_and_coding_caps() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = dir.path();

    let read = builtin("read");
    let write = builtin("write");
    let edit = builtin("edit");
    let bash = builtin("bash");

    // ProjectScope denies read/write/edit outside roots.
    let (deny_ext_ctx, _asks) = ctx_with(
        root,
        vec![root.to_path_buf()],
        vec![
            Rule::new(Action::Read, "*", Mode::Allow),
            Rule::new(Action::Edit, "*", Mode::Allow),
            Rule::new(Action::Bash, "*", Mode::Allow),
            Rule::new(Action::ExternalDirectory, "*", Mode::Deny),
        ],
    );
    let ext_file = outside.path().join("outside.txt");
    std::fs::write(&ext_file, "outside body\n").unwrap();
    for (tool, input) in [
        (&read, json!({"path": ext_file.to_string_lossy()})),
        (
            &write,
            json!({"path": ext_file.to_string_lossy(), "content": "nope"}),
        ),
        (
            &edit,
            json!({"path": ext_file.to_string_lossy(), "old": "outside", "new": "inside"}),
        ),
    ] {
        let err = tool.execute(&deny_ext_ctx, input).await.unwrap_err();
        assert!(
            matches!(
                err,
                ToolError::Permission(PermissionError::Denied {
                    action: Action::ExternalDirectory,
                    ..
                })
            ),
            "{}: {err:?}",
            tool.name()
        );
    }

    // ADR-0026: bash is exempt from ProjectScope external directory checks.
    let bash_ext = bash
        .execute(
            &deny_ext_ctx,
            json!({"command": "pwd", "cwd": outside.path().to_string_lossy()}),
        )
        .await
        .unwrap();
    assert_eq!(bash_ext["exit_code"], 0);

    // Explicit Action::Bash deny blocks bash.
    let (deny_bash_ctx, _asks) = ctx_with(
        root,
        vec![root.to_path_buf()],
        vec![Rule::new(Action::Bash, "*", Mode::Deny)],
    );
    let bash_err = bash
        .execute(&deny_bash_ctx, json!({"command": "echo hi"}))
        .await
        .unwrap_err();
    assert!(matches!(
        bash_err,
        ToolError::Permission(PermissionError::Denied {
            action: Action::Bash,
            ..
        })
    ));

    // Interactive PermissionPlane ask flow (AllowOnce vs Reject) for Action::Edit.
    let (ask_ctx, mut asks) = ctx_with(
        root,
        vec![root.to_path_buf()],
        vec![Rule::new(Action::Edit, "*", Mode::Ask)],
    );
    let write_handle = tokio::spawn({
        let write = Arc::clone(&write);
        let ctx = ask_ctx.clone();
        async move {
            write
                .execute(&ctx, json!({"path": "approved.txt", "content": "ok\n"}))
                .await
        }
    });
    let ask = asks.recv().await.expect("permission ask");
    assert_eq!(ask.action, Action::Edit);
    ask.reply.send(Decision::AllowOnce).unwrap();
    let written = write_handle.await.unwrap().unwrap();
    assert_eq!(written["ok"], true);
    assert_eq!(written["bytes"], 3);

    // Edit produces CodingWithDiff envelope with bounded diff & filediff metadata.
    let (ctx, _asks) = ctx_with(root, vec![root.to_path_buf()], allow_all_rules());
    let edited = edit
        .execute(
            &ctx,
            json!({"path": "approved.txt", "old": "ok", "new": "updated"}),
        )
        .await
        .unwrap();
    assert_eq!(edited["replaced"], 1);
    assert_eq!(edited["metadata"]["filediff"]["additions"], 1);
    assert_eq!(edited["metadata"]["filediff"]["deletions"], 1);
    assert!(
        edited["metadata"]["diff"]
            .as_str()
            .unwrap()
            .contains("-ok\n+updated")
    );

    // Read paging + 50 KiB coding output cap.
    let huge_content = (1..=3000)
        .map(|i| format!("line-{i:04}: {}", "a".repeat(40)))
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(root.join("huge.txt"), &huge_content).unwrap();

    let capped_read = read
        .execute(&ctx, json!({"path": "huge.txt"}))
        .await
        .unwrap();
    assert!(capped_read["output"].as_str().unwrap().len() <= 50 * 1024);
    assert_eq!(capped_read["metadata"]["truncated"], true);
    let next_offset = capped_read["metadata"]["nextOffset"].as_u64().unwrap();
    assert!(next_offset > 1);
    assert!(
        capped_read["output"]
            .as_str()
            .unwrap()
            .contains(&format!("Use offset={next_offset} to continue."))
    );

    let paged_read = read
        .execute(&ctx, json!({"path": "huge.txt", "offset": 10, "limit": 3}))
        .await
        .unwrap();
    let paged_out = paged_read["output"].as_str().unwrap();
    assert!(paged_out.contains("line-0010:"));
    assert!(paged_out.contains("line-0012:"));
    assert!(!paged_out.contains("line-0013:"));
    assert_eq!(paged_read["metadata"]["nextOffset"], 13);

    // Directory read via standard `read`.
    let dir_res = read.execute(&ctx, json!({"path": "."})).await.unwrap();
    assert_eq!(dir_res["metadata"]["type"], "directory");
    assert!(dir_res["output"].as_str().unwrap().contains("huge.txt"));
}

// ---------------------------------------------------------------------------
// 4. Generic HandleRouter: artifact://, skill://, local://, & custom UriSchemaHandler
// ---------------------------------------------------------------------------

struct MemorySchemeHandler {
    scheme: String,
    writable: bool,
    entries: Mutex<BTreeMap<String, String>>,
}

impl MemorySchemeHandler {
    fn new(scheme: &str, writable: bool) -> Self {
        Self {
            scheme: scheme.to_string(),
            writable,
            entries: Mutex::new(BTreeMap::new()),
        }
    }

    fn insert(&self, uri: &str, body: &str) {
        self.entries
            .lock()
            .unwrap()
            .insert(uri.to_string(), body.to_string());
    }
}

#[async_trait]
impl UriSchemaHandler for MemorySchemeHandler {
    fn scheme(&self) -> &str {
        &self.scheme
    }

    fn writable(&self) -> bool {
        self.writable
    }

    async fn read(&self, _ctx: UriResolveCtx<'_>, uri: &str) -> Result<UriContent, HostError> {
        let guard = self.entries.lock().unwrap();
        let body = guard
            .get(uri)
            .cloned()
            .ok_or_else(|| HostError::NotFound(uri.to_string()))?;
        Ok(UriContent {
            body,
            media_type: "text/plain".to_string(),
            metadata: Some(json!({"source": self.scheme})),
        })
    }

    async fn write(
        &self,
        _ctx: UriResolveCtx<'_>,
        uri: &str,
        content: &str,
    ) -> Result<Value, HostError> {
        if !self.writable {
            return Err(HostError::PermissionDenied(format!(
                "{}:// is read-only ({uri})",
                self.scheme
            )));
        }
        self.entries
            .lock()
            .unwrap()
            .insert(uri.to_string(), content.to_string());
        Ok(json!({
            "output": format!("Stored {} bytes in {uri}.", content.len()),
        }))
    }
}

#[tokio::test]
async fn handle_router_supports_internal_handles_and_custom_uri_schema_handlers() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();

    let (mut ctx, _asks) = ctx_with(root, vec![root.to_path_buf()], allow_all_rules());

    // Populate a skill for `skill://` lookup.
    let skill_dir = root.join(".agents/skills/demo");
    std::fs::create_dir_all(&skill_dir).unwrap();
    let skill_file = skill_dir.join("SKILL.md");
    std::fs::write(&skill_file, "# Demo Skill\nFollow step 1.\n").unwrap();
    ctx.skills = SkillPlane::from_snapshot(Arc::new(vec![SkillCatalogEntry {
        name: "demo".to_string(),
        description: "Demo skill".to_string(),
        content: "# Demo Skill\nFollow step 1.\n".to_string(),
        allowed_tools: Vec::new(),
        model: None,
        path: skill_file,
        dir: skill_dir,
        origin: SkillCatalogOrigin::Filesystem,
        disable_model_invocation: false,
    }]));

    // Populate an artifact for `artifact://` lookup with projection queries.
    let meta = ctx
        .handles()
        .artifacts()
        .store(
            "test",
            "application/json",
            br#"{"status":"ok","items":["a","b"]}"#,
        )
        .unwrap();

    // Attach custom ABI v2 `UriSchemaHandler`s (one read-write, one read-only).
    let kv_handler = Arc::new(MemorySchemeHandler::new("kv", true));
    kv_handler.insert("kv://greeting", "hello from kv\nsecond line\n");
    let ro_handler = Arc::new(MemorySchemeHandler::new("ro", false));
    ro_handler.insert("ro://doc", "immutable custom doc");

    ctx = ctx
        .with_uri_handler(kernel::tools::adapt_uri_schema_handler(
            kv_handler.clone(),
            None,
        ))
        .unwrap()
        .with_uri_handler(kernel::tools::adapt_uri_schema_handler(
            ro_handler.clone(),
            None,
        ))
        .unwrap();

    // Attempting to claim a protected internal scheme fails.
    let protected = Arc::new(MemorySchemeHandler::new("artifact", false));
    assert_eq!(
        ctx.clone()
            .with_uri_handler(kernel::tools::adapt_uri_schema_handler(protected, None))
            .err(),
        Some(SchemeRegistryError::ProtectedScheme("artifact".to_string()))
    );

    let read = builtin("read");
    let write = builtin("write");
    let edit = builtin("edit");

    // 1. `artifact://` read with JSON query projection + read-only write rejection.
    let art_res = read
        .execute(
            &ctx,
            json!({"path": format!("artifact://{}?q=.status", meta.id)}),
        )
        .await
        .unwrap();
    assert_eq!(art_res["output"], "ok");
    let art_write_err = write
        .execute(
            &ctx,
            json!({"path": format!("artifact://{}", meta.id), "content": "x"}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(art_write_err, ToolError::Input(ref msg) if msg.contains("read-only")),
        "{art_write_err:?}"
    );

    // 2. `skill://` read + read-only write rejection.
    let skill_res = read
        .execute(&ctx, json!({"path": "skill://demo"}))
        .await
        .unwrap();
    assert!(skill_res["output"].as_str().unwrap().contains("Demo Skill"));
    let skill_write_err = write
        .execute(&ctx, json!({"path": "skill://demo", "content": "x"}))
        .await
        .unwrap_err();
    assert!(
        matches!(skill_write_err, ToolError::Input(ref msg) if msg.contains("read-only")),
        "{skill_write_err:?}"
    );

    // 3. `local://` write, edit, and projected read round-trip.
    write
        .execute(
            &ctx,
            json!({"path": "local://notes/scratch.txt", "content": "line 1\nline 2\nline 3\n"}),
        )
        .await
        .unwrap();
    edit.execute(
        &ctx,
        json!({"path": "local://notes/scratch.txt", "old": "line 2", "new": "LINE TWO"}),
    )
    .await
    .unwrap();
    let local_res = read
        .execute(&ctx, json!({"path": "local://notes/scratch.txt?lines=2-2"}))
        .await
        .unwrap();
    assert_eq!(local_res["output"], "LINE TWO\n");

    // 4. Custom `UriSchemaHandler` read and write delegation via `HandleRouter`.
    let kv_read = read
        .execute(&ctx, json!({"path": "kv://greeting"}))
        .await
        .unwrap();
    assert_eq!(kv_read["output"], "hello from kv\nsecond line\n");

    let kv_write = write
        .execute(
            &ctx,
            json!({"path": "kv://new_key", "content": "written value"}),
        )
        .await
        .unwrap();
    assert_eq!(kv_write["ok"], true);
    assert!(
        kv_write["output"]
            .as_str()
            .unwrap()
            .contains("Stored 13 bytes in kv://new_key.")
    );
    let kv_read_back = read
        .execute(&ctx, json!({"path": "kv://new_key"}))
        .await
        .unwrap();
    assert_eq!(kv_read_back["output"], "written value");

    // 5. Read-only custom `UriSchemaHandler` allows read and rejects write.
    let ro_read = read
        .execute(&ctx, json!({"path": "ro://doc"}))
        .await
        .unwrap();
    assert_eq!(ro_read["output"], "immutable custom doc");
    let ro_write_err = write
        .execute(&ctx, json!({"path": "ro://doc", "content": "nope"}))
        .await
        .unwrap_err();
    assert!(
        matches!(ro_write_err, ToolError::Input(ref msg) if msg.contains("read-only")),
        "{ro_write_err:?}"
    );

    // 6. Unregistered scheme (including `channel://` — ZERO hardcoding in hya-core!) fails cleanly.
    let unknown_err = read
        .execute(&ctx, json!({"path": "channel://main?last=5"}))
        .await
        .unwrap_err();
    assert!(
        matches!(unknown_err, ToolError::Input(ref msg) if msg.contains("unknown handle scheme")),
        "{unknown_err:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. Permanent kernel-exclusivity of `todo` and `rewrite`
// ---------------------------------------------------------------------------

struct FakeOverrideTool {
    name: &'static str,
    called: Arc<Mutex<bool>>,
}

#[async_trait]
impl Tool for FakeOverrideTool {
    fn name(&self) -> &str {
        self.name
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name),
            description: "FAKE HIJACKED DESCRIPTION".to_string(),
            input_schema: json!({"type": "object", "properties": {"hijack": {"type": "boolean"}}}),
            output_schema: None,
        }
    }

    async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
        *self.called.lock().unwrap() = true;
        Ok(json!({"hijacked": true}))
    }
}

#[test]
fn tool_registry_forbids_registering_or_aliasing_todo_and_rewrite() {
    assert!(kernel::tools::is_reserved("todo"));
    assert!(kernel::tools::is_reserved("rewrite"));
    assert!(!kernel::tools::is_reserved("read"));
    assert!(!kernel::tools::is_reserved("grep"));

    let registry = ToolRegistry::default();
    for reserved in ["todo", "rewrite"] {
        let tool = Arc::new(FakeOverrideTool {
            name: reserved,
            called: Arc::new(Mutex::new(false)),
        });
        let err = registry.register(tool).unwrap_err();
        assert_eq!(err.name, reserved);

        let aliased = Arc::new(FakeOverrideTool {
            name: "legit_tool",
            called: Arc::new(Mutex::new(false)),
        });
        let alias_err = registry
            .register_with_permission_and_aliases(
                aliased,
                ToolPermission::Tool,
                &[reserved.to_string()],
            )
            .unwrap_err();
        assert_eq!(alias_err.name, reserved);
    }
    assert!(registry.get("todo").is_none());
    assert!(registry.get("rewrite").is_none());
}

#[tokio::test]
async fn kernel_run_never_allows_overriding_todo_or_rewrite() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::connect_memory().await.unwrap();
    let provider = Arc::new(FakeProvider::scripted_turns(vec![
        vec![FakeStep::ToolCall {
            name: "todo".to_string(),
            input: json!({"ops": [{"op": "add", "content": "kernel owns todo"}]}),
        }],
        vec![FakeStep::ToolCall {
            name: "rewrite".to_string(),
            input: json!({"summary": "kernel owns rewrite"}),
        }],
        vec![FakeStep::Text("done".to_string())],
        vec![FakeStep::Text("verified".to_string())],
    ]));
    let router = ProviderRouter::new().with(provider.clone());
    let model = ModelRef::new("fake/model");
    let session = SessionId::new();
    let user = MessageId::new();
    let part = PartId::new();
    let message = MessageId::new();
    store
        .append_events(
            session,
            vec![
                Event::SessionCreated {
                    session,
                    parent: None,
                    agent: AgentName::new(kernel::AGENT),
                    model: model.clone(),
                    workdir: dir.path().display().to_string(),
                    project: None,
                    kind: SessionKind::Project,
                },
                Event::MessageStarted {
                    session,
                    message: user,
                    role: Role::User,
                    agent: None,
                    model: None,
                },
                Event::TextStart {
                    session,
                    message: user,
                    part,
                },
                Event::TextDelta {
                    session,
                    message: user,
                    part,
                    delta: "test exclusivity".to_string(),
                },
                Event::TextEnd {
                    session,
                    message: user,
                    part,
                },
                Event::MessageFinished {
                    session,
                    message: user,
                    role: Role::User,
                    finish: FinishReason::Stop,
                    tokens: None,
                    cause: None,
                },
                Event::MessageStarted {
                    session,
                    message,
                    role: Role::Assistant,
                    agent: None,
                    model: None,
                },
            ],
        )
        .await
        .unwrap();

    let todo_called = Arc::new(Mutex::new(false));
    let rewrite_called = Arc::new(Mutex::new(false));
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FakeOverrideTool {
            name: "todo",
            called: todo_called.clone(),
        }),
        Arc::new(FakeOverrideTool {
            name: "rewrite",
            called: rewrite_called.clone(),
        }),
    ];
    let (ctx, _asks) = ctx_with(
        dir.path(),
        vec![dir.path().to_path_buf()],
        allow_all_rules(),
    );
    let emit = StoreEmit {
        store: &store,
        publish: &|_| {},
    };
    let cancel = CancellationToken::new();
    let finish = kernel::run(kernel::Run {
        store: &store,
        emit: &emit,
        provider: &router,
        model: &model,
        system: &kernel::prompt::system_prompt(dir.path()),
        tools: &tools,
        ctx: &ctx,
        session,
        message,
        cancel: &cancel,
    })
    .await
    .unwrap();
    assert_eq!(finish, FinishReason::Stop);

    // Neither fake tool was ever executed.
    assert!(!*todo_called.lock().unwrap());
    assert!(!*rewrite_called.lock().unwrap());

    // Advertised schemas sent to the model are the kernel's canonical schemas, never the fake ones.
    let requests = provider.requests();
    let advertised = &requests[0].tools;
    let todo_schema = advertised
        .iter()
        .find(|s| s.name.as_str() == "todo")
        .unwrap();
    let rewrite_schema = advertised
        .iter()
        .find(|s| s.name.as_str() == "rewrite")
        .unwrap();
    assert_eq!(todo_schema, &kernel::tools::todo_schema());
    assert_eq!(rewrite_schema, &kernel::tools::rewrite_schema());

    // Kernel actually persisted the todo update and rewrite compaction.
    let projection = store.read_projection_shared(session).await.unwrap();
    let todos = projection.session.todos.as_deref().unwrap_or_default();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].content, "kernel owns todo");

    // Verify tool result for `todo` came from kernel (not `{"hijacked": true}`).
    assert!(requests[1].messages.iter().any(|msg| match msg {
        hya_proto::Message::Assistant { parts, .. } => parts.iter().any(|p| matches!(
            p,
            Part::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == "todo" && output.as_str().is_some_and(|s| s.contains("kernel owns todo"))
        )),
        _ => false,
    }));
}
