//! Core Code Mode runtime promoted into `hya-bare` (ADR-0039).
//!
//! Launches `packages/hya-code-mode/runtime.ts` (or `<prefix>/lib/hya/code-mode/runtime.ts`
//! in an installed release) as a capability-bearing Bun process exposing
//! `code-mode__run`. `tools.*` and `workspace.state` are always available;
//! `agent()` / `agents.run` are dynamically gated on whether `hya/multi-agent`
//! is loaded.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hya_bundle::BuiltinBundleSelection;
use hya_engine::{
    CoreError, HostSessionReads, RuntimeSource, RuntimeSourceExport, RuntimeSourceId,
};
use hya_plugin::PluginHost;
use hya_plugin::config::PluginSpec;
use hya_plugin::messages::PluginKindWire;
use hya_tool::ToolPermission;
use sha2::{Digest as _, Sha256};

use crate::config::CodeMode;
use crate::plugin_adapter::{PluginHostAdapter, session_usage_reader};

/// Plugin identifier registered for the core Code Mode runtime.
pub const PLUGIN_ID: &str = "code-mode";

/// Provider-facing tool name exposed when Code Mode is enabled.
pub const TOOL_NAME: &str = CodeMode::TOOL;

/// Built-in bundle identity that gates `agent()` / `agents.run` inside the VM.
pub const MULTI_AGENT_BUNDLE_ID: &str = "hya/multi-agent";

/// Environment override pointing to the directory containing `runtime.ts`.
pub const CODE_MODE_DIR_ENV: &str = "HYA_CODE_MODE_DIR";

/// Environment flag forwarded to `runtime.ts` (`"1"` or `"0"`).
pub const CODE_MODE_MULTI_AGENT_ENV: &str = "HYA_CODE_MODE_MULTI_AGENT";

/// Per-call timeout for a Code Mode script invocation (1 hour, matching the
/// former `hya/code-mode` bundle manifest).
pub const DEFAULT_TIMEOUT_MS: u64 = 3_600_000;

/// Resolve the directory containing `runtime.ts` in override, installed-adjacent,
/// then workspace order.
#[must_use]
pub fn code_mode_dir() -> PathBuf {
    let executable = std::env::current_exe().unwrap_or_default();
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("."));
    resolve_code_mode_dir(
        std::env::var_os(CODE_MODE_DIR_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
        &executable,
        workspace_root,
    )
}

fn resolve_code_mode_dir(
    override_dir: Option<PathBuf>,
    executable: &Path,
    workspace_root: &Path,
) -> PathBuf {
    if let Some(override_dir) = override_dir {
        return override_dir;
    }
    let installed = executable
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("../lib/hya/code-mode");
    if installed.join("runtime.ts").is_file() {
        return installed;
    }
    workspace_root.join("packages/hya-code-mode")
}

/// Resolve the `runtime.ts` entrypoint for the core Code Mode process.
#[must_use]
pub fn runtime_entrypoint() -> PathBuf {
    code_mode_dir().join("runtime.ts")
}

/// Directory storing Code Mode traces, memos, saved scripts, and optional `config.yml`.
#[must_use]
pub fn config_dir(config_file: &Path) -> PathBuf {
    config_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("code-mode")
}

/// Optional Code Mode `config.yml` (checks `<config>/code-mode/config.yml`, then
/// legacy `<config>/bundles/hya%2Fcode-mode/config.yml`).
#[must_use]
pub fn config_file_path(config_file: &Path) -> Option<PathBuf> {
    let primary = config_dir(config_file).join("config.yml");
    if primary.is_file() {
        return Some(primary);
    }
    let legacy = config_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("bundles/hya%2Fcode-mode/config.yml");
    legacy.is_file().then_some(legacy)
}

/// Whether `agent()` / `agents.run` should be exposed inside the Code Mode VM.
///
/// Returns `true` when `hya/multi-agent` is present in `loaded_bundles` or
/// enabled by `selection`, and `false` otherwise (for example under
/// `profile: bare` or `disable: [hya/multi-agent]`).
#[must_use]
pub fn multi_agent_enabled(selection: &BuiltinBundleSelection, loaded_bundles: &[&str]) -> bool {
    loaded_bundles.contains(&MULTI_AGENT_BUNDLE_ID) || selection.is_enabled(MULTI_AGENT_BUNDLE_ID)
}

