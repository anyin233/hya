//! Allow/ask/deny authorization for tool invocations and resource access.
//!
//! Two layers cooperate:
//! 1. **Invocation policy** — regex rules over tool / MCP / Bash command subjects
//!    evaluated once before execution ([`InvocationPolicy`]).
//! 2. **Resource rules** — action + wildcard pattern last-match-wins rules used
//!    by tools mid-execution ([`PermissionRules`] / [`PermissionPlane::assert`]).
//!
//! Call-scoped grants from a successful invocation authorize later resource
//! checks **except** [`Action::ExternalDirectory`], which always re-evaluates.

use std::collections::HashSet;
use std::sync::Arc;

// allow: SIZE_OK - permission rules, async asks, and tests already share one module.
use hya_proto::{MessageId, PermissionRequestId, SessionId, ToolCallId};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Mutex, mpsc, oneshot};

/// Resource operation category used by legacy rules and user-facing asks.
///
/// Serialized lowercase in saved-permission rows (`read`, `edit`, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Invocation-level native tool subject (generic tool call).
    Tool,
    /// Read file/directory contents (`read`, `ls`).
    Read,
    /// Mutate file contents (`write`, `edit`, `apply_patch`).
    Edit,
    /// Path pattern search (`glob`, `find`).
    Glob,
    /// Content regex search (`grep`).
    Grep,
    /// Bash command execution (`bash`).
    Bash,
    /// Subagent spawn (`task`).
    Task,
    /// MCP tool call (`mcp__…`).
    Mcp,
    /// HTTP(S) fetch (`webfetch`).
    WebFetch,
    /// Provider-backed web search (`websearch`).
    WebSearch,
    /// Session todo list replace (`todowrite`).
    TodoWrite,
    /// Load a skill by name (`skill`).
    Skill,
    /// Language-server query (`lsp`).
    Lsp,
    /// Path outside the session workdir trust boundary (never covered by call grant).
    ExternalDirectory,
}

/// Object of a resource-level permission check.
///
/// Flattened via [`Resource::pattern`] for `*` wildcard matching.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    /// Tool name for invocation-level tool subjects.
    Tool(String),
    /// Resolved filesystem path (display form).
    Path(String),
    /// Glob or grep pattern string.
    Glob(String),
    /// Shell command text, or namespaced MCP tool name.
    Command(String),
    /// Subagent / agent type id.
    Subagent(String),
    /// Fetched URL.
    Url(String),
    /// Web-search query string.
    WebSearch(String),
    /// Skill name.
    Skill(String),
    /// Matches every pattern (`pattern()` → `"*"`); used for whole-action grants.
    Any,
}

impl Resource {
    /// Single string used by the `*` wildcard rule matcher.
    #[must_use]
    pub fn pattern(&self) -> String {
        match self {
            Resource::Tool(s)
            | Resource::Path(s)
            | Resource::Glob(s)
            | Resource::Command(s)
            | Resource::Subagent(s)
            | Resource::Url(s)
            | Resource::WebSearch(s)
            | Resource::Skill(s) => s.clone(),
            Resource::Any => "*".to_string(),
        }
    }
}

/// Outcome of evaluating a permission rule for a subject or resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Proceed without prompting.
    Allow,
    /// Prompt the user (or interceptor) unless a remembered grant applies.
    Ask,
    /// Hard reject; authoritative even after invocation approval when explicit.
    Deny,
}

/// Invocation-layer global model controlling how unmapped tools behave.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionModel {
    /// Auto-approve remaining resource checks (explicit deny still wins).
    Allow,
    /// Use last matching rule and tool-class fallback (read-only allow, else ask).
    #[default]
    Default,
    /// Ask unless deny or an exact remembered grant exists.
    Strict,
    /// Bypass invocation and resource checks entirely (including deny).
    Danger,
}

/// Domain of an invocation subject matched by regex rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionTarget {
    /// Canonical or plugin tool name.
    Tool,
    /// Namespaced MCP tool name.
    Mcp,
    /// Full Bash command string after before-hooks.
    Command,
}

/// One ordered invocation regex rule before compilation into [`InvocationPolicy`].
#[derive(Clone, Debug)]
pub struct InvocationRule {
    /// Which subject domain this selector applies to.
    pub target: PermissionTarget,
    /// Rust regular expression over the subject value.
    pub selector: String,
    /// Mode when this rule is the last match.
    pub permission: Mode,
}

