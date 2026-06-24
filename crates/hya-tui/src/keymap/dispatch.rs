use std::time::{Duration, Instant};

use crate::contracts::{BindingId, Key, KeyChord, KeyEvent};

use super::binding::key_events_match;
use super::modes::KeymapMode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    pub command: BindingId,
    pub chord: KeyChord,
    pub mode: Option<KeymapMode>,
    pub priority: i16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    Matched(BindingId),
    Pending,
    Cleared,
    Unmatched,
}

#[derive(Debug, Clone)]
pub struct KeymapDispatcher {
    bindings: Vec<KeyBinding>,
    leader: KeyChord,
    leader_timeout: Duration,
    pending: Vec<KeyEvent>,
    pending_deadline: Option<Instant>,
}

impl KeymapDispatcher {
    #[must_use]
    pub fn new(bindings: Vec<KeyBinding>, leader: KeyChord, leader_timeout_ms: u64) -> Self {
        Self {
            bindings,
            leader,
            leader_timeout: Duration::from_millis(leader_timeout_ms),
            pending: Vec::new(),
            pending_deadline: None,
        }
    }

    #[must_use]
    pub fn bindings(&self) -> &[KeyBinding] {
        &self.bindings
    }

    #[must_use]
    pub fn pending_sequence(&self) -> &[KeyEvent] {
        &self.pending
    }

    #[must_use]
    pub fn leader_timeout(&self) -> Duration {
        self.leader_timeout
    }

    #[must_use]
    pub fn continuations(&self) -> Vec<(KeyEvent, BindingId)> {
        self.bindings
            .iter()
            .filter(|binding| binding.chord.0.len() > self.pending.len())
            .filter(|binding| {
                binding
                    .chord
                    .0
                    .iter()
                    .zip(&self.pending)
                    .all(|(expected, actual)| key_events_match(expected, actual))
            })
            .map(|binding| (binding.chord.0[self.pending.len()], binding.command.clone()))
            .collect()
    }

    pub fn clear_pending(&mut self) {
        self.pending.clear();
        self.pending_deadline = None;
    }

    #[must_use]
    pub fn dispatch(&mut self, event: KeyEvent, mode: KeymapMode) -> DispatchOutcome {
        self.dispatch_at(event, mode, Instant::now())
    }

    #[must_use]
    pub fn dispatch_at(
        &mut self,
        event: KeyEvent,
        mode: KeymapMode,
        now: Instant,
    ) -> DispatchOutcome {
        self.clear_expired_pending(now);
        if !self.pending.is_empty() && event.key == Key::Esc {
            self.clear_pending();
            return DispatchOutcome::Cleared;
        }
        if !self.pending.is_empty() && event.key == Key::Backspace {
            self.pending.pop();
            if self.pending.is_empty() {
                self.pending_deadline = None;
                return DispatchOutcome::Cleared;
            }
            self.pending_deadline = self.deadline_for_pending(now);
            return DispatchOutcome::Pending;
        }

        let mut sequence = self.pending.clone();
        sequence.push(event);
        if let Some(command) = self.match_exact(&sequence, &mode) {
            self.clear_pending();
            return DispatchOutcome::Matched(command);
        }
        if self.has_prefix(&sequence, &mode) {
            self.pending = sequence;
            self.pending_deadline = self.deadline_for_pending(now);
            return DispatchOutcome::Pending;
        }
        self.clear_pending();
        DispatchOutcome::Unmatched
    }

    fn match_exact(&self, sequence: &[KeyEvent], mode: &KeymapMode) -> Option<BindingId> {
        self.bindings
            .iter()
            .filter(|binding| binding.active_in(mode) && chord_matches(&binding.chord, sequence))
            .max_by_key(|binding| binding.priority)
            .map(|binding| binding.command.clone())
    }

    fn has_prefix(&self, sequence: &[KeyEvent], mode: &KeymapMode) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.active_in(mode) && chord_starts_with(&binding.chord, sequence))
    }

    fn clear_expired_pending(&mut self, now: Instant) {
        if self
            .pending_deadline
            .is_some_and(|deadline| now >= deadline)
        {
            self.clear_pending();
        }
    }

    fn deadline_for_pending(&self, now: Instant) -> Option<Instant> {
        sequence_starts_with(&self.pending, &self.leader).then_some(now + self.leader_timeout)
    }
}

impl KeyBinding {
    fn active_in(&self, current: &KeymapMode) -> bool {
        self.mode.as_ref().is_none_or(|mode| mode == current)
    }
}

fn chord_matches(chord: &KeyChord, sequence: &[KeyEvent]) -> bool {
    chord.0.len() == sequence.len()
        && chord
            .0
            .iter()
            .zip(sequence)
            .all(|(expected, actual)| key_events_match(expected, actual))
}

fn chord_starts_with(chord: &KeyChord, sequence: &[KeyEvent]) -> bool {
    sequence.len() < chord.0.len()
        && chord
            .0
            .iter()
            .zip(sequence)
            .all(|(expected, actual)| key_events_match(expected, actual))
}

fn sequence_starts_with(sequence: &[KeyEvent], prefix: &KeyChord) -> bool {
    prefix.0.len() <= sequence.len()
        && prefix
            .0
            .iter()
            .zip(sequence)
            .all(|(expected, actual)| key_events_match(expected, actual))
}
