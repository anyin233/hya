use std::time::{Duration, Instant};

use super::Controller;

impl Controller {
    pub fn leader_keybindings_timeout(&self) -> Option<Duration> {
        self.leader_keybindings_timeout_at(Instant::now())
    }

    pub(super) fn leader_keybindings_timeout_at(&self, now: Instant) -> Option<Duration> {
        self.app
            .keybindings
            .as_ref()
            .and_then(|_| self.leader_key.timeout_remaining_at(now))
    }

    pub fn expire_leader_keybindings(&mut self) -> bool {
        self.expire_leader_keybindings_at(Instant::now())
    }

    pub(super) fn expire_leader_keybindings_at(&mut self, now: Instant) -> bool {
        if self.leader_key.expire_if_stale_at(now) {
            self.app.keybindings = None;
            return true;
        }
        false
    }
}
