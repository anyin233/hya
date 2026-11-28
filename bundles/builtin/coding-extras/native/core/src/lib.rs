//! Native ABI v2 bundle (`hya/coding-extras`) providing:
//!
//! - **Tools (7):** `ls`, `glob`, `find`, `apply_patch`, `ask_user`, `lsp`,
//!   and `project_activity`.
//! - **Service (`NativeService`):** [`CodingExtrasService`] managing the
//!   configured LSP language servers and external code formatters from
//!   `hya/coding-extras`'s bundle `config.yml`.
//! - **Hooks (`NativeHook`):**
//!   - [`PostEditHook`] (`tool.execute.after`): runs configured code formatters
//!     (with UTF-8 BOM preservation) and collects LSP error diagnostics after
//!     both standard `hya-bare` `write`/`edit` and `hya/hashline` `write`/`edit`.
//!   - [`AskUserVisibilityHook`] (`tool.visibility`): hides `ask_user` when no
//!     interactive human receiver is connected.
//! - **BundleApi (`NativeBundleApiHandler`):** [`LspApi`] serving
//!   `GET /lsp/symbols` (workspace symbol search).
//!
//! The tools, hooks, and API all read the formatter and LSP planes from one
//! shared [`CodingExtrasState`]; nothing else configures them.

use std::sync::Arc;

use hya_runtime_abi::{HostRuntime, NativeHook, NativeService};
use hya_tool::Tool;

mod apply_patch;
mod ask_user;
mod file_diff;
/// Code formatter provider and configuration parser.
pub mod formatter;
mod fs_tools;
/// Native hooks (`PostEditHook`, `AskUserVisibilityHook`).
pub mod hooks;
mod lsp_api;
mod lsp_post_edit;
/// Language server lifecycle and configuration.
pub mod lsp_service;
mod lsp_tool;
mod project_activity;
/// Shared state and [`NativeService`] for LSP and formatter lifecycles.
pub mod service;
mod utf8_bom;

pub use formatter::BuiltinFormatterProvider;
pub use hooks::{
    ASK_USER_VISIBILITY_HOOK_ID, AskUserVisibilityHook, POST_EDIT_HOOK_ID, PostEditHook,
};
pub use lsp_api::LspApi;
pub use service::{
    BUNDLE_ID, CodingExtrasConfig, CodingExtrasService, CodingExtrasState, SERVICE_ID,
};

/// Construct the seven coding-extras tools sharing `state`.
#[must_use]
pub fn coding_extras_tools(state: Arc<CodingExtrasState>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(fs_tools::LsTool),
        Arc::new(fs_tools::GlobTool),
        Arc::new(fs_tools::FindTool),
        Arc::new(apply_patch::ApplyPatchTool::new(Arc::clone(&state))),
        Arc::new(ask_user::AskUserTool),
        Arc::new(lsp_tool::LspTool::new(state)),
        Arc::new(project_activity::ProjectActivityTool),
    ]
}

/// Construct the coding-extras [`NativeHook`]s sharing `state` and `runtime`.
#[must_use]
pub fn coding_extras_hooks(
    state: Arc<CodingExtrasState>,
    runtime: Option<Arc<dyn HostRuntime>>,
) -> Vec<Arc<dyn NativeHook>> {
    vec![
        Arc::new(PostEditHook::new(state, runtime)),
        Arc::new(AskUserVisibilityHook),
    ]
}

/// Construct the coding-extras [`NativeService`] sharing `state`.
#[must_use]
pub fn coding_extras_service(state: Arc<CodingExtrasState>) -> Arc<dyn NativeService> {
    Arc::new(CodingExtrasService::new(state))
}

