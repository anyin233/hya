//! Shared fixtures and helpers for `hya/coding-extras` integration tests.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_coding_extras_core::formatter::{FormatterError, FormatterProvider, FormatterStatus};
use hya_coding_extras_core::lsp_service::{LspError, LspProvider, LspRequest};
use hya_proto::SessionId;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, ToolCtx};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

pub fn deny(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Deny)
}

pub fn tempdir(tag: &str) -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("hya-{tag}-{nanos}-{}-{id}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::canonicalize(&dir).unwrap()
}

pub struct TempDirGuard(pub PathBuf);

impl TempDirGuard {
    pub fn new(path: &Path) -> Self {
        Self(path.to_path_buf())
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn ctx_with(rules: Vec<Rule>, workdir: PathBuf) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    ToolCtx::new(workdir, permission, CancellationToken::new())
}

pub fn ctx_in_session(rules: Vec<Rule>, workdir: PathBuf, session: SessionId) -> ToolCtx {
    let mut ctx = ctx_with(rules, workdir);
    ctx.session = Some(session);
    ctx.permission = ctx.permission.for_session(session);
    ctx.interaction = ctx.interaction.for_session(session);
    ctx
}

pub struct RewritingFormatter;

#[async_trait]
impl FormatterProvider for RewritingFormatter {
    async fn status(&self, _workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    async fn format_file(&self, _workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        tokio::fs::write(file, "formatted\n")
            .await
            .map_err(|error| FormatterError(error.to_string()))?;
        Ok(true)
    }
}

#[derive(Debug)]
pub struct Touch {
    pub file: PathBuf,
    pub content: String,
}

#[derive(Clone)]
pub struct RecordingLsp {
    pub touches: Arc<Mutex<Vec<Touch>>>,
    pub diagnostics: Value,
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

    async fn diagnostics(&self, _workdir: &Path, _targets: &[&Path]) -> Result<Value, LspError> {
        Ok(self.diagnostics.clone())
    }
}

pub fn error_diagnostics(path: &Path, message: &str) -> Value {
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