impl InvocationRule {
    /// Construct an uncompiled rule.
    #[must_use]
    pub fn new(target: PermissionTarget, selector: impl Into<String>, permission: Mode) -> Self {
        Self {
            target,
            selector: selector.into(),
            permission,
        }
    }
}

/// Exact invocation subject used for native "allow always" grants.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExactSubject {
    /// Domain of the subject.
    pub target: PermissionTarget,
    /// Concrete name or command string.
    pub value: String,
}

impl ExactSubject {
    /// Build an exact subject for grant storage and ask coalescing.
    #[must_use]
    pub fn new(target: PermissionTarget, value: impl Into<String>) -> Self {
        Self {
            target,
            value: value.into(),
        }
    }

    fn permission(&self) -> (Action, Resource) {
        match self.target {
            PermissionTarget::Tool => (Action::Tool, Resource::Tool(self.value.clone())),
            PermissionTarget::Mcp => (Action::Mcp, Resource::Command(self.value.clone())),
            PermissionTarget::Command => (Action::Bash, Resource::Command(self.value.clone())),
        }
    }
}

/// Subjects presented to the invocation evaluator for one tool call.
#[derive(Clone, Debug)]
pub struct Invocation {
    subjects: Vec<ExactSubject>,
    primary: ExactSubject,
    fallback: Mode,
}

impl Invocation {
    /// Single tool subject with the given classification fallback mode.
    #[must_use]
    pub fn tool(name: impl Into<String>, fallback: Mode) -> Self {
        let primary = ExactSubject::new(PermissionTarget::Tool, name);
        Self {
            subjects: vec![primary.clone()],
            primary,
            fallback,
        }
    }

    /// MCP tool subject; fallback is always ask.
    #[must_use]
    pub fn mcp(name: impl Into<String>) -> Self {
        let primary = ExactSubject::new(PermissionTarget::Mcp, name);
        Self {
            subjects: vec![primary.clone()],
            primary,
            fallback: Mode::Ask,
        }
    }

    /// Bash invocation: matches both tool name and full command string.
    #[must_use]
    pub fn command(tool: impl Into<String>, command: impl Into<String>) -> Self {
        let primary = ExactSubject::new(PermissionTarget::Command, command);
        Self {
            subjects: vec![
                ExactSubject::new(PermissionTarget::Tool, tool),
                primary.clone(),
            ],
            primary,
            fallback: Mode::Ask,
        }
    }
}

/// Result of evaluating an [`Invocation`] under an [`InvocationPolicy`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationDecision {
    /// Allow, ask, or deny for this call.
    pub mode: Mode,
    /// Subject that determined the decision (for exact grants).
    pub subject: ExactSubject,
}

#[derive(Clone, Debug)]
struct CompiledInvocationRule {
    target: PermissionTarget,
    selector: Regex,
    permission: Mode,
}

/// Compiled invocation rules plus the active [`PermissionModel`].
#[derive(Clone, Debug)]
pub struct InvocationPolicy {
    model: PermissionModel,
    rules: Vec<CompiledInvocationRule>,
}

impl Default for InvocationPolicy {
    fn default() -> Self {
        Self {
            model: PermissionModel::Default,
            rules: Vec::new(),
        }
    }
}

impl InvocationPolicy {
    /// Compile selector strings into regexes for evaluation.
    ///
    /// # Errors
    /// Returns an error when a configured selector is not a valid regular expression.
    pub fn compile(
        model: PermissionModel,
        rules: Vec<InvocationRule>,
    ) -> Result<Self, regex::Error> {
        let rules = rules
            .into_iter()
            .map(|rule| {
                Ok(CompiledInvocationRule {
                    target: rule.target,
                    selector: Regex::new(&rule.selector)?,
                    permission: rule.permission,
                })
            })
            .collect::<Result<Vec<_>, regex::Error>>()?;
        Ok(Self { model, rules })
    }

    /// Active global model for unmapped subjects.
    #[must_use]
    pub fn model(&self) -> PermissionModel {
        self.model
    }

    /// Replace the global model without recompiling rules.
    #[must_use]
    pub fn with_model(mut self, model: PermissionModel) -> Self {
        self.model = model;
        self
    }