/// Register all `hya/coding-extras` tools, services, hooks, and API handlers into `out`.
pub fn register_bundle(
    ctx: &hya_runtime_abi::NativePluginContext,
    out: &mut hya_runtime_abi::NativeRegistrations,
) {
    let state = Arc::new(CodingExtrasState::new());
    let runtime = Arc::clone(ctx.runtime());
    out.tools.extend(coding_extras_tools(Arc::clone(&state)));
    out.services.push(coding_extras_service(Arc::clone(&state)));
    out.api_handlers
        .push(Arc::new(LspApi::new(Arc::clone(&state))));
    out.hooks.extend(coding_extras_hooks(state, Some(runtime)));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    use async_trait::async_trait;
    use hya_bundle::{Bundle, PreparedAgent};
    use hya_proto::{
        ActorClaim, AgentName, Envelope, Event, EventSeq, Message, MessageId, ModelRef, Projection,
        SessionId, ToolCallId,
    };
    use hya_runtime_abi::{
        BundleApiInvocation, BundleApiResult, BundleCatalogSummary, BundleClaimLease,
        BundleClaimRequest, BundleStateEntry, ExtensionEntry, ExtensionKind, HookPoint,
        HostBundleStorage, HostCatalogReader, HostError, HostEventBus, HostExtensionRegistry,
        HostModelCaller, HostRuntime, HostSessionStore, HostTurnRunner, ModelCallRequest,
        ModelCallResponse, ModelStreamChunk, ModelWindowInfo, NativeHook, NativeService,
        NativeToolAfterInput, NativeToolAfterOutcome, NativeToolResult, SessionListFilter,
        SessionSummaryMeta, ToolVisibilityInput, ToolVisibilityOutcome, TurnOutcome,
        TurnSpawnRequest, TurnStartRequest, TurnStatus,
    };
    use hya_tool::{AgentDef, SkillCatalogEntry, path::display_path};
    use serde_json::{Value, json};
    use tokio::sync::{Mutex, broadcast, mpsc};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::formatter::{
        FormatterConfig, FormatterEntry, FormatterError, FormatterPlane, FormatterProvider,
        FormatterStatus,
    };
    use crate::lsp_service::{LspError, LspPlane, LspProvider, LspRequest};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "hya-coding-extras-{label}-{}-{id}",
                std::process::id()
            ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct StubHost {
        config: Value,
        workdir: Option<PathBuf>,
    }

    #[async_trait]
    impl HostSessionStore for StubHost {
        async fn session_exists(&self, _session: SessionId) -> Result<bool, HostError> {
            Ok(false)
        }
        async fn read_events(
            &self,
            _session: SessionId,
            _after_seq: Option<EventSeq>,
            _limit: Option<usize>,
        ) -> Result<Vec<Envelope>, HostError> {
            Ok(Vec::new())
        }
        async fn append_events(
            &self,
            _session: SessionId,
            _claim: Option<ActorClaim>,
            _events: Vec<Event>,
        ) -> Result<Vec<Envelope>, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn append_extension_event(
            &self,
            _session: SessionId,
            _bundle: &str,
            _kind: &str,
            _payload: Value,
            _claim: Option<ActorClaim>,
        ) -> Result<Envelope, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn read_projection(&self, _session: SessionId) -> Result<Arc<Projection>, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn read_extension_state(
            &self,
            _session: SessionId,
            _bundle: &str,
        ) -> Result<Option<Value>, HostError> {
            Ok(None)
        }
        async fn session_metadata(
            &self,
            session: SessionId,
        ) -> Result<Option<SessionSummaryMeta>, HostError> {
            Ok(self.workdir.as_ref().map(|dir| SessionSummaryMeta {
                session,
                parent: None,
                agent: None,
                model: None,
                workdir: Some(dir.to_string_lossy().into_owned()),
                title: None,
                project: None,
                archived: false,
                metadata: Value::Null,
                event_count: 0,
            }))
        }
        async fn list_sessions(
            &self,
            _filter: SessionListFilter,
        ) -> Result<Vec<SessionSummaryMeta>, HostError> {
            Ok(Vec::new())
        }
    }

    #[async_trait]
    impl HostBundleStorage for StubHost {
        async fn get_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _key: &str,
        ) -> Result<Option<BundleStateEntry>, HostError> {
            Ok(None)
        }
        async fn put_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _key: &str,
            _value: Value,
            _expected_version: Option<u64>,
        ) -> Result<BundleStateEntry, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn delete_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _key: &str,
        ) -> Result<bool, HostError> {
            Ok(false)
        }
        async fn list_state(
            &self,
            _bundle: &str,
            _scope: &str,
            _prefix: Option<&str>,
        ) -> Result<Vec<BundleStateEntry>, HostError> {
            Ok(Vec::new())
        }
        async fn acquire_claim(
            &self,
            _request: BundleClaimRequest,
        ) -> Result<BundleClaimLease, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn renew_claim(
            &self,
            _lease: &BundleClaimLease,
            _ttl_millis: u64,
        ) -> Result<BundleClaimLease, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn release_claim(&self, _lease: &BundleClaimLease) -> Result<(), HostError> {
            Ok(())
        }
        async fn read_claim(
            &self,
            _bundle: &str,
            _resource_key: &str,
        ) -> Result<Option<BundleClaimLease>, HostError> {
            Ok(None)
        }
    }

    #[async_trait]
    impl HostTurnRunner for StubHost {
        async fn spawn_session(&self, _request: TurnSpawnRequest) -> Result<SessionId, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn run_turn(
            &self,
            _request: TurnStartRequest,
            _cancel: CancellationToken,
        ) -> Result<TurnOutcome, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn spawn_session_and_run(
            &self,
            _request: TurnSpawnRequest,
            _cancel: CancellationToken,
        ) -> Result<TurnOutcome, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn steer_turn(&self, _session: SessionId, _message: String) -> Result<(), HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn cancel_turn(
            &self,
            _session: SessionId,
            _reason: Option<String>,
        ) -> Result<bool, HostError> {
            Ok(false)
        }
        async fn turn_status(&self, _session: SessionId) -> Result<TurnStatus, HostError> {
            Ok(TurnStatus::Idle)
        }
    }

    #[async_trait]
    impl HostModelCaller for StubHost {
        async fn complete(
            &self,
            _request: ModelCallRequest,
            _cancel: CancellationToken,
        ) -> Result<ModelCallResponse, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn stream(
            &self,
            _request: ModelCallRequest,
            _cancel: CancellationToken,
        ) -> Result<mpsc::Receiver<Result<ModelStreamChunk, HostError>>, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn compact_window(
            &self,
            _model: &ModelRef,
            _messages: &[Message],
        ) -> Result<Option<String>, HostError> {
            Ok(None)
        }
        async fn model_window(
            &self,
            _model: &ModelRef,
        ) -> Result<Option<ModelWindowInfo>, HostError> {
            Ok(None)
        }
    }

    #[async_trait]
    impl HostEventBus for StubHost {
        fn publish(&self, _envelope: Envelope) -> Result<(), HostError> {
            Ok(())
        }
        fn subscribe(&self) -> Result<broadcast::Receiver<Envelope>, HostError> {
            Ok(broadcast::channel(1).1)
        }
        async fn publish_extension(
            &self,
            _session: SessionId,
            _bundle: &str,
            _kind: &str,
            _payload: Value,
        ) -> Result<(), HostError> {
            Ok(())
        }
    }

    #[async_trait]
    impl HostCatalogReader for StubHost {
        async fn list_agents(
            &self,
            _session: Option<SessionId>,
        ) -> Result<Vec<AgentDef>, HostError> {
            Ok(Vec::new())
        }
        async fn get_agent(
            &self,
            _name: &AgentName,
            _session: Option<SessionId>,
        ) -> Result<Option<PreparedAgent>, HostError> {
            Ok(None)
        }
        async fn list_skills(
            &self,
            _session: Option<SessionId>,
        ) -> Result<Vec<SkillCatalogEntry>, HostError> {
            Ok(Vec::new())
        }
        async fn list_bundles(&self) -> Result<Vec<BundleCatalogSummary>, HostError> {
            Ok(Vec::new())
        }
        async fn get_bundle(&self, _id: &str) -> Result<Option<Bundle>, HostError> {
            Ok(None)
        }
        async fn bundle_config(&self, bundle_id: &str) -> Result<Value, HostError> {
            if bundle_id == BUNDLE_ID {
                Ok(self.config.clone())
            } else {
                Ok(Value::Null)
            }
        }
        async fn global_config(&self) -> Result<Value, HostError> {
            Ok(Value::Null)
        }
    }

    #[async_trait]
    impl HostExtensionRegistry for StubHost {
        async fn list_extensions(
            &self,
            _kind: Option<ExtensionKind>,
        ) -> Result<Vec<ExtensionEntry>, HostError> {
            Ok(Vec::new())
        }
        async fn has_bundle(&self, _bundle_id: &str) -> Result<bool, HostError> {
            Ok(false)
        }
        async fn invoke_bundle_api(
            &self,
            _call: BundleApiInvocation,
        ) -> Result<BundleApiResult, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
        async fn invoke_tool(
            &self,
            _session: Option<SessionId>,
            _tool_name: &str,
            _input: Value,
            _cancel: CancellationToken,
        ) -> Result<Value, HostError> {
            Err(HostError::Unavailable("stub".into()))
        }
    }

    impl HostRuntime for StubHost {
        fn session_store(&self) -> &dyn HostSessionStore {
            self
        }
        fn bundle_storage(&self) -> &dyn HostBundleStorage {
            self
        }
        fn turn_runner(&self) -> &dyn HostTurnRunner {
            self
        }
        fn model_caller(&self) -> &dyn HostModelCaller {
            self
        }
        fn event_bus(&self) -> &dyn HostEventBus {
            self
        }
        fn catalog_reader(&self) -> &dyn HostCatalogReader {
            self
        }
        fn extension_registry(&self) -> &dyn HostExtensionRegistry {
            self
        }
    }

    struct RewritingFormatter;

    #[async_trait]
    impl FormatterProvider for RewritingFormatter {
        async fn status(&self, _workdir: &Path) -> Result<Vec<FormatterStatus>, FormatterError> {
            Ok(Vec::new())
        }

        async fn format_file(&self, _workdir: &Path, file: &Path) -> Result<bool, FormatterError> {
            tokio::fs::write(file, b"formatted\n")
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
        async fn diagnostics(
            &self,
            _workdir: &Path,
            _targets: &[&Path],
        ) -> Result<Value, LspError> {
            Ok(self.diagnostics.clone())
        }
    }

    #[tokio::test]
    async fn service_lifecycle_reads_bundle_config_and_reports_health() {
        let state = Arc::new(CodingExtrasState::new());
        let service = CodingExtrasService::new(Arc::clone(&state));
        assert_eq!(service.id(), SERVICE_ID);
        assert!(!service.health().await.healthy);

        let host = Arc::new(StubHost {
            config: json!({
                "formatter": {
                    "customfmt": {
                        "command": ["true", "$FILE"],
                        "extensions": [".rs"]
                    }
                },
                "lsp": false
            }),
            workdir: None,
        });
        service.start(host).await.unwrap();
        let health = service.health().await;
        assert!(health.healthy);
        assert_eq!(health.status, "ready");
        assert!(state.formatter().is_configured());
        assert!(!state.lsp().is_configured());

        let rows = state.formatter().status(Path::new(".")).await.unwrap();
        assert!(
            rows.iter()
                .any(|row| serde_json::to_value(row).unwrap()["name"] == "customfmt")
        );

        service.stop().await.unwrap();
        assert!(!service.health().await.healthy);
        assert!(!state.formatter().is_configured());
    }

    #[tokio::test]
    async fn post_edit_hook_runs_formatter_bom_and_lsp_on_both_bare_and_hashline_envelopes() {
        let dir = TempDir::new("post-edit");
        let target = dir.0.join("main.rs");
        let target_display = display_path(&target);
        let diagnostics = json!({
            target_display.clone(): [{
                "severity": 1,
                "range": { "start": { "line": 0, "character": 0 } },
                "message": "syntax error"
            }]
        });

        let cases = [
            // Standard hya-bare `write` envelope (metadata.path)
            (
                "write",
                json!({
                    "title": "main.rs",
                    "output": "Wrote 4 bytes to main.rs.",
                    "ok": true,
                    "metadata": { "path": target_display.clone(), "bytes": 4 }
                }),
            ),
            // Standard hya-bare `edit` envelope (metadata.path + metadata.filediff.file)
            (
                "edit",
                json!({
                    "title": "main.rs",
                    "output": "Replaced 1 occurrence in main.rs.",
                    "replaced": 1,
                    "metadata": {
                        "path": target_display.clone(),
                        "filediff": { "file": target_display.clone() }
                    }
                }),
            ),
            // `hya/hashline` `write` envelope (metadata.path + empty diagnostics)
            (
                "write",
                json!({
                    "ok": true,
                    "title": "main.rs",
                    "output": "Wrote file successfully.",
                    "metadata": { "path": target_display.clone(), "diagnostics": {} }
                }),
            ),
            // `hya/hashline` `edit` envelope (metadata.filediff.file only, no metadata.path)
            (
                "edit",
                json!({
                    "title": "main.rs",
                    "output": "Edit applied successfully.",
                    "metadata": {
                        "classification": "applied",
                        "diagnostics": {},
                        "filediff": { "file": target_display.clone() }
                    }
                }),
            ),
        ];

        for (tool_name, envelope) in cases {
            tokio::fs::write(&target, b"\xEF\xBB\xBFraw\n")
                .await
                .unwrap();
            let touches = Arc::new(Mutex::new(Vec::new()));
            let state = Arc::new(CodingExtrasState::with_planes(
                FormatterPlane::new(Arc::new(RewritingFormatter)),
                LspPlane::new(Arc::new(RecordingLsp {
                    touches: Arc::clone(&touches),
                    diagnostics: diagnostics.clone(),
                })),
            ));
            let host = Arc::new(StubHost {
                config: Value::Null,
                workdir: Some(dir.0.clone()),
            });
            let hook = PostEditHook::new(state, Some(host));
            assert_eq!(hook.hook_points(), &[HookPoint::ToolExecuteAfter]);

            let NativeToolAfterOutcome::Continue {
                result: NativeToolResult::Ok { output, .. },
            } = hook
                .tool_execute_after(NativeToolAfterInput {
                    session: SessionId::new(),
                    message: MessageId::new(),
                    call: ToolCallId::new(),
                    tool: tool_name.to_string(),
                    input: json!({ "path": "main.rs" }),
                    result: NativeToolResult::Ok {
                        output: envelope,
                        time_ms: 5,
                    },
                })
                .await
                .unwrap()
            else {
                panic!("expected Ok tool result");
            };

            // Formatter ran AND UTF-8 BOM was preserved.
            assert_eq!(
                tokio::fs::read(&target).await.unwrap(),
                b"\xEF\xBB\xBFformatted\n",
                "{tool_name}"
            );
            // LSP observed formatted bytes after formatter.
            let recorded = touches.lock().await;
            assert_eq!(recorded.len(), 1, "{tool_name}");
            assert_eq!(recorded[0].0, target);
            assert!(recorded[0].1.contains("formatted\n"));
            // Diagnostics were attached to metadata and appended to output.
            assert_eq!(
                output["metadata"]["diagnostics"], diagnostics,
                "{tool_name}"
            );
            let text = output["output"].as_str().unwrap();
            assert!(text.contains("LSP errors detected in main.rs"), "{text}");
            assert!(text.contains("ERROR [1:1] syntax error"), "{text}");
        }
    }

    #[tokio::test]
    async fn post_edit_hook_skips_noop_edits_and_custom_uris() {
        let dir = TempDir::new("skip");
        let target = dir.0.join("main.rs");
        tokio::fs::write(&target, "raw\n").await.unwrap();
        let touches = Arc::new(Mutex::new(Vec::new()));
        let state = Arc::new(CodingExtrasState::with_planes(
            FormatterPlane::new(Arc::new(RewritingFormatter)),
            LspPlane::new(Arc::new(RecordingLsp {
                touches: Arc::clone(&touches),
                diagnostics: json!({}),
            })),
        ));
        let hook = PostEditHook::new(state, None);

        // No-op edit does not format or touch LSP.
        let _ = hook
            .tool_execute_after(NativeToolAfterInput {
                session: SessionId::new(),
                message: MessageId::new(),
                call: ToolCallId::new(),
                tool: "edit".into(),
                input: json!({}),
                result: NativeToolResult::Ok {
                    output: json!({
                        "title": "main.rs",
                        "output": "No changes were made.",
                        "metadata": {
                            "classification": "noop",
                            "filediff": { "file": display_path(&target) }
                        }
                    }),
                    time_ms: 1,
                },
            })
            .await
            .unwrap();
        assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "raw\n");
        assert!(touches.lock().await.is_empty());

        // Custom URI write is ignored cleanly.
        let _ = hook
            .tool_execute_after(NativeToolAfterInput {
                session: SessionId::new(),
                message: MessageId::new(),
                call: ToolCallId::new(),
                tool: "write".into(),
                input: json!({}),
                result: NativeToolResult::Ok {
                    output: json!({
                        "title": "memo://note-1",
                        "output": "ok",
                        "metadata": { "path": "memo://note-1" }
                    }),
                    time_ms: 1,
                },
            })
            .await
            .unwrap();
        assert!(touches.lock().await.is_empty());
    }

    #[tokio::test]
    async fn ask_user_visibility_hook_filters_non_interactive_turns() {
        let hook = AskUserVisibilityHook;
        assert_eq!(hook.hook_points(), &[HookPoint::ToolVisibility]);

        let hidden = hook
            .tool_visibility(ToolVisibilityInput {
                session: SessionId::new(),
                agent: None,
                candidate_tools: vec!["read".into(), "ask_user".into()],
                interactive: false,
            })
            .await
            .unwrap();
        assert_eq!(
            hidden,
            ToolVisibilityOutcome::Hide {
                tools: vec!["ask_user".into()]
            }
        );

        let visible = hook
            .tool_visibility(ToolVisibilityInput {
                session: SessionId::new(),
                agent: None,
                candidate_tools: vec!["read".into(), "ask_user".into()],
                interactive: true,
            })
            .await
            .unwrap();
        assert_eq!(visible, ToolVisibilityOutcome::Preserve);
    }

    #[tokio::test]
    async fn formatter_status_and_custom_execution_work_in_bundle() {
        let provider = BuiltinFormatterProvider::new(FormatterConfig::Custom(BTreeMap::from([(
            "treefmt".to_string(),
            FormatterEntry {
                command: Some(vec!["treefmt".to_string(), "$FILE".to_string()]),
                extensions: Some(vec![".nix".to_string()]),
                ..FormatterEntry::default()
            },
        )])));
        let rows = provider.status(Path::new(".")).await.unwrap();
        let json_rows = serde_json::to_value(rows).unwrap();
        assert!(json_rows.as_array().unwrap().iter().any(|item| {
            item == &json!({"name": "treefmt", "extensions": [".nix"], "enabled": true})
        }));

        let dir = TempDir::new("custom-fmt");
        let target = dir.0.join("note.txt");
        tokio::fs::write(&target, "raw\n").await.unwrap();
        let script = dir.0.join("formatter.sh");
        tokio::fs::write(&script, "printf '%s\\n' \"$FORMATTER_MARK\" > \"$1\"\n")
            .await
            .unwrap();
        let exec_provider =
            BuiltinFormatterProvider::new(FormatterConfig::Custom(BTreeMap::from([(
                "textfmt".to_string(),
                FormatterEntry {
                    command: Some(vec![
                        "sh".into(),
                        script.to_string_lossy().into_owned(),
                        "$FILE".into(),
                    ]),
                    environment: BTreeMap::from([("FORMATTER_MARK".into(), "formatted".into())]),
                    extensions: Some(vec![".txt".into()]),
                    ..FormatterEntry::default()
                },
            )])));
        assert!(exec_provider.format_file(&dir.0, &target).await.unwrap());
        assert_eq!(
            tokio::fs::read_to_string(&target).await.unwrap(),
            "formatted\n"
        );
    }
}
