//! Lockstep Native ABI v2 dynamic-library loader and async runtime bridge.
//!
//! ABI v2 unifies every native bundle extension behind four C symbols
//! (`hya_native_abi_v2`, `hya_native_with_runtime_v2`,
//! `hya_native_share_deadline_v2`, `hya_native_register_v2`) and injects an
//! `Arc<dyn HostRuntime>` into [`NativePluginContext`]. All async methods on
//! registered traits are wrapped so every future poll enters the bundle's
//! Tokio runtime via `hya_native_with_runtime_v2`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll};

use async_trait::async_trait;
use hya_bundle::{FirstPartySource, first_party_bundle, first_party_source, load_first_party};
use hya_context::rewrite::Rewriter;
use hya_proto::{Envelope, ModelRef, SessionId};
use hya_runtime_abi::{
    AbiDigestFn, AbiError, DriverRunOutcome, GoalDriver, GoalRunRequest, HookPoint, HostError,
    HostRuntime, LoopDriver, LoopRunRequest, NativeAgentSpawnInput, NativeApiEndpoint,
    NativeBundleApiHandler, NativeBundleApiReply, NativeBundleApiRequest, NativeChatParamsInput,
    NativeCommandBeforeInput, NativeCompactionAfterInput, NativeCompactionBeforeInput,
    NativeCompactionDecision, NativeGoalEvaluateVerdict, NativeHook, NativeLoopPlanInput,
    NativeMessageUserBeforeInput, NativeModelFallbackInput, NativePermissionApproveInput,
    NativePlannerOutput, NativePluginContext, NativeRegistrations, NativeService,
    NativeSessionLifecycleInput, NativeTextCompleteInput, NativeToolAfterInput,
    NativeToolAfterOutcome, NativeToolBeforeInput, NativeToolBeforeOutcome, NativeVerifierVerdict,
    RegisterV2Fn, SYMBOL_ABI_V2, SYMBOL_REGISTER_V2, SYMBOL_SHARE_DEADLINE_V2,
    SYMBOL_WITH_RUNTIME_V2, ServiceHealth, ShareDeadlineFn, ToolVisibilityInput,
    ToolVisibilityOutcome, UriContent, UriResolveCtx, UriSchemaHandler, abi_digest_v2,
    deadline_cell_v2,
};
use hya_tool::{Action, Decision, Resource, Tool};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::native_bundle::{
    LibrarySource, NativeRewriter, NativeTool, RuntimeEntry, cache_library,
    library_source_for_package, native_cache_root, record_library_source, symbol,
};

struct GenericPollState<R> {
    future: *mut Pin<Box<dyn Future<Output = R> + Send + 'static>>,
    context: *mut Context<'static>,
    result: Option<Poll<R>>,
}

unsafe extern "C" fn poll_generic_future<R>(state: *mut libc::c_void) {
    // SAFETY: `with_runtime` invokes this callback synchronously while the
    // caller's future, context, and state are live. Their erased lifetimes are
    // never retained beyond this call.
    let state = unsafe { &mut *state.cast::<GenericPollState<R>>() };
    // SAFETY: the synchronous callback owns exclusive access for this poll.
    let future = unsafe { &mut *state.future };
    let context = unsafe { &mut *state.context };
    state.result = Some(future.as_mut().poll(context));
}