    /// Evaluate ordered regex rules under the configured model.
    #[must_use]
    pub fn evaluate(&self, invocation: &Invocation) -> InvocationDecision {
        let mut matches = self.rules.iter().filter_map(|rule| {
            invocation
                .subjects
                .iter()
                .find(|subject| {
                    subject.target == rule.target && rule.selector.is_match(&subject.value)
                })
                .map(|subject| (rule.permission, subject))
        });

        match self.model {
            PermissionModel::Allow => matches.find(|(mode, _)| *mode == Mode::Deny).map_or_else(
                || InvocationDecision {
                    mode: Mode::Allow,
                    subject: invocation.primary.clone(),
                },
                |(_, subject)| InvocationDecision {
                    mode: Mode::Deny,
                    subject: subject.clone(),
                },
            ),
            PermissionModel::Default => matches.next_back().map_or_else(
                || InvocationDecision {
                    mode: invocation.fallback,
                    subject: invocation.primary.clone(),
                },
                |(mode, subject)| InvocationDecision {
                    mode,
                    subject: subject.clone(),
                },
            ),
            PermissionModel::Strict => matches.find(|(mode, _)| *mode == Mode::Deny).map_or_else(
                || InvocationDecision {
                    mode: Mode::Ask,
                    subject: invocation.primary.clone(),
                },
                |(_, subject)| InvocationDecision {
                    mode: Mode::Deny,
                    subject: subject.clone(),
                },
            ),
            PermissionModel::Danger => InvocationDecision {
                mode: Mode::Allow,
                subject: invocation.primary.clone(),
            },
        }
    }
}

/// User or interceptor answer to an ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Permit only this call.
    AllowOnce,
    /// Permit and remember according to [`RememberScope`].
    AllowAlways,
    /// Deny, optionally recording human feedback for the model.
    Reject {
        /// Optional free-text reason shown to the agent.
        feedback: Option<String>,
    },
}

/// One resource-level allow/ask/deny entry (last match wins per action).
#[derive(Clone, Debug)]
pub struct Rule {
    /// Action this rule applies to.
    pub action: Action,
    /// `*` wildcard pattern compared to [`Resource::pattern`].
    pub resource_pattern: String,
    /// Mode when this rule is the last matching entry.
    pub mode: Mode,
}

impl Rule {
    /// Build a resource rule.
    #[must_use]
    pub fn new(action: Action, resource_pattern: impl Into<String>, mode: Mode) -> Self {
        Self {
            action,
            resource_pattern: resource_pattern.into(),
            mode,
        }
    }
}

/// Ordered list of resource rules used by snapshot and persistent planes.
#[derive(Clone, Debug, Default)]
pub struct PermissionRules {
    /// Rules evaluated last-match-wins for a given action.
    pub rules: Vec<Rule>,
}

impl PermissionRules {
    /// Wrap an ordered rule list.
    #[must_use]
    pub fn new(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

    /// Evaluate all matching rules for `action`/`resource`; default is ask.
    #[must_use]
    pub fn evaluate(&self, action: Action, resource: &Resource) -> Mode {
        let target = resource.pattern();
        let mut mode = Mode::Ask;
        for rule in &self.rules {
            if rule.action == action && glob_match(&rule.resource_pattern, &target) {
                mode = rule.mode;
            }
        }
        mode
    }

    /// Clone rules and append `extra` for a turn-scoped overlay.
    #[must_use]
    pub fn derive_child(&self, extra: Vec<Rule>) -> PermissionRules {
        let mut rules = self.rules.clone();
        rules.extend(extra);
        PermissionRules { rules }
    }
}

/// `*` wildcard match (two-pointer, greedy backtrack). Used for permission
/// resource patterns like `git *`, `/tmp/*`, `*.rs`.
#[must_use]
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Denial or infrastructure failure from the permission plane.
#[derive(Error, Debug)]
pub enum PermissionError {
    /// Explicit deny or user reject.
    #[error("permission denied: {action:?} on {resource:?}{}", .feedback.as_deref().map_or(String::new(), |f| format!(" — user says: {f}")))]
    Denied {
        /// Action that was refused.
        action: Action,
        /// Resource that was refused.
        resource: Resource,
        /// Optional human feedback from a reject decision.
        feedback: Option<String>,
    },
    /// Ask channel closed or dropped before a reply arrived.
    #[error("permission channel unavailable")]
    Unavailable,
}

/// Outstanding ask delivered to the TUI/CLI for a user decision.
pub struct AskRequest {
    /// Correlation id for this permission prompt.
    pub id: PermissionRequestId,
    /// Session that triggered the ask, when known.
    pub session: Option<SessionId>,
    /// Message id of the tool-bearing assistant turn, when known.
    pub message_id: Option<MessageId>,
    /// Tool-call id correlation, when known.
    pub call_id: Option<ToolCallId>,
    /// Action under review.
    pub action: Action,
    /// Resource under review.
    pub resource: Resource,
    /// How an allow-always reply will be remembered.
    pub remember: RememberScope,
    /// Oneshot for the caller's decision.
    pub reply: oneshot::Sender<Decision>,
}

/// How an allow-always decision is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RememberScope {
    /// Legacy resource grant: store `Rule(action, "*", Allow)` for the whole action.
    LegacyAction,
    /// Native invocation grant: remember only the exact subject.
    Exact(ExactSubject),
}

