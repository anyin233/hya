//! `apply_patch`, `lsp`, formatter-before-LSP post-edit pipelines, and UTF-8 BOM restoration.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::SessionId;
use hya_tool::{
    Action, FormatterPlane, LspError, LspOperation, LspPlane, LspProvider, LspRequest, ToolError,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::common::{
    RecordingLsp, RewritingFormatter, StaticBytesFormatter, TempDirGuard, allow, ctx_with,
    ctx_with_planes, deny, error_diagnostics, tempdir,
};

#[tokio::test]
async fn apply_patch_add_update_move_delete_permission_and_missing_context() {
    let dir = tempdir("apply-patch");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::create_dir_all(dir.join("src")).await.unwrap();
    tokio::fs::write(dir.join("src/app.txt"), "hello\nold\n")
        .await
        .unwrap();
    tokio::fs::write(dir.join("remove.txt"), "obsolete\n")
        .await
        .unwrap();

    let tool = hya_tool_loader::builtins().get("apply_patch").unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
    let patch = "*** Begin Patch\n\
*** Add File: notes/todo.txt\n\
+ship apply_patch\n\
*** Update File: src/app.txt\n\
@@\n\
 hello\n\
-old\n\
+new\n\
*** Update File: src/app.txt\n\
*** Move to: src/main.txt\n\
@@\n\
 hello\n\
 new\n\
*** Delete File: remove.txt\n\
*** End Patch\n";
    let out = tool
        .execute(&ctx, json!({ "patchText": patch }))
        .await
        .unwrap();

    assert_eq!(
        tokio::fs::read_to_string(dir.join("notes/todo.txt"))
            .await
            .unwrap(),
        "ship apply_patch\n"
    );
    assert_eq!(
        tokio::fs::read_to_string(dir.join("src/main.txt"))
            .await
            .unwrap(),
        "hello\nnew\n"
    );
    assert!(!dir.join("src/app.txt").exists() && !dir.join("remove.txt").exists());
    let output = out["output"].as_str().unwrap();
    assert!(
        output.contains("A notes/todo.txt")
            && output.contains("M src/main.txt")
            && output.contains("D remove.txt")
    );
    assert_eq!(out["metadata"]["files"].as_array().unwrap().len(), 4);

    // Edit permission required before writing.
    let denied = ctx_with(vec![deny(Action::Edit, "*")], dir.clone());
    assert!(
        tool.execute(&denied, json!({ "patchText": "*** Begin Patch\n*** Add File: blocked.txt\n+no\n*** End Patch\n" }))
            .await
            .is_err()
    );
    assert!(!dir.join("blocked.txt").exists());

    // Missing context fails without mutating target.
    assert!(
        tool.execute(&ctx, json!({ "patchText": "*** Begin Patch\n*** Update File: src/main.txt\n@@\n-missing\n+x\n*** End Patch\n" }))
            .await
            .is_err()
    );
    assert_eq!(
        tokio::fs::read_to_string(dir.join("src/main.txt"))
            .await
            .unwrap(),
        "hello\nnew\n"
    );
}

#[tokio::test]
async fn post_edit_formatter_runs_before_lsp_across_write_edit_and_apply_patch() {
    let reg = hya_tool_loader::builtins();
    for (tool_name, setup, input, diag_msg) in [
        (
            "write",
            None,
            json!({ "path": "main.rs", "content": "raw\n" }),
            "bad write",
        ),
        (
            "edit",
            Some("old\n"),
            json!({ "path": "main.rs", "edits": [{ "op": "replace_text", "oldText": "old\n", "newText": "new\n" }] }),
            "bad edit",
        ),
        (
            "apply_patch",
            None,
            json!({ "patchText": "*** Begin Patch\n*** Add File: main.rs\n+raw\n*** End Patch\n" }),
            "bad patch",
        ),
    ] {
        let dir = tempdir("fmt-lsp");
        let _guard = TempDirGuard::new(&dir);
        let target = dir.join("main.rs");
        if let Some(seed) = setup {
            tokio::fs::write(&target, seed).await.unwrap();
        }
        let diagnostics = error_diagnostics(&target, diag_msg);
        let touches = Arc::new(Mutex::new(Vec::new()));
        let lsp = LspPlane::new(Arc::new(RecordingLsp {
            touches: touches.clone(),
            diagnostics: diagnostics.clone(),
        }));
        let ctx = ctx_with_planes(
            vec![allow(Action::Edit, "*")],
            dir.clone(),
            FormatterPlane::new(Arc::new(RewritingFormatter)),
            lsp,
            CancellationToken::new(),
            None,
        );
        let out = reg
            .get(tool_name)
            .unwrap()
            .execute(&ctx, input)
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "formatted\n",
            "{tool_name}"
        );
        assert_eq!(out["metadata"]["diagnostics"], diagnostics, "{tool_name}");
        assert!(
            out["output"].as_str().unwrap().contains(diag_msg),
            "{tool_name}"
        );
        let calls = touches.lock().await;
        assert_eq!(calls.len(), 1, "{tool_name}");
        assert_eq!(calls[0].file, target);
        assert_eq!(calls[0].content, "formatted\n");
    }
}

