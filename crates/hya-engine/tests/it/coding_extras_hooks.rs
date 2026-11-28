//! End-to-end `SessionEngine` integration tests for `hya/coding-extras`
//! native hooks (`PostEditHook` and `AskUserVisibilityHook`).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use hya_coding_extras_core::{CodingExtrasState, coding_extras_hooks};
use hya_engine::{AgentCatalog, EventBus, RuntimeRegistry, SessionEngine};
use hya_proto::{
    Event, FinishReason, MessageId, ModelRef, PartProjection, SessionId, ToolPartState,
};
use hya_provider::{
    Capabilities, CompletionRequest, EventStream, FakeProvider, FakeStep, Provider, ProviderError,
    ProviderRouter,
};
use hya_store::SessionStore;
use hya_tool::{
    Action, FormatterError, FormatterPlane, FormatterProvider, FormatterStatus, InteractionPlane,
    LspError, LspPlane, LspProvider, LspRequest, Mode, PermissionPlane, PermissionRules, Rule,
    ToolPermission, ToolRegistry, path::display_path,
};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::support;

struct RecordingFormatter {
    calls: Arc<Mutex<Vec<PathBuf>>>,
}

#[async_trait]
impl FormatterProvider for RecordingFormatter {
    async fn status(&self, _workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError> {
        Ok(Vec::new())
    }

    async fn format_file(&self, _workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
        self.calls.lock().await.push(file.to_path_buf());
        let text = tokio::fs::read_to_string(file)
            .await
            .map_err(|error| FormatterError(error.to_string()))?;
        let formatted = text.replace("UNFORMATTED", "FORMATTED");
        tokio::fs::write(file, formatted.as_bytes())
            .await
            .map_err(|error| FormatterError(error.to_string()))?;
        Ok(true)
    }
}

struct RecordingLsp {
    touches: Arc<Mutex<Vec<(PathBuf, String)>>>,
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
        self.touches
            .lock()
            .await
            .push((file.to_path_buf(), content));
        Ok(())
    }

    async fn diagnostics(&self, _workdir: &Path, _targets: &[&Path]) -> Result<Value, LspError> {
        Ok(self.diagnostics.clone())
    }
}

fn bare_file_tools_registry() -> ToolRegistry {
    let registry = ToolRegistry::default();
    for tool in hya_engine::kernel::tools::bare_tools() {
        let name = tool.name();
        if hya_engine::kernel::tools::is_reserved(name) {
            continue;
        }
        let permission = match name {
            "read" | "grep" | "skill" => ToolPermission::ReadOnly,
            "bash" => ToolPermission::Command,
            _ => ToolPermission::Tool,
        };
        registry.register_builtin(tool, permission, [], "hya/bare");
    }
    registry
}

fn hashline_file_tools_registry() -> ToolRegistry {
    hya_tool_loader::try_from_tool_families(&["hya/hashline"]).expect("load hya/hashline")
}

