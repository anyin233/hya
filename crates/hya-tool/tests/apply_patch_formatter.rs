#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use hya_tool::{
    Action, FormatterError, FormatterPlane, FormatterProvider, FormatterStatus, InteractionPlane,
    LspPlane, Mode, PermissionPlane, PermissionRules, Rule, SkillPlane, SpawnerPlane, TodoPlane,
    ToolCtx, ToolRegistry, WebSearchPlane,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hya-apply-patch-formatter-test-{nanos}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with_formatter(rules: Vec<Rule>, workdir: PathBuf, formatter: FormatterPlane) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
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

struct RewritingFormatter;

#[async_trait]
impl FormatterProvider for RewritingFormatter {
    async fn status(&self, _workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    async fn format_file(&self, _workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        tokio::fs::write(file, "formatted\n").await.unwrap();
        Ok(true)
    }
}

#[tokio::test]
async fn apply_patch_runs_formatter_after_file_changes() {
    // Given: an apply_patch tool context with an injected formatter provider.
    let dir = tempdir();
    let tool = ToolRegistry::builtins().get("apply_patch").unwrap();
    let formatter = FormatterPlane::new(Arc::new(RewritingFormatter));
    let ctx = ctx_with_formatter(vec![allow(Action::Edit, "*")], dir.clone(), formatter);

    // When: the patch creates a file that needs formatting.
    let patch = r#"*** Begin Patch
*** Add File: notes/todo.txt
+raw
*** End Patch
"#;
    tool.execute(&ctx, json!({ "patchText": patch }))
        .await
        .unwrap();

    // Then: the file content reflects formatter output, not the raw patch body.
    assert_eq!(
        tokio::fs::read_to_string(dir.join("notes/todo.txt"))
            .await
            .unwrap(),
        "formatted\n"
    );
}