async fn poll_future_with_runtime<R>(
    mut future: Pin<Box<dyn Future<Output = R> + Send + '_>>,
    with_runtime: RuntimeEntry,
    handle: &tokio::runtime::Handle,
    bridge_error: impl Fn() -> R,
) -> R {
    std::future::poll_fn(|context| {
        let mut state = GenericPollState {
            future: (&mut future as *mut Pin<Box<dyn Future<Output = R> + Send + '_>>)
                .cast::<Pin<Box<dyn Future<Output = R> + Send + 'static>>>(),
            context: (context as *mut Context<'_>).cast::<Context<'static>>(),
            result: None,
        };
        // SAFETY: `with_runtime` belongs to the checked lockstep library and
        // polls synchronously before these locals drop.
        unsafe {
            (with_runtime)(
                (handle as *const tokio::runtime::Handle).cast(),
                poll_generic_future::<R>,
                (&mut state as *mut GenericPollState<R>).cast(),
            );
        }
        state.result.unwrap_or_else(|| Poll::Ready(bridge_error()))
    })
    .await
}

async fn poll_host_result<T>(
    future: Pin<Box<dyn Future<Output = Result<T, HostError>> + Send + '_>>,
    with_runtime: RuntimeEntry,
) -> Result<T, HostError> {
    let handle = tokio::runtime::Handle::try_current().map_err(|error| {
        HostError::Unavailable(format!(
            "native bundle extension needs a Tokio runtime: {error}"
        ))
    })?;
    poll_future_with_runtime(future, with_runtime, &handle, || {
        Err(HostError::Internal(
            "native runtime bridge did not poll the future".to_string(),
        ))
    })
    .await
}

// ---------------------------------------------------------------------------
// Async extension adapters wrapping every future poll with `with_runtime`
// ---------------------------------------------------------------------------

struct NativeServiceAdapter {
    inner: Arc<dyn NativeService>,
    with_runtime: RuntimeEntry,
}

#[async_trait]
impl NativeService for NativeServiceAdapter {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn start(&self, runtime: Arc<dyn HostRuntime>) -> Result<(), HostError> {
        poll_host_result(self.inner.start(runtime), self.with_runtime).await
    }

    async fn stop(&self) -> Result<(), HostError> {
        poll_host_result(self.inner.stop(), self.with_runtime).await
    }

    async fn health(&self) -> ServiceHealth {
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return ServiceHealth::unhealthy("no active Tokio runtime for service health check");
        };
        poll_future_with_runtime(self.inner.health(), self.with_runtime, &handle, || {
            ServiceHealth::unhealthy("native runtime bridge did not poll service health")
        })
        .await
    }

    fn is_session_busy(&self, session: SessionId) -> bool {
        self.inner.is_session_busy(session)
    }

    fn cancel_session(&self, session: SessionId) -> bool {
        self.inner.cancel_session(session)
    }

    async fn wait_session_idle(&self, session: SessionId) -> Result<(), HostError> {
        poll_host_result(self.inner.wait_session_idle(session), self.with_runtime).await
    }
}

struct NativeHookAdapter {
    inner: Arc<dyn NativeHook>,
    with_runtime: RuntimeEntry,
}

#[async_trait]
impl NativeHook for NativeHookAdapter {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn hook_points(&self) -> &[HookPoint] {
        self.inner.hook_points()
    }

    async fn tool_execute_before(
        &self,
        input: NativeToolBeforeInput,
    ) -> Result<NativeToolBeforeOutcome, HostError> {
        poll_host_result(self.inner.tool_execute_before(input), self.with_runtime).await
    }

    async fn tool_execute_after(
        &self,
        input: NativeToolAfterInput,
    ) -> Result<NativeToolAfterOutcome, HostError> {
        poll_host_result(self.inner.tool_execute_after(input), self.with_runtime).await
    }

    async fn tool_visibility(
        &self,
        input: ToolVisibilityInput,
    ) -> Result<ToolVisibilityOutcome, HostError> {
        poll_host_result(self.inner.tool_visibility(input), self.with_runtime).await
    }

    async fn compaction_before(
        &self,
        input: NativeCompactionBeforeInput,
    ) -> Result<NativeCompactionDecision, HostError> {
        poll_host_result(self.inner.compaction_before(input), self.with_runtime).await
    }

    async fn compaction_after(&self, input: NativeCompactionAfterInput) -> Result<(), HostError> {
        poll_host_result(self.inner.compaction_after(input), self.with_runtime).await
    }

    async fn session_start(&self, input: NativeSessionLifecycleInput) -> Result<(), HostError> {
        poll_host_result(self.inner.session_start(input), self.with_runtime).await
    }

    async fn session_end(&self, input: NativeSessionLifecycleInput) -> Result<(), HostError> {
        poll_host_result(self.inner.session_end(input), self.with_runtime).await
    }

    async fn agent_spawn(&self, input: NativeAgentSpawnInput) -> Result<(), HostError> {
        poll_host_result(self.inner.agent_spawn(input), self.with_runtime).await
    }

    async fn command_execute_before(
        &self,
        input: NativeCommandBeforeInput,
    ) -> Result<String, HostError> {
        poll_host_result(self.inner.command_execute_before(input), self.with_runtime).await
    }

    async fn message_user_before(
        &self,
        input: NativeMessageUserBeforeInput,
    ) -> Result<String, HostError> {
        poll_host_result(self.inner.message_user_before(input), self.with_runtime).await
    }

    async fn text_complete(&self, input: NativeTextCompleteInput) -> Result<String, HostError> {
        poll_host_result(self.inner.text_complete(input), self.with_runtime).await
    }

    async fn chat_params(&self, input: NativeChatParamsInput) -> Result<Value, HostError> {
        poll_host_result(self.inner.chat_params(input), self.with_runtime).await
    }

    async fn model_fallback(
        &self,
        input: NativeModelFallbackInput,
    ) -> Result<Option<ModelRef>, HostError> {
        poll_host_result(self.inner.model_fallback(input), self.with_runtime).await
    }

    async fn permission_ask(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Result<Option<Decision>, HostError> {
        poll_host_result(
            self.inner.permission_ask(session, action, resource),
            self.with_runtime,
        )
        .await
    }

    async fn permission_approve(
        &self,
        input: NativePermissionApproveInput,
    ) -> Result<Option<Decision>, HostError> {
        poll_host_result(self.inner.permission_approve(input), self.with_runtime).await
    }

    async fn goal_evaluate(
        &self,
        condition: &str,
        transcript: &str,
    ) -> Result<Option<NativeGoalEvaluateVerdict>, HostError> {
        poll_host_result(
            self.inner.goal_evaluate(condition, transcript),
            self.with_runtime,
        )
        .await
    }

    async fn loop_verify(
        &self,
        target: &str,
        transcript: &str,
    ) -> Result<Option<NativeVerifierVerdict>, HostError> {
        poll_host_result(
            self.inner.loop_verify(target, transcript),
            self.with_runtime,
        )
        .await
    }

    async fn loop_plan(
        &self,
        input: NativeLoopPlanInput,
    ) -> Result<Option<NativePlannerOutput>, HostError> {
        poll_host_result(self.inner.loop_plan(input), self.with_runtime).await
    }

    async fn loop_should_stop(
        &self,
        target: &str,
        transcript: &str,
    ) -> Result<Option<String>, HostError> {
        poll_host_result(
            self.inner.loop_should_stop(target, transcript),
            self.with_runtime,
        )
        .await
    }

    async fn on_event(&self, envelope: &Envelope) -> Result<(), HostError> {
        poll_host_result(self.inner.on_event(envelope), self.with_runtime).await
    }
}

struct NativeBundleApiHandlerAdapter {
    inner: Arc<dyn NativeBundleApiHandler>,
    with_runtime: RuntimeEntry,
}

#[async_trait]
impl NativeBundleApiHandler for NativeBundleApiHandlerAdapter {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn endpoints(&self) -> &[NativeApiEndpoint] {
        self.inner.endpoints()
    }

    async fn handle(
        &self,
        request: NativeBundleApiRequest,
    ) -> Result<NativeBundleApiReply, HostError> {
        poll_host_result(self.inner.handle(request), self.with_runtime).await
    }
}

struct UriSchemaHandlerAdapter {
    inner: Arc<dyn UriSchemaHandler>,
    with_runtime: RuntimeEntry,
}

#[async_trait]
impl UriSchemaHandler for UriSchemaHandlerAdapter {
    fn scheme(&self) -> &str {
        self.inner.scheme()
    }

    fn writable(&self) -> bool {
        self.inner.writable()
    }

    async fn read(&self, ctx: UriResolveCtx<'_>, uri: &str) -> Result<UriContent, HostError> {
        poll_host_result(self.inner.read(ctx, uri), self.with_runtime).await
    }

    async fn write(
        &self,
        ctx: UriResolveCtx<'_>,
        uri: &str,
        content: &str,
    ) -> Result<Value, HostError> {
        poll_host_result(self.inner.write(ctx, uri, content), self.with_runtime).await
    }
}

struct GoalDriverAdapter {
    inner: Arc<dyn GoalDriver>,
    with_runtime: RuntimeEntry,
}

#[async_trait]
impl GoalDriver for GoalDriverAdapter {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn run_goal(
        &self,
        runtime: Arc<dyn HostRuntime>,
        request: GoalRunRequest,
        cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError> {
        poll_host_result(
            self.inner.run_goal(runtime, request, cancel),
            self.with_runtime,
        )
        .await
    }
}

struct LoopDriverAdapter {
    inner: Arc<dyn LoopDriver>,
    with_runtime: RuntimeEntry,
}

#[async_trait]
impl LoopDriver for LoopDriverAdapter {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn run_loop(
        &self,
        runtime: Arc<dyn HostRuntime>,
        request: LoopRunRequest,
        cancel: CancellationToken,
    ) -> Result<DriverRunOutcome, HostError> {
        poll_host_result(
            self.inner.run_loop(runtime, request, cancel),
            self.with_runtime,
        )
        .await
    }
}

fn wrap_registrations_with_runtime(
    regs: NativeRegistrations,
    with_runtime: RuntimeEntry,
) -> NativeRegistrations {
    NativeRegistrations {
        tools: regs
            .tools
            .into_iter()
            .map(|inner| {
                Arc::new(NativeTool {
                    inner,
                    with_runtime,
                }) as Arc<dyn Tool>
            })
            .collect(),
        rewriters: regs
            .rewriters
            .into_iter()
            .map(|inner| {
                Arc::new(NativeRewriter {
                    inner,
                    with_runtime,
                }) as Arc<dyn Rewriter>
            })
            .collect(),
        services: regs
            .services
            .into_iter()
            .map(|inner| {
                Arc::new(NativeServiceAdapter {
                    inner,
                    with_runtime,
                }) as Arc<dyn NativeService>
            })
            .collect(),
        hooks: regs
            .hooks
            .into_iter()
            .map(|inner| {
                Arc::new(NativeHookAdapter {
                    inner,
                    with_runtime,
                }) as Arc<dyn NativeHook>
            })
            .collect(),
        api_handlers: regs
            .api_handlers
            .into_iter()
            .map(|inner| {
                Arc::new(NativeBundleApiHandlerAdapter {
                    inner,
                    with_runtime,
                }) as Arc<dyn NativeBundleApiHandler>
            })
            .collect(),
        projection_reducers: regs.projection_reducers,
        uri_schema_handlers: regs
            .uri_schema_handlers
            .into_iter()
            .map(|inner| {
                Arc::new(UriSchemaHandlerAdapter {
                    inner,
                    with_runtime,
                }) as Arc<dyn UriSchemaHandler>
            })
            .collect(),
        goal_drivers: regs
            .goal_drivers
            .into_iter()
            .map(|inner| {
                Arc::new(GoalDriverAdapter {
                    inner,
                    with_runtime,
                }) as Arc<dyn GoalDriver>
            })
            .collect(),
        loop_drivers: regs
            .loop_drivers
            .into_iter()
            .map(|inner| {
                Arc::new(LoopDriverAdapter {
                    inner,
                    with_runtime,
                }) as Arc<dyn LoopDriver>
            })
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// Dynamic library open, ABI v2 digest check, and selective loader
// ---------------------------------------------------------------------------

static LIBRARIES_V2: OnceLock<Mutex<HashMap<String, usize>>> = OnceLock::new();
static DEADLINE_SHARED_V2: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();

#[derive(Clone, Copy)]
struct CheckedLibraryV2 {
    handle: usize,
    with_runtime: RuntimeEntry,
    share_deadline: ShareDeadlineFn,
    register: RegisterV2Fn,
}

/// Outcome of loading a selective list of native ABI v2 bundles.
#[derive(Default)]
pub struct LoadedNativeBundles {
    /// Combined registrations across every loaded bundle in order.
    pub registrations: NativeRegistrations,
    /// Identities of bundles that were successfully loaded.
    pub loaded_identities: Vec<String>,
    /// Identities of requested bundles skipped because they were disabled.
    pub skipped_disabled: Vec<String>,
}

/// Non-panicking selective loader for ABI v2 native bundles.
#[derive(Clone)]
pub struct NativeBundleLoader {
    runtime: Arc<dyn HostRuntime>,
    disabled: BTreeSet<String>,
    search_dirs: Vec<PathBuf>,
}

impl NativeBundleLoader {
    /// Create a loader wired to `runtime`.
    #[must_use]
    pub fn new(runtime: Arc<dyn HostRuntime>) -> Self {
        Self {
            runtime,
            disabled: BTreeSet::new(),
            search_dirs: Vec::new(),
        }
    }

    /// Mark the specified bundle identities as disabled (e.g. `hya/hashline`).
    #[must_use]
    pub fn with_disabled<I, S>(mut self, disabled: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.disabled = disabled.into_iter().map(Into::into).collect();
        self
    }

    /// Add explicit directories searched before default Cargo/installed paths.
    #[must_use]
    pub fn with_search_dirs(mut self, dirs: Vec<PathBuf>) -> Self {
        self.search_dirs = dirs;
        self
    }

    /// Return `true` when `identity` is marked disabled on this loader.
    #[must_use]
    pub fn is_disabled(&self, identity: &str) -> bool {
        self.disabled.contains(identity)
    }

    /// Load a single native ABI v2 bundle by identity (e.g. `hya/goal`).
    ///
    /// # Errors
    /// Returns [`AbiError::DisabledOrMissing`] if disabled or not found,
    /// [`AbiError::DigestMismatch`] on ABI incompatibility, or symbol/load
    /// diagnostics without panicking.
    pub fn load_bundle(&self, identity: &str) -> Result<NativeRegistrations, AbiError> {
        if self.is_disabled(identity) {
            return Err(AbiError::DisabledOrMissing {
                bundle: identity.to_string(),
                detail: "bundle is disabled by configuration".to_string(),
            });
        }
        load_v2_inner(identity, &self.search_dirs, Arc::clone(&self.runtime))
    }

    /// Load a native ABI v2 bundle directly from an explicit shared library path.
    ///
    /// # Errors
    /// Returns [`AbiError`] if disabled, missing, ABI-incompatible, or if
    /// registration fails.
    pub fn load_from_path(
        &self,
        identity: &str,
        library_path: &Path,
    ) -> Result<NativeRegistrations, AbiError> {
        if self.is_disabled(identity) {
            return Err(AbiError::DisabledOrMissing {
                bundle: identity.to_string(),
                detail: "bundle is disabled by configuration".to_string(),
            });
        }
        load_v2_from_path(identity, library_path, Arc::clone(&self.runtime))
    }

    /// Selectively load `identities`, skipping disabled bundles cleanly and
    /// returning a typed [`AbiError`] if any enabled bundle cannot be loaded.
    ///
    /// # Errors
    /// Returns [`AbiError`] for the first enabled bundle that is missing,
    /// fails ABI verification, or fails registration.
    pub fn load_selected(&self, identities: &[&str]) -> Result<LoadedNativeBundles, AbiError> {
        let mut loaded = LoadedNativeBundles::default();
        for &identity in identities {
            if self.is_disabled(identity) {
                loaded.skipped_disabled.push(identity.to_string());
                continue;
            }
            let regs = self.load_bundle(identity)?;
            loaded.registrations.extend(regs);
            loaded.loaded_identities.push(identity.to_string());
        }
        Ok(loaded)
    }
}

/// Load one native ABI v2 bundle by identity using `runtime`.
///
/// # Errors
/// Returns [`AbiError`] when the library is missing, fails `hya_native_abi_v2`
/// digest verification, lacks required ABI v2 symbols, or fails registration.
pub fn load_v2(
    identity: &str,
    runtime: Arc<dyn HostRuntime>,
) -> Result<NativeRegistrations, AbiError> {
    load_v2_inner(identity, &[], runtime)
}

/// Load one native ABI v2 bundle from an explicit shared library `path`.
///
/// # Errors
/// Returns [`AbiError`] when the path is missing, fails `hya_native_abi_v2`
/// digest verification, lacks required ABI v2 symbols, or fails registration.
pub fn load_v2_from_path(
    identity: &str,
    path: &Path,
    runtime: Arc<dyn HostRuntime>,
) -> Result<NativeRegistrations, AbiError> {
    if identity.trim().is_empty() {
        return Err(AbiError::InvalidBundleIdentity(identity.to_string()));
    }
    if !path.is_file() {
        return Err(AbiError::DisabledOrMissing {
            bundle: identity.to_string(),
            detail: format!("library does not exist at {}", path.display()),
        });
    }
    let cache_key = path
        .canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned();
    let checked = open_or_cached_v2(identity, &cache_key, path)?;
    invoke_register_v2(identity, checked, runtime)
}

fn bundle_stem(identity: &str) -> Result<String, AbiError> {
    let name = identity
        .strip_prefix("hya/")
        .filter(|rest| !rest.is_empty())
        .ok_or_else(|| AbiError::InvalidBundleIdentity(identity.to_string()))?;
    if name == "workflow" {
        return Ok("hya_workflow_bundle".to_string());
    }
    Ok(format!("hya_{}", name.replace('-', "_")))
}

fn bundle_package_stem(identity: &str) -> Result<String, AbiError> {
    let name = identity
        .strip_prefix("hya/")
        .filter(|rest| !rest.is_empty())
        .ok_or_else(|| AbiError::InvalidBundleIdentity(identity.to_string()))?;
    Ok(format!("hya_{}", name.replace('-', "_")))
}

fn load_v2_inner(
    identity: &str,
    search_dirs: &[PathBuf],
    runtime: Arc<dyn HostRuntime>,
) -> Result<NativeRegistrations, AbiError> {
    let stem = bundle_stem(identity)?;
    let filename = format!(
        "{}{}{}",
        std::env::consts::DLL_PREFIX,
        stem,
        std::env::consts::DLL_SUFFIX
    );

    for dir in search_dirs {
        let candidate = dir.join(&filename);
        if candidate.is_file() {
            let checked = open_or_cached_v2(identity, &stem, &candidate)?;
            record_library_source(&stem, &candidate);
            return invoke_register_v2(identity, checked, runtime);
        }
    }

    if let Some(&cached_handle) = LIBRARIES_V2
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&stem)
    {
        let checked = resolve_v2_symbols(identity, cached_handle)?;
        return invoke_register_v2(identity, checked, runtime);
    }

    let checked = open_discovered_v2(identity, &stem, &filename)?;
    invoke_register_v2(identity, checked, runtime)
}

fn open_or_cached_v2(
    identity: &str,
    cache_key: &str,
    path: &Path,
) -> Result<CheckedLibraryV2, AbiError> {
    let libraries = LIBRARIES_V2.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(&handle) = libraries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(cache_key)
    {
        return resolve_v2_symbols(identity, handle);
    }
    let checked = load_checked_v2(identity, path)?;
    ensure_deadline_shared_v2(checked.handle, checked.share_deadline);
    libraries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(cache_key.to_string(), checked.handle);
    Ok(checked)
}

fn ensure_deadline_shared_v2(handle: usize, share_deadline: ShareDeadlineFn) {
    let shared = DEADLINE_SHARED_V2.get_or_init(|| Mutex::new(HashSet::new()));
    let mut set = shared
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if set.insert(handle) {
        // SAFETY: `share_deadline` was verified by `abi_digest_v2()` and
        // `deadline_cell_v2()` points to the host's process-lifetime cell.
        unsafe { share_deadline(deadline_cell_v2()) };
    }
}

#[cfg(unix)]
fn open_discovered_v2(
    identity: &str,
    stem: &str,
    filename: &str,
) -> Result<CheckedLibraryV2, AbiError> {
    let executable = std::env::current_exe().map_err(|error| AbiError::LoadFailed {
        bundle: identity.to_string(),
        detail: error.to_string(),
    })?;
    let parent = executable.parent().ok_or_else(|| AbiError::LoadFailed {
        bundle: identity.to_string(),
        detail: "backend executable has no parent directory".to_string(),
    })?;
    let package_stem = bundle_package_stem(identity)?;
    match library_source_for_package(parent, filename, stem, &package_stem) {
        Some(LibrarySource::Local(libraries)) => {
            let mut failures = Vec::new();
            for library in &libraries {
                match load_checked_v2(identity, library) {
                    Ok(checked) => {
                        ensure_deadline_shared_v2(checked.handle, checked.share_deadline);
                        LIBRARIES_V2
                            .get_or_init(|| Mutex::new(HashMap::new()))
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(stem.to_string(), checked.handle);
                        record_library_source(stem, library);
                        return Ok(checked);
                    }
                    Err(error) => failures.push(error),
                }
            }
            if failures.len() == 1
                && let Some(first) = failures.pop()
            {
                return Err(first);
            }
            Err(AbiError::LoadFailed {
                bundle: identity.to_string(),
                detail: failures
                    .into_iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("; "),
            })
        }
        Some(LibrarySource::Package(package)) => {
            record_library_source(stem, &package);
            let extracted = extract_packaged_library_v2(parent, &package, identity, filename)?;
            let checked = load_checked_v2(identity, &extracted);
            cleanup_extracted_temp(&extracted);
            let checked = checked?;
            ensure_deadline_shared_v2(checked.handle, checked.share_deadline);
            LIBRARIES_V2
                .get_or_init(|| Mutex::new(HashMap::new()))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(stem.to_string(), checked.handle);
            Ok(checked)
        }
        None => Err(AbiError::DisabledOrMissing {
            bundle: identity.to_string(),
            detail: format!("native bundle `{stem}` is missing near {executable:?}"),
        }),
    }
}

#[cfg(not(unix))]
fn open_discovered_v2(
    identity: &str,
    _stem: &str,
    _filename: &str,
) -> Result<CheckedLibraryV2, AbiError> {
    Err(AbiError::LoadFailed {
        bundle: identity.to_string(),
        detail: "native bundle dynamic loading requires a Unix dynamic loader".to_string(),
    })
}

#[cfg(unix)]
fn cleanup_extracted_temp(extracted: &Path) {
    if let Some(directory) = extracted.parent().filter(|directory| {
        directory
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| name.starts_with("hya-native-v2-"))
    }) {
        let _ = std::fs::remove_file(extracted);
        let _ = std::fs::remove_dir(directory);
    }
}

#[cfg(unix)]
fn extract_packaged_library_v2(
    executable_dir: &Path,
    package: &Path,
    identity: &str,
    filename: &str,
) -> Result<PathBuf, AbiError> {
    use std::os::unix::fs::DirBuilderExt as _;

    let map_bundle_err = |detail: String| AbiError::LoadFailed {
        bundle: identity.to_string(),
        detail,
    };

    let staged;
    let catalog = match first_party_source(executable_dir, identity) {
        Some(FirstPartySource::Package(resolved)) if resolved == package => {
            first_party_bundle(identity).map_err(|error| map_bundle_err(error.to_string()))?
        }
        _ => {
            staged = load_first_party(&FirstPartySource::Package(package.to_path_buf()), identity)
                .map_err(|error| map_bundle_err(error.to_string()))?;
            &staged
        }
    };
    let [bundle] = catalog.bundles() else {
        return Err(map_bundle_err(format!(
            "{} must contain exactly one bundle",
            package.display()
        )));
    };
    let libraries = bundle
        .extensions()
        .iter()
        .filter(|resource| resource.binary_base64.is_some())
        .collect::<Vec<_>>();
    let [library] = libraries.as_slice() else {
        return Err(map_bundle_err(format!(
            "{} must contain exactly one native library",
            package.display()
        )));
    };
    let payload = library
        .source_bytes()
        .map_err(|error| map_bundle_err(error.to_string()))?;

    if let Some(root) = native_cache_root()
        && let Ok(path) = cache_library(&root, &payload, filename)
    {
        return Ok(path);
    }

    let digest = Sha256::digest(&payload);
    let prefix = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let directory =
        std::env::temp_dir().join(format!("hya-native-v2-{}-{prefix}", std::process::id()));
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&directory)
        .map_err(|error| map_bundle_err(format!("create private native directory: {error}")))?;
    let path = directory.join(filename);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(&payload)
                .map_err(|error| map_bundle_err(error.to_string()))?;
            file.sync_all()
                .map_err(|error| map_bundle_err(error.to_string()))?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::read(&path).map_err(|err| map_bundle_err(err.to_string()))? != payload {
                return Err(map_bundle_err(format!(
                    "native library cache collision at {}",
                    path.display()
                )));
            }
        }
        Err(error) => return Err(map_bundle_err(error.to_string())),
    }
    Ok(path)
}

