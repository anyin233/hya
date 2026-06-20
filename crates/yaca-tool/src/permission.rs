use std::sync::Arc;

use thiserror::Error;
use tokio::sync::{Mutex, mpsc, oneshot};
use yaca_proto::PermissionRequestId;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Read,
    Edit,
    Glob,
    Grep,
    Bash,
    Task,
    ExternalDirectory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    Path(String),
    Glob(String),
    Command(String),
    Subagent(String),
    Any,
}

impl Resource {
    #[must_use]
    pub fn pattern(&self) -> String {
        match self {
            Resource::Path(s)
            | Resource::Glob(s)
            | Resource::Command(s)
            | Resource::Subagent(s) => s.clone(),
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    AllowOnce,
    AllowAlways,
    Reject,
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
    #[error("permission denied: {action:?} on {resource:?}")]
    Denied { action: Action, resource: Resource },
    #[error("permission channel unavailable")]
    Unavailable,
}

pub struct AskRequest {
    pub id: PermissionRequestId,
    pub action: Action,
    pub resource: Resource,
    pub reply: oneshot::Sender<Decision>,
}

#[derive(Clone)]
pub struct PermissionPlane {
    snapshot: Arc<PermissionRules>,
    persistent: Arc<Mutex<PermissionRules>>,
    asks: mpsc::UnboundedSender<AskRequest>,
}

impl PermissionPlane {
    #[must_use]
    pub fn new(rules: PermissionRules) -> (Self, mpsc::UnboundedReceiver<AskRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let plane = Self {
            snapshot: Arc::new(rules.clone()),
            persistent: Arc::new(Mutex::new(rules)),
            asks: tx,
        };
        (plane, rx)
    }

    pub async fn assert(&self, action: Action, resource: Resource) -> Result<(), PermissionError> {
        match self.snapshot.evaluate(action, &resource) {
            Mode::Allow => Ok(()),
            Mode::Deny => Err(PermissionError::Denied { action, resource }),
            Mode::Ask => {
                let (tx, rx) = oneshot::channel();
                let req = AskRequest {
                    id: PermissionRequestId::new(),
                    action,
                    resource: resource.clone(),
                    reply: tx,
                };
                self.asks
                    .send(req)
                    .map_err(|_| PermissionError::Unavailable)?;
                match rx.await.map_err(|_| PermissionError::Unavailable)? {
                    Decision::AllowOnce => Ok(()),
                    Decision::AllowAlways => {
                        self.persistent.lock().await.rules.push(Rule::new(
                            action,
                            resource.pattern(),
                            Mode::Allow,
                        ));
                        Ok(())
                    }
                    Decision::Reject => Err(PermissionError::Denied { action, resource }),
                }
            }
        }
    }
}
