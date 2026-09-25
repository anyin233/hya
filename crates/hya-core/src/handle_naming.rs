//! Subagent handle naming (0.41.0): a leaf is `<prefix>-<operator>`.
//!
//! The spawning agent supplies the role prefix (the `task` tool's `name`,
//! defaulting to the agent id; see [`hya_tool::normalize_handle_prefix`]) and
//! the harness appends ONE randomly chosen Arknights operator name from
//! [`operator_names`], e.g. `scout-suzuran`; the canonical handle keeps the
//! parent path (`main/scout-suzuran`, `main/dev-exusiai/general-amiya`).
//!
//! A leaf is never reused within a team: every candidate is checked against
//! the leaf of every handle the team ever registered (live roster, archived
//! rows, inboxes, and channel memberships — all durable) plus the leaves this
//! process minted for the team. On collision the draw is retried; after
//! [`SINGLE_NAME_ATTEMPTS`] failures (or once the single names are exhausted)
//! the leaf falls back to two distinct operator names
//! (`scout-suzuran-amiya`).
//!
//! Randomness is safe for replay: the minted handle is recorded in the event
//! log (`AgentRegistered`) and replay reads it back — nothing re-derives a
//! handle. Handles are opaque: operator names may contain `-`
//! (`blue-poison`, `skadi-the-corrupting-heart`), so a leaf is never parsed
//! back into prefix and name.

use std::collections::{BTreeSet, HashMap};
use std::hash::{BuildHasher, Hasher};
use std::sync::{Mutex, OnceLock};

use hya_proto::{Projection, SessionId, scope};

/// The checked-in operator name list: one lowercase ASCII name per line.
///
/// Snapshot of the prts.wiki operator list (干员一览, `data-en` attributes)
/// taken 2026-09-25, normalized NFKD→ASCII, lowercased, apostrophes dropped,
/// every other non-alphanumeric run turned into `-`, deduplicated and sorted.
/// No name is filtered out. Never fetched at build or run time.
const OPERATOR_NAMES_TXT: &str = include_str!("handle_names.txt");

/// Random single-name draws before falling back to two names.
pub const SINGLE_NAME_ATTEMPTS: usize = 16;
/// Random two-name draws before the deterministic sweep.
pub const PAIR_NAME_ATTEMPTS: usize = 64;

/// Every operator name, in file order.
#[must_use]
pub fn operator_names() -> &'static [&'static str] {
    static NAMES: OnceLock<Vec<&'static str>> = OnceLock::new();
    NAMES.get_or_init(|| {
        OPERATOR_NAMES_TXT
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect()
    })
}

/// Source of the random draws; injectable so tests can seed or force it.
pub trait HandleRng: Send {
    /// A uniformly distributed index in `0..bound`; `bound` is never 0.
    fn pick(&mut self, bound: usize) -> usize;
}

/// SplitMix64: small, fast, and good enough to spread handle names.
#[derive(Clone, Debug)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// A generator with a fixed seed (reproducible draws).
    #[must_use]
    pub fn seeded(seed: u64) -> Self {
        Self { state: seed }
    }

    /// A generator seeded from the process's hash randomness and the clock.
    #[must_use]
    pub fn from_entropy() -> Self {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| elapsed.as_nanos()),
        );
        Self::seeded(hasher.finish())
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

impl HandleRng for SplitMix64 {
    fn pick(&mut self, bound: usize) -> usize {
        // Lemire's multiply-shift: uniform enough for `bound` ≪ 2^64.
        let wide = u128::from(self.next_u64()) * (bound as u128);
        usize::try_from(wide >> 64).unwrap_or(0)
    }
}

/// Mint a fresh leaf `<prefix>-<operator>` that is not in `taken`.
///
/// Tries [`SINGLE_NAME_ATTEMPTS`] random single names, then
/// [`PAIR_NAME_ATTEMPTS`] random pairs of distinct names, then sweeps every
/// pair from a random start; the numbered last resort only exists so the
/// function is total (it needs every one of the ~175k pairs taken).
pub fn mint_leaf(prefix: &str, taken: &BTreeSet<String>, rng: &mut dyn HandleRng) -> String {
    let names = operator_names();
    let count = names.len();
    let free = |leaf: &String| !taken.contains(leaf);
    if count == 0 {
        return (1..)
            .map(|ordinal| format!("{prefix}-{ordinal}"))
            .find(free)
            .unwrap_or_default();
    }
    let single = |index: usize| format!("{prefix}-{}", names[index]);
    let pair = |first: usize, second: usize| format!("{prefix}-{}-{}", names[first], names[second]);
    for _ in 0..SINGLE_NAME_ATTEMPTS {
        let leaf = single(rng.pick(count));
        if free(&leaf) {
            return leaf;
        }
    }
    if count >= 2 {
        for _ in 0..PAIR_NAME_ATTEMPTS {
            let first = rng.pick(count);
            let mut second = rng.pick(count - 1);
            if second >= first {
                second += 1;
            }
            let leaf = pair(first, second);
            if free(&leaf) {
                return leaf;
            }
        }
        let start = rng.pick(count);
        for step in 0..count {
            let first = (start + step) % count;
            for offset in 1..count {
                let leaf = pair(first, (first + offset) % count);
                if free(&leaf) {
                    return leaf;
                }
            }
        }
    }
    (1..)
        .map(|ordinal| format!("{}-{ordinal}", pair(0, count.min(2) - 1)))
        .find(free)
        .unwrap_or_default()
}

