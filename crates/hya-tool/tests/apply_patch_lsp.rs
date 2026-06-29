#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_tool::{
    Action, FormatterError, FormatterPlane, FormatterProvider, InteractionPlane, LspError,
    LspPlane, LspProvider, LspRequest, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane,
    SpawnerPlane, TodoPlane, ToolCtx, ToolRegistry, WebSearchPlane,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
struct Touch {
    file: PathBuf,
    content: String,
}

#[derive(Clone)]
struct RecordingLsp {
    touches: Arc<Mutex<Vec<Touch>>>,
    diagnostics: Value,
}

#[async_trait]
impl LspProvider for RecordingLsp {
    async fn has_clients(&self, _file: &Path) -> Result<bool, LspError> {
        Ok(true)
    }

    async fn execute(&self, _request: LspRequest) -> Result<Vec<Value>, LspError> {
        Ok(Vec::new())
    }

    async fn touch_file(&self, file: &Path, _kind: &str) -> Result<(), LspError> {
        let content = tokio::fs::read_to_string(file)
            .await
            .map_err(|error| LspError(error.to_string()))?;
        self.touches.lock().await.push(Touch {
            file: file.to_path_buf(),
            content,
        });
        Ok(())
    }

    async fn diagnostics(&self) -> Result<Value, LspError> {
        Ok(self.diagnostics.clone())
    }
}

struct RewritingFormatter;

#[async_trait]
impl FormatterProvider for RewritingFormatter {
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    async fn format_file(&self, _workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        tokio::fs::write(file, "formatted\n").await.unwrap();
        Ok(true)
    }
}

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-apply-patch-lsp-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(workdir: PathBuf, lsp: LspPlane, formatter: FormatterPlane) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![Rule::new(
        Action::Edit,
        "*",
        Mode::Allow,
    )]));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        permission,
        interaction,
        spawner,
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp,
        formatter,
        workdir,
        cancel: CancellationToken::new(),
    }
}

fn error_diagnostics(path: &Path, message: &str) -> Value {
    json!({
        path.to_string_lossy().to_string(): [{
            "severity": 1,
            "range": {
                "start": { "line": 1, "character": 2 },
                "end": { "line": 1, "character": 5 }
            },
            "message": message
        }]
    })
}

#[tokio::test]
async fn apply_patch_touches_lsp_after_formatter_and_returns_diagnostics() {
    // Given: apply_patch, a formatter that rewrites content, and an LSP provider.
    let dir = tempdir();
    let target = dir.join("src/main.rs");
    let diagnostics = error_diagnostics(&target, "bad patch");
    let touches = Arc::new(Mutex::new(Vec::new()));
    let lsp = LspPlane::new(Arc::new(RecordingLsp {
        touches: touches.clone(),
        diagnostics: diagnostics.clone(),
    }));
    let formatter = FormatterPlane::new(Arc::new(RewritingFormatter));
    let ctx = ctx_with(dir.clone(), lsp, formatter);
    let tool = ToolRegistry::builtins().get("apply_patch").unwrap();

    // When: apply_patch creates a file and formatter runs before LSP touch.
    let out = tool
        .execute(
            &ctx,
            json!({
                "patchText": "*** Begin Patch\n*** Add File: src/main.rs\n+raw\n*** End Patch\n"
            }),
        )
        .await
        .unwrap();

    // Then: LSP sees formatted content and diagnostics are returned.
    assert_eq!(out["metadata"]["diagnostics"], diagnostics);
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("LSP errors detected in src/main.rs, please fix:")
    );
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .contains("ERROR [2:3] bad patch")
    );
    let calls = touches.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].file, target);
    assert_eq!(calls[0].content, "formatted\n");
}
