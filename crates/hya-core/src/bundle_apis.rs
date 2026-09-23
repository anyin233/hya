//! Bundle-registered HTTP endpoints and the host reads behind their capability.
//!
//! A bundle with an explicit `extensions.process` may declare its own API
//! endpoints (`apis:` in its manifest): `(method, scope, path template)`
//! triples. The published runtime source carries a [`BundleApiProvider`] that
//! forwards a routed request to that process; the process answers using a
//! request-scoped, read-only host capability backed by [`HostSessionReads`].
//! Write methods (`POST`/`PUT`/`PATCH`/`DELETE`) only change state the bundle
//! process owns itself: the host capability never grants writes.
//!
//! Every read here folds the session event logs through the shared projection
//! reducer ([`hya_store::SessionStore::read_projection`]); there is no parallel
//! read model.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{
    AgentName, ModelRef, OutputSplit, SessionId, SessionUsage, UsagePurpose, UsageTotals,
};
use hya_store::SessionStore;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use hya_bundle::{ApiMethod, ApiPathTemplate, ApiScope};

use crate::error::CoreError;

/// Upper bound on sessions folded into one usage report (cycle/runaway guard).
pub const MAX_USAGE_REPORT_SESSIONS: usize = 512;

/// Which sessions a `session.usage` report covers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageScope {
    /// Only the bound session's own log.
    Session,
    /// The bound session plus every descendant subagent session (recursive).
    #[default]
    Tree,
    /// The whole spawn tree of the bound session's lineage root.
    Root,
}

/// Usage totals with the output split into thinking / visible / unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageTotalsReport {
    /// The folded counters (`input`, `cache_read`, `cache_write`, `output`,
    /// `reasoning`, `reasoning_unknown_output`, `rounds`, `legacy_messages`).
    #[serde(flatten)]
    pub totals: UsageTotals,
    /// `output` split per [`UsageTotals::output_split`];
    /// `thinking + visible + unknown == output`.
    pub split: OutputSplit,
}

impl From<UsageTotals> for UsageTotalsReport {
    fn from(totals: UsageTotals) -> Self {
        Self {
            totals,
            split: totals.output_split(),
        }
    }
}

/// One session's (or a merged tree's) usage in report form: the
/// [`SessionUsage`] maps (always present, possibly empty) plus a grand total.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UsageReport {
    /// Totals keyed by serving model (`unattributed` for legacy usage).
    pub by_model: BTreeMap<ModelRef, UsageTotalsReport>,
    /// Totals keyed by call purpose.
    pub by_purpose: BTreeMap<UsagePurpose, UsageTotalsReport>,
    /// Sum over every model.
    pub total: UsageTotalsReport,
}

impl From<&SessionUsage> for UsageReport {
    fn from(usage: &SessionUsage) -> Self {
        Self {
            by_model: usage
                .by_model
                .iter()
                .map(|(model, totals)| (model.clone(), UsageTotalsReport::from(*totals)))
                .collect(),
            by_purpose: usage
                .by_purpose
                .iter()
                .map(|(purpose, totals)| (*purpose, UsageTotalsReport::from(*totals)))
                .collect(),
            total: UsageTotalsReport::from(usage.total()),
        }
    }
}

/// One session row of a usage report.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionUsageRow {
    /// The session.
    pub session: SessionId,
    /// Its parent session, when it is a subagent/child session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    /// Agent bound to the session, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentName>,
    /// This session's own usage (its log only).
    pub usage: UsageReport,
}

/// Result of the `session.usage` host capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionUsageReport {
    /// The session the capability is bound to.
    pub session: SessionId,
    /// Requested scope.
    pub scope: UsageScope,
    /// Root of the reported set (`session` unless `scope` is `root`).
    pub root: SessionId,
    /// Per-session rows in breadth-first spawn order, root first.
    pub sessions: Vec<SessionUsageRow>,
    /// Every row merged.
    pub total: UsageReport,
    /// True when the tree exceeded [`MAX_USAGE_REPORT_SESSIONS`] and was cut.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
}

