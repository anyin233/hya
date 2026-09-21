//! Registration and dispatch of external URI schemes.
//!
//! The internal handle families (`artifact://`, `skill://`, `local://`) stay a
//! closed set owned by [`super::HandleRouter`]. Bundles and other runtime
//! sources can *register additional schemes* — `db://x/y` handled by the
//! bundle's query tool — through [`SchemeRegistry`], and the runtime publishes
//! one winning binding per scheme the way it publishes bare-name masks.
//!
//! Dispatch never consults a process-global table: [`SchemeDispatch`] is the
//! per-view table of schemes whose owning tool is present in exactly that
//! compiled view, and [`SchemeReadTool`] / [`SchemeWriteTool`] are the
//! decorators a compiled view installs on its own `read` / `write` tools. An
//! agent whose view lacks the owning tool keeps the historical behavior for
//! that scheme (an unknown-scheme input error), which is what keeps a scheme
//! extension from leaking past the agent it was granted to.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::ToolSchema;
use serde_json::{Value, json};
use thiserror::Error;

use super::HandleError;
use crate::tool::{Tool, ToolCtx, ToolError, ToolResultPolicy};

/// Handle families owned by the router itself; they can never be registered.
const INTERNAL_SCHEMES: [&str; 3] = ["artifact", "skill", "local"];

/// Why a scheme could not be registered.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum SchemeRegistryError {
    /// The scheme addresses an internal handle family.
    #[error("scheme `{0}` is reserved for internal handles and cannot be registered")]
    ProtectedScheme(String),
    /// Another owner already holds the scheme.
    #[error("scheme `{scheme}` is already owned by `{owner}`")]
    AlreadyOwned {
        /// The contested scheme.
        scheme: String,
        /// Label of the incumbent owner.
        owner: String,
    },
}

/// One source's binding of an external URI scheme to its owning tool.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SchemeBinding {
    owner: String,
    canonical_tool: String,
    writable: bool,
}

impl SchemeBinding {
    /// Build a binding: `owner` is the contributing source label, and
    /// `canonical_tool` the canonical name of the tool that serves the scheme.
    #[must_use]
    pub fn new(
        owner: impl Into<String>,
        canonical_tool: impl Into<String>,
        writable: bool,
    ) -> Self {
        Self {
            owner: owner.into(),
            canonical_tool: canonical_tool.into(),
            writable,
        }
    }

    /// Contributing source label (e.g. `bundle:vecdb`).
    #[must_use]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    /// Canonical name of the tool that serves the scheme.
    #[must_use]
    pub fn canonical_tool(&self) -> &str {
        &self.canonical_tool
    }

    /// Whether the scheme accepts write dispatch in addition to reads.
    #[must_use]
    pub const fn writable(&self) -> bool {
        self.writable
    }
}

/// Owner of the `scheme → binding` table for one publication scope.
///
/// Registering an internal scheme is a hard error naming the protected scheme,
/// and re-registering a scheme another owner already holds is a hard error
/// naming the incumbent. Cross-source contention is adjudicated by the caller
/// (the runtime applies its masking total order) before it reaches this table.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SchemeRegistry {
    bindings: BTreeMap<String, SchemeBinding>,
}

