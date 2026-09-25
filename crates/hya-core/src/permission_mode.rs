//! Session permission modes: the per-session-tree switch between asking the
//! user (`manual`), bypassing every permission check (`yolo`), and letting a
//! bundle-declared approver answer asks first (`<bundle-id>/<mode-id>`).
//!
//! The mode is event-sourced on the lineage root
//! ([`hya_proto::Event::SessionPermissionModeSet`]) and folded into
//! `SessionProjection.permission_mode`; descendants inherit the root's mode.
//! The engine derives each tool call's [`PermissionPlane`] from the mode read
//! at that call, so a switch applies to the next permission check without a
//! restart and without mutating the process-wide plane.

use std::sync::Arc;

use async_trait::async_trait;
use hya_proto::{AgentName, SessionId};
use hya_tool::{
    Action, Decision, PermissionInterceptor, PermissionModel, PermissionPlane, Resource,
};
use sha2::{Digest, Sha256};

use crate::hooks::{HookDispatcher, PermissionApproveInput};

/// Wire name of the built-in mode that sends asks to the user.
pub const MANUAL: &str = "manual";
/// Wire name of the built-in mode that bypasses every permission check.
pub const YOLO: &str = "yolo";
/// `source` of the built-in modes in [`PublishedPermissionMode`].
pub const BUILTIN_SOURCE: &str = "builtin";

/// A parsed session permission mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionPermissionMode {
    /// Asks go to the user (the process policy, with `danger` lowered to
    /// `default`).
    Manual,
    /// Every check is allowed immediately, including explicit deny rules
    /// (the `danger` invocation model, same as `--yolo`).
    Yolo,
    /// Manual policy plus the declaring bundle's `permission.approve` hook,
    /// consulted after every `permission.ask` interceptor deferred.
    Bundle {
        /// Declaring bundle id.
        bundle: String,
        /// Bundle-local mode id.
        mode: String,
    },
}

impl SessionPermissionMode {
    /// Parse `manual`, `yolo`, or `<bundle-id>/<mode-id>`.
    ///
    /// A bundle mode splits at the last `/`; both parts must be non-empty
    /// and the bundle id must itself contain a `/` (bundle ids are
    /// `<publisher>/<name>`). Availability is checked separately.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            MANUAL => Some(Self::Manual),
            YOLO => Some(Self::Yolo),
            _ => {
                let (bundle, mode) = text.rsplit_once('/')?;
                (!mode.is_empty() && bundle.contains('/') && !bundle.starts_with('/')).then(|| {
                    Self::Bundle {
                        bundle: bundle.to_string(),
                        mode: mode.to_string(),
                    }
                })
            }
        }
    }

    /// Wire form (`manual`, `yolo`, `<bundle-id>/<mode-id>`).
    #[must_use]
    pub fn as_wire(&self) -> String {
        match self {
            Self::Manual => MANUAL.to_string(),
            Self::Yolo => YOLO.to_string(),
            Self::Bundle { bundle, mode } => format!("{bundle}/{mode}"),
        }
    }

    /// Mode used when the root projection records none: `yolo` when the
    /// process invocation model is `danger` (config or `--yolo`), else
    /// `manual`.
    #[must_use]
    pub fn process_default(process_model: Option<PermissionModel>) -> Self {
        if process_model == Some(PermissionModel::Danger) {
            Self::Yolo
        } else {
            Self::Manual
        }
    }
}

/// One permission mode declared by a runtime source (a bundle's
/// `permission_modes:` entry).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimePermissionMode {
    /// Bundle-local mode id.
    pub id: String,
    /// Short human-readable name.
    pub title: String,
    /// Longer description (may be empty).
    pub description: String,
}

/// One selectable mode as listed by `ListPermissionModes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishedPermissionMode {
    /// Full mode id accepted by `UpdateSession` (`manual`, `yolo`, or
    /// `<bundle-id>/<mode-id>`).
    pub id: String,
    /// Short human-readable name.
    pub title: String,
    /// Longer description (may be empty).
    pub description: String,
    /// `builtin` or the declaring bundle id.
    pub source: String,
}

/// The two built-in modes, in listing order.
#[must_use]
pub fn builtin_permission_modes() -> Vec<PublishedPermissionMode> {
    vec![
        PublishedPermissionMode {
            id: MANUAL.to_string(),
            title: "Manual".to_string(),
            description: "Ask the user before actions that need permission.".to_string(),
            source: BUILTIN_SOURCE.to_string(),
        },
        PublishedPermissionMode {
            id: YOLO.to_string(),
            title: "Yolo".to_string(),
            description: "Allow every action without asking, including explicitly denied ones."
                .to_string(),
            source: BUILTIN_SOURCE.to_string(),
        },
    ]
}