#[cfg(unix)]
fn load_checked_v2(identity: &str, library: &Path) -> Result<CheckedLibraryV2, AbiError> {
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt as _;

    let path = CString::new(library.as_os_str().as_bytes()).map_err(|_| AbiError::LoadFailed {
        bundle: identity.to_string(),
        detail: "native bundle path contains a NUL byte".to_string(),
    })?;
    // SAFETY: `path` is NUL terminated; `RTLD_NOW` resolves all symbols before
    // any bundle code runs.
    let raw_handle = unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if raw_handle.is_null() {
        // SAFETY: `dlerror` returns a process-owned NUL-terminated diagnostic.
        let detail = unsafe { libc::dlerror() };
        let detail = if detail.is_null() {
            "unknown loader failure".to_string()
        } else {
            // SAFETY: non-null `dlerror` points to its NUL-terminated message.
            unsafe { CStr::from_ptr(detail) }
                .to_string_lossy()
                .into_owned()
        };
        return Err(AbiError::LoadFailed {
            bundle: identity.to_string(),
            detail,
        });
    }
    let handle = raw_handle as usize;

    let verify_result = (|| -> Result<CheckedLibraryV2, AbiError> {
        let abi_ptr = lookup_v2_symbol(identity, handle, SYMBOL_ABI_V2)?;
        // SAFETY: `hya_native_abi_v2` has C ABI `fn(*mut u8)` writing 32 bytes.
        let abi_fn: AbiDigestFn = unsafe { std::mem::transmute(abi_ptr) };
        let mut actual = [0_u8; 32];
        // SAFETY: `actual` is a writable 32-byte array.
        unsafe { abi_fn(actual.as_mut_ptr()) };
        if actual != abi_digest_v2() {
            return Err(AbiError::DigestMismatch {
                bundle: identity.to_string(),
            });
        }
        resolve_v2_symbols(identity, handle)
    })();

    if verify_result.is_err() {
        let already_cached = LIBRARIES_V2
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(|&cached| cached == handle);
        if !already_cached {
            // SAFETY: `raw_handle` was opened in this call and is not retained.
            unsafe { libc::dlclose(raw_handle) };
        }
    }

    verify_result
}

