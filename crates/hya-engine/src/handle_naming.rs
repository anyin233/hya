//! Subagent handle naming (0.41.0): a leaf is `<prefix>-<operator>`.
//!
//! Pure naming helpers live in [`hya_proto::handle_naming`]; this module
//! re-exports them and adds [`HandleNamer`] for in-process uncommitted
//! reservations during subagent spawn.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use hya_proto::SessionId;

pub use hya_proto::handle_naming::{
    HandleRng, PAIR_NAME_ATTEMPTS, SINGLE_NAME_ATTEMPTS, SplitMix64, mint_leaf, operator_names,
    team_leaves,
};

/// Process-wide RNG and uncommitted reservations. Durable team history owns
/// uniqueness after registration; aborted preparations release their names too.
pub(crate) struct HandleNamer {
    rng: Mutex<Box<dyn HandleRng>>,
    minted: Mutex<HashMap<SessionId, BTreeSet<String>>>,
}

pub(crate) struct HandleReservation {
    pub(crate) leaf: String,
    root: SessionId,
    namer: Arc<HandleNamer>,
}

impl Drop for HandleReservation {
    fn drop(&mut self) {
        let mut minted = lock(&self.namer.minted);
        if let Some(names) = minted.get_mut(&self.root) {
            names.remove(&self.leaf);
            if names.is_empty() {
                minted.remove(&self.root);
            }
        }
    }
}

impl HandleNamer {
    pub(crate) fn new(rng: Box<dyn HandleRng>) -> Self {
        Self {
            rng: Mutex::new(rng),
            minted: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub(crate) fn reserved_count(&self, root: SessionId) -> usize {
        lock(&self.minted).get(&root).map_or(0, BTreeSet::len)
    }

    pub(crate) fn set_rng(&self, rng: Box<dyn HandleRng>) {
        *lock(&self.rng) = rng;
    }

    pub(crate) fn reserve(
        self: &Arc<Self>,
        root: SessionId,
        prefix: &str,
        taken: BTreeSet<String>,
    ) -> HandleReservation {
        HandleReservation {
            leaf: self.mint(root, prefix, taken),
            root,
            namer: self.clone(),
        }
    }

    /// Mint and reserve a leaf for `prefix` in `root`'s team; `taken` holds
    /// the team's durable leaves (see [`team_leaves`]).
    pub(crate) fn mint(
        &self,
        root: SessionId,
        prefix: &str,
        mut taken: BTreeSet<String>,
    ) -> String {
        let mut minted = lock(&self.minted);
        let reserved = minted.entry(root).or_default();
        taken.extend(reserved.iter().cloned());
        let leaf = mint_leaf(prefix, &taken, lock(&self.rng).as_mut());
        reserved.insert(leaf.clone());
        leaf
    }
}

/// Environment variable that seeds the handle RNG (a decimal `u64`) for
/// reproducible names, e.g. in process-level E2E runs.
pub const HANDLE_SEED_ENV: &str = "HYA_HANDLE_SEED";

impl Default for HandleNamer {
    fn default() -> Self {
        let seeded = std::env::var(HANDLE_SEED_ENV)
            .ok()
            .and_then(|seed| seed.trim().parse::<u64>().ok());
        Self::new(Box::new(
            seeded.map_or_else(SplitMix64::from_entropy, SplitMix64::seeded),
        ))
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use hya_proto::scope;

    use super::*;

    /// Always draws `value` (clamped to the bound): forces collisions.
    struct Fixed(usize);

    impl HandleRng for Fixed {
        fn pick(&mut self, bound: usize) -> usize {
            self.0.min(bound - 1)
        }
    }

    #[test]
    fn operator_name_list_max_length_is_valid_leaf() {
        let names = operator_names();
        let longest = names.iter().max_by_key(|n| n.len()).copied().unwrap();
        let prefix = "a".repeat(hya_tool::HANDLE_PREFIX_MAX_LEN);
        let idx = names.iter().position(|n| *n == longest).unwrap();
        let leaf = mint_leaf(&prefix, &BTreeSet::new(), &mut Fixed(idx));
        assert_eq!(leaf, format!("{prefix}-{longest}"));
        assert!(scope::is_valid_leaf(&leaf));
    }

    #[test]
    fn collision_pair_fallback_and_per_team_uniqueness() {
        let names = operator_names();
        let taken: BTreeSet<String> = [format!("scout-{}", names[0])].into_iter().collect();
        let leaf = mint_leaf("scout", &taken, &mut Fixed(0));
        assert_eq!(leaf, format!("scout-{}-{}", names[0], names[1]));
        assert!(!taken.contains(&leaf));

        let mut all_taken: BTreeSet<String> = names.iter().map(|n| format!("dev-{n}")).collect();
        all_taken.insert(format!("dev-{}-{}", names[0], names[1]));
        let pair = mint_leaf("dev", &all_taken, &mut Fixed(0));
        assert!(!all_taken.contains(&pair) && pair.starts_with("dev-"));

        let namer = HandleNamer::new(Box::new(Fixed(3)));
        let root = SessionId::new();
        let first = namer.mint(root, "scout", BTreeSet::new());
        let second = namer.mint(root, "scout", BTreeSet::new());
        assert_ne!(first, second);
        assert_eq!(
            namer.mint(SessionId::new(), "scout", BTreeSet::new()),
            first
        );
    }
}
