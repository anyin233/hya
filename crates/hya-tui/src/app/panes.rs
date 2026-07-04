//! Multi-pane (tmux-style) layout state for the default TUI (ADR-0003).
//!
//! The TUI is single-focus in its data model: [`crate::state::AppState::route`]
//! always describes the **main** pane, and every input/submit path reads that
//! route. This module adds a PARALLEL set of read-only auxiliary panes that
//! observe other agents' sessions live. The main pane is index-0 and can never be
//! closed; aux panes are user-launchable and strictly read-only — no code here (or
//! in the submit path) ever routes user input into an aux session. That invariant
//! is structural: the input bar is bound to `AppState.route` (main), and this
//! module owns no prompt/submit state at all.

use crate::render::scroll::ScrollState;

/// How an auxiliary observation view is presented relative to the main view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneLayoutKind {
    /// No auxiliary observation view is open.
    #[cfg(test)]
    MainOnly,
    /// Focused full-frame observation tab.
    Tab,
    /// Main and observation view split left/right.
    VerticalSplit,
    /// Main and observation view split top/bottom.
    HorizontalSplit,
}

/// A read-only auxiliary pane observing another agent's session.
pub(crate) struct AuxPane {
    /// The observed session id (the agent's own session).
    pub session_id: String,
    /// The agent's stable handle, shown as the pane label.
    pub handle: String,
    /// Current placement for this observation view.
    pub layout: PaneLayoutKind,
    /// Independent scroll position for this pane's transcript.
    pub scroll: ScrollState,
}

/// Which pane currently has focus for scroll / close / cycle. The main pane is a
/// distinct variant (not an index) because it is always present and uncloseable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PaneFocus {
    /// The persistent main-agent pane.
    Main,
    /// An auxiliary pane, indexed into [`PaneState::aux`].
    Aux(usize),
}

/// The set of aux panes plus which pane is focused. The main pane is implicit
/// (always present) and is never stored here — only aux panes are.
pub(crate) struct PaneState {
    /// Read-only auxiliary panes, in open order.
    pub aux: Vec<AuxPane>,
    /// The currently focused pane.
    pub focus: PaneFocus,
    /// The auxiliary view currently visible beside/main-overlaid with the main pane.
    active_aux: Option<usize>,
}

impl Default for PaneState {
    fn default() -> Self {
        Self {
            aux: Vec::new(),
            focus: PaneFocus::Main,
            active_aux: None,
        }
    }
}

impl PaneState {
    /// Whether focus is on the main (input-bearing) pane.
    #[must_use]
    pub fn is_main_focused(&self) -> bool {
        matches!(self.focus, PaneFocus::Main)
    }

    /// The focused aux pane, if any (`None` when the main pane is focused or the
    /// stored index is stale).
    #[must_use]
    pub fn focused_aux(&self) -> Option<&AuxPane> {
        match self.focus {
            PaneFocus::Main => None,
            PaneFocus::Aux(index) => self.aux.get(index),
        }
    }

    /// Mutable access to the focused aux pane, if any.
    pub fn focused_aux_mut(&mut self) -> Option<&mut AuxPane> {
        match self.focus {
            PaneFocus::Main => None,
            PaneFocus::Aux(index) => self.aux.get_mut(index),
        }
    }

    /// The active visible aux pane, even when focus has returned to the main pane.
    #[must_use]
    pub fn active_aux(&self) -> Option<&AuxPane> {
        self.active_aux.and_then(|index| self.aux.get(index))
    }

    /// Mutable access to the active visible aux pane, if any.
    pub fn active_aux_mut(&mut self) -> Option<&mut AuxPane> {
        self.active_aux.and_then(|index| self.aux.get_mut(index))
    }