#[cfg(not(unix))]
fn load_checked_v2(identity: &str, _library: &Path) -> Result<CheckedLibraryV2, AbiError> {
    Err(AbiError::LoadFailed {
        bundle: identity.to_string(),
        detail: "native bundle dynamic loading requires Unix".to_string(),
    })
}

fn lookup_v2_symbol(identity: &str, handle: usize, name: &[u8]) -> Result<usize, AbiError> {
    symbol(handle, name).map_err(|_| AbiError::MissingSymbol {
        bundle: identity.to_string(),
        symbol: String::from_utf8_lossy(name)
            .trim_end_matches('\0')
            .to_string(),
    })
}

fn resolve_v2_symbols(identity: &str, handle: usize) -> Result<CheckedLibraryV2, AbiError> {
    let with_runtime_ptr = lookup_v2_symbol(identity, handle, SYMBOL_WITH_RUNTIME_V2)?;
    let share_deadline_ptr = lookup_v2_symbol(identity, handle, SYMBOL_SHARE_DEADLINE_V2)?;
    let register_ptr = lookup_v2_symbol(identity, handle, SYMBOL_REGISTER_V2)?;
    // SAFETY: every symbol was exported by a library whose `hya_native_abi_v2`
    // digest matched `abi_digest_v2()` for this build.
    let with_runtime: RuntimeEntry = unsafe { std::mem::transmute(with_runtime_ptr) };
    // SAFETY: matched lockstep ABI v2 symbol signature.
    let share_deadline: ShareDeadlineFn = unsafe { std::mem::transmute(share_deadline_ptr) };
    // SAFETY: matched lockstep ABI v2 symbol signature.
    let register: RegisterV2Fn = unsafe { std::mem::transmute(register_ptr) };
    Ok(CheckedLibraryV2 {
        handle,
        with_runtime,
        share_deadline,
        register,
    })
}

