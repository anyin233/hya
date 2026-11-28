use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{ActorClaim, OperationId, SessionId, ToolCallId, ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::handle::{ArtifactPlane, HandleRouter, UriSchemaPlane};
use crate::interaction::InteractionPlane;
use crate::permission::{Invocation, Mode, PermissionPlane};
use crate::skill::SkillPlane;

pub use hya_tool_error::ToolError;

/// Canonical tool names permanently reserved for the `hya-core` kernel loop.
///
/// No bundle, plugin, MCP server, or registry caller may register, override,
/// shadow, or alias either name (ADR-0035, ADR-0039).
pub const RESERVED_KERNEL_TOOLS: [&str; 2] = ["rewrite", hya_proto::context::TODO_TOOL];

/// Whether `name` is permanently reserved for the kernel loop (`rewrite` or `todo`).
#[must_use]
pub fn is_reserved_kernel_tool(name: &str) -> bool {
    RESERVED_KERNEL_TOOLS.contains(&name)
}

/// Registry rejected a registration because the name (or alias) is already taken
/// or permanently reserved by the kernel.
#[derive(Error, Debug, Clone, PartialEq, Eq)]
#[error("duplicate or reserved tool name: {name}")]
pub struct DuplicateName {
    /// Conflicting or reserved tool or alias name.
    pub name: String,
}

/// Parent session identity attached to [`ToolCtx::extensions`] for nested or
/// subagent turns.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ParentSession(pub SessionId);

/// Lockstep-ABI-safe type-erased plane entry owned behind [`Arc`].
///
/// Keyed by [`std::any::type_name`] rather than [`std::any::TypeId`] so host
/// binaries and native `cdylib` bundles compiled in separate Cargo invocations
/// (which can produce distinct crate SVH hashes and thus distinct `TypeId`s)
/// still share extension planes safely across the lockstep ABI v2 boundary.
struct ErasedPlane {
    ptr: *mut (),
    drop_fn: unsafe fn(*mut ()),
    size: usize,
    align: usize,
}

// SAFETY: `ErasedPlane::new` only accepts `T: Send + Sync + 'static`, and
// lookups only expose shared references `&T` while the `Arc<ErasedPlane>` is alive.
unsafe impl Send for ErasedPlane {}
// SAFETY: same as `Send` above (`T: Sync` and no interior mutation through `ptr`).
unsafe impl Sync for ErasedPlane {}

impl Drop for ErasedPlane {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` was created by `Box::into_raw(Box::new(plane))` in
        // `ErasedPlane::new::<T>`, `self.drop_fn` reconstructs that exact
        // `Box<T>` in the allocating side's allocator, and `Drop` runs once.
        unsafe { (self.drop_fn)(self.ptr) };
    }
}

impl ErasedPlane {
    fn new<T: Send + Sync + 'static>(plane: T) -> Self {
        unsafe fn drop_erased<T>(ptr: *mut ()) {
            // SAFETY: `ptr` was produced by `Box::into_raw(Box::new::<T>(...))`.
            unsafe { drop(Box::from_raw(ptr.cast::<T>())) };
        }
        Self {
            ptr: Box::into_raw(Box::new(plane)).cast::<()>(),
            drop_fn: drop_erased::<T>,
            size: std::mem::size_of::<T>(),
            align: std::mem::align_of::<T>(),
        }
    }

    fn downcast_ref<T: Send + Sync + 'static>(&self) -> Option<&T> {
        if self.size != std::mem::size_of::<T>() || self.align != std::mem::align_of::<T>() {
            return None;
        }
        // SAFETY: the caller already matched `std::any::type_name::<T>()` under
        // the lockstep ABI and verified `size` and `align`; `self.ptr` remains
        // valid and immutable for the borrow lifetime of `&self`.
        Some(unsafe { &*self.ptr.cast::<T>() })
    }
}

/// Type-indexed container of optional domain planes attached to [`ToolCtx`].
///
/// Core `hya-bare` passes an empty [`ExtensionPlanes`]. Extension bundles and
/// host adapters attach domain planes (such as [`crate::SpawnerPlane`],
/// [`crate::MailboxPlane`], [`crate::LifecyclePlane`], [`crate::WorkflowPlane`],
/// [`crate::ProjectActivityPlane`], [`crate::LspPlane`], [`crate::FormatterPlane`],
/// agent rosters, or [`ParentSession`]) without coupling [`ToolCtx`] to domain
/// subsystems (ADR-0039).
#[derive(Clone, Default)]
pub struct ExtensionPlanes {
    planes: Option<Arc<BTreeMap<String, Arc<ErasedPlane>>>>,
}

impl std::fmt::Debug for ExtensionPlanes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtensionPlanes")
            .field("count", &self.len())
            .finish()
    }
}

impl ExtensionPlanes {
    /// Create an empty extension plane set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of registered extension planes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.planes.as_ref().map_or(0, |map| map.len())
    }

    /// Whether no extension planes are attached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Insert or replace an extension plane of type `T`.
    pub fn insert<T: Send + Sync + 'static>(&mut self, plane: T) {
        let map = Arc::make_mut(self.planes.get_or_insert_with(|| Arc::new(BTreeMap::new())));
        map.insert(
            std::any::type_name::<T>().to_string(),
            Arc::new(ErasedPlane::new(plane)),
        );
    }

    /// Builder-style variant of [`Self::insert`].
    #[must_use]
    pub fn with<T: Send + Sync + 'static>(mut self, plane: T) -> Self {
        self.insert(plane);
        self
    }

    /// Borrow an attached extension plane of type `T`, if present.
    #[must_use]
    pub fn get<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.planes
            .as_ref()?
            .get(std::any::type_name::<T>())
            .and_then(|entry| entry.downcast_ref::<T>())
    }

    /// Whether an extension plane of type `T` is attached.
    #[must_use]
    pub fn contains<T: Send + Sync + 'static>(&self) -> bool {
        self.planes
            .as_ref()
            .is_some_and(|map| map.contains_key(std::any::type_name::<T>()))
    }
}

/// Per-call runtime context passed to every [`Tool::execute`].
///
/// Contains only single-agent v2 core planes plus [`ExtensionPlanes`] for
/// optional bundle/domain planes (ADR-0039); tools assert permissions and call
/// planes without holding the session engine itself.
#[derive(Clone)]
pub struct ToolCtx {
    /// Session working directory used for path resolution.
    pub workdir: PathBuf,
    /// Workspace roots of the session (ADR-0024), resolved fresh at each
    /// turn start: a Project session's roots in order (the workdir prepended
    /// when it lies inside none of them), otherwise just `[workdir]`. Ordered
    /// and deduplicated; never empty for an engine-built context. The file
    /// tools' path boundary ([`crate::ProjectScope`], ADR-0026).
    pub roots: Vec<PathBuf>,
    /// Cancellation token for cooperative abort.
    pub cancel: CancellationToken,
    /// Active session id when the tool runs inside a session.
    pub session: Option<SessionId>,
    /// Persisted operation identity for this tool call.
    pub operation: ToolOperation,
    /// Call-scoped permission plane (after invocation authorization).
    pub permission: PermissionPlane,
    /// Channel for human questions (`question` / `ask_user` / `plan_exit`).
    pub interaction: InteractionPlane,
    /// Skill catalog plane for the `skill` tool.
    pub skills: SkillPlane,
    /// Spilled tool-output artifact store (`artifact://`).
    pub artifacts: ArtifactPlane,
    /// Dynamic URI scheme handlers registered on this context's handle router.
    pub schemas: UriSchemaPlane,
    /// The calling agent's other tools, callable as part of this call
    /// (disconnected inside a nested call; see [`crate::nested`]).
    pub nested: crate::NestedToolPlane,
    /// Optional domain planes attached by extension bundles and host adapters.
    pub extensions: ExtensionPlanes,
}

impl ToolCtx {
    /// Build a minimal single-agent [`ToolCtx`] with disconnected optional
    /// planes and empty [`ExtensionPlanes`].
    #[must_use]
    pub fn new(workdir: PathBuf, permission: PermissionPlane, cancel: CancellationToken) -> Self {
        let roots = vec![workdir.clone()];
        Self {
            workdir,
            roots,
            cancel,
            session: None,
            operation: ToolOperation::from_tool_call(ToolCallId::new()),
            permission,
            interaction: InteractionPlane::new().0,
            skills: SkillPlane::default(),
            artifacts: ArtifactPlane,
            schemas: UriSchemaPlane::default(),
            nested: crate::NestedToolPlane::disconnected(),
            extensions: ExtensionPlanes::default(),
        }
    }

    /// Attach or replace a typed domain plane in [`Self::extensions`].
    #[must_use]
    pub fn with_extension<T: Send + Sync + 'static>(mut self, plane: T) -> Self {
        self.extensions.insert(plane);
        self
    }

    /// Borrow a typed domain plane from [`Self::extensions`], if attached.
    #[must_use]
    pub fn extension<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.extensions.get::<T>()
    }

    /// Internal resource URLs reachable from this call.
    ///
    /// Built from the call's own `workdir` and skill catalog rather than stored
    /// alongside them, so a router can never resolve against a different session
    /// than the context it came from.
    ///
    /// This is the agent's own namespace — spilled output, skill bodies, scratch
    /// payloads. It has no bearing on how `read`, `write`, `grep`, or `bash`
    /// treat an ordinary filesystem path.
    #[must_use]
    pub fn handles(&self) -> HandleRouter {
        HandleRouter::new(&self.workdir)
            .with_skills(self.skills.clone())
            .with_artifacts(self.artifacts.store(&self.workdir))
            .with_actor_claim(self.operation.actor_claim())
            .with_uri_handlers(Arc::clone(self.schemas.uri_handlers()))
    }

    /// Register a dynamic URI scheme handler on this context's handle router.
    ///
    /// # Errors
    /// Returns [`crate::handle::SchemeRegistryError`] if the handler attempts to
    /// claim an internal scheme (`artifact`, `skill`, `local`) or collides with
    /// an already registered dynamic scheme.
    pub fn with_uri_handler(
        mut self,
        handler: Arc<dyn crate::handle::DynamicUriHandler>,
    ) -> Result<Self, crate::handle::SchemeRegistryError> {
        self.schemas = self.schemas.with_uri_handler(handler)?;
        Ok(self)
    }
}