impl SchemeRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `scheme` addresses one of the router-owned internal families.
    #[must_use]
    pub fn is_internal_scheme(scheme: &str) -> bool {
        INTERNAL_SCHEMES.contains(&scheme)
    }

    /// Whether `scheme` is a publishable token: at least two characters of
    /// `[a-zA-Z0-9_-]` and no `__` separator.
    #[must_use]
    pub fn is_valid_scheme_token(scheme: &str) -> bool {
        scheme.len() >= 2
            && !scheme.contains("__")
            && scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    }

    /// Register `scheme`, rejecting internal families and incumbent owners.
    ///
    /// # Errors
    /// Returns [`SchemeRegistryError::ProtectedScheme`] for `artifact`,
    /// `skill`, or `local`, and [`SchemeRegistryError::AlreadyOwned`] when the
    /// scheme is already bound, naming the current owner.
    pub fn register(
        &mut self,
        scheme: impl Into<String>,
        binding: SchemeBinding,
    ) -> Result<(), SchemeRegistryError> {
        let scheme = scheme.into();
        if Self::is_internal_scheme(&scheme) {
            return Err(SchemeRegistryError::ProtectedScheme(scheme));
        }
        if let Some(incumbent) = self.bindings.get(&scheme) {
            return Err(SchemeRegistryError::AlreadyOwned {
                scheme,
                owner: incumbent.owner.clone(),
            });
        }
        self.bindings.insert(scheme, binding);
        Ok(())
    }

    /// The binding registered for `scheme`, if any.
    #[must_use]
    pub fn resolve(&self, scheme: &str) -> Option<&SchemeBinding> {
        self.bindings.get(scheme)
    }

    /// Every registered binding, keyed by scheme.
    #[must_use]
    pub fn bindings(&self) -> &BTreeMap<String, SchemeBinding> {
        &self.bindings
    }
}

/// One view-resolved scheme handler: the winning binding plus the owning tool
/// as that specific compiled view resolves it.
#[derive(Clone)]
pub struct SchemeHandler {
    binding: SchemeBinding,
    tool: Arc<dyn Tool>,
}

impl SchemeHandler {
    /// Pair a winning binding with the owning tool resolved inside one view.
    #[must_use]
    pub fn new(binding: SchemeBinding, tool: Arc<dyn Tool>) -> Self {
        Self { binding, tool }
    }

    /// The winning scheme binding.
    #[must_use]
    pub const fn binding(&self) -> &SchemeBinding {
        &self.binding
    }

    /// The owning tool as this view resolves it.
    #[must_use]
    pub fn tool(&self) -> &Arc<dyn Tool> {
        &self.tool
    }
}

/// The per-view scheme dispatch table.
///
/// Built once when a compiled resource view is assembled, from the published
/// scheme bindings whose owning tool that view actually contains. There is no
/// global fallback: a view without the owner has no entry and no dispatch.
#[derive(Clone, Default)]
pub struct SchemeDispatch {
    handlers: Arc<BTreeMap<String, SchemeHandler>>,
}

impl std::fmt::Debug for SchemeDispatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SchemeDispatch")
            .field("schemes", &self.handlers.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SchemeDispatch {
    /// Build a dispatch table from `scheme → handler` entries.
    #[must_use]
    pub fn new(handlers: BTreeMap<String, SchemeHandler>) -> Self {
        Self {
            handlers: Arc::new(handlers),
        }
    }

    /// Whether this view resolves no schemes at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.handlers.is_empty()
    }

    /// The handler for `scheme` in this view, if its owner is present.
    #[must_use]
    pub fn lookup(&self, scheme: &str) -> Option<&SchemeHandler> {
        self.handlers.get(scheme)
    }
}

/// The external scheme of a `scheme://`-shaped argument, if it is not one of
/// the router-owned internal families.
fn external_scheme_of(text: &str) -> Option<&str> {
    let (scheme, _) = text.split_once("://")?;
    (!SchemeRegistry::is_internal_scheme(scheme)).then_some(scheme)
}

/// Extract the path-ish argument a read or write call addressed.
fn addressed_text(input: &Value) -> Option<&str> {
    input
        .get("path")
        .and_then(Value::as_str)
        .or_else(|| input.get("filePath").and_then(Value::as_str))
}

