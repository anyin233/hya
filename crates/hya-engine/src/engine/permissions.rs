use std::sync::Arc;

use hya_proto::{AgentName, Event, SessionId};
use hya_tool::PermissionPlane;

use super::SessionEngine;
use crate::{CoreError, TurnBinding};

impl SessionEngine {
    /// Mode used by a session tree whose root records none: `yolo` when the
    /// process invocation model is `danger` (config or `--yolo`), else
    /// `manual`.
    #[must_use]
    pub fn default_permission_mode(&self) -> crate::permission_mode::SessionPermissionMode {
        crate::permission_mode::SessionPermissionMode::process_default(
            self.permission.invocation_model(),
        )
    }

    /// The lineage root of `session` and the permission mode it records
    /// (`None` when the tree uses the process default).
    async fn recorded_permission_mode(
        &self,
        session: SessionId,
    ) -> Result<(SessionId, Option<String>), CoreError> {
        let (root, _) = self.session_lineage(session).await?;
        let mode = self
            .store
            .with_projection(root, |projection| {
                projection.session.permission_mode.clone()
            })
            .await?;
        Ok((root, mode))
    }

    /// Effective permission mode of `session`'s tree in wire form
    /// (`manual`, `yolo`, or `<bundle-id>/<mode-id>`): the root's recorded
    /// mode, else [`Self::default_permission_mode`].
    ///
    /// # Errors
    /// Returns store errors while walking the lineage.
    pub async fn permission_mode(&self, session: SessionId) -> Result<String, CoreError> {
        let (_, recorded) = self.recorded_permission_mode(session).await?;
        Ok(recorded.unwrap_or_else(|| self.default_permission_mode().as_wire()))
    }

    /// Every selectable permission mode in the global view: the built-in
    /// `manual` and `yolo`, then each base-published bundle's declared modes
    /// (after refreshing the runtime catalog so newly installed bundles
    /// appear). Project bundles' modes are not listed (see
    /// [`Self::session_permission_modes`]).
    pub async fn permission_modes(&self) -> Vec<crate::permission_mode::PublishedPermissionMode> {
        self.refresh_catalog_for_bundle_api().await;
        let mut modes = crate::permission_mode::builtin_permission_modes();
        modes.extend(self.runtime.published_permission_modes());
        modes
    }

    /// Every permission mode selectable in `session`: the built-ins, then
    /// each bundle mode published in the session's catalog scope (a Project
    /// session also sees its Project bundles' modes).
    ///
    /// # Errors
    /// [`CoreError::Invalid`] for an unknown session; bind failures.
    pub async fn session_permission_modes(
        &self,
        session: SessionId,
    ) -> Result<Vec<crate::permission_mode::PublishedPermissionMode>, CoreError> {
        let binding = self.session_scope_binding(session).await?;
        let mut modes = crate::permission_mode::builtin_permission_modes();
        modes.extend(binding.published_permission_modes());
        Ok(modes)
    }

    /// Set the permission mode of `session`'s tree.
    ///
    /// The event is appended to the lineage root, so every descendant
    /// (subagent) session uses the same mode. `mode` must be `manual`,
    /// `yolo`, or a `<bundle-id>/<mode-id>` published by an installed
    /// bundle. The change applies to the next permission check of every
    /// session in the tree, including turns already running. Returns the
    /// root session.
    ///
    /// # Errors
    /// Returns [`CoreError::Invalid`] for an absent session or an unknown or
    /// unavailable mode, plus store errors from event emission.
    pub async fn set_permission_mode(
        &self,
        session: SessionId,
        mode: &str,
    ) -> Result<SessionId, CoreError> {
        let parsed = crate::permission_mode::SessionPermissionMode::parse(mode)
            .ok_or_else(|| CoreError::Invalid(format!("unknown permission mode: {mode:?}")))?;
        if let crate::permission_mode::SessionPermissionMode::Bundle { .. } = &parsed {
            let wire = parsed.as_wire();
            // Validate in the session's scope; an unknown session falls back
            // to the global view and fails the existence check below.
            let available = match self.session_permission_modes(session).await {
                Ok(modes) => modes,
                Err(_) => self.permission_modes().await,
            };
            if !available.iter().any(|published| published.id == wire) {
                return Err(CoreError::Invalid(format!(
                    "permission mode is not available: {mode:?}"
                )));
            }
        }
        if !self.session_exists(session).await? {
            return Err(CoreError::Invalid(format!("session not found: {session}")));
        }
        let (root, _) = self.session_lineage(session).await?;
        if !self.session_exists(root).await? {
            return Err(CoreError::Invalid(format!(
                "session root not found: {root}"
            )));
        }
        self.emit(
            root,
            Event::SessionPermissionModeSet {
                session: root,
                mode: parsed.as_wire(),
            },
        )
        .await?;
        Ok(root)
    }

    /// Derive one tool call's permission plane for `session` from its tree's
    /// current mode (read now, so a switch applies to the next check).
    ///
    /// A recorded mode this binary cannot parse, or a bundle mode whose
    /// bundle no longer publishes it in `binding`, behaves as `manual`.
    pub(crate) async fn mode_permission_plane(
        &self,
        binding: &TurnBinding,
        session: SessionId,
        agent: Option<&AgentName>,
    ) -> Result<PermissionPlane, CoreError> {
        use crate::permission_mode::{ModeApprover, SessionPermissionMode, derive_plane};
        let (root, recorded) = self.recorded_permission_mode(session).await?;
        let mode = match recorded {
            Some(recorded) => {
                SessionPermissionMode::parse(&recorded).unwrap_or(SessionPermissionMode::Manual)
            }
            None => self.default_permission_mode(),
        };
        let approver = match &mode {
            SessionPermissionMode::Bundle { bundle, mode } => {
                binding.permission_mode_hooks(bundle, mode).map(|hooks| {
                    Arc::new(ModeApprover::new(
                        hooks,
                        root,
                        agent.cloned(),
                        bundle.clone(),
                        mode.clone(),
                    )) as Arc<dyn hya_tool::PermissionInterceptor>
                })
            }
            SessionPermissionMode::Manual | SessionPermissionMode::Yolo => None,
        };
        Ok(derive_plane(&self.permission, &mode, approver))
    }
}