/// Immutable identity of the persisted tool invocation and its admission operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ToolOperation {
    source_tool_call_id: ToolCallId,
    operation_id: OperationId,
    actor_claim: Option<ActorClaim>,
}

impl ToolOperation {
    /// Derive operation identity from a provider tool-call id (no actor claim yet).
    #[must_use]
    pub fn from_tool_call(source_tool_call_id: ToolCallId) -> Self {
        Self {
            source_tool_call_id,
            operation_id: OperationId::from_tool_call(source_tool_call_id),
            actor_claim: None,
        }
    }

    /// Attach the optional actor claim used by resident/subagent fences.
    #[must_use]
    pub const fn with_actor_claim(mut self, actor_claim: Option<ActorClaim>) -> Self {
        self.actor_claim = actor_claim;
        self
    }

    /// Provider-facing tool-call id that originated this operation.
    #[must_use]
    pub fn source_tool_call_id(self) -> ToolCallId {
        self.source_tool_call_id
    }

    /// Stable operation id used for admission and conflict detection.
    #[must_use]
    pub fn operation_id(self) -> OperationId {
        self.operation_id
    }

    /// Actor claim, when this call runs under a fenced resident or subagent.
    #[must_use]
    pub const fn actor_claim(self) -> Option<ActorClaim> {
        self.actor_claim
    }
}

