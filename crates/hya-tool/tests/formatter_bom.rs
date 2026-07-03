#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_tool::{
    Action, FormatterError, FormatterPlane, FormatterProvider, InteractionPlane, LspPlane, Mode,
    PermissionPlane, PermissionRules, Rule, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx,
    ToolRegistry, WebSearchPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

fn tempdir() -> PathBuf {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "hya-formatter-bom-{nanos}-{}-{id}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with_formatter(workdir: PathBuf, formatter: FormatterPlane) -> ToolCtx {
    let (permission, _rx) =
        PermissionPlane::new(PermissionRules::new(vec![allow(Action::Edit, "*")]));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        permission,
        interaction,
        spawner,
        mailbox: hya_tool::MailboxPlane::disconnected(),
        session: None,
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp: LspPlane::default(),
        formatter,
        agents: hya_tool::AgentCatalogPlane::default(),
        workdir,
        cancel: CancellationToken::new(),
    }
}

struct BomDroppingFormatter;

#[async_trait]
impl FormatterProvider for BomDroppingFormatter {
    async fn status(
        &self,
        _workdir: &Path,
    ) -> Result<Vec<hya_tool::FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    async fn format_file(&self, _workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        tokio::fs::write(file, b"formatted\n").await.unwrap();
        Ok(true)
    }
}

#[tokio::test]
async fn write_restores_utf8_bom_after_formatter_rewrites_file() {
    // Given: an existing UTF-8 BOM file and a formatter that rewrites without BOM.
    let dir = tempdir();
    let target = dir.join("bom.txt");
    tokio::fs::write(&target, b"\xEF\xBB\xBFold\n")
        .await
        .unwrap();
    let formatter = FormatterPlane::new(Arc::new(BomDroppingFormatter));
    let ctx = ctx_with_formatter(dir.clone(), formatter);
    let tool = ToolRegistry::builtins().get("write").unwrap();

    // When: write runs and the formatter rewrites the file.
    tool.execute(&ctx, json!({ "filePath": "bom.txt", "content": "new\n" }))
        .await
        .unwrap();

    // Then: hya restores the desired BOM after formatter output.
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"\xEF\xBB\xBFformatted\n"
    );
}

#[tokio::test]
async fn edit_restores_utf8_bom_after_formatter_rewrites_file() {
    // Given: an existing UTF-8 BOM file and a formatter that rewrites without BOM.
    let dir = tempdir();
    let target = dir.join("bom.txt");
    tokio::fs::write(&target, b"\xEF\xBB\xBFold\n")
        .await
        .unwrap();
    let formatter = FormatterPlane::new(Arc::new(BomDroppingFormatter));
    let ctx = ctx_with_formatter(dir.clone(), formatter);
    let tool = ToolRegistry::builtins().get("edit").unwrap();

    // When: edit runs and the formatter rewrites the file.
    tool.execute(
        &ctx,
        json!({
            "filePath": "bom.txt",
            "oldString": "old\n",
            "newString": "new\n"
        }),
    )
    .await
    .unwrap();

    // Then: hya restores the desired BOM after formatter output.
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"\xEF\xBB\xBFformatted\n"
    );
}

#[tokio::test]
async fn apply_patch_restores_utf8_bom_after_formatter_rewrites_file() {
    // Given: an existing UTF-8 BOM file and a formatter that rewrites without BOM.
    let dir = tempdir();
    let target = dir.join("bom.txt");
    tokio::fs::write(&target, b"\xEF\xBB\xBFold\n")
        .await
        .unwrap();
    let formatter = FormatterPlane::new(Arc::new(BomDroppingFormatter));
    let ctx = ctx_with_formatter(dir.clone(), formatter);
    let tool = ToolRegistry::builtins().get("apply_patch").unwrap();

    // When: apply_patch updates the file without requiring BOM in the patch body.
    tool.execute(
        &ctx,
        json!({
            "patchText": "*** Begin Patch\n*** Update File: bom.txt\n@@\n-old\n+new\n*** End Patch\n"
        }),
    )
    .await
    .unwrap();

    // Then: hya restores the desired BOM after formatter output.
    assert_eq!(
        tokio::fs::read(&target).await.unwrap(),
        b"\xEF\xBB\xBFformatted\n"
    );
}
