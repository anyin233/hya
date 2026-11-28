//! Real cdylib fixture exporting a full ABI v2 bundle via `export_native_bundle_v2!`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use hya_bundle::{ApiMethod, ApiScope};
use hya_context::rewrite::{RewriteCtx, Rewriter};
use hya_proto::{ToolName, ToolSchema};
use hya_runtime_abi::{
    DriverRunOutcome, ExtensionEventContext, GoalDriver, GoalRunRequest, HookPoint, HostError,
    HostRuntime, LoopDriver, LoopRunRequest, NativeApiEndpoint, NativeBundleApiHandler,
    NativeBundleApiReply, NativeBundleApiRequest, NativeCompactionBeforeInput,
    NativeCompactionDecision, NativeHook, NativeService, NativeToolBeforeInput,
    NativeToolBeforeOutcome, ProjectionReducer, ServiceHealth, ToolVisibilityInput,
    ToolVisibilityOutcome, TurnStartRequest, UriContent, UriResolveCtx, UriSchemaHandler,
};
use hya_tool::{Tool, ToolCtx, ToolError};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

struct TestV2Tool {
    bundle_id: String,
    runtime: Arc<dyn HostRuntime>,
}

#[async_trait]
impl Tool for TestV2Tool {
    fn name(&self) -> &str {
        "v2_probe"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("v2_probe"),
            description: "Probe ABI v2 tool runtime, deadline, and HostRuntime dispatch"
                .to_string(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        tokio::time::sleep(Duration::from_millis(2)).await;
        let written = self
            .runtime
            .bundle_storage()
            .put_state(&self.bundle_id, "global", "probe", input.clone(), None)
            .await
            .map_err(ToolError::from)?;
        let read_back = self
            .runtime
            .bundle_storage()
            .get_state(&self.bundle_id, "global", "probe")
            .await
            .map_err(ToolError::from)?;
        let budget_secs = hya_tool::deadline::current().map(|d| d.budget().as_secs());
        let parent_session = ctx
            .extension::<hya_tool::ParentSession>()
            .map(|parent| parent.0.to_string());
        Ok(json!({
            "bundle_id": self.bundle_id,
            "written_version": written.version,
            "read_value": read_back.map(|e| e.value),
            "budget_secs": budget_secs,
            "parent_session": parent_session,
        }))
    }
}

struct TestV2Rewriter;

#[async_trait]
impl Rewriter for TestV2Rewriter {
    async fn before_round(&self, _cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        tokio::task::yield_now().await;
        Ok(false)
    }

    async fn rewrite_now(&self, _cx: RewriteCtx<'_>) -> Result<bool, ToolError> {
        tokio::task::yield_now().await;
        Ok(true)
    }
}

struct TestV2Service {
    started: AtomicBool,
}

#[async_trait]
impl NativeService for TestV2Service {
    fn id(&self) -> &str {
        "probe-service"
    }

    async fn start(&self, runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        tokio::time::sleep(Duration::from_millis(1)).await;
        let _ = runtime.catalog_reader().global_config().await?;
        self.started.store(true, Ordering::SeqCst);
        Ok(())
    }

    async fn stop(&self) -> Result<(), HostError> {
        tokio::task::yield_now().await;
        self.started.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn health(&self) -> ServiceHealth {
        tokio::task::yield_now().await;
        if self.started.load(Ordering::SeqCst) {
            ServiceHealth::healthy("running")
        } else {
            ServiceHealth::unhealthy("stopped")
        }
    }
}

struct TestV2Hook;

#[async_trait]
impl NativeHook for TestV2Hook {
    fn id(&self) -> &str {
        "probe-hook"
    }

    fn hook_points(&self) -> &[HookPoint] {
        &[
            HookPoint::ToolExecuteBefore,
            HookPoint::ToolVisibility,
            HookPoint::CompactionBefore,
        ]
    }

    async fn tool_execute_before(
        &self,
        input: NativeToolBeforeInput,
    ) -> Result<NativeToolBeforeOutcome, HostError> {
        tokio::task::yield_now().await;
        if input.tool == "blocked_tool" {
            Ok(NativeToolBeforeOutcome::Veto {
                reason: "vetoed by v2 hook".to_string(),
            })
        } else {
            Ok(NativeToolBeforeOutcome::Continue { input: input.input })
        }
    }

    async fn tool_visibility(
        &self,
        _input: ToolVisibilityInput,
    ) -> Result<ToolVisibilityOutcome, HostError> {
        tokio::task::yield_now().await;
        Ok(ToolVisibilityOutcome::Hide {
            tools: vec!["edit".to_string()],
        })
    }