#[tokio::test]
async fn formatter_bom_restoration_across_write_edit_and_apply_patch() {
    let reg = hya_tool_loader::builtins();
    let bom_dropper = || {
        FormatterPlane::new(Arc::new(StaticBytesFormatter {
            output: b"formatted\n",
        }))
    };

    for tool_name in ["write", "edit", "apply_patch"] {
        let dir = tempdir("fmt-bom");
        let _guard = TempDirGuard::new(&dir);
        let target = dir.join("bom.txt");
        tokio::fs::write(&target, b"\xEF\xBB\xBFold\n")
            .await
            .unwrap();
        let ctx = ctx_with_planes(
            vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
            dir.clone(),
            bom_dropper(),
            LspPlane::default(),
            CancellationToken::new(),
            None,
        );
        let input = match tool_name {
            "write" => json!({ "path": "bom.txt", "content": "new\n" }),
            "edit" => {
                let read_out = reg
                    .get("read")
                    .unwrap()
                    .execute(&ctx, json!({ "path": "bom.txt" }))
                    .await
                    .unwrap();
                let anchor = read_out["output"]
                    .as_str()
                    .unwrap()
                    .lines()
                    .find(|l| l.starts_with("1#"))
                    .unwrap()
                    .split(':')
                    .next()
                    .unwrap();
                json!({ "path": "bom.txt", "edits": [{"op": "replace", "pos": anchor, "lines": ["new"]}] })
            }
            _ => {
                json!({ "patchText": "*** Begin Patch\n*** Update File: bom.txt\n@@\n-old\n+new\n*** End Patch\n" })
            }
        };
        reg.get(tool_name)
            .unwrap()
            .execute(&ctx, input)
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(&target).await.unwrap(),
            b"\xEF\xBB\xBFformatted\n",
            "{tool_name}"
        );
    }
}

#[derive(Clone)]
struct FakeLsp {
    available: bool,
    result: Vec<Value>,
    requests: Arc<Mutex<Vec<LspRequest>>>,
}

#[async_trait]
impl LspProvider for FakeLsp {
    async fn has_clients(&self, _file: &std::path::Path) -> Result<bool, LspError> {
        Ok(self.available)
    }

    async fn execute(&self, request: LspRequest) -> Result<Vec<Value>, LspError> {
        self.requests.lock().await.push(request);
        Ok(self.result.clone())
    }
}