/// Read-only host services a request-scoped bundle capability may use.
///
/// Implementations must only read; they never append events.
#[async_trait]
pub trait HostSessionReads: Send + Sync {
    /// Fold per-session and total usage for `session` under `scope`.
    ///
    /// # Errors
    /// Store failures, or [`CoreError::Invalid`] when `session` has no log.
    async fn session_usage(
        &self,
        session: SessionId,
        scope: UsageScope,
    ) -> Result<SessionUsageReport, CoreError>;
}

/// [`HostSessionReads`] over the durable session store.
#[derive(Clone)]
pub struct StoreSessionReads {
    store: SessionStore,
}

impl StoreSessionReads {
    /// Read through `store`.
    #[must_use]
    pub fn new(store: SessionStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl HostSessionReads for StoreSessionReads {
    async fn session_usage(
        &self,
        session: SessionId,
        scope: UsageScope,
    ) -> Result<SessionUsageReport, CoreError> {
        session_usage_report(&self.store, session, scope).await
    }
}

/// Fold a usage report for `session` from the replayed projections.
///
/// `tree` walks `members[].child` spawn edges recorded on each parent log,
/// breadth-first, visiting each session once; `root` first walks the
/// `SessionCreated{parent}` chain to the lineage root.
///
/// # Errors
/// Store failures, or [`CoreError::Invalid`] when `session` has no log.
pub async fn session_usage_report(
    store: &SessionStore,
    session: SessionId,
    scope: UsageScope,
) -> Result<SessionUsageReport, CoreError> {
    if !store.session_exists(session).await? {
        return Err(CoreError::Invalid(format!("unknown session {session}")));
    }
    let root = match scope {
        UsageScope::Root => lineage_root(store, session).await?,
        UsageScope::Session | UsageScope::Tree => session,
    };
    let mut queue = VecDeque::from([root]);
    let mut seen = BTreeSet::from([root]);
    let mut sessions = Vec::new();
    let mut merged = SessionUsage::default();
    let mut truncated = false;
    while let Some(current) = queue.pop_front() {
        if sessions.len() >= MAX_USAGE_REPORT_SESSIONS {
            truncated = true;
            break;
        }
        let projection = store.read_projection(current).await?;
        let state = projection.session;
        if scope != UsageScope::Session {
            for child in state.members.iter().filter_map(|member| member.child) {
                if seen.insert(child) {
                    queue.push_back(child);
                }
            }
        }
        merged.merge(&state.usage);
        sessions.push(SessionUsageRow {
            session: current,
            parent: state.parent,
            agent: state.agent,
            usage: UsageReport::from(&state.usage),
        });
    }
    Ok(SessionUsageReport {
        session,
        scope,
        root,
        sessions,
        total: UsageReport::from(&merged),
        truncated,
    })
}

async fn lineage_root(store: &SessionStore, session: SessionId) -> Result<SessionId, CoreError> {
    let mut current = session;
    let mut seen = BTreeSet::from([session]);
    for _ in 0..MAX_USAGE_REPORT_SESSIONS {
        match store.read_projection(current).await?.session.parent {
            Some(parent) if seen.insert(parent) => current = parent,
            _ => break,
        }
    }
    Ok(current)
}

/// Maximum serialized size of one bundle API request body (512 KiB).
///
/// The body travels inside one `api/request` JSON-RPC frame, and the plugin
/// stdio transport caps a frame at 1 MiB; half of that leaves room for the
/// request envelope (path, parameters, query) and JSON escaping.
pub const MAX_BUNDLE_API_BODY_BYTES: usize = 512 * 1024;

/// One endpoint a bundle declares (manifest `apis:`), as published.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceApi {
    /// Endpoint id.
    pub id: String,
    /// HTTP method.
    pub method: ApiMethod,
    /// Mount scope.
    pub scope: ApiScope,
    /// Path template below the bundle mount.
    pub path: ApiPathTemplate,
    /// Manifest description (may be empty).
    pub description: String,
    /// Parsed request-body JSON Schema, when declared.
    pub request_schema: Option<Value>,
    /// Parsed response-body JSON Schema, when declared.
    pub response_schema: Option<Value>,
}

/// A bundle API call as a transport receives it, before routing.
#[derive(Clone, Debug, PartialEq)]
pub struct BundleApiCall {
    /// Bundle identity id.
    pub bundle: String,
    /// Requested HTTP method.
    pub method: ApiMethod,
    /// The session for a session-scoped call; `None` for a global call.
    pub session: Option<SessionId>,
    /// Concrete request path below the bundle mount (`/items/a%2Fb`),
    /// percent-encoding preserved.
    pub path: String,
    /// Query parameters.
    pub query: BTreeMap<String, String>,
    /// JSON request body; `Value::Null` when the request carried none.
    pub body: Value,
}

impl BundleApiCall {
    /// Scope implied by the presence of a session.
    #[must_use]
    pub fn scope(&self) -> ApiScope {
        if self.session.is_some() {
            ApiScope::Session
        } else {
            ApiScope::Global
        }
    }
}

/// A call routed to one declared endpoint; what the provider forwards.
#[derive(Clone, Debug, PartialEq)]
pub struct BundleApiRequest {
    /// Matched endpoint id.
    pub api: String,
    /// HTTP method.
    pub method: ApiMethod,
    /// Concrete request path below the bundle mount, percent-encoding
    /// preserved.
    pub path: String,
    /// Template parameters bound by the match, percent-decoded.
    pub path_params: BTreeMap<String, String>,
    /// Query parameters.
    pub query: BTreeMap<String, String>,
    /// JSON request body (`Value::Null` when absent).
    pub body: Value,
    /// Bound session (session scope only).
    pub session: Option<SessionId>,
}

/// The process's answer to one routed request.
#[derive(Clone, Debug, PartialEq)]
pub struct BundleApiReply {
    /// HTTP status in `200..=599`.
    pub status: u16,
    /// JSON body (`Value::Null` for no body).
    pub body: Value,
}

/// A served bundle API call: the matched endpoint plus the process's answer.
#[derive(Clone, Debug, PartialEq)]
pub struct BundleApiOutcome {
    /// Matched endpoint id.
    pub api: String,
    /// HTTP status in `200..=599`.
    pub status: u16,
    /// JSON body (`Value::Null` for no body).
    pub body: Value,
}

/// Why a bundle API call failed before or while reaching the process.
#[derive(Debug, thiserror::Error)]
pub enum BundleApiError {
    /// The session of a session-scoped call has no log.
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    /// Unknown bundle, or no endpoint of the scope matches the path under
    /// any method.
    #[error("bundle `{bundle}` serves no {scope} API endpoint at `{path}`")]
    NotFound {
        /// Bundle id.
        bundle: String,
        /// Requested scope.
        scope: ApiScope,
        /// Requested path.
        path: String,
    },
    /// The path matches endpoints of the scope, but none for this method.
    #[error(
        "bundle `{bundle}` endpoint `{path}` does not allow {method} (allowed: {})",
        allow.iter().map(|method| method.as_str()).collect::<Vec<_>>().join(", ")
    )]
    MethodNotAllowed {
        /// Bundle id.
        bundle: String,
        /// Requested path.
        path: String,
        /// Requested method.
        method: ApiMethod,
        /// Methods that do match the path, in canonical order.
        allow: Vec<ApiMethod>,
    },
    /// The call itself is malformed (path escape, oversized body).
    #[error("bad bundle API request: {0}")]
    BadRequest(String),
    /// The bundle process failed, timed out, or answered malformed data.
    #[error("API endpoint `{api}` of bundle `{bundle}` failed: {detail}")]
    Failed {
        /// Bundle id.
        bundle: String,
        /// Endpoint id.
        api: String,
        /// Bounded diagnostic.
        detail: String,
    },
    /// The session store failed.
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// Forwards a routed API request to the bundle process that declared it.
///
/// **Contract:** called only for an endpoint id the bundle declared, after
/// routing; the provider mints a request-scoped read-only capability (bound
/// to `request.session` for a session-scoped endpoint, to no session for a
/// global one) and returns the process's status and JSON body. Failures
/// (process error, timeout, malformed reply) are a plain diagnostic string.
#[async_trait]
pub trait BundleApiProvider: Send + Sync {
    /// Answer one routed request.
    ///
    /// # Errors
    /// A bounded diagnostic when the process cannot answer.
    async fn request(&self, request: BundleApiRequest) -> Result<BundleApiReply, String>;
}

