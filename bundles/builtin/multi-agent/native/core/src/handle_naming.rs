//! Subagent handle naming (`<prefix>-<operator>`) and uncommitted name reservations.
//!
//! Pure naming primitives live in [`hya_proto::handle_naming`]; this module
//! re-exports them and provides [`team_leaves`] and [`HandleNamer`] for
//! in-process reservation tracking across concurrent spawns.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use hya_proto::{Projection, SessionId, scope};

pub use hya_proto::handle_naming::{
    HandleRng, PAIR_NAME_ATTEMPTS, SINGLE_NAME_ATTEMPTS, SplitMix64, mint_leaf, operator_names,
};

/// Environment variable that seeds the handle RNG (a decimal `u64`) for
/// reproducible names in process-level E2E runs.
pub const HANDLE_SEED_ENV: &str = "HYA_HANDLE_SEED";

/// Collect every handle leaf already used in a team's durable history.
#[must_use]
pub fn team_leaves(projection: &Projection) -> BTreeSet<String> {
    let team = hya_tool::extension::team_projection(projection);
    let mut leaves: BTreeSet<String> = [scope::ROOT_HANDLE, scope::HARNESS_HANDLE]
        .into_iter()
        .map(str::to_string)
        .collect();
    let handles = team
        .roster
        .keys()
        .chain(team.archived.keys())
        .chain(team.inboxes.keys())
        .chain(
            team.channels
                .values()
                .flat_map(|channel| channel.members.iter()),
        );
    for handle in handles {
        leaves.insert(scope::leaf(handle).to_string());
    }
    leaves
}

/// Process-wide RNG and uncommitted handle reservations per team root.
pub struct HandleNamer {
    rng: Mutex<Box<dyn HandleRng>>,
    minted: Mutex<HashMap<SessionId, BTreeSet<String>>>,
}

/// RAII reservation releasing an uncommitted leaf on drop.
pub struct HandleReservation {
    /// Reserved handle leaf (e.g. `hya-scout-suzuran`).
    pub leaf: String,
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
    /// Create a namer backed by a custom [`HandleRng`].
    #[must_use]
    pub fn new(rng: Box<dyn HandleRng>) -> Self {
        Self {
            rng: Mutex::new(rng),
            minted: Mutex::new(HashMap::new()),
        }
    }

    /// Number of currently reserved uncommitted leaves for `root`.
    #[must_use]
    pub fn reserved_count(&self, root: SessionId) -> usize {
        lock(&self.minted).get(&root).map_or(0, BTreeSet::len)
    }

    /// Replace the RNG implementation (primarily for deterministic tests).
    pub fn set_rng(&self, rng: Box<dyn HandleRng>) {
        *lock(&self.rng) = rng;
    }

    /// Reserve a fresh leaf for `prefix` in `root`'s team, releasing it on drop.
    #[must_use]
    pub fn reserve(
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

    /// Mint and reserve a leaf for `prefix` in `root`'s team; `taken` holds the
    /// team's durable leaves (see [`team_leaves`]).
    pub fn mint(&self, root: SessionId, prefix: &str, mut taken: BTreeSet<String>) -> String {
        let mut minted = lock(&self.minted);
        let reserved = minted.entry(root).or_default();
        taken.extend(reserved.iter().cloned());
        let leaf = mint_leaf(prefix, &taken, lock(&self.rng).as_mut());
        reserved.insert(leaf.clone());
        leaf
    }
}

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
