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

/// A read-only auxiliary pane observing another agent's session.
pub(crate) struct AuxPane {
    /// The observed session id (the agent's own session).
    pub session_id: String,
    /// The agent's stable handle, shown as the pane label.
    pub handle: String,
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
}

impl Default for PaneState {
    fn default() -> Self {
        Self {
            aux: Vec::new(),
            focus: PaneFocus::Main,
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

    /// Open (or re-focus) a read-only aux pane for `session_id`. Opening an already
    /// open session just focuses it rather than duplicating the pane. Returns `true`
    /// when a NEW pane was created (so the caller can backfill its transcript).
    pub fn open_aux(&mut self, session_id: String, handle: String) -> bool {
        if let Some(index) = self.aux.iter().position(|p| p.session_id == session_id) {
            self.focus = PaneFocus::Aux(index);
            return false;
        }
        self.aux.push(AuxPane {
            session_id,
            handle,
            scroll: ScrollState::default(),
        });
        self.focus = PaneFocus::Aux(self.aux.len() - 1);
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
                true
            }
            PaneFocus::Aux(_) => {
                // Stale index: recover by focusing main.
                self.focus = PaneFocus::Main;
                false
            }
        }
    }

    /// Cycle focus forward: main → aux[0] → aux[1] → … → main. A no-op with no aux
    /// panes (focus stays on main).
    pub fn cycle(&mut self) {
        if self.aux.is_empty() {
            self.focus = PaneFocus::Main;
            return;
        }
        self.focus = match self.focus {
            PaneFocus::Main => PaneFocus::Aux(0),
            PaneFocus::Aux(index) if index + 1 < self.aux.len() => PaneFocus::Aux(index + 1),
            PaneFocus::Aux(_) => PaneFocus::Main,
        };
    }

    /// Return focus to the main pane.
    pub fn focus_main(&mut self) {
        self.focus = PaneFocus::Main;
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
    fn open_aux_focuses_and_dedupes() {
        let mut panes = PaneState::default();
        assert!(panes.open_aux("ses_a".to_owned(), "a".to_owned()));
        assert_eq!(panes.focus, PaneFocus::Aux(0));
        // Re-opening the same session focuses it without duplicating.
        assert!(!panes.open_aux("ses_a".to_owned(), "a".to_owned()));
        assert_eq!(panes.aux.len(), 1);
        assert!(panes.open_aux("ses_b".to_owned(), "b".to_owned()));
        assert_eq!(panes.aux.len(), 2);
        assert_eq!(panes.focus, PaneFocus::Aux(1));
    }

    #[test]
    fn close_aux_falls_back_to_main() {
        let mut panes = PaneState::default();
        panes.open_aux("ses_a".to_owned(), "a".to_owned());
        assert!(panes.close_focused());
        assert!(panes.is_main_focused());
        assert!(panes.aux.is_empty());
    }

    #[test]
    fn cycle_wraps_main_through_aux_back_to_main() {
        let mut panes = PaneState::default();
        panes.open_aux("ses_a".to_owned(), "a".to_owned());
        panes.open_aux("ses_b".to_owned(), "b".to_owned());
        panes.focus_main();
        panes.cycle();
        assert_eq!(panes.focus, PaneFocus::Aux(0));
        panes.cycle();
        assert_eq!(panes.focus, PaneFocus::Aux(1));
        panes.cycle();
        assert!(panes.is_main_focused());
    }
}