    /// Open, focus, and place a read-only aux pane for `session_id`.
    ///
    /// Re-selecting an already open subagent moves the existing view to the
    /// requested placement instead of creating a duplicate.
    pub fn open_aux_with_layout(
        &mut self,
        session_id: String,
        handle: String,
        layout: PaneLayoutKind,
    ) -> bool {
        if let Some(index) = self.aux.iter().position(|p| p.session_id == session_id) {
            self.aux[index].layout = layout;
            self.focus = PaneFocus::Aux(index);
            self.active_aux = Some(index);
            return false;
        }
        self.aux.push(AuxPane {
            session_id,
            handle,
            layout,
            scroll: ScrollState::default(),
        });
        let index = self.aux.len() - 1;
        self.focus = PaneFocus::Aux(index);
        self.active_aux = Some(index);
        true
    }

    /// Close the focused pane. The MAIN pane can never be closed, so this is a
    /// no-op (returning `false`) when the main pane is focused. On closing an aux
    /// pane, focus falls back to the main pane. Returns `true` when a pane closed.
    pub fn close_focused(&mut self) -> bool {
        match self.focus {
            PaneFocus::Main => false,
            PaneFocus::Aux(index) if index < self.aux.len() => {
                self.aux.remove(index);
                self.focus = PaneFocus::Main;
                self.normalize_active_after_remove(index);
                true
            }
            PaneFocus::Aux(_) => {
                // Stale index: recover by focusing main.
                self.focus = PaneFocus::Main;
                false
            }
        }
    }

    /// Close an aux pane by observed session id. Returns `true` when a pane closed.
    pub fn close_session(&mut self, session_id: &str) -> bool {
        let Some(index) = self
            .aux
            .iter()
            .position(|pane| pane.session_id == session_id)
        else {
            return false;
        };
        self.aux.remove(index);
        self.normalize_focus_after_remove(index);
        self.normalize_active_after_remove(index);
        true
    }

    /// Reindex pane focus after removing an aux slot, falling back to main when needed.
    fn normalize_focus_after_remove(&mut self, removed: usize) {
        self.focus = match (self.focus, self.aux.len()) {
            (PaneFocus::Main, _) | (_, 0) => PaneFocus::Main,
            (PaneFocus::Aux(index), _) if index == removed => PaneFocus::Main,
            (PaneFocus::Aux(index), _) if index > removed => PaneFocus::Aux(index - 1),
            (PaneFocus::Aux(index), len) if index < len => PaneFocus::Aux(index),
            _ => PaneFocus::Main,
        };
    }

    fn normalize_active_after_remove(&mut self, removed: usize) {
        self.active_aux = match (self.active_aux, self.aux.len()) {
            (_, 0) => None,
            (Some(active), len) if active == removed => Some(removed.min(len - 1)),
            (Some(active), _) if active > removed => Some(active - 1),
            (Some(active), len) if active < len => Some(active),
            _ => Some(0),
        };
    }

    /// Cycle focus forward: main → aux[0] → aux[1] → … → main. A no-op with no aux
    /// panes (focus stays on main).
    pub fn cycle(&mut self) {
        if self.aux.is_empty() {
            self.focus = PaneFocus::Main;
            self.active_aux = None;
            return;
        }
        self.focus = match self.focus {
            PaneFocus::Main => PaneFocus::Aux(0),
            PaneFocus::Aux(index) if index + 1 < self.aux.len() => PaneFocus::Aux(index + 1),
            PaneFocus::Aux(_) => PaneFocus::Main,
        };
        if let PaneFocus::Aux(index) = self.focus {
            self.active_aux = Some(index);
        }
    }

    /// Return focus to the main pane.
    pub fn focus_main(&mut self) {
        self.focus = PaneFocus::Main;
    }