fn invoke_register_v2(
    identity: &str,
    checked: CheckedLibraryV2,
    runtime: Arc<dyn HostRuntime>,
) -> Result<NativeRegistrations, AbiError> {
    let ctx = NativePluginContext::new(identity, runtime);
    let mut regs = NativeRegistrations::new();
    let mut error_out: Option<String> = None;
    // SAFETY: `checked.register` was resolved from a library whose ABI v2 digest
    // matched this host build, and `ctx`, `regs`, and `error_out` are live.
    let code = unsafe { (checked.register)(&ctx, &mut regs, &mut error_out) };
    if code != 0 {
        return Err(AbiError::RegistrationFailed {
            bundle: identity.to_string(),
            detail: error_out.unwrap_or_else(|| format!("register exited with status {code}")),
        });
    }
    if let Some(reserved) = regs
        .tools
        .iter()
        .find(|tool| hya_tool::is_reserved_kernel_tool(tool.name()))
    {
        return Err(AbiError::RegistrationFailed {
            bundle: identity.to_string(),
            detail: format!(
                "tool `{}` is permanently kernel-exclusive and cannot be registered by a bundle",
                reserved.name()
            ),
        });
    }
    Ok(wrap_registrations_with_runtime(regs, checked.with_runtime))
}

// ---------------------------------------------------------------------------
// MinimalHostRuntime — reusable lightweight HostRuntime implementation
// ---------------------------------------------------------------------------