const BUILTIN_DISPATCH_IDENTITY_DOMAIN_V1: &[u8] = b"hya.tool.builtin-dispatch/v1";

/// Policy used when bounding a successful tool result before persistence.
///
/// The default keeps the historical 5,000-character tail behavior. Coding
/// tools opt into shape-aware bounds so their presentation envelope remains
/// structured while large fields are capped independently.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ToolResultPolicy {
    /// Apply the legacy display-text cap to arbitrary JSON values.
    #[default]
    Default,
    /// Preserve a structured result whose tool already bounds its display text.
    SelfBudgeted,
    /// Preserve a bounded coding-tool presentation envelope.
    Coding,
    /// Preserve a coding envelope and an independently bounded edit diff.
    CodingWithDiff,
}

/// Model-callable capability: stable name, advertised schema, and async execution.
///
/// Implementations must not assume a particular UI. They assert permissions on
/// [`ToolCtx::permission`], use planes for side effects, and return JSON values
/// the engine will optionally pass through [`crate::cap_tool_output`].
#[async_trait]
pub trait Tool: Send + Sync {
    /// Canonical tool name as registered and advertised to the model.
    fn name(&self) -> &str;
    /// JSON Schema describing required and optional arguments.
    fn schema(&self) -> ToolSchema;
    /// Result bounding policy used after successful execution.
    ///
    /// The default keeps external tools source-compatible and preserves the
    /// historical arbitrary-value cap. Built-in coding adapters override this
    /// method explicitly rather than relying on their name.
    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Default
    }
    /// Run the tool body with validated (or raw) `input` and the call context.
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError>;
}