/// Run one tool call through the scheme dispatch table.
///
/// Returns `None` when the call does not address a scheme this view resolves,
/// leaving the wrapped tool — and therefore every historical behavior —
/// untouched. A registered read dispatches the owning tool with the full
/// handle text as its `reference` argument; a registered but non-writable
/// scheme fails the call with the same read-only diagnostic a write to
/// `artifact://` produces.
async fn try_scheme_dispatch(
    dispatch: &SchemeDispatch,
    ctx: &ToolCtx,
    input: &Value,
    writing: bool,
) -> Option<Result<Value, ToolError>> {
    let text = addressed_text(input)?;
    let scheme = external_scheme_of(text)?;
    let handler = dispatch.lookup(scheme)?;
    if writing && !handler.binding().writable() {
        return Some(Err(ToolError::Input(
            HandleError::SchemeNotWritable(scheme.to_string()).to_string(),
        )));
    }
    if ctx.cancel.is_cancelled() {
        return Some(Err(ToolError::Cancelled));
    }
    let result = handler
        .tool()
        .execute(ctx, json!({ "reference": text }))
        .await;
    Some(result)
}

/// Read decorator a compiled view installs when that view resolves one or more
/// external schemes: registered schemes dispatch to their owning tool through
/// the same call context, everything else flows to the wrapped read unchanged.
///
/// The decorator keeps the wrapped tool's name, schema, and result policy, so
/// `read` itself is never replaced — it remains the single entry point every
/// handle goes through.
pub struct SchemeReadTool {
    inner: Arc<dyn Tool>,
    dispatch: SchemeDispatch,
}

impl SchemeReadTool {
    /// Wrap `inner` (the view's read tool) with `dispatch`.
    #[must_use]
    pub fn new(inner: Arc<dyn Tool>, dispatch: SchemeDispatch) -> Self {
        Self { inner, dispatch }
    }
}

#[async_trait]
impl Tool for SchemeReadTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn schema(&self) -> ToolSchema {
        self.inner.schema()
    }

    fn result_policy(&self) -> ToolResultPolicy {
        self.inner.result_policy()
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        match try_scheme_dispatch(&self.dispatch, ctx, &input, false).await {
            Some(result) => result,
            None => self.inner.execute(ctx, input).await,
        }
    }
}

/// Write decorator matching [`SchemeReadTool`]: only schemes bound with
/// `writable: true` accept write dispatch; others fail with the read-only
/// diagnostic, and everything else flows to the wrapped write unchanged.
pub struct SchemeWriteTool {
    inner: Arc<dyn Tool>,
    dispatch: SchemeDispatch,
}

impl SchemeWriteTool {
    /// Wrap `inner` (the view's write tool) with `dispatch`.
    #[must_use]
    pub fn new(inner: Arc<dyn Tool>, dispatch: SchemeDispatch) -> Self {
        Self { inner, dispatch }
    }
}

#[async_trait]
impl Tool for SchemeWriteTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn schema(&self) -> ToolSchema {
        self.inner.schema()
    }

    fn result_policy(&self) -> ToolResultPolicy {
        self.inner.result_policy()
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        match try_scheme_dispatch(&self.dispatch, ctx, &input, true).await {
            Some(result) => result,
            None => self.inner.execute(ctx, input).await,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn external_scheme_of_skips_internal_families_and_non_handles() {
        assert_eq!(external_scheme_of("db://x/y"), Some("db"));
        assert_eq!(external_scheme_of("artifact://x"), None);
        assert_eq!(external_scheme_of("skill://x"), None);
        assert_eq!(external_scheme_of("local://x"), None);
        assert_eq!(external_scheme_of("plain.txt"), None);
    }

    #[test]
    fn dispatch_lookup_only_answers_present_schemes() {
        let dispatch = SchemeDispatch::new(BTreeMap::from([(
            "db".to_string(),
            SchemeHandler::new(
                SchemeBinding::new("plugin:vecdb", "vecdb__query", false),
                Arc::new(StubTool),
            ),
        )]));
        assert!(!dispatch.is_empty());
        assert!(dispatch.lookup("db").is_some());
        assert!(dispatch.lookup("kv").is_none());
    }

    struct StubTool;

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            "stub__tool"
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: hya_proto::ToolName::new("stub__tool"),
                description: String::new(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
            }
        }

        async fn execute(&self, _ctx: &ToolCtx, _input: Value) -> Result<Value, ToolError> {
            Ok(json!({}))
        }
    }
}