async fn completed_tool_outputs(
    engine: &SessionEngine,
    session: SessionId,
    tool: &str,
) -> Vec<Value> {
    let projection = engine.read_projection(session).await.unwrap();
    projection
        .session
        .messages
        .iter()
        .flat_map(|message| message.parts.iter())
        .filter_map(|part| match part {
            PartProjection::Tool {
                name,
                state: ToolPartState::Completed { output, .. },
                ..
            } if name.as_str() == tool => Some(output.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn post_edit_hook_fires_on_both_bare_and_hashline_write_and_edit() {
    for mode in ["bare", "hashline"] {
        let dir = support::TestDir::new(&format!("coding-extras-{mode}"));
        let target = dir.path().join("demo.rs");
        let target_display = display_path(&target);
        let diagnostics = json!({
            target_display.clone(): [{
                "severity": 1,
                "range": { "start": { "line": 0, "character": 0 } },
                "message": format!("lint error in {mode}")
            }]
        });

        let formatted_calls = Arc::new(Mutex::new(Vec::new()));
        let lsp_touches = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(CodingExtrasState::with_planes(
            FormatterPlane::new(Arc::new(RecordingFormatter {
                calls: Arc::clone(&formatted_calls),
            })),
            LspPlane::new(Arc::new(RecordingLsp {
                touches: Arc::clone(&lsp_touches),
                diagnostics: diagnostics.clone(),
            })),
        ));
        let native_hooks = coding_extras_hooks(state, None);

        let write_input = json!({
            "path": "demo.rs",
            "content": "fn main() { /* UNFORMATTED */ }\n"
        });
        let edit_input = if mode == "bare" {
            json!({
                "path": "demo.rs",
                "old": "FORMATTED",
                "new": "UNFORMATTED_AGAIN"
            })
        } else {
            json!({
                "path": "demo.rs",
                "old_text": "FORMATTED",
                "new_text": "UNFORMATTED_AGAIN"
            })
        };

        let provider = Arc::new(FakeProvider::scripted_turns(vec![
            vec![
                FakeStep::ToolCall {
                    name: "write".into(),
                    input: write_input,
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::ToolCall {
                    name: "edit".into(),
                    input: edit_input,
                },
                FakeStep::Finish(FinishReason::ToolCalls),
            ],
            vec![
                FakeStep::Text("done".into()),
                FakeStep::Finish(FinishReason::Stop),
            ],
        ]));

        let registry = if mode == "bare" {
            bare_file_tools_registry()
        } else {
            hashline_file_tools_registry()
        };
        let store = SessionStore::connect_memory().await.unwrap();
        let router = Arc::new(ProviderRouter::new().with(provider));
        let runtime = Arc::new(RuntimeRegistry::new(
            registry,
            Arc::new(
                AgentCatalog::with_minimal_agent(Arc::new(
                    hya_bundle::BundleCatalog::from_prepared(&[]).unwrap(),
                ))
                .unwrap(),
            ),
        ));
        let (permission, _asks) = PermissionPlane::new(PermissionRules::new(vec![
            Rule::new(Action::Tool, "*", Mode::Allow),
            Rule::new(Action::Edit, "*", Mode::Allow),
            Rule::new(Action::Read, "*", Mode::Allow),
        ]));
        let engine = SessionEngine::new(store, router, runtime, permission, EventBus::new(64))
            .with_hook_list(native_hooks);

        let session = support::create_root_session(&engine, "hya-bare", "fake", dir.path()).await;
        engine
            .admit_user_prompt(session, "write and edit demo.rs".into())
            .await
            .unwrap();
        let finish = engine
            .run_turn(
                session,
                &support::agent_spec("hya-bare", "fake", dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(finish, FinishReason::Stop, "mode={mode}");

        // Formatter ran after both `write` and `edit`.
        assert_eq!(formatted_calls.lock().await.len(), 2, "mode={mode}");
        // LSP observed formatted bytes after both `write` and `edit`.
        let touches = lsp_touches.lock().await;
        assert_eq!(touches.len(), 2, "mode={mode}");
        assert!(touches[0].1.contains("FORMATTED"), "mode={mode}");
        assert!(touches[1].1.contains("FORMATTED_AGAIN"), "mode={mode}");
        // Both persisted tool outputs carry LSP diagnostics.
        for tool in ["write", "edit"] {
            let outputs = completed_tool_outputs(&engine, session, tool).await;
            assert_eq!(outputs.len(), 1, "mode={mode} tool={tool}");
            assert_eq!(outputs[0]["metadata"]["diagnostics"], diagnostics);
            let text = outputs[0]["output"].as_str().unwrap();
            assert!(
                text.contains(&format!("lint error in {mode}")),
                "mode={mode} tool={tool} output={text}"
            );
        }
    }
}

struct RecordingToolsProvider {
    offered: Arc<Mutex<Vec<Vec<String>>>>,
}

#[async_trait]
impl Provider for RecordingToolsProvider {
    fn id(&self) -> &str {
        "recording"
    }

    fn capabilities(&self, _model: &ModelRef) -> Option<Capabilities> {
        Some(Capabilities {
            streaming_tool_calls: true,
            ..Capabilities::default()
        })
    }

    async fn stream(
        &self,
        req: CompletionRequest,
        session: SessionId,
        message: MessageId,
    ) -> Result<EventStream, ProviderError> {
        let names = req
            .tools
            .iter()
            .map(|schema| schema.name.to_string())
            .collect();
        self.offered.lock().await.push(names);
        Ok(Box::pin(futures::stream::iter([Ok(
            Event::MessageFinished {
                session,
                message,
                role: hya_proto::Role::Assistant,
                finish: FinishReason::Stop,
                tokens: None,
                cause: None,
            },
        )])))
    }
}

#[tokio::test]
async fn ask_user_visibility_hook_filters_offered_tools_by_interaction_availability() {
    let dir = support::TestDir::new("ask-user-visibility");
    let state = Arc::new(CodingExtrasState::new());
    let native_hooks = coding_extras_hooks(state, None);

    for interactive in [false, true] {
        let offered = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(RecordingToolsProvider {
            offered: Arc::clone(&offered),
        });
        let (interaction, _questions_rx) = InteractionPlane::new();
        let mut engine = support::engine_with_provider(provider, PermissionRules::default())
            .await
            .with_hook_list(native_hooks.clone());
        if interactive {
            engine = engine.with_interaction(interaction);
        }
        let session =
            support::create_root_session(&engine, "hya-main", "recording", dir.path()).await;
        engine
            .admit_user_prompt(session, "hello".into())
            .await
            .unwrap();
        let finish = engine
            .run_turn(
                session,
                &support::agent_spec("hya-main", "recording", dir.path()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(finish, FinishReason::Stop);

        let recorded = offered.lock().await;
        assert_eq!(recorded.len(), 1);
        assert_eq!(
            recorded[0].iter().any(|name| name == "ask_user"),
            interactive,
            "interactive={interactive}, tools={:?}",
            recorded[0]
        );
    }
}