/// A tool exposed under a different provider-facing name than its inner
/// implementation. Used for qualified contributed names
/// (`{namespace}__{local}`) and legacy alias spellings; `execute` always
/// delegates to the inner tool.
pub struct NamedTool {
    name: String,
    inner: Arc<dyn Tool>,
}

impl NamedTool {
    /// Wrap `inner` so it is registered and advertised as `name`.
    pub fn new(name: impl Into<String>, inner: Arc<dyn Tool>) -> Self {
        Self {
            name: name.into(),
            inner,
        }
    }
}

#[async_trait]
impl Tool for NamedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn schema(&self) -> ToolSchema {
        let mut schema = self.inner.schema();
        schema.name = ToolName::new(self.name.clone());
        schema
    }

    fn result_policy(&self) -> ToolResultPolicy {
        self.inner.result_policy()
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        self.inner.execute(ctx, input).await
    }
}

#[cfg(test)]
mod result_policy_tests {
    use super::*;

    struct ExplicitPolicyTool;

    #[async_trait]
    impl Tool for ExplicitPolicyTool {
        fn name(&self) -> &str {
            "wrapped"
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new("wrapped"),
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
            }
        }

        fn result_policy(&self) -> ToolResultPolicy {
            ToolResultPolicy::CodingWithDiff
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            Ok(Value::Null)
        }
    }

    struct DefaultPolicyTool;

    #[async_trait]
    impl Tool for DefaultPolicyTool {
        fn name(&self) -> &str {
            "read"
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: ToolName::new("read"),
                description: String::new(),
                input_schema: serde_json::json!({"type": "object"}),
                output_schema: None,
            }
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            Ok(Value::Null)
        }
    }

    #[test]
    fn external_tool_gets_legacy_policy_by_default() {
        assert_eq!(DefaultPolicyTool.result_policy(), ToolResultPolicy::Default);
    }

    #[test]
    fn named_tool_forwards_inner_policy() {
        let named = NamedTool {
            name: "alias".to_string(),
            inner: Arc::new(ExplicitPolicyTool),
        };
        assert_eq!(named.name(), "alias");
        assert_eq!(named.result_policy(), ToolResultPolicy::CodingWithDiff);
    }

    #[test]
    fn extension_planes_insert_get_replace_and_clone() {
        #[derive(Clone, Debug, PartialEq, Eq)]
        struct TestPlane(&'static str);

        let mut planes = ExtensionPlanes::new();
        assert!(planes.is_empty());
        assert_eq!(planes.len(), 0);
        assert!(!planes.contains::<TestPlane>());
        assert_eq!(planes.get::<TestPlane>(), None);

        planes.insert(TestPlane("first"));
        assert!(!planes.is_empty());
        assert_eq!(planes.len(), 1);
        assert!(planes.contains::<TestPlane>());
        assert_eq!(planes.get::<TestPlane>(), Some(&TestPlane("first")));

        let cloned = planes.clone();
        planes.insert(TestPlane("second"));
        assert_eq!(planes.get::<TestPlane>(), Some(&TestPlane("second")));
        assert_eq!(cloned.get::<TestPlane>(), Some(&TestPlane("first")));

        let session = SessionId::new();
        let (permission, _rx) = PermissionPlane::new(crate::PermissionRules::default());
        let ctx = ToolCtx::new(PathBuf::from("."), permission, CancellationToken::new())
            .with_extension(ParentSession(session))
            .with_extension(TestPlane("ctx"));
        assert_eq!(
            ctx.extension::<ParentSession>(),
            Some(&ParentSession(session))
        );
        assert_eq!(ctx.extension::<TestPlane>(), Some(&TestPlane("ctx")));

        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        struct DropCounter(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for DropCounter {
            fn drop(&mut self) {
                self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
        }
        {
            let p1 = ExtensionPlanes::new().with(DropCounter(Arc::clone(&drops)));
            let p2 = p1.clone();
            assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 0);
            drop(p1);
            assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 0);
            drop(p2);
        }
        assert_eq!(drops.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

/// Mutable tool catalog used to assemble a complete runtime candidate.
///
/// A live session engine consumes an immutable [`ToolRegistrySnapshot`] instead.
/// Mutating this builder after snapshotting does not alter an effective runtime
/// view.
#[derive(Default)]
pub struct ToolRegistry {
    inner: std::sync::RwLock<ToolRegistryInner>,
}

/// An alternate exported spelling and whether models see its schema.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AliasVisibility {
    /// Dispatchable compatibility spelling omitted from advertised schemas.
    Hidden,
    /// Dispatchable spelling that is also advertised to models.
    #[deprecated(note = "Register a canonical tool name to advertise an additional spelling")]
    Public,
}

#[derive(Clone, Default)]
struct ToolRegistryInner {
    tools: HashMap<String, ResolvedTool>,
    aliases: HashMap<String, ResolvedTool>,
    dispatch_identities: HashMap<String, [u8; 32]>,
    advertised_aliases: BTreeSet<String>,
    bundle_origins: HashMap<String, &'static str>,
}

/// Immutable, lock-free tool view retained by an admitted turn.
#[derive(Clone)]
pub struct ToolRegistrySnapshot {
    inner: Arc<ToolRegistryInner>,
}

/// Invocation-level permission class attached at registration time.
///
/// Drives the default mode for [`ResolvedTool::invocation`]: read-only and task
/// default to allow, general tools and MCP default to ask, and commands build a
/// dual tool+command subject from the `command` field.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolPermission {
    /// Local discovery and read tools that default to allow under `default`.
    ReadOnly,
    /// Subagent launch (`task`); defaults to allow at the invocation layer.
    Task,
    /// Mutating or network tools that default to ask.
    Tool,
    /// Shell tools that also subject-match the full command string.
    Command,
    /// MCP-bridged tools; subject is the namespaced MCP name.
    Mcp,
}

/// A registered tool together with its invocation permission class.
#[derive(Clone)]
pub struct ResolvedTool {
    /// Shared tool implementation.
    pub tool: Arc<dyn Tool>,
    /// How the engine builds the pre-execution [`Invocation`].
    pub permission: ToolPermission,
}

impl ResolvedTool {
    /// Build the invocation subject(s) used by [`PermissionPlane::authorize`].
    ///
    /// # Errors
    /// Returns [`ToolError::Input`] when a command tool is missing a string `command`.
    pub fn invocation(&self, input: &Value) -> Result<Invocation, ToolError> {
        let name = self.tool.name();
        match self.permission {
            ToolPermission::ReadOnly | ToolPermission::Task => {
                Ok(Invocation::tool(name, Mode::Allow))
            }
            ToolPermission::Tool => Ok(Invocation::tool(name, Mode::Ask)),
            ToolPermission::Command => input
                .get("command")
                .and_then(Value::as_str)
                .map(|command| Invocation::command(name, command))
                .ok_or_else(|| ToolError::Input("command must be a string".to_string())),
            ToolPermission::Mcp => Ok(Invocation::mcp(name)),
        }
    }
}

impl ToolRegistry {
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, ToolRegistryInner> {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, ToolRegistryInner> {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Freeze the current builder contents into an immutable runtime view.
    #[must_use]
    pub fn snapshot(&self) -> ToolRegistrySnapshot {
        ToolRegistrySnapshot {
            inner: Arc::new(self.read().clone()),
        }
    }

    /// Start an offline candidate builder from an immutable runtime view.
    #[must_use]
    pub fn from_snapshot(snapshot: &ToolRegistrySnapshot) -> Self {
        Self {
            inner: std::sync::RwLock::new((*snapshot.inner).clone()),
        }
    }

    /// Compare a candidate with a frozen view by names, permission classes,
    /// aliases, and executor identity.
    #[must_use]
    pub fn logically_matches(&self, snapshot: &ToolRegistrySnapshot) -> bool {
        let candidate = self.read();
        maps_match(&candidate.tools, &snapshot.inner.tools)
            && maps_match(&candidate.aliases, &snapshot.inner.aliases)
            && candidate.dispatch_identities == snapshot.inner.dispatch_identities
            && candidate.bundle_origins == snapshot.inner.bundle_origins
    }

    /// Register a tool on this candidate builder through a shared reference.
    pub fn register(&self, tool: Arc<dyn Tool>) -> Result<(), DuplicateName> {
        self.register_with_permission(tool, ToolPermission::Tool)
    }

    /// Register a candidate tool with an explicit permission class.
    pub fn register_with_permission(
        &self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
    ) -> Result<(), DuplicateName> {
        self.register_with_permission_and_aliases(tool, permission, &[])
    }

    /// Register one canonical tool plus aliases after validating the entire
    /// name set. Candidate builders use this before an immutable publication.
    pub fn register_with_permission_and_aliases(
        &self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
        aliases: &[String],
    ) -> Result<(), DuplicateName> {
        self.register_with_permission_and_aliases_and_identity(tool, permission, aliases, None)
    }

    /// Register a tool with aliases and an explicit dispatch-identity digest.
    ///
    /// # Errors
    /// Returns [`DuplicateName`] when the canonical name or any alias collides.
    pub fn register_with_permission_and_aliases_and_dispatch_identity(
        &self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
        aliases: &[String],
        identity: [u8; 32],
    ) -> Result<(), DuplicateName> {
        self.register_with_permission_and_aliases_and_identity(
            tool,
            permission,
            aliases,
            Some(identity),
        )
    }

    fn register_with_permission_and_aliases_and_identity(
        &self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
        aliases: &[String],
        identity: Option<[u8; 32]>,
    ) -> Result<(), DuplicateName> {
        let name = tool.name().to_string();
        if is_reserved_kernel_tool(&name) {
            return Err(DuplicateName { name });
        }
        let mut inner = self.write();
        if inner.tools.contains_key(&name) || inner.aliases.contains_key(&name) {
            return Err(DuplicateName { name });
        }
        let mut pending = std::collections::BTreeSet::new();
        for alias in aliases {
            if is_reserved_kernel_tool(alias)
                || alias == &name
                || !pending.insert(alias.as_str())
                || inner.tools.contains_key(alias)
                || inner.aliases.contains_key(alias)
            {
                return Err(DuplicateName {
                    name: alias.clone(),
                });
            }
        }
        let resolved = ResolvedTool { tool, permission };
        inner.tools.insert(name.clone(), resolved.clone());
        for alias in aliases {
            inner.aliases.insert(alias.clone(), resolved.clone());
        }
        if let Some(identity) = identity {
            inner.dispatch_identities.insert(name, identity);
        }
        Ok(())
    }

    /// Look up the tool implementation by canonical name or hidden alias.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.resolve(name).map(|resolved| resolved.tool)
    }

    /// Bundle identity that supplies a dynamically loaded builtin tool.
    #[must_use]
    pub fn builtin_bundle_origin(&self, canonical: &str) -> Option<&'static str> {
        self.read().bundle_origins.get(canonical).copied()
    }

    /// Resolve name or alias to the full [`ResolvedTool`] (tool + permission class).
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<ResolvedTool> {
        let inner = self.read();
        inner
            .tools
            .get(name)
            .or_else(|| inner.aliases.get(name))
            .cloned()
    }

    /// Remove a canonical tool and every alias that pointed at it.
    pub fn remove(&self, name: &str) {
        let mut inner = self.write();
        if inner.tools.remove(name).is_some() {
            inner.dispatch_identities.remove(name);
            inner.bundle_origins.remove(name);
        }
        inner
            .aliases
            .retain(|alias, resolved| alias != name && resolved.tool.name() != name);
    }

    /// Collect advertised schemas for every **canonical** tool (aliases excluded).
    #[must_use]
    pub fn schemas(&self) -> Vec<ToolSchema> {
        let inner = self.read();
        let mut schemas = inner
            .tools
            .values()
            .map(|resolved| resolved.tool.schema())
            .collect::<Vec<_>>();
        schemas.extend(inner.advertised_aliases.iter().filter_map(|alias| {
            inner.aliases.get(alias).map(|resolved| {
                let mut schema = resolved.tool.schema();
                schema.name = ToolName::new(alias.clone());
                schema
            })
        }));
        schemas
    }

    /// Register a native builtin with its dispatch identity, origin, and aliases.
    ///
    /// # Panics
    ///
    /// Panics if the canonical name or any alias collides with a registered name.
    #[doc(hidden)]
    #[allow(deprecated, reason = "Preserve advertising for legacy public aliases")]
    pub fn register_builtin<'a>(
        &self,
        tool: Arc<dyn Tool>,
        permission: ToolPermission,
        aliases: impl IntoIterator<Item = (&'a str, AliasVisibility)>,
        origin: &'static str,
    ) {
        let name = tool.name().to_string();
        assert!(
            !is_reserved_kernel_tool(&name),
            "built-in tool `{name}` is reserved for the kernel and cannot be registered"
        );
        let mut inner = self.write();
        let replaced = inner.tools.insert(
            name.clone(),
            ResolvedTool {
                tool: Arc::clone(&tool),
                permission,
            },
        );
        assert!(
            replaced.is_none(),
            "duplicate built-in tool name `{name}` in the fixed builtin list"
        );
        if let Some(identity) = builtin_dispatch_identity(&name) {
            inner.dispatch_identities.insert(name.clone(), identity);
        }
        inner.bundle_origins.insert(name, origin);
        for (alias, visibility) in aliases {
            assert!(
                !is_reserved_kernel_tool(alias),
                "built-in alias `{alias}` is reserved for the kernel and cannot be registered"
            );
            let replaced = inner.aliases.insert(
                alias.to_string(),
                ResolvedTool {
                    tool: Arc::clone(&tool),
                    permission,
                },
            );
            assert!(
                replaced.is_none() && !inner.tools.contains_key(alias),
                "duplicate tool-family preset alias `{alias}`"
            );
            if visibility == AliasVisibility::Public {
                inner.advertised_aliases.insert(alias.to_string());
            }
        }
    }
}

fn maps_match(left: &HashMap<String, ResolvedTool>, right: &HashMap<String, ResolvedTool>) -> bool {
    left.len() == right.len()
        && left.iter().all(|(name, left)| {
            right.get(name).is_some_and(|right| {
                left.permission == right.permission && Arc::ptr_eq(&left.tool, &right.tool)
            })
        })
}

fn builtin_dispatch_identity(canonical: &str) -> Option<[u8; 32]> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(BUILTIN_DISPATCH_IDENTITY_DOMAIN_V1);
    append_length_prefixed(&mut bytes, hya_version::BACKEND_VERSION.as_bytes())?;
    append_length_prefixed(&mut bytes, canonical.as_bytes())?;
    Some(Sha256::digest(bytes).into())
}