/// The leaf of every handle `projection`'s team ever registered: live roster,
/// archived rows, inbox owners, and channel members — plus the reserved
/// `main` and `harness`.
#[must_use]
pub fn team_leaves(projection: &Projection) -> BTreeSet<String> {
    let team = &projection.team;
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

/// Process-wide minting state: the injectable RNG plus the leaves this
/// process already minted per team root (so two concurrent spawns that read
/// the same projection can never pick the same leaf).
pub(crate) struct HandleNamer {
    rng: Mutex<Box<dyn HandleRng>>,
    minted: Mutex<HashMap<SessionId, BTreeSet<String>>>,
}

impl HandleNamer {
    pub(crate) fn new(rng: Box<dyn HandleRng>) -> Self {
        Self {
            rng: Mutex::new(rng),
            minted: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn set_rng(&self, rng: Box<dyn HandleRng>) {
        *lock(&self.rng) = rng;
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

    use super::*;

    /// Always draws `value` (clamped to the bound): forces collisions.
    struct Fixed(usize);

    impl HandleRng for Fixed {
        fn pick(&mut self, bound: usize) -> usize {
            self.0.min(bound - 1)
        }
    }

    fn is_name(line: &str) -> bool {
        !line.is_empty()
            && line.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            })
    }

    #[test]
    fn the_name_list_is_non_empty_well_formed_and_unique() {
        let names = operator_names();
        assert!(!names.is_empty());
        let raw_lines: Vec<&str> = OPERATOR_NAMES_TXT.lines().collect();
        assert_eq!(raw_lines.len(), names.len(), "no blank or padded lines");
        for name in names {
            assert!(
                is_name(name),
                "`{name}` must match ^[a-z0-9]+(-[a-z0-9]+)*$"
            );
        }
        let unique: BTreeSet<&&str> = names.iter().collect();
        assert_eq!(unique.len(), names.len(), "names are unique");
    }

    #[test]
    fn a_leaf_is_the_prefix_and_one_listed_name() {
        let mut rng = SplitMix64::seeded(7);
        for _ in 0..200 {
            let leaf = mint_leaf("scout", &BTreeSet::new(), &mut rng);
            let name = leaf.strip_prefix("scout-").expect("prefix");
            assert!(operator_names().contains(&name), "{leaf}");
        }
    }

    #[test]
    fn seeded_draws_are_reproducible() {
        let draw = |seed| {
            let mut rng = SplitMix64::seeded(seed);
            (0..8)
                .map(|_| mint_leaf("dev", &BTreeSet::new(), &mut rng))
                .collect::<Vec<_>>()
        };
        assert_eq!(draw(42), draw(42));
    }

    #[test]
    fn a_taken_leaf_falls_back_to_two_distinct_names() {
        let names = operator_names();
        let taken: BTreeSet<String> = [format!("scout-{}", names[0])].into_iter().collect();
        let leaf = mint_leaf("scout", &taken, &mut Fixed(0));
        assert_eq!(leaf, format!("scout-{}-{}", names[0], names[1]));
        assert!(!taken.contains(&leaf));
    }

    #[test]
    fn every_single_name_taken_still_mints_a_fresh_pair() {
        let names = operator_names();
        let mut taken: BTreeSet<String> = names.iter().map(|name| format!("dev-{name}")).collect();
        taken.insert(format!("dev-{}-{}", names[0], names[1]));
        let leaf = mint_leaf("dev", &taken, &mut Fixed(0));
        assert!(!taken.contains(&leaf), "{leaf}");
        assert!(leaf.starts_with("dev-"), "{leaf}");
    }

    #[test]
    fn the_longest_leaf_is_admitted() {
        let longest = operator_names()
            .iter()
            .max_by_key(|name| name.len())
            .copied()
            .unwrap();
        let prefix = "a".repeat(hya_tool::HANDLE_PREFIX_MAX_LEN);
        let index = operator_names()
            .iter()
            .position(|name| *name == longest)
            .unwrap();
        let leaf = mint_leaf(&prefix, &BTreeSet::new(), &mut Fixed(index));
        assert_eq!(leaf, format!("{prefix}-{longest}"));
        assert!(scope::is_valid_leaf(&leaf));
    }

    #[test]
    fn the_namer_never_hands_out_the_same_leaf_twice_per_team() {
        let namer = HandleNamer::new(Box::new(Fixed(3)));
        let root = SessionId::new();
        let first = namer.mint(root, "scout", BTreeSet::new());
        let second = namer.mint(root, "scout", BTreeSet::new());
        assert_ne!(first, second);
        let other_team = namer.mint(SessionId::new(), "scout", BTreeSet::new());
        assert_eq!(other_team, first, "uniqueness is per team");
    }
}