/// The endpoints a published bundle source serves plus the provider that
/// answers them. Retains the source owner so an in-flight request keeps its
/// generation's process and materialized root alive across a swap.
#[derive(Clone)]
pub struct SourceApis {
    pub(crate) apis: Vec<SourceApi>,
    pub(crate) provider: Arc<dyn BundleApiProvider>,
    pub(crate) _owner: Option<Arc<dyn crate::RuntimeSourceOwner>>,
}

impl SourceApis {
    /// Declared endpoints, sorted by id.
    #[must_use]
    pub fn apis(&self) -> &[SourceApi] {
        &self.apis
    }

    /// Resolve `path` (percent-decoded `segments`) under `method` and `scope`.
    ///
    /// Prepare rejects overlapping templates per method and scope, so at most
    /// one endpoint matches.
    ///
    /// # Errors
    /// [`BundleApiError::MethodNotAllowed`] when other methods match the
    /// path, otherwise [`BundleApiError::NotFound`].
    pub fn route(
        &self,
        bundle: &str,
        method: ApiMethod,
        scope: ApiScope,
        path: &str,
        segments: &[String],
    ) -> Result<(&SourceApi, BTreeMap<String, String>), BundleApiError> {
        let mut allow = BTreeSet::new();
        for api in self.apis.iter().filter(|api| api.scope == scope) {
            if let Some(params) = api.path.match_segments(segments) {
                if api.method == method {
                    return Ok((api, params));
                }
                allow.insert(api.method);
            }
        }
        if allow.is_empty() {
            Err(BundleApiError::NotFound {
                bundle: bundle.to_string(),
                scope,
                path: path.to_string(),
            })
        } else {
            Err(BundleApiError::MethodNotAllowed {
                bundle: bundle.to_string(),
                path: path.to_string(),
                method,
                allow: allow.into_iter().collect(),
            })
        }
    }

