use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{Mutex, mpsc, oneshot};
use yaca_proto::{PermissionRequestId, SessionId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
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
            Resource::Path(s)
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
    pub action: Action,
    pub resource: Resource,
    pub reply: oneshot::Sender<Decision>,
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
    asks: mpsc::UnboundedSender<AskRequest>,
    session: Option<SessionId>,
    interceptor: Option<Arc<dyn PermissionInterceptor>>,
}

impl PermissionPlane {
    #[must_use]
    pub fn new(rules: PermissionRules) -> (Self, mpsc::UnboundedReceiver<AskRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let plane = Self {
            snapshot: Arc::new(rules.clone()),
            persistent: Arc::new(Mutex::new(rules)),
            asks: tx,
            session: None,
            interceptor: None,
        };
        (plane, rx)
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

    pub async fn assert(&self, action: Action, resource: Resource) -> Result<(), PermissionError> {
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
        if self.persistent.lock().await.evaluate(action, &resource) == Mode::Allow {
            return Ok(());
        }
        if let Some(interceptor) = &self.interceptor
            && let Some(decision) = interceptor.intercept(self.session, action, &resource).await
        {
            return self.apply_decision(action, resource, decision).await;
        }
        let (tx, rx) = oneshot::channel();
        let req = AskRequest {
            id: PermissionRequestId::new(),
            session: self.session,
            action,
            resource: resource.clone(),
            reply: tx,
        };
        self.asks
            .send(req)
            .map_err(|_| PermissionError::Unavailable)?;
        let decision = rx.await.map_err(|_| PermissionError::Unavailable)?;
        self.apply_decision(action, resource, decision).await
    }

    async fn apply_decision(
        &self,
        action: Action,
        resource: Resource,
        decision: Decision,
    ) -> Result<(), PermissionError> {
        match decision {
            Decision::AllowOnce => Ok(()),
            Decision::AllowAlways => {
                // Widen to the whole action (e.g. all bash), not the exact resource.
                self.persistent
                    .lock()
                    .await
                    .rules
                    .push(Rule::new(action, "*", Mode::Allow));
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
    use yaca_proto::SessionId;

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

    #[test]
    fn mcp_action_round_trips_as_lowercase_json() {
        let encoded = serde_json::to_string(&Action::Mcp).expect("serialize action");
        assert_eq!(encoded, "\"mcp\"");
        let decoded: Action = serde_json::from_str(&encoded).expect("deserialize action");
        assert_eq!(decoded, Action::Mcp);
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