type BundleConfigCallback = Arc<dyn Fn(&str) -> Value + Send + Sync>;

/// Lightweight [`HostRuntime`] implementation for callers that only need
/// per-bundle configuration lookup (or no-op defaults for standalone tool
/// family / test loading).
pub struct MinimalHostRuntime {
    bundle_config: BundleConfigCallback,
}

impl MinimalHostRuntime {
    /// Create a no-op runtime whose `bundle_config` always returns `Value::Null`.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            bundle_config: Arc::new(|_| Value::Null),
        }
    }

    /// Create a runtime backed by a custom per-bundle config reader callback.
    #[must_use]
    pub fn with_bundle_config<F>(reader: F) -> Self
    where
        F: Fn(&str) -> Value + Send + Sync + 'static,
    {
        Self {
            bundle_config: Arc::new(reader),
        }
    }
}

#[async_trait]
impl hya_runtime_abi::HostSessionStore for MinimalHostRuntime {
    async fn session_exists(&self, _session: SessionId) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn read_events(
        &self,
        _session: SessionId,
        _after_seq: Option<hya_proto::EventSeq>,
        _limit: Option<usize>,
    ) -> Result<Vec<Envelope>, HostError> {
        Ok(Vec::new())
    }

    async fn append_events(
        &self,
        _session: SessionId,
        _claim: Option<hya_proto::ActorClaim>,
        _events: Vec<hya_proto::Event>,
    ) -> Result<Vec<Envelope>, HostError> {
        Err(HostError::Unavailable("session store unavailable".into()))
    }