    /// Close every observation view and return focus to the main pane.
    pub fn clear(&mut self) {
        self.aux.clear();
        self.focus = PaneFocus::Main;
        self.active_aux = None;
    }
    /// The currently focused layout mode.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn layout_kind(&self) -> PaneLayoutKind {
        self.active_aux()
            .map_or(PaneLayoutKind::MainOnly, |pane| pane.layout)
    }

    /// Tab-bar labels: `(label, focused)` for the main pane followed by each aux
    /// pane, in order.
    #[must_use]
    pub fn tab_labels(&self) -> Vec<(String, bool)> {
        let mut labels = vec![("main".to_owned(), self.is_main_focused())];
        for (index, pane) in self.aux.iter().enumerate() {
            labels.push((pane.handle.clone(), self.focus == PaneFocus::Aux(index)));
        }
        labels
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_pane_cannot_be_closed() {
        let mut panes = PaneState::default();
        assert!(!panes.close_focused(), "main pane never closes");
        assert!(panes.is_main_focused());
    }

    #[test]
    fn open_aux_with_layout_focuses_and_dedupes() {
        let mut panes = PaneState::default();
        assert!(panes.open_aux_with_layout(
            "ses_a".to_owned(),
            "a".to_owned(),
            PaneLayoutKind::Tab,
        ));
        assert_eq!(panes.focus, PaneFocus::Aux(0));
        // Re-opening the same session focuses it without duplicating.
        assert!(!panes.open_aux_with_layout(
            "ses_a".to_owned(),
            "a".to_owned(),
            PaneLayoutKind::Tab,
        ));
        assert_eq!(panes.aux.len(), 1);
        assert!(panes.open_aux_with_layout(
            "ses_b".to_owned(),
            "b".to_owned(),
            PaneLayoutKind::Tab,
        ));
        assert_eq!(panes.aux.len(), 2);
        assert_eq!(panes.focus, PaneFocus::Aux(1));
    }

    #[test]
    fn close_aux_falls_back_to_main() {
        let mut panes = PaneState::default();
        panes.open_aux_with_layout("ses_a".to_owned(), "a".to_owned(), PaneLayoutKind::Tab);
        assert!(panes.close_focused());
        assert!(panes.is_main_focused());
        assert!(panes.aux.is_empty());
    }

    #[test]
    fn cycle_wraps_main_through_aux_back_to_main() {
        let mut panes = PaneState::default();
        panes.open_aux_with_layout("ses_a".to_owned(), "a".to_owned(), PaneLayoutKind::Tab);
        panes.open_aux_with_layout("ses_b".to_owned(), "b".to_owned(), PaneLayoutKind::Tab);
        panes.focus_main();
        panes.cycle();
        assert_eq!(panes.focus, PaneFocus::Aux(0));
        panes.cycle();
        assert_eq!(panes.focus, PaneFocus::Aux(1));
        panes.cycle();
        assert!(panes.is_main_focused());
    }

    #[test]
    fn lifecycle_auto_close_reindexes_focus_and_active_aux_after_session_targeted_remove() {
        let mut panes = PaneState::default();
        panes.open_aux_with_layout("ses_a".to_owned(), "a".to_owned(), PaneLayoutKind::Tab);
        panes.open_aux_with_layout("ses_b".to_owned(), "b".to_owned(), PaneLayoutKind::Tab);

        assert_eq!(panes.focus, PaneFocus::Aux(1), "ses_b starts focused");
        assert_eq!(panes.active_aux, Some(1), "ses_b starts active beside main");

        assert!(
            panes.close_session("ses_a"),
            "ses_a should close by observed session id"
        );

        assert_eq!(
            panes.focus,
            PaneFocus::Aux(0),
            "closing an earlier observed session should shift focus onto the surviving pane instead of leaving a stale index"
        );
        assert_eq!(
            panes.active_aux,
            Some(0),
            "the visible aux slot should also shift to the surviving pane"
        );
        assert_eq!(
            panes.focused_aux().map(|pane| pane.session_id.as_str()),
            Some("ses_b"),
            "the focused pane should still be the same surviving session after reindexing"
        );
        assert_eq!(
            panes.active_aux().map(|pane| pane.session_id.as_str()),
            Some("ses_b"),
            "the visible aux pane should still render the surviving session after reindexing"
        );
    }
}
