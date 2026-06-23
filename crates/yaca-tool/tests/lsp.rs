#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use yaca_proto::SessionId;
use yaca_tool::{
    Action, InteractionPlane, LspError, LspOperation, LspPlane, LspProvider, LspRequest, Mode,
    PermissionPlane, PermissionRules, Rule, SkillPlane, SpawnerPlane, TodoPlane, ToolCtx,
    ToolError, ToolRegistry, WebSearchPlane,
};

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

fn allow(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Allow)
}

fn deny(action: Action, pat: &str) -> Rule {
    Rule::new(action, pat, Mode::Deny)
}

fn tempdir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("yaca-lsp-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn ctx_with(rules: Vec<Rule>, lsp: LspPlane, workdir: PathBuf) -> ToolCtx {
    let session = SessionId::new();
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(rules));
    let (interaction, _irx) = InteractionPlane::new();
    let (spawner, _srx) = SpawnerPlane::new();
    ToolCtx {
        permission: permission.for_session(session),
        interaction: interaction.for_session(session),
        spawner,
        session: Some(session),
        parent_session: None,
        todo: TodoPlane::default(),
        skills: SkillPlane::default(),
        websearch: WebSearchPlane::default(),
        lsp,
        workdir,
        cancel: CancellationToken::new(),
    }
}

#[test]
fn lsp_schema_includes_open_code_descriptions() {
    // Given
    let tool = ToolRegistry::builtins().get("lsp").unwrap();

    // When
    let schema = tool.schema();
    let properties = &schema.input_schema["properties"];

    // Then
    assert!(schema.description.contains("Supported operations"));
    assert_eq!(
        properties["operation"]["description"],
        "The LSP operation to perform"
    );
    assert_eq!(
        properties["filePath"]["description"],
        "The absolute or relative path to the file"
    );
    assert_eq!(
        properties["line"]["description"],
        "The line number (1-based, as shown in editors)"
    );
    assert_eq!(
        properties["character"]["description"],
        "The character offset (1-based, as shown in editors)"
    );
    assert_eq!(
        properties["query"]["description"],
        "Search query for workspaceSymbol. Empty string requests all symbols."
    );
}

#[tokio::test]
async fn lsp_calls_provider_and_returns_open_code_result_shape() {
    // Given
    let workdir = tempdir();
    let src = workdir.join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
    let result = vec![json!({
        "uri": "file:///tmp/target.rs",
        "range": { "start": { "line": 0, "character": 0 } }
    })];
    let requests = Arc::new(Mutex::new(Vec::new()));
    let lsp = LspPlane::new(Arc::new(FakeLsp {
        available: true,
        result: result.clone(),
        requests: requests.clone(),
    }));
    let ctx = ctx_with(vec![allow(Action::Lsp, "*")], lsp, workdir.clone());
    let tool = ToolRegistry::builtins().get("lsp").unwrap();

    // When
    let out = tool
        .execute(
            &ctx,
            json!({
                "operation": "goToDefinition",
                "filePath": "src/main.rs",
                "line": 3,
                "character": 5
            }),
        )
        .await
        .unwrap();

    // Then
    assert_eq!(out["title"], "goToDefinition src/main.rs:3:5");
    assert_eq!(out["metadata"]["result"], json!(result));
    assert_eq!(
        out["output"],
        serde_json::to_string_pretty(&result).unwrap()
    );
    let calls = requests.lock().await;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].operation, LspOperation::GoToDefinition);
    assert_eq!(calls[0].file, workdir.join("src/main.rs"));
    assert_eq!(calls[0].line, 2);
    assert_eq!(calls[0].character, 4);
}

#[tokio::test]
async fn lsp_reports_no_server_when_no_provider_is_available() {
    // Given
    let workdir = tempdir();
    std::fs::write(workdir.join("main.rs"), "fn main() {}\n").unwrap();
    let ctx = ctx_with(vec![allow(Action::Lsp, "*")], LspPlane::default(), workdir);
    let tool = ToolRegistry::builtins().get("lsp").unwrap();

    // When
    let result = tool
        .execute(
            &ctx,
            json!({
                "operation": "hover",
                "filePath": "main.rs",
                "line": 1,
                "character": 1
            }),
        )
        .await;

    // Then
    assert!(
        matches!(result, Err(ToolError::Other(message)) if message == "No LSP server available for this file type.")
    );
}

#[tokio::test]
async fn lsp_requires_permission_before_calling_provider() {
    // Given
    let workdir = tempdir();
    std::fs::write(workdir.join("main.rs"), "fn main() {}\n").unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let lsp = LspPlane::new(Arc::new(FakeLsp {
        available: true,
        result: Vec::new(),
        requests: requests.clone(),
    }));
    let ctx = ctx_with(vec![deny(Action::Lsp, "*")], lsp, workdir);
    let tool = ToolRegistry::builtins().get("lsp").unwrap();

    // When
    let result = tool
        .execute(
            &ctx,
            json!({
                "operation": "hover",
                "filePath": "main.rs",
                "line": 1,
                "character": 1
            }),
        )
        .await;

    // Then
    assert!(matches!(result, Err(ToolError::Permission(_))));
    assert!(requests.lock().await.is_empty());
}