    async fn append_extension_event(
        &self,
        _session: SessionId,
        _bundle: &str,
        _kind: &str,
        _payload: Value,
        _claim: Option<hya_proto::ActorClaim>,
    ) -> Result<Envelope, HostError> {
        Err(HostError::Unavailable("session store unavailable".into()))
    }

    async fn read_projection(
        &self,
        _session: SessionId,
    ) -> Result<Arc<hya_proto::Projection>, HostError> {
        Err(HostError::Unavailable("session store unavailable".into()))
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
        _session: SessionId,
    ) -> Result<Option<hya_runtime_abi::SessionSummaryMeta>, HostError> {
        Ok(None)
    }

    async fn list_sessions(
        &self,
        _filter: hya_runtime_abi::SessionListFilter,
    ) -> Result<Vec<hya_runtime_abi::SessionSummaryMeta>, HostError> {
        Ok(Vec::new())
    }
}

#[async_trait]
impl hya_runtime_abi::HostBundleStorage for MinimalHostRuntime {
    async fn get_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
    ) -> Result<Option<hya_runtime_abi::BundleStateEntry>, HostError> {
        Ok(None)
    }

    async fn put_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
        _value: Value,
        _expected_version: Option<u64>,
    ) -> Result<hya_runtime_abi::BundleStateEntry, HostError> {
        Err(HostError::Unavailable("bundle storage unavailable".into()))
    }

    async fn delete_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _key: &str,
    ) -> Result<bool, HostError> {
        Err(HostError::Unavailable("bundle storage unavailable".into()))
    }

    async fn list_state(
        &self,
        _bundle: &str,
        _scope: &str,
        _prefix: Option<&str>,
    ) -> Result<Vec<hya_runtime_abi::BundleStateEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn acquire_claim(
        &self,
        _request: hya_runtime_abi::BundleClaimRequest,
    ) -> Result<hya_runtime_abi::BundleClaimLease, HostError> {
        Err(HostError::Unavailable("bundle storage unavailable".into()))
    }

    async fn renew_claim(
        &self,
        _lease: &hya_runtime_abi::BundleClaimLease,
        _ttl_millis: u64,
    ) -> Result<hya_runtime_abi::BundleClaimLease, HostError> {
        Err(HostError::Unavailable("bundle storage unavailable".into()))
    }

    async fn release_claim(
        &self,
        _lease: &hya_runtime_abi::BundleClaimLease,
    ) -> Result<(), HostError> {
        Err(HostError::Unavailable("bundle storage unavailable".into()))
    }

    async fn read_claim(
        &self,
        _bundle: &str,
        _resource_key: &str,
    ) -> Result<Option<hya_runtime_abi::BundleClaimLease>, HostError> {
        Ok(None)
    }
}

#[async_trait]
impl hya_runtime_abi::HostTurnRunner for MinimalHostRuntime {
    async fn spawn_session(
        &self,
        _request: hya_runtime_abi::TurnSpawnRequest,
    ) -> Result<SessionId, HostError> {
        Err(HostError::Unavailable("turn runner unavailable".into()))
    }

    async fn run_turn(
        &self,
        _request: hya_runtime_abi::TurnStartRequest,
        _cancel: CancellationToken,
    ) -> Result<hya_runtime_abi::TurnOutcome, HostError> {
        Err(HostError::Unavailable("turn runner unavailable".into()))
    }

    async fn spawn_session_and_run(
        &self,
        _request: hya_runtime_abi::TurnSpawnRequest,
        _cancel: CancellationToken,
    ) -> Result<hya_runtime_abi::TurnOutcome, HostError> {
        Err(HostError::Unavailable("turn runner unavailable".into()))
    }

    async fn steer_turn(&self, _session: SessionId, _message: String) -> Result<(), HostError> {
        Err(HostError::Unavailable("turn runner unavailable".into()))
    }