/// Derive one tool call's plane from the process plane and the mode.
///
/// `yolo` → the `danger` invocation model on this plane only. `manual` and
/// bundle modes → the process policy with a process-level `danger` lowered
/// to `default`, so asks really reach the user (or approver). `approver`
/// (bundle modes only) is appended after the existing interceptors. The
/// process plane itself is never mutated: rules, grants, and the ask channel
/// stay shared.
#[must_use]
pub(crate) fn derive_plane(
    process: &PermissionPlane,
    mode: &SessionPermissionMode,
    approver: Option<Arc<dyn PermissionInterceptor>>,
) -> PermissionPlane {
    match mode {
        SessionPermissionMode::Yolo => process.with_invocation_model(PermissionModel::Danger),
        SessionPermissionMode::Manual | SessionPermissionMode::Bundle { .. } => {
            let plane = if process.invocation_model() == Some(PermissionModel::Danger) {
                process.with_invocation_model(PermissionModel::Default)
            } else {
                process.clone()
            };
            match approver {
                Some(approver) => plane.append_interceptor(approver),
                None => plane,
            }
        }
    }
}

/// Interceptor that forwards an ask to the declaring bundle's
/// `permission.approve` hook; `None` (defer, failure) falls through to the
/// user ask.
pub(crate) struct ModeApprover {
    hooks: Arc<dyn HookDispatcher>,
    root_session: SessionId,
    agent: Option<AgentName>,
    bundle: String,
    mode: String,
}

impl ModeApprover {
    pub(crate) fn new(
        hooks: Arc<dyn HookDispatcher>,
        root_session: SessionId,
        agent: Option<AgentName>,
        bundle: String,
        mode: String,
    ) -> Self {
        Self {
            hooks,
            root_session,
            agent,
            bundle,
            mode,
        }
    }
}

const MODE_APPROVER_IDENTITY_DOMAIN_V1: &[u8] = b"hya.permission.mode-approver/v1";

#[async_trait]
impl PermissionInterceptor for ModeApprover {
    fn semantic_identity_v1(&self) -> Option<[u8; 32]> {
        let mut bytes = MODE_APPROVER_IDENTITY_DOMAIN_V1.to_vec();
        for part in [self.bundle.as_bytes(), self.mode.as_bytes()] {
            bytes.extend_from_slice(&u64::try_from(part.len()).ok()?.to_be_bytes());
            bytes.extend_from_slice(part);
        }
        Some(Sha256::digest(bytes).into())
    }

    async fn intercept(
        &self,
        session: Option<SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        self.hooks
            .permission_approve(PermissionApproveInput {
                session: session.unwrap_or(self.root_session),
                root_session: self.root_session,
                agent: self.agent.clone(),
                mode: self.mode.clone(),
                action,
                resource: resource.clone(),
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_wire_round_trip() {
        assert_eq!(
            SessionPermissionMode::parse("manual"),
            Some(SessionPermissionMode::Manual)
        );
        assert_eq!(
            SessionPermissionMode::parse("yolo"),
            Some(SessionPermissionMode::Yolo)
        );
        let bundle = SessionPermissionMode::parse("acme/approver/careful");
        assert_eq!(
            bundle,
            Some(SessionPermissionMode::Bundle {
                bundle: "acme/approver".to_string(),
                mode: "careful".to_string()
            })
        );
        assert_eq!(
            bundle.map(|mode| mode.as_wire()).as_deref(),
            Some("acme/approver/careful")
        );
        for invalid in ["", "danger", "careful", "acme/", "acme/approver/", "/x/y"] {
            assert_eq!(SessionPermissionMode::parse(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn process_default_follows_the_process_model() {
        assert_eq!(
            SessionPermissionMode::process_default(Some(PermissionModel::Danger)),
            SessionPermissionMode::Yolo
        );
        for model in [
            None,
            Some(PermissionModel::Default),
            Some(PermissionModel::Allow),
            Some(PermissionModel::Strict),
        ] {
            assert_eq!(
                SessionPermissionMode::process_default(model),
                SessionPermissionMode::Manual
            );
        }
    }

    #[test]
    fn derived_planes_never_touch_the_process_plane() {
        let policy = hya_tool::InvocationPolicy::compile(PermissionModel::Danger, Vec::new())
            .unwrap_or_default();
        let (process, _asks) =
            PermissionPlane::new_with_policy(hya_tool::PermissionRules::default(), policy);
        let manual = derive_plane(&process, &SessionPermissionMode::Manual, None);
        assert_eq!(manual.invocation_model(), Some(PermissionModel::Default));
        assert_eq!(process.invocation_model(), Some(PermissionModel::Danger));

        let (strict, _asks) = PermissionPlane::new_with_policy(
            hya_tool::PermissionRules::default(),
            hya_tool::InvocationPolicy::compile(PermissionModel::Strict, Vec::new())
                .unwrap_or_default(),
        );
        let yolo = derive_plane(&strict, &SessionPermissionMode::Yolo, None);
        assert_eq!(yolo.invocation_model(), Some(PermissionModel::Danger));
        assert_eq!(strict.invocation_model(), Some(PermissionModel::Strict));
        let manual = derive_plane(&strict, &SessionPermissionMode::Manual, None);
        assert_eq!(manual.invocation_model(), Some(PermissionModel::Strict));
    }
}