impl RememberScope {
    /// Pattern string for diagnostics (`"*"` or the exact subject value).
    #[must_use]
    pub fn pattern(&self) -> &str {
        match self {
            Self::LegacyAction => "*",
            Self::Exact(subject) => &subject.value,
        }
    }
}

/// Optional async hook consulted after remembered grants and before the user ask.
///
/// Used by the plugin permission bridge. Returning `Some` short-circuits the
/// interactive prompt; `None` defers to the host ask channel.
#[async_trait::async_trait]
pub trait PermissionInterceptor: Send + Sync {
    /// Optional digest mixed into [`PermissionPlane::semantic_identity_v1`].
    #[must_use]
    fn semantic_identity_v1(&self) -> Option<[u8; 32]> {
        None
    }

    /// Decide for this action/resource, or defer with `None`.
    async fn intercept(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision>;
}

/// Session-scoped permission state: rules, grants, policy, interceptor, and ask channel.
#[derive(Clone)]
pub struct PermissionPlane {
    snapshot: Arc<PermissionRules>,
    persistent: Arc<Mutex<PermissionRules>>,
    invocation_policy: Option<Arc<InvocationPolicy>>,
    native_grants: Arc<Mutex<HashSet<ExactSubject>>>,
    asks: mpsc::UnboundedSender<AskRequest>,
    session: Option<SessionId>,
    message_id: Option<MessageId>,
    call_id: Option<ToolCallId>,
    call_grant: bool,
    interceptor: Option<Arc<dyn PermissionInterceptor>>,
}

impl PermissionPlane {
    /// Create a plane with resource rules only (no invocation policy).
    #[must_use]
    pub fn new(rules: PermissionRules) -> (Self, mpsc::UnboundedReceiver<AskRequest>) {
        Self::new_inner(rules, None)
    }

    /// Create a plane with resource rules and a compiled invocation policy.
    #[must_use]
    pub fn new_with_policy(
        rules: PermissionRules,
        invocation_policy: InvocationPolicy,
    ) -> (Self, mpsc::UnboundedReceiver<AskRequest>) {
        Self::new_inner(rules, Some(invocation_policy))
    }

    fn new_inner(
        rules: PermissionRules,
        invocation_policy: Option<InvocationPolicy>,
    ) -> (Self, mpsc::UnboundedReceiver<AskRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let plane = Self {
            snapshot: Arc::new(rules.clone()),
            persistent: Arc::new(Mutex::new(rules)),
            invocation_policy: invocation_policy.map(Arc::new),
            native_grants: Arc::default(),
            asks: tx,
            session: None,
            message_id: None,
            call_id: None,
            call_grant: false,
            interceptor: None,
        };
        (plane, rx)
    }

    /// Clone of the immutable snapshot rules for this plane.
    #[must_use]
    pub fn snapshot_rules(&self) -> PermissionRules {
        self.snapshot.as_ref().clone()
    }

