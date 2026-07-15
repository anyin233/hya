use std::collections::HashSet;
use std::sync::Arc;

// allow: SIZE_OK - permission rules, async asks, and tests already share one module.
use hya_proto::{MessageId, PermissionRequestId, SessionId, ToolCallId};
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Tool,
    Read,
    Edit,
    Glob,
    Grep,
    Bash,
    Task,
    Mcp,
    WebFetch,
    WebSearch,
    TodoWrite,
    Skill,
    Lsp,
    ExternalDirectory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    Tool(String),
    Path(String),
    Glob(String),
    Command(String),
    Subagent(String),
    Url(String),
    WebSearch(String),
    Skill(String),
    Any,
}

impl Resource {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionModel {
    Allow,
    #[default]
    Default,
    Strict,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionTarget {
    Tool,
    Mcp,
    Command,
}

#[derive(Clone, Debug)]
pub struct InvocationRule {
    pub target: PermissionTarget,
    pub selector: String,
    pub permission: Mode,
}

impl InvocationRule {
    #[must_use]
    pub fn new(target: PermissionTarget, selector: impl Into<String>, permission: Mode) -> Self {
        Self {
            target,
            selector: selector.into(),
            permission,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ExactSubject {
    pub target: PermissionTarget,
    pub value: String,
}

impl ExactSubject {
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

#[derive(Clone, Debug)]
pub struct Invocation {
    subjects: Vec<ExactSubject>,
    primary: ExactSubject,
    fallback: Mode,
}

impl Invocation {
    #[must_use]
    pub fn tool(name: impl Into<String>, fallback: Mode) -> Self {
        let primary = ExactSubject::new(PermissionTarget::Tool, name);
        Self {
            subjects: vec![primary.clone()],
            primary,
            fallback,
        }
    }

    #[must_use]
    pub fn mcp(name: impl Into<String>) -> Self {
        let primary = ExactSubject::new(PermissionTarget::Mcp, name);
        Self {
            subjects: vec![primary.clone()],
            primary,
            fallback: Mode::Ask,
        }
    }

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InvocationDecision {
    pub mode: Mode,
    pub subject: ExactSubject,
}

#[derive(Clone, Debug)]
struct CompiledInvocationRule {
    target: PermissionTarget,
    selector: Regex,
    permission: Mode,
}

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

    #[must_use]
    pub fn model(&self) -> PermissionModel {
        self.model
    }

    #[must_use]
    pub fn with_model(mut self, model: PermissionModel) -> Self {
        self.model = model;
        self
    }

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    AllowAlways,
    Reject { feedback: Option<String> },
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub action: Action,
    pub resource_pattern: String,
    pub mode: Mode,
}

impl Rule {
    #[must_use]
    pub fn new(action: Action, resource_pattern: impl Into<String>, mode: Mode) -> Self {
        Self {
            action,
            resource_pattern: resource_pattern.into(),
            mode,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PermissionRules {
    pub rules: Vec<Rule>,
}

impl PermissionRules {
    #[must_use]
    pub fn new(rules: Vec<Rule>) -> Self {
        Self { rules }
    }

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

#[derive(Error, Debug)]
pub enum PermissionError {
    #[error("permission denied: {action:?} on {resource:?}{}", .feedback.as_deref().map_or(String::new(), |f| format!(" — user says: {f}")))]
    Denied {
        action: Action,
        resource: Resource,
        feedback: Option<String>,
    },
    #[error("permission channel unavailable")]
    Unavailable,
}

pub struct AskRequest {
    pub id: PermissionRequestId,
    pub session: Option<SessionId>,
    pub message_id: Option<MessageId>,
    pub call_id: Option<ToolCallId>,
    pub action: Action,
    pub resource: Resource,
    pub remember: RememberScope,
    pub reply: oneshot::Sender<Decision>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RememberScope {
    LegacyAction,
    Exact(ExactSubject),
}

impl RememberScope {
    #[must_use]
    pub fn pattern(&self) -> &str {
        match self {
            Self::LegacyAction => "*",
            Self::Exact(subject) => &subject.value,
        }
    }
}

#[async_trait::async_trait]
pub trait PermissionInterceptor: Send + Sync {
    async fn intercept(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision>;
}

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
    #[must_use]
    pub fn new(rules: PermissionRules) -> (Self, mpsc::UnboundedReceiver<AskRequest>) {
        Self::new_inner(rules, None)
    }

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

    #[must_use]
    pub fn snapshot_rules(&self) -> PermissionRules {
        self.snapshot.as_ref().clone()
    }

    #[must_use]
    pub fn with_interceptor(mut self, interceptor: Arc<dyn PermissionInterceptor>) -> Self {
        self.interceptor = Some(interceptor);
        self
    }

    #[must_use]
    pub fn for_session(&self, session: SessionId) -> Self {
        let mut plane = self.clone();
        plane.session = Some(session);
        plane
    }

    #[must_use]
    pub fn for_tool_call(&self, message_id: MessageId, call_id: ToolCallId) -> Self {
        let mut plane = self.clone();
        plane.message_id = Some(message_id);
        plane.call_id = Some(call_id);
        plane
    }

    #[must_use]
    pub fn with_snapshot_rules(&self, extra: Vec<Rule>) -> Self {
        let mut plane = self.clone();
        if !extra.is_empty() {
            plane.snapshot = Arc::new(self.snapshot.derive_child(extra));
        }
        plane
    }

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

    pub async fn assert(&self, action: Action, resource: Resource) -> Result<(), PermissionError> {
        if self
            .invocation_policy
            .as_ref()
            .is_some_and(|policy| policy.model == PermissionModel::Danger)
        {
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
                .authorize(&Invocation::command("shell", "git status"))
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
            InvocationRule::new(PermissionTarget::Tool, "^shell$", Mode::Ask),
            InvocationRule::new(PermissionTarget::Command, "^git ", Mode::Allow),
            InvocationRule::new(PermissionTarget::Command, "^git push$", Mode::Deny),
        ];
        let shell = Invocation::command("shell", "git status");
        let push = Invocation::command("shell", "git push");

        let default = InvocationPolicy::compile(PermissionModel::Default, rules.clone())
            .expect("compile default policy");
        assert_eq!(default.evaluate(&shell).mode, Mode::Allow);
        assert_eq!(
            default.evaluate(&shell).subject,
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
        assert_eq!(allow.evaluate(&shell).mode, Mode::Allow);
        assert_eq!(allow.evaluate(&push).mode, Mode::Deny);
        assert_eq!(
            allow.evaluate(&Invocation::mcp("mcp__github__issue")).mode,
            Mode::Allow
        );

        let strict = InvocationPolicy::compile(PermissionModel::Strict, rules.clone())
            .expect("compile strict policy");
        assert_eq!(strict.evaluate(&shell).mode, Mode::Ask);
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