    /// Route `call` and forward it through the retained provider.
    ///
    /// # Errors
    /// [`BundleApiError::BadRequest`] for a malformed path or a body over
    /// [`MAX_BUNDLE_API_BODY_BYTES`], the routing errors of [`Self::route`],
    /// or [`BundleApiError::Failed`] with the provider diagnostic (including
    /// a status outside `200..=599` or a body on a `204`/`205`/`304`).
    pub async fn invoke(&self, call: BundleApiCall) -> Result<BundleApiOutcome, BundleApiError> {
        let segments =
            hya_bundle::split_request_path(&call.path).map_err(BundleApiError::BadRequest)?;
        if !call.body.is_null() {
            let size = serde_json::to_vec(&call.body)
                .map_err(|error| BundleApiError::BadRequest(error.to_string()))?
                .len();
            if size > MAX_BUNDLE_API_BODY_BYTES {
                return Err(BundleApiError::BadRequest(format!(
                    "request body is {size} bytes; at most {MAX_BUNDLE_API_BODY_BYTES} are accepted"
                )));
            }
        }
        let scope = call.scope();
        let (api, path_params) =
            self.route(&call.bundle, call.method, scope, &call.path, &segments)?;
        let api_id = api.id.clone();
        let failed = |detail: String| BundleApiError::Failed {
            bundle: call.bundle.clone(),
            api: api_id.clone(),
            detail,
        };
        let reply = self
            .provider
            .request(BundleApiRequest {
                api: api.id.clone(),
                method: call.method,
                path: call.path.clone(),
                path_params,
                query: call.query.clone(),
                body: call.body.clone(),
                session: call.session,
            })
            .await
            .map_err(failed)?;
        if !(200..=599).contains(&reply.status) {
            return Err(failed(format!(
                "status {} is outside 200..=599",
                reply.status
            )));
        }
        if matches!(reply.status, 204 | 205 | 304) && !reply.body.is_null() {
            return Err(failed(format!(
                "status {} must not carry a body",
                reply.status
            )));
        }
        Ok(BundleApiOutcome {
            api: api_id,
            status: reply.status,
            body: reply.body,
        })
    }
}

/// One bundle's published endpoints, for discovery listings.
#[derive(Clone, Debug, PartialEq)]
pub struct PublishedBundleApis {
    /// Bundle identity id.
    pub bundle: String,
    /// Declared endpoints, sorted by id.
    pub apis: Vec<SourceApi>,
}