    /// Stable identity for immutable permission policy semantics (rules + policy + interceptor).
    #[must_use]
    pub fn semantic_identity_v1(&self) -> Option<[u8; 32]> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(PERMISSION_SEMANTIC_IDENTITY_DOMAIN_V1);
        append_permission_rules(&mut bytes, self.snapshot.as_ref())?;
        match self.invocation_policy.as_deref() {
            None => bytes.push(0),
            Some(policy) => {
                bytes.push(1);
                append_permission_model(&mut bytes, policy.model);
                append_count(&mut bytes, policy.rules.len())?;
                for rule in &policy.rules {
                    append_permission_target(&mut bytes, rule.target);
                    append_bytes(&mut bytes, rule.selector.as_str().as_bytes())?;
                    append_mode(&mut bytes, rule.permission);
                }
            }
        }
        match self.interceptor.as_deref() {
            None => bytes.push(0),
            Some(interceptor) => {
                bytes.push(1);
                append_bytes(&mut bytes, &interceptor.semantic_identity_v1()?)?;
            }
        }
        Some(Sha256::digest(bytes).into())
    }

    /// Install an interceptor (for example the plugin permission bridge).
    #[must_use]
    pub fn with_interceptor(mut self, interceptor: Arc<dyn PermissionInterceptor>) -> Self {
        self.interceptor = Some(interceptor);
        self
    }

    /// Scope asks and grants to a session id.
    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        let mut plane = self.clone();
        plane.session = Some(session);
        plane
    }

    /// Attach message/tool-call correlation for native asks.
    #[must_use]
    pub fn for_tool_call(&self, message_id: MessageId, call_id: ToolCallId) -> Self {
        let mut plane = self.clone();
        plane.message_id = Some(message_id);
        plane.call_id = Some(call_id);
        plane
    }

    /// Layer temporary snapshot rules (for example per-turn external directories).
    #[must_use]
    pub fn with_snapshot_rules(&self, extra: Vec<Rule>) -> Self {
        let mut plane = self.clone();
        if !extra.is_empty() {
            plane.snapshot = Arc::new(self.snapshot.derive_child(extra));
        }
        plane
    }

    /// Evaluate invocation policy; on success returns a call-scoped plane with grant set.
    ///
    /// # Errors
    /// Returns [`PermissionError::Denied`] or [`PermissionError::Unavailable`].
    pub async fn authorize(&self, invocation: &Invocation) -> Result<Self, PermissionError> {
        let Some(policy) = &self.invocation_policy else {
            return Ok(self.clone());
        };
        let decision = policy.evaluate(invocation);
        let (action, resource) = decision.subject.permission();
        match decision.mode {
            Mode::Allow => Ok(self.authorized()),
            Mode::Deny => Err(PermissionError::Denied {
                action,
                resource,
                feedback: None,
            }),
            Mode::Ask => {
                if self.native_grants.lock().await.contains(&decision.subject) {
                    return Ok(self.authorized());
                }
                let remember = RememberScope::Exact(decision.subject);
                if let Some(interceptor) = &self.interceptor
                    && let Some(decision) =
                        interceptor.intercept(self.session, action, &resource).await
                {
                    self.apply_decision(action, resource, remember, decision)
                        .await?;
                    return Ok(self.authorized());
                }
                self.ask(action, resource, remember).await?;
                Ok(self.authorized())
            }
        }
    }

    fn authorized(&self) -> Self {
        let mut plane = self.clone();
        plane.call_grant = true;
        plane
    }

    /// Resource-level check used by tools mid-execution.
    ///
    /// Call grants satisfy later asserts except [`Action::ExternalDirectory`].
    ///
    /// # Errors
    /// Returns [`PermissionError::Denied`] or [`PermissionError::Unavailable`].
    pub async fn assert(&self, action: Action, resource: Resource) -> Result<(), PermissionError> {
        let model = self.invocation_policy.as_ref().map(|policy| policy.model);
        // `danger` bypasses every resource check, including explicit Deny rules.
        if model == Some(PermissionModel::Danger) {
            return Ok(());
        }
        // Precedence: a snapshot Allow/Deny is authoritative. Only on Ask do we
        // consult the accumulated "allow always" rules, then fall through to the user.
        match self.snapshot.evaluate(action, &resource) {
            Mode::Allow => return Ok(()),
            Mode::Deny => {
                return Err(PermissionError::Denied {
                    action,
                    resource,
                    feedback: None,
                });
            }
            Mode::Ask => {}
        }
        // `allow` auto-approves remaining resource checks (including ExternalDirectory)
        // so `permission.model: allow` never prompts. Explicit Deny above still wins.
        if model == Some(PermissionModel::Allow) {
            return Ok(());
        }
        if self.call_grant && action != Action::ExternalDirectory {
            return Ok(());
        }
        if self.persistent.lock().await.evaluate(action, &resource) == Mode::Allow {
            return Ok(());
        }
        if let Some(interceptor) = &self.interceptor
            && let Some(decision) = interceptor.intercept(self.session, action, &resource).await
        {
            return self
                .apply_decision(action, resource, RememberScope::LegacyAction, decision)
                .await;
        }
        self.ask(action, resource, RememberScope::LegacyAction)
            .await
    }

    async fn ask(
        &self,
        action: Action,
        resource: Resource,
        remember: RememberScope,
    ) -> Result<(), PermissionError> {
        let (tx, rx) = oneshot::channel();
        let req = AskRequest {
            id: PermissionRequestId::new(),
            session: self.session,
            message_id: self.message_id,
            call_id: self.call_id,
            action,
            resource: resource.clone(),
            remember: remember.clone(),
            reply: tx,
        };
        self.asks
            .send(req)
            .map_err(|_| PermissionError::Unavailable)?;
        let decision = rx.await.map_err(|_| PermissionError::Unavailable)?;
        self.apply_decision(action, resource, remember, decision)
            .await
    }

    async fn apply_decision(
        &self,
        action: Action,
        resource: Resource,
        remember: RememberScope,
        decision: Decision,
    ) -> Result<(), PermissionError> {
        match decision {
            Decision::AllowOnce => Ok(()),
            Decision::AllowAlways => {
                match remember {
                    RememberScope::LegacyAction => {
                        self.persistent.lock().await.rules.push(Rule::new(
                            action,
                            "*",
                            Mode::Allow,
                        ));
                    }
                    RememberScope::Exact(subject) => {
                        self.native_grants.lock().await.insert(subject);
                    }
                }
                Ok(())
            }
            Decision::Reject { feedback } => Err(PermissionError::Denied {
                action,
                resource,
                feedback,
            }),
        }
    }
}

