//! Read-only bundle session views and the host reads behind their capability.
//!
//! A bundle with an explicit `extensions.process` may declare named views
//! (`views:` in its manifest). The published runtime source carries a
//! [`BundleViewProvider`] that forwards a view request to that process; the
//! process answers using a request-scoped, read-only host capability backed by
//! [`HostSessionReads`].
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

/// One view a bundle declares (manifest `views:`), as published.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceView {
    /// View id.
    pub id: String,
    /// Manifest description (may be empty).
    pub description: String,
}

/// Why a bundle view request failed.
#[derive(Debug, thiserror::Error)]
pub enum BundleViewError {
    /// The session id has no log.
    #[error("session not found: {0}")]
    SessionNotFound(SessionId),
    /// No published bundle with this id serves views.
    #[error("bundle `{0}` serves no views")]
    BundleNotFound(String),
    /// The bundle does not declare this view.
    #[error("bundle `{bundle}` declares no view `{view}`")]
    ViewNotFound {
        /// Bundle id.
        bundle: String,
        /// Requested view id.
        view: String,
    },
    /// The bundle process failed, timed out, or answered malformed data.
    #[error("view `{view}` of bundle `{bundle}` failed: {detail}")]
    Failed {
        /// Bundle id.
        bundle: String,
        /// View id.
        view: String,
        /// Bounded diagnostic.
        detail: String,
    },
    /// The session store failed.
    #[error(transparent)]
    Core(#[from] CoreError),
}

/// Forwards a view request to the bundle process that declared it.
///
/// **Contract:** called only for a view id the bundle declared; the provider
/// mints a request-scoped read-only capability bound to `session` and returns
/// the process's JSON body unchanged. Failures (process error, timeout,
/// malformed reply) are reported as a plain diagnostic string.
#[async_trait]
pub trait BundleViewProvider: Send + Sync {
    /// Compute view `view` for `session` with the caller's `query`.
    ///
    /// # Errors
    /// A bounded diagnostic when the process cannot answer.
    async fn get_view(
        &self,
        view: &str,
        session: SessionId,
        query: BTreeMap<String, String>,
    ) -> Result<Value, String>;
}

/// The views a published bundle source serves plus the provider that answers
/// them. Retains the source owner so an in-flight request keeps its
/// generation's process and materialized root alive across a swap.
#[derive(Clone)]
pub struct SourceViews {
    pub(crate) views: Vec<SourceView>,
    pub(crate) provider: Arc<dyn BundleViewProvider>,
    pub(crate) _owner: Option<Arc<dyn crate::RuntimeSourceOwner>>,
}

impl SourceViews {
    /// Declared views, sorted by id.
    #[must_use]
    pub fn views(&self) -> &[SourceView] {
        &self.views
    }

    /// Answer one view request through the retained provider.
    ///
    /// # Errors
    /// [`BundleViewError::ViewNotFound`] for an undeclared id, otherwise
    /// [`BundleViewError::Failed`] with the provider diagnostic.
    pub async fn get(
        &self,
        bundle: &str,
        view: &str,
        session: SessionId,
        query: BTreeMap<String, String>,
    ) -> Result<Value, BundleViewError> {
        if !self.views.iter().any(|declared| declared.id == view) {
            return Err(BundleViewError::ViewNotFound {
                bundle: bundle.to_string(),
                view: view.to_string(),
            });
        }
        self.provider
            .get_view(view, session, query)
            .await
            .map_err(|detail| BundleViewError::Failed {
                bundle: bundle.to_string(),
                view: view.to_string(),
                detail,
            })
    }
}

/// One bundle's published views, for discovery listings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedBundleViews {
    /// Bundle identity id.
    pub bundle: String,
    /// Declared views, sorted by id.
    pub views: Vec<SourceView>,
}