fn append_length_prefixed(bytes: &mut Vec<u8>, value: &[u8]) -> Option<()> {
    let length = u64::try_from(value.len()).ok()?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
    Some(())
}

impl ToolRegistrySnapshot {
    /// Domain-separated dispatch identity for a canonical tool, when recorded.
    #[must_use]
    pub fn dispatch_identity_v1(&self, canonical: &str) -> Option<[u8; 32]> {
        self.inner.dispatch_identities.get(canonical).copied()
    }

    /// Resolve a canonical name or hidden alias without locking.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<ResolvedTool> {
        self.inner
            .tools
            .get(name)
            .or_else(|| self.inner.aliases.get(name))
            .cloned()
    }

    /// Advertised schemas for canonical tools only (aliases are not listed).
    #[must_use]
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.inner
            .tools
            .values()
            .map(|resolved| resolved.tool.schema())
            .collect()
    }

    /// Canonical effective tools, excluding alternate registry aliases.
    #[must_use]
    pub fn canonical_tools(&self) -> Vec<(String, ResolvedTool)> {
        self.inner
            .tools
            .iter()
            .map(|(name, resolved)| (name.clone(), resolved.clone()))
            .collect()
    }

    /// Alias spellings whose resolved tool name matches `canonical`.
    /// Narrow seam used by resource-view compilation to project candidate
    /// effective aliases; there is no bulk public alias dump.
    #[must_use]
    pub fn aliases_for_canonical(&self, canonical: &str) -> Vec<String> {
        let mut names = self
            .inner
            .aliases
            .iter()
            .filter(|(_, resolved)| resolved.tool.name() == canonical)
            .map(|(alias, _)| alias.clone())
            .collect::<Vec<_>>();
        names.sort();
        names
    }
}

/// Construct an object-input tool schema from property and required-key sets.
#[must_use]
pub fn obj_schema(name: &str, description: &str, props: Value, required: &[&str]) -> ToolSchema {
    ToolSchema {
        name: ToolName::new(name),
        description: description.to_string(),
        input_schema: json!({ "type": "object", "properties": props, "required": required }),
        output_schema: None,
    }
}