    async fn cancel_turn(
        &self,
        _session: SessionId,
        _reason: Option<String>,
    ) -> Result<bool, HostError> {
        Err(HostError::Unavailable("turn runner unavailable".into()))
    }

    async fn turn_status(
        &self,
        _session: SessionId,
    ) -> Result<hya_runtime_abi::TurnStatus, HostError> {
        Ok(hya_runtime_abi::TurnStatus::Idle)
    }
}

#[async_trait]
impl hya_runtime_abi::HostModelCaller for MinimalHostRuntime {
    async fn complete(
        &self,
        _request: hya_runtime_abi::ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<hya_runtime_abi::ModelCallResponse, HostError> {
        Err(HostError::Unavailable("model caller unavailable".into()))
    }

    async fn stream(
        &self,
        _request: hya_runtime_abi::ModelCallRequest,
        _cancel: CancellationToken,
    ) -> Result<
        tokio::sync::mpsc::Receiver<Result<hya_runtime_abi::ModelStreamChunk, HostError>>,
        HostError,
    > {
        Err(HostError::Unavailable("model caller unavailable".into()))
    }

    async fn compact_window(
        &self,
        _model: &ModelRef,
        _messages: &[hya_proto::Message],
    ) -> Result<Option<String>, HostError> {
        Ok(None)
    }

    async fn model_window(
        &self,
        _model: &ModelRef,
    ) -> Result<Option<hya_runtime_abi::ModelWindowInfo>, HostError> {
        Ok(None)
    }
}

#[async_trait]
impl hya_runtime_abi::HostEventBus for MinimalHostRuntime {
    fn publish(&self, _envelope: Envelope) -> Result<(), HostError> {
        Err(HostError::Unavailable("event bus unavailable".into()))
    }

    fn subscribe(&self) -> Result<tokio::sync::broadcast::Receiver<Envelope>, HostError> {
        Err(HostError::Unavailable("event bus unavailable".into()))
    }

    async fn publish_extension(
        &self,
        _session: SessionId,
        _bundle: &str,
        _kind: &str,
        _payload: Value,
    ) -> Result<(), HostError> {
        Err(HostError::Unavailable("event bus unavailable".into()))
    }
}

#[async_trait]
impl hya_runtime_abi::HostCatalogReader for MinimalHostRuntime {
    async fn list_agents(
        &self,
        _session: Option<SessionId>,
    ) -> Result<Vec<hya_tool::AgentDef>, HostError> {
        Ok(Vec::new())
    }

    async fn get_agent(
        &self,
        _name: &hya_proto::AgentName,
        _session: Option<SessionId>,
    ) -> Result<Option<hya_bundle::PreparedAgent>, HostError> {
        Ok(None)
    }

    async fn list_skills(
        &self,
        _session: Option<SessionId>,
    ) -> Result<Vec<hya_tool::SkillCatalogEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn list_bundles(&self) -> Result<Vec<hya_runtime_abi::BundleCatalogSummary>, HostError> {
        Ok(Vec::new())
    }

    async fn get_bundle(&self, _id: &str) -> Result<Option<hya_bundle::Bundle>, HostError> {
        Ok(None)
    }

    async fn bundle_config(&self, bundle_id: &str) -> Result<Value, HostError> {
        Ok((self.bundle_config)(bundle_id))
    }

    async fn global_config(&self) -> Result<Value, HostError> {
        Ok(Value::Null)
    }
}

#[async_trait]
impl hya_runtime_abi::HostExtensionRegistry for MinimalHostRuntime {
    async fn list_extensions(
        &self,
        _kind: Option<hya_runtime_abi::ExtensionKind>,
    ) -> Result<Vec<hya_runtime_abi::ExtensionEntry>, HostError> {
        Ok(Vec::new())
    }

    async fn has_bundle(&self, _bundle_id: &str) -> Result<bool, HostError> {
        Ok(false)
    }

    async fn invoke_bundle_api(
        &self,
        _call: hya_runtime_abi::BundleApiInvocation,
    ) -> Result<hya_runtime_abi::BundleApiResult, HostError> {
        Err(HostError::Unavailable(
            "extension registry unavailable".into(),
        ))
    }

    async fn invoke_tool(
        &self,
        _session: Option<SessionId>,
        _tool_name: &str,
        _input: Value,
        _cancel: CancellationToken,
    ) -> Result<Value, HostError> {
        Err(HostError::Unavailable(
            "extension registry unavailable".into(),
        ))
    }
}

impl HostRuntime for MinimalHostRuntime {
    fn session_store(&self) -> &dyn hya_runtime_abi::HostSessionStore {
        self
    }

    fn bundle_storage(&self) -> &dyn hya_runtime_abi::HostBundleStorage {
        self
    }

    fn turn_runner(&self) -> &dyn hya_runtime_abi::HostTurnRunner {
        self
    }

    fn model_caller(&self) -> &dyn hya_runtime_abi::HostModelCaller {
        self
    }

    fn event_bus(&self) -> &dyn hya_runtime_abi::HostEventBus {
        self
    }

    fn catalog_reader(&self) -> &dyn hya_runtime_abi::HostCatalogReader {
        self
    }

    fn extension_registry(&self) -> &dyn hya_runtime_abi::HostExtensionRegistry {
        self
    }
}

/// Minimal [`HostRuntime`] whose capability planes return no-op / unavailable defaults.
#[must_use]
pub fn noop_host_runtime() -> Arc<dyn HostRuntime> {
    static NOOP: std::sync::OnceLock<Arc<dyn HostRuntime>> = std::sync::OnceLock::new();
    Arc::clone(NOOP.get_or_init(|| Arc::new(MinimalHostRuntime::empty())))
}