/// Build the [`PluginSpec`] for the core Code Mode Bun process.
///
/// # Errors
/// Returns [`CoreError::Invalid`] if Bun or `runtime.ts` cannot be located.
pub fn plugin_spec(config_file: &Path, multi_agent: bool) -> Result<PluginSpec, CoreError> {
    let bun = crate::plugins::find_bun()
        .ok_or_else(|| CoreError::Invalid("Bun is required for core code-mode runtime".into()))?;
    let dir = code_mode_dir();
    let entry = dir.join("runtime.ts");
    if !entry.is_file() {
        return Err(CoreError::Invalid(format!(
            "core code-mode runtime entrypoint missing: {}",
            entry.display()
        )));
    }
    let cfg_dir = config_dir(config_file);
    let mut env = BTreeMap::new();
    env.insert(
        "HYA_BUNDLE_CONFIG_DIR".into(),
        cfg_dir.to_string_lossy().into_owned(),
    );
    if let Some(cfg_file) = config_file_path(config_file) {
        env.insert(
            "HYA_BUNDLE_CONFIG_FILE".into(),
            cfg_file.to_string_lossy().into_owned(),
        );
    }
    env.insert(
        CODE_MODE_MULTI_AGENT_ENV.into(),
        if multi_agent { "1" } else { "0" }.into(),
    );
    if let Ok(path) = std::env::var("PATH") {
        env.insert("PATH".into(), path);
    }
    let command = vec![
        bun.to_string_lossy().into_owned(),
        "run".into(),
        entry.to_string_lossy().into_owned(),
        "run".into(),
    ];
    Ok(PluginSpec {
        id: PLUGIN_ID.into(),
        kind: PluginKindWire::Bun,
        command,
        timeout_ms: Some(DEFAULT_TIMEOUT_MS),
        env,
        plugin_dir: None,
        launch: Default::default(),
    }
    .in_bundle(dir))
}

/// Build the core Code Mode [`PluginSpec`] for `mode`, returning `Ok(None)` when
/// `mode == CodeMode::Normal`.
///
/// # Errors
/// Propagates [`plugin_spec`] errors when Code Mode is enabled.
pub fn plugin_spec_for_mode(
    mode: CodeMode,
    config_file: &Path,
    selection: &BuiltinBundleSelection,
    loaded_bundles: &[&str],
) -> Result<Option<PluginSpec>, CoreError> {
    match mode {
        CodeMode::Normal => Ok(None),
        CodeMode::WithTools | CodeMode::Only => {
            let multi_agent = multi_agent_enabled(selection, loaded_bundles);
            plugin_spec(config_file, multi_agent).map(Some)
        }
    }
}