const PERMISSION_SEMANTIC_IDENTITY_DOMAIN_V1: &[u8] = b"hya.permission.semantic-identity/v1";

fn append_permission_rules(bytes: &mut Vec<u8>, rules: &PermissionRules) -> Option<()> {
    append_count(bytes, rules.rules.len())?;
    for rule in &rules.rules {
        append_action(bytes, rule.action);
        append_bytes(bytes, rule.resource_pattern.as_bytes())?;
        append_mode(bytes, rule.mode);
    }
    Some(())
}

fn append_count(bytes: &mut Vec<u8>, count: usize) -> Option<()> {
    let count = u64::try_from(count).ok()?;
    bytes.extend_from_slice(&count.to_be_bytes());
    Some(())
}

fn append_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Option<()> {
    let length = u64::try_from(value.len()).ok()?;
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(value);
    Some(())
}

fn append_action(bytes: &mut Vec<u8>, action: Action) {
    bytes.push(match action {
        Action::Tool => 0,
        Action::Read => 1,
        Action::Edit => 2,
        Action::Glob => 3,
        Action::Grep => 4,
        Action::Bash => 5,
        Action::Task => 6,
        Action::Mcp => 7,
        Action::WebFetch => 8,
        Action::WebSearch => 9,
        Action::TodoWrite => 10,
        Action::Skill => 11,
        Action::Lsp => 12,
        Action::ExternalDirectory => 13,
    });
}

fn append_mode(bytes: &mut Vec<u8>, mode: Mode) {
    bytes.push(match mode {
        Mode::Allow => 0,
        Mode::Ask => 1,
        Mode::Deny => 2,
    });
}

fn append_permission_model(bytes: &mut Vec<u8>, model: PermissionModel) {
    bytes.push(match model {
        PermissionModel::Allow => 0,
        PermissionModel::Default => 1,
        PermissionModel::Strict => 2,
        PermissionModel::Danger => 3,
    });
}