#[tokio::test]
async fn lsp_schema_execution_no_server_and_permission_gate() {
    let tool = hya_tool_loader::builtins().get("lsp").unwrap();
    let schema = tool.schema();
    assert!(schema.description.contains("Supported operations"));
    assert_eq!(
        schema.input_schema["properties"]["line"]["description"],
        "The line number (1-based, as shown in editors)"
    );

    let dir = tempdir("lsp");
    let _guard = TempDirGuard::new(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();

    // Happy path (1-based input -> 0-based LSP request).
    let expected = vec![
        json!({ "uri": "file:///tmp/target.rs", "range": { "start": { "line": 0, "character": 0 } } }),
    ];
    let requests = Arc::new(Mutex::new(Vec::new()));
    let lsp = LspPlane::new(Arc::new(FakeLsp {
        available: true,
        result: expected.clone(),
        requests: requests.clone(),
    }));
    let ctx = ctx_with_planes(
        vec![allow(Action::Lsp, "*")],
        dir.clone(),
        FormatterPlane::default(),
        lsp,
        CancellationToken::new(),
        Some(SessionId::new()),
    );
    let out = tool
        .execute(&ctx, json!({ "operation": "goToDefinition", "filePath": "src/main.rs", "line": 3, "character": 5 }))
        .await
        .unwrap();
    assert_eq!(out["title"], "goToDefinition src/main.rs:3:5");
    assert_eq!(out["metadata"]["result"], json!(expected));
    let calls = requests.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].operation, LspOperation::GoToDefinition);
    assert_eq!((calls[0].line, calls[0].character), (2, 4));

    // Disconnected plane reports no server.
    let no_lsp = ctx_with(vec![allow(Action::Lsp, "*")], dir.clone());
    assert!(matches!(
        tool.execute(&no_lsp, json!({ "operation": "hover", "filePath": "src/main.rs", "line": 1, "character": 1 })).await,
        Err(ToolError::Other(m)) if m == "No LSP server available for this file type."
    ));

    // Permission denied blocks before calling provider.
    let denied_reqs = Arc::new(Mutex::new(Vec::new()));
    let denied_ctx = ctx_with_planes(
        vec![deny(Action::Lsp, "*")],
        dir,
        FormatterPlane::default(),
        LspPlane::new(Arc::new(FakeLsp {
            available: true,
            result: vec![],
            requests: denied_reqs.clone(),
        })),
        CancellationToken::new(),
        Some(SessionId::new()),
    );
    assert!(matches!(
        tool.execute(
            &denied_ctx,
            json!({ "operation": "hover", "filePath": "src/main.rs", "line": 1, "character": 1 })
        )
        .await,
        Err(ToolError::Permission(_))
    ));
    assert!(denied_reqs.lock().await.is_empty());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn lsp_dot_workdir_and_dot_file_reach_provider_as_current_dir() {
    const CHILD_DIR: &str = "HYA_LSP_DOT_TEST_CHILD_DIR";
    let Some(child_dir) = std::env::var_os(CHILD_DIR) else {
        let dir = tempdir("lsp-dot");
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "patch_hooks::lsp_dot_workdir_and_dot_file_reach_provider_as_current_dir",
                "--nocapture",
            ])
            .current_dir(&dir)
            .env(CHILD_DIR, &dir)
            .output()
            .unwrap();
        if dir.exists() {
            let _ = std::fs::remove_dir(&dir);
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed;"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    };

    let tool = hya_tool_loader::from_tool_families(&["hya/extended-tools"])
        .get("lsp")
        .unwrap();
    std::fs::remove_dir(child_dir).unwrap();
    assert!(std::env::current_dir().is_err());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let lsp = LspPlane::new(Arc::new(FakeLsp {
        available: true,
        result: vec![],
        requests: requests.clone(),
    }));
    let dot = PathBuf::from(".");
    lsp.workspace_symbols(&dot.join("."), "symbol".into())
        .await
        .unwrap();
    let ctx = ctx_with_planes(
        vec![
            allow(Action::Lsp, "*"),
            allow(Action::ExternalDirectory, "*"),
        ],
        dot.clone(),
        FormatterPlane::default(),
        lsp,
        CancellationToken::new(),
        Some(SessionId::new()),
    );
    tool.execute(
        &ctx,
        json!({"operation": "workspaceSymbol", "filePath": ".", "line": 1, "character": 1}),
    )
    .await
    .unwrap();
    let calls = requests.lock().await;
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|c| c.file == dot && c.operation == LspOperation::WorkspaceSymbol)
    );
}