    async fn compaction_before(
        &self,
        _input: NativeCompactionBeforeInput,
    ) -> Result<NativeCompactionDecision, HostError> {
        tokio::task::yield_now().await;
        Ok(NativeCompactionDecision::Replace {
            instructions: "keep v2 summary concise".to_string(),
        })
    }
}

struct TestV2ApiHandler {
    endpoints: Vec<NativeApiEndpoint>,
}

#[async_trait]
impl NativeBundleApiHandler for TestV2ApiHandler {
    fn id(&self) -> &str {
        "probe-api"
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        &self.endpoints
    }

    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        tokio::task::yield_now().await;
        Ok(NativeBundleApiReply {
            status: 200,
            body: json!({
                "api": request.api,
                "path": request.path,
                "echo": request.body,
            }),
        })
    }
}

struct TestV2Reducer {
    bundle_id: String,
}

impl ProjectionReducer for TestV2Reducer {
    fn bundle_id(&self) -> &str {
        &self.bundle_id
    }

    fn initial_state(&self) -> Value {
        json!({ "count": 0, "kinds": [] })
    }

    fn apply(&self, state: &mut Value, event: &ExtensionEventContext) -> Result<(), HostError> {
        let count = state
            .get("count")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .saturating_add(1);
        let mut kinds = state
            .get("kinds")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        kinds.push(Value::String(event.kind.clone()));
        *state = json!({ "count": count, "kinds": kinds });
        Ok(())
    }
}

struct TestV2UriHandler;

#[async_trait]
impl UriSchemaHandler for TestV2UriHandler {
    fn scheme(&self) -> &str {
        "channel"
    }

    fn writable(&self) -> bool {
        true
    }

    async fn read(&self, ctx: UriResolveCtx<'_>, uri: &str) -> Result<UriContent, HostError> {
        tokio::task::yield_now().await;
        let has_runtime = ctx.runtime.is_some();
        Ok(UriContent {
            body: format!("resolved:{uri}:runtime={has_runtime}"),
            media_type: "text/plain".to_string(),
            metadata: Some(json!({ "workdir": ctx.workdir.display().to_string() })),
        })
    }

    async fn write(
        &self,
        _ctx: UriResolveCtx<'_>,
        uri: &str,
        content: &str,
    ) -> Result<Value, HostError> {
        tokio::task::yield_now().await;
        Ok(json!({ "uri": uri, "bytes": content.len() }))
    }
}

struct TestV2GoalDriver;

#[async_trait]
impl GoalDriver for TestV2GoalDriver {
    fn id(&self) -> &str {
        "test-goal"
    }

    async fn run_goal(
        &self,
        runtime: Arc<dyn HostRuntime>,
        request: GoalRunRequest,
        cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError> {
        tokio::time::sleep(Duration::from_millis(1)).await;
        let turn = runtime
            .turn_runner()
            .run_turn(
                TurnStartRequest {
                    session: request.session,
                    prompt: request.goal,
                    agent: request.agent,
                    model: request.model,
                    system_override: None,
                    metadata: None,
                    actor_claim: None,
                },
                cancel,
            )
            .await?;
        Ok(DriverRunOutcome::Achieved {
            iterations: 1,
            reason: turn.output_text,
        })
    }
}

struct TestV2LoopDriver;

#[async_trait]
impl LoopDriver for TestV2LoopDriver {
    fn id(&self) -> &str {
        "test-loop"
    }

    async fn run_loop(
        &self,
        _runtime: Arc<dyn HostRuntime>,
        request: LoopRunRequest,
        _cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError> {
        tokio::task::yield_now().await;
        Ok(DriverRunOutcome::Achieved {
            iterations: 2,
            reason: format!("verified: {}", request.target),
        })
    }
}

hya_runtime_abi::export_native_bundle_v2!(|ctx, out| {
    if ctx.bundle_id() == "hya/fail-register" {
        return Err(HostError::InvalidInput(
            "intentional registration failure".to_string(),
        ));
    }
    let bundle_id = ctx.bundle_id().to_string();
    let runtime = Arc::clone(ctx.runtime());
    out.tools.push(Arc::new(TestV2Tool {
        bundle_id: bundle_id.clone(),
        runtime,
    }));
    out.rewriters.push(Arc::new(TestV2Rewriter));
    out.services.push(Arc::new(TestV2Service {
        started: AtomicBool::new(false),
    }));
    out.hooks.push(Arc::new(TestV2Hook));
    out.api_handlers.push(Arc::new(TestV2ApiHandler {
        endpoints: vec![NativeApiEndpoint {
            id: "ping".to_string(),
            method: ApiMethod::Post,
            scope: ApiScope::Global,
            path: "/ping".to_string(),
            description: "Echo probe".to_string(),
        }],
    }));
    out.projection_reducers
        .push(Arc::new(TestV2Reducer { bundle_id }));
    out.uri_schema_handlers.push(Arc::new(TestV2UriHandler));
    out.goal_drivers.push(Arc::new(TestV2GoalDriver));
    out.loop_drivers.push(Arc::new(TestV2LoopDriver));
    let _unused: BTreeMap<String, String> = BTreeMap::new();
    Ok::<(), HostError>(())
});