fn append_permission_target(bytes: &mut Vec<u8>, target: PermissionTarget) {
    bytes.push(match target {
        PermissionTarget::Tool => 0,
        PermissionTarget::Mcp => 1,
        PermissionTarget::Command => 2,
    });
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use hya_proto::SessionId;

    #[tokio::test]
    async fn ask_request_carries_session() {
        let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
        let session = SessionId::new();
        let scoped = plane.for_session(session);
        let task = tokio::spawn(async move {
            scoped
                .assert(Action::Bash, Resource::Command("ls".to_string()))
                .await
        });
        let req = rx.recv().await.expect("ask request");
        assert_eq!(req.session, Some(session));
        req.reply.send(Decision::AllowOnce).expect("send reply");
        task.await.expect("join").expect("assert ok");
    }

    #[tokio::test]
    async fn ask_request_carries_tool_correlation() {
        let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
        let message = MessageId::new();
        let call = ToolCallId::new();
        let scoped = plane.for_tool_call(message, call);
        let task = tokio::spawn(async move {
            scoped
                .assert(Action::Bash, Resource::Command("ls".to_string()))
                .await
        });

        let req = rx.recv().await.expect("ask request");

        assert_eq!(req.message_id, Some(message));
        assert_eq!(req.call_id, Some(call));
        req.reply.send(Decision::AllowOnce).expect("send reply");
        task.await.expect("join").expect("assert ok");
    }

    #[tokio::test]
    async fn dropped_reply_is_unavailable() {
        let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
        let task = tokio::spawn(async move {
            plane
                .assert(Action::Bash, Resource::Command("ls".to_string()))
                .await
        });
        let req = rx.recv().await.expect("ask request");
        drop(req.reply);
        let result = task.await.expect("join");
        assert!(matches!(result, Err(PermissionError::Unavailable)));
    }

    #[tokio::test]
    async fn native_allow_always_is_exact_and_deny_stays_authoritative() {
        let policy = InvocationPolicy::compile(
            PermissionModel::Default,
            vec![
                InvocationRule::new(PermissionTarget::Tool, "^write$", Mode::Ask),
                InvocationRule::new(PermissionTarget::Command, "^git status$", Mode::Ask),
                InvocationRule::new(PermissionTarget::Tool, "^blocked$", Mode::Deny),
            ],
        )
        .expect("compile policy");
        let (plane, mut rx) = PermissionPlane::new_with_policy(PermissionRules::default(), policy);

        let first = plane.clone();
        let task =
            tokio::spawn(
                async move { first.authorize(&Invocation::tool("write", Mode::Ask)).await },
            );
        let req = rx.recv().await.expect("native ask");
        assert_eq!(
            req.remember,
            RememberScope::Exact(ExactSubject::new(PermissionTarget::Tool, "write"))
        );
        req.reply.send(Decision::AllowAlways).expect("send reply");
        task.await.expect("join").expect("authorized");

        plane
            .authorize(&Invocation::tool("write", Mode::Ask))
            .await
            .expect("exact grant is remembered");
        assert!(rx.try_recv().is_err());

        let command = plane.clone();
        let task = tokio::spawn(async move {
            command
                .authorize(&Invocation::command("bash", "git status"))
                .await
        });
        let req = rx.recv().await.expect("command asks");
        req.reply.send(Decision::AllowAlways).expect("send reply");
        task.await.expect("join").expect("command authorized");

        assert!(matches!(
            plane
                .authorize(&Invocation::command("blocked", "git status"))
                .await,
            Err(PermissionError::Denied { .. })
        ));

        let other = plane.clone();
        let task = tokio::spawn(async move {
            other
                .authorize(&Invocation::tool("write_other", Mode::Ask))
                .await
        });
        let req = rx.recv().await.expect("different subject asks");
        req.reply.send(Decision::AllowOnce).expect("send reply");
        let authorized = task.await.expect("join").expect("authorized once");

        authorized
            .assert(Action::Edit, Resource::Any)
            .await
            .expect("call grant suppresses duplicate primary ask");
        let external = authorized.clone();
        let task = tokio::spawn(async move {
            external
                .assert(
                    Action::ExternalDirectory,
                    Resource::Path("/tmp/*".to_string()),
                )
                .await
        });
        let req = rx
            .recv()
            .await
            .expect("external directory remains separate");
        req.reply.send(Decision::AllowOnce).expect("send reply");
        task.await.expect("join").expect("external allowed");
    }

    #[tokio::test]
    async fn legacy_allow_always_remains_action_wide() {
        let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
        let first = plane.clone();
        let task = tokio::spawn(async move {
            first
                .assert(Action::Bash, Resource::Command("pwd".to_string()))
                .await
        });
        let req = rx.recv().await.expect("legacy ask");
        assert_eq!(req.remember, RememberScope::LegacyAction);
        req.reply.send(Decision::AllowAlways).expect("send reply");
        task.await.expect("join").expect("legacy allowed");

        plane
            .assert(Action::Bash, Resource::Command("ls".to_string()))
            .await
            .expect("legacy grant covers the action");
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn mcp_action_round_trips_as_lowercase_json() {
        let encoded = serde_json::to_string(&Action::Mcp).expect("serialize action");
        assert_eq!(encoded, "\"mcp\"");
        let decoded: Action = serde_json::from_str(&encoded).expect("deserialize action");
        assert_eq!(decoded, Action::Mcp);
    }

    #[test]
    fn invocation_policy_evaluates_models_rules_and_fallbacks() {
        let rules = vec![
            InvocationRule::new(PermissionTarget::Tool, "^bash$", Mode::Ask),
            InvocationRule::new(PermissionTarget::Command, "^git ", Mode::Allow),
            InvocationRule::new(PermissionTarget::Command, "^git push$", Mode::Deny),
        ];
        let bash = Invocation::command("bash", "git status");
        let push = Invocation::command("bash", "git push");

        let default = InvocationPolicy::compile(PermissionModel::Default, rules.clone())
            .expect("compile default policy");
        assert_eq!(default.evaluate(&bash).mode, Mode::Allow);
        assert_eq!(
            default.evaluate(&bash).subject,
            ExactSubject::new(PermissionTarget::Command, "git status")
        );
        assert_eq!(default.evaluate(&push).mode, Mode::Deny);
        assert_eq!(
            default
                .evaluate(&Invocation::tool("read", Mode::Allow))
                .mode,
            Mode::Allow
        );
        assert_eq!(
            default
                .evaluate(&Invocation::tool("task", Mode::Allow))
                .mode,
            Mode::Allow
        );
        assert_eq!(
            default
                .evaluate(&Invocation::tool("webfetch", Mode::Ask))
                .mode,
            Mode::Ask
        );

        let allow = InvocationPolicy::compile(PermissionModel::Allow, rules.clone())
            .expect("compile allow policy");
        assert_eq!(allow.evaluate(&bash).mode, Mode::Allow);
        assert_eq!(allow.evaluate(&push).mode, Mode::Deny);
        assert_eq!(
            allow.evaluate(&Invocation::mcp("mcp__github__issue")).mode,
            Mode::Allow
        );

        let strict = InvocationPolicy::compile(PermissionModel::Strict, rules.clone())
            .expect("compile strict policy");
        assert_eq!(strict.evaluate(&bash).mode, Mode::Ask);
        assert_eq!(strict.evaluate(&push).mode, Mode::Deny);

        let danger = InvocationPolicy::compile(PermissionModel::Danger, rules)
            .expect("compile danger policy");
        assert_eq!(danger.evaluate(&push).mode, Mode::Allow);

        assert!(
            InvocationPolicy::compile(
                PermissionModel::Default,
                vec![InvocationRule::new(
                    PermissionTarget::Tool,
                    "(",
                    Mode::Allow,
                )],
            )
            .is_err()
        );
    }

    struct AlwaysInterceptor(Option<Decision>);

    #[async_trait::async_trait]
    impl PermissionInterceptor for AlwaysInterceptor {
        async fn intercept(
            &self,
            _session: Option<SessionId>,
            _action: Action,
            _resource: &Resource,
        ) -> Option<Decision> {
            self.0.clone()
        }
    }

    #[tokio::test]
    async fn interceptor_short_circuits_the_ask_channel() {
        let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
        let plane = plane.with_interceptor(Arc::new(AlwaysInterceptor(Some(Decision::AllowOnce))));
        plane
            .assert(Action::Bash, Resource::Command("ls".to_string()))
            .await
            .expect("interceptor allows");
        assert!(
            rx.try_recv().is_err(),
            "ask channel must receive nothing when the interceptor answers"
        );
    }

    #[tokio::test]
    async fn interceptor_defer_falls_through_to_ask_channel() {
        let (plane, mut rx) = PermissionPlane::new(PermissionRules::default());
        let plane = plane.with_interceptor(Arc::new(AlwaysInterceptor(None)));
        let task = tokio::spawn(async move {
            plane
                .assert(Action::Bash, Resource::Command("ls".to_string()))
                .await
        });
        let req = rx.recv().await.expect("ask request after defer");
        req.reply.send(Decision::AllowOnce).expect("send reply");
        task.await.expect("join").expect("assert ok");
    }
}