/// Spawn and connect the core Code Mode process as a [`RuntimeSource`] exporting
/// `code-mode__run`.
///
/// # Errors
/// Returns [`CoreError::Invalid`] if the process fails to start or does not
/// declare the expected `run` tool.
pub async fn prepare_runtime_source(
    config_file: &Path,
    multi_agent: bool,
    reads: Option<Arc<dyn HostSessionReads>>,
) -> Result<RuntimeSource, CoreError> {
    let spec = plugin_spec(config_file, multi_agent)?;
    let host = Arc::new(
        PluginHost::connect(spec, crate::host_info(), reads.map(session_usage_reader))
            .await
            .map_err(|error| {
                CoreError::Invalid(format!("start core code-mode runtime: {error}"))
            })?,
    );
    let plugin =
        host.prepared_plugins().into_iter().next().ok_or_else(|| {
            CoreError::Invalid("core code-mode host has no prepared plugin".into())
        })?;
    let adapter = Arc::new(PluginHostAdapter::new(host));
    let mut exports = Vec::new();
    for tool in plugin.tools() {
        if tool.name() != "run" {
            return Err(CoreError::Invalid(format!(
                "core code-mode runtime exported unexpected tool `{}`",
                tool.name()
            )));
        }
        let exposed = adapter.named_tool(TOOL_NAME.to_string(), tool);
        exports.push(RuntimeSourceExport::tool(
            "run",
            TOOL_NAME,
            Vec::new(),
            exposed,
            ToolPermission::Tool,
        ));
    }
    if exports.is_empty() {
        return Err(CoreError::Invalid(
            "core code-mode runtime did not export `run`".into(),
        ));
    }
    let mut digest = Sha256::new();
    digest.update(b"hya:core-code-mode:v1\0");
    digest.update(plugin.canonical_declaration());
    Ok(RuntimeSource::new(
        RuntimeSourceId::plugin(PLUGIN_ID),
        digest.finalize().into(),
        adapter,
        exports,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used)]

    use super::*;
    use async_trait::async_trait;
    use hya_bundle::BuiltinBundleProfile;
    use hya_proto::{SessionId, ToolName, ToolSchema};
    use hya_tool::{
        NestedToolPlane, NestedTools, PermissionModel, PermissionPlane, PermissionRules, ToolCtx,
        ToolError,
    };
    use serde_json::{Value, json};
    use std::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    struct MockNested {
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl MockNested {
        fn new() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<(String, Value)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl NestedTools for MockNested {
        fn list(&self) -> Vec<ToolSchema> {
            vec![ToolSchema {
                name: ToolName::new("echo"),
                description: "Echo input back".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": { "value": { "type": "string" } },
                    "required": ["value"],
                }),
                output_schema: None,
            }]
        }

        async fn call(&self, name: &str, input: Value) -> Result<Value, ToolError> {
            self.calls
                .lock()
                .unwrap()
                .push((name.to_string(), input.clone()));
            if name == "task" {
                return Ok(json!({
                    "metadata": {
                        "members": [{
                            "member": "main/worker",
                            "session": "hysec_child",
                            "status": "done",
                            "summary": "worker finished",
                        }]
                    }
                }));
            }
            Ok(json!({ "echoed": input }))
        }

        async fn workspace_state(&self) -> Result<Option<String>, ToolError> {
            Ok(Some("rev-test".into()))
        }
    }

    fn tool_ctx(nested: Arc<MockNested>, workdir: PathBuf) -> ToolCtx {
        let permission = PermissionPlane::new(PermissionRules::new(Vec::new()))
            .0
            .with_invocation_model(PermissionModel::Danger);
        let mut ctx = ToolCtx::new(workdir, permission, CancellationToken::new());
        ctx.nested = NestedToolPlane::new(nested);
        ctx.session = Some(SessionId::new());
        ctx
    }

    fn tempdir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "hya-code-mode-{label}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn multi_agent_enabled_tracks_selection_and_loaded_bundles() {
        assert!(multi_agent_enabled(&BuiltinBundleSelection::full(), &[]));
        assert!(!multi_agent_enabled(&BuiltinBundleSelection::bare(), &[]));
        assert!(multi_agent_enabled(
            &BuiltinBundleSelection::bare(),
            &[MULTI_AGENT_BUNDLE_ID],
        ));
        assert!(!multi_agent_enabled(
            &BuiltinBundleSelection {
                profile: BuiltinBundleProfile::Full,
                enable: Vec::new(),
                disable: vec![MULTI_AGENT_BUNDLE_ID.to_string()],
            },
            &[],
        ));
        assert!(multi_agent_enabled(
            &BuiltinBundleSelection {
                profile: BuiltinBundleProfile::Bare,
                enable: vec![MULTI_AGENT_BUNDLE_ID.to_string()],
                disable: Vec::new(),
            },
            &[],
        ));
    }

    #[test]
    fn plugin_spec_for_mode_respects_normal_with_tools_only_and_multi_agent_env() {
        let root = tempdir("spec");
        let cfg = root.join("config.yaml");
        assert!(
            plugin_spec_for_mode(CodeMode::Normal, &cfg, &BuiltinBundleSelection::full(), &[])
                .unwrap()
                .is_none()
        );
        if crate::plugins::find_bun().is_none() {
            let _ = std::fs::remove_dir_all(root);
            return;
        }
        let bare_spec = plugin_spec_for_mode(
            CodeMode::WithTools,
            &cfg,
            &BuiltinBundleSelection::bare(),
            &[],
        )
        .unwrap()
        .expect("with_tools enables code-mode in bare");
        assert_eq!(bare_spec.id, PLUGIN_ID);
        assert_eq!(
            bare_spec
                .env
                .get(CODE_MODE_MULTI_AGENT_ENV)
                .map(String::as_str),
            Some("0")
        );

        let full_spec =
            plugin_spec_for_mode(CodeMode::Only, &cfg, &BuiltinBundleSelection::full(), &[])
                .unwrap()
                .expect("only enables code-mode");
        assert_eq!(
            full_spec
                .env
                .get(CODE_MODE_MULTI_AGENT_ENV)
                .map(String::as_str),
            Some("1")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn code_mode_runtime_executes_with_and_without_multi_agent() {
        if crate::plugins::find_bun().is_none() {
            return;
        }
        let root = tempdir("runtime");
        let cfg = root.join("config.yaml");

        // 1. Without multi-agent (`hya/multi-agent` not loaded): tools.* works, agent()/agents are hidden & rejected.
        let source_bare = prepare_runtime_source(&cfg, false, None)
            .await
            .expect("connect bare code-mode");
        let runtime_bare = Arc::new(hya_engine::RuntimeRegistry::new(
            hya_tool::ToolRegistry::default(),
            Arc::new(
                hya_engine::AgentCatalog::new(Arc::new(
                    hya_bundle::BundleCatalog::from_prepared(&[]).unwrap(),
                ))
                .unwrap(),
            ),
        ));
        runtime_bare
            .refresh(|candidate| candidate.upsert_sources(vec![source_bare]))
            .unwrap();
        let binding_bare = runtime_bare.bind_turn(&root).unwrap();
        let tool_bare = binding_bare
            .resolve_tool(TOOL_NAME)
            .expect("code-mode__run")
            .tool;
        assert!(
            !tool_bare.schema().description.contains("agents.run"),
            "bare code-mode description must omit agents.run"
        );
        let nested_bare = Arc::new(MockNested::new());
        let ctx_bare = tool_ctx(Arc::clone(&nested_bare), root.clone());
        let ok_bare = tool_bare
            .execute(
                &ctx_bare,
                json!({
                    "script": "const r = await tools.echo({ value: 'bare' }); return { r, agent: typeof agent, agents: typeof agents };"
                }),
            )
            .await
            .expect("bare script executes");
        let out_bare = ok_bare.as_str().unwrap();
        assert!(out_bare.contains("\"value\":\"bare\""), "{out_bare}");
        assert!(out_bare.contains("\"agent\":\"undefined\""), "{out_bare}");
        assert!(out_bare.contains("\"agents\":\"undefined\""), "{out_bare}");

        let blocked_bare = tool_bare
            .execute(
                &ctx_bare,
                json!({
                    "script": "return await agent({ agent: 'hya-scout', prompt: 'blocked' });"
                }),
            )
            .await
            .expect_err("blocked agent() fails pre-run check");
        let blocked_out = blocked_bare.to_string();
        assert!(blocked_out.contains("hya/multi-agent"), "{blocked_out}");
        assert!(
            nested_bare.calls().iter().all(|(name, _)| name != "task"),
            "blocked agent() must not invoke task"
        );
        runtime_bare.shutdown_sources().await;

        // 2. With multi-agent (`hya/multi-agent` loaded): tools.* and agent() both execute.
        let source_full = prepare_runtime_source(&cfg, true, None)
            .await
            .expect("connect full code-mode");
        let runtime_full = Arc::new(hya_engine::RuntimeRegistry::new(
            hya_tool::ToolRegistry::default(),
            Arc::new(
                hya_engine::AgentCatalog::new(Arc::new(
                    hya_bundle::BundleCatalog::from_prepared(&[]).unwrap(),
                ))
                .unwrap(),
            ),
        ));
        runtime_full
            .refresh(|candidate| candidate.upsert_sources(vec![source_full]))
            .unwrap();
        let binding_full = runtime_full.bind_turn(&root).unwrap();
        let tool_full = binding_full
            .resolve_tool(TOOL_NAME)
            .expect("code-mode__run")
            .tool;
        assert!(
            tool_full.schema().description.contains("agents.run"),
            "full code-mode description must advertise agents.run"
        );
        let nested_full = Arc::new(MockNested::new());
        let ctx_full = tool_ctx(Arc::clone(&nested_full), root.clone());
        let ok_full = tool_full
            .execute(
                &ctx_full,
                json!({
                    "script": "const t = await tools.echo({ value: 'full' }); const a = await agent({ agent: 'hya-scout', prompt: 'go' }); return { t, status: a.status, report: a.report };"
                }),
            )
            .await
            .expect("full script executes");
        let out_full = ok_full.as_str().unwrap();
        assert!(out_full.contains("\"value\":\"full\""), "{out_full}");
        assert!(
            out_full.contains("\"report\":\"worker finished\""),
            "{out_full}"
        );
        assert!(
            nested_full.calls().iter().any(|(name, _)| name == "task"),
            "enabled agent() must invoke task"
        );
        runtime_full.shutdown_sources().await;

        let _ = std::fs::remove_dir_all(root);
    }
}
