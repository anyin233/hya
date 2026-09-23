//! Bounded process-local state for the native hashline runtime.
//!
//! Snapshot history follows the observable state behavior of
//! `pi-hashline-edit` 0.8.3 (MIT, pinned by git head
//! `ba7db9943d0f58499b24c1f6bd64722580f772a5`). Session/workdir scoping and
//! hard bounds are hya safety additions. The state stores only bounded content
//! snapshots and digests; it does not build diagnostics or log payloads.

use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use hya_proto::SessionId;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, MutexGuard};
use tokio_util::sync::CancellationToken;

/// Maximum number of `(session, workdir, target)` entries retained globally.
pub(super) const MAX_TARGETS: usize = 8;
/// Maximum number of newest-first versions retained for one target entry.
pub(super) const MAX_VERSIONS_PER_TARGET: usize = 4;
/// Maximum UTF-8 bytes retained by all snapshot versions globally.
pub(super) const MAX_TOTAL_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
/// Number of fixed async mutation-lock shards.
pub(super) const MUTATION_LOCK_SHARD_COUNT: usize = 64;

/// Isolation key for snapshot history and mutation guards.
///
/// The workdir and target are normalized lexically, but target resolution and
/// symlink policy remain the filesystem layer's responsibility. `None` is a
/// deliberate no-session scope and is distinct from every concrete session.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct StateKey {
    session: Option<SessionId>,
    workdir: PathBuf,
    target: PathBuf,
}

impl StateKey {
    /// Build a state key from a session scope, workdir, and resolved target.
    pub(super) fn new(
        session: Option<SessionId>,
        workdir: impl AsRef<Path>,
        target: impl AsRef<Path>,
    ) -> Self {
        Self {
            session,
            workdir: normalize_path(workdir.as_ref()),
            target: normalize_path(target.as_ref()),
        }
    }

    #[cfg(test)]
    /// Hash only the normalized target path for compatibility lock selection.
    #[must_use]
    fn shard_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.target.hash(&mut hasher);
        hasher.finish()
    }
}
/// Filesystem identity used only to select a fixed mutation-lock shard.
///
/// Snapshot state remains keyed by [`StateKey`]. Ordinary and missing targets
/// use their normalized resolved path, while Unix hard-linked files use their
/// device and inode so aliases serialize through one lock.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum MutationIdentity {
    /// Lexically normalized resolved path for ordinary or missing targets.
    Path(PathBuf),
    /// Unix device/inode identity shared by hard-link aliases.
    #[cfg(unix)]
    Inode {
        /// Device number containing the inode.
        device: u64,
        /// Inode number identifying the file.
        inode: u64,
    },
}

impl MutationIdentity {
    /// Build a path identity from a resolved or missing target path.
    ///
    /// # Parameters
    /// - `path`: Target path to normalize without following links.
    ///
    /// # Returns
    /// A path-based filesystem mutation identity.
    pub(super) fn path(path: impl AsRef<Path>) -> Self {
        Self::Path(normalize_path(path.as_ref()))
    }

    /// Hash this identity for deterministic fixed-shard selection.
    #[must_use]
    fn shard_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

/// Failure while waiting for a fixed mutation-lock shard.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LockError {
    /// The caller cancelled before the shard became available.
    Cancelled,
}

/// Result of observing one no-op edit payload for a target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NoOpOutcome {
    /// The no-op is within the two-attempt soft allowance.
    Allowed,
    /// The same no-op payload reached the hard loop threshold.
    RejectLoop,
}

/// One successful payload guard, retained without the payload or file text.
struct AppliedPayload {
    payload_digest: [u8; 32],
    post_edit_digest: [u8; 32],
}

/// Consecutive no-op counter for one target and one payload digest.
struct NoOpSequence {
    payload_digest: [u8; 32],
    count: u8,
}

/// One retained content version for a target.
struct SnapshotVersion {
    content: Arc<str>,
}

/// State associated with one isolated target key.
struct TargetState {
    key: StateKey,
    versions: VecDeque<SnapshotVersion>,
    bytes: usize,
    applied_payload: Option<AppliedPayload>,
    no_op: Option<NoOpSequence>,
}

impl TargetState {
    /// Create an empty target entry for a normalized state key.
    fn new(key: StateKey) -> Self {
        Self {
            key,
            versions: VecDeque::with_capacity(MAX_VERSIONS_PER_TARGET),
            bytes: 0,
            applied_payload: None,
            no_op: None,
        }
    }

    /// Return the bytes currently accounted for by this target's versions.
    #[must_use]
    fn byte_count(&self) -> usize {
        self.bytes
    }

    /// Report whether this entry has no retained state at all.
    #[must_use]
    fn is_empty(&self) -> bool {
        self.versions.is_empty() && self.applied_payload.is_none() && self.no_op.is_none()
    }
}
/// Borrowed or owned content accepted by snapshot insertion.
pub(super) trait SnapshotContent {
    /// Borrow normalized UTF-8 content without allocating.
    fn as_str(&self) -> &str;

    /// Materialize owned content after bounded checks pass.
    fn into_arc(self) -> Arc<str>;
}

impl SnapshotContent for Arc<str> {
    /// Borrow bytes held by an existing snapshot arc.
    fn as_str(&self) -> &str {
        self.as_ref()
    }

    /// Reuse the existing owned snapshot arc.
    fn into_arc(self) -> Arc<str> {
        self
    }
}

impl SnapshotContent for &str {
    /// Borrow caller-owned text before any allocation.
    fn as_str(&self) -> &str {
        self
    }

    /// Allocate one owned arc for retained snapshot content.
    fn into_arc(self) -> Arc<str> {
        Arc::<str>::from(self)
    }
}

impl SnapshotContent for String {
    /// Borrow owned text before any allocation.
    fn as_str(&self) -> &str {
        self.as_ref()
    }

    /// Convert owned text into the retained arc representation.
    fn into_arc(self) -> Arc<str> {
        Arc::<str>::from(self)
    }
}

/// Bounded newest-first snapshots and per-target duplicate/no-op guards.
///
/// The vector of target entries is intentionally bounded instead of backed by
/// an attacker-sized map. `total_bytes` is updated at each insertion/removal,
/// so checking the global content budget is constant-time.
pub(super) struct SnapshotState {
    targets: Vec<TargetState>,
    total_bytes: usize,
}

impl SnapshotState {
    /// Create an empty bounded snapshot state.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            targets: Vec::with_capacity(MAX_TARGETS),
            total_bytes: 0,
        }
    }

    /// Remember a normalized text snapshot and return its canonical `Arc`.
    ///
    /// The borrowed content is checked for oversize and front fusion before it
    /// is materialized. An equal newest version is fused and moved to the
    /// front without adding to the byte total. A single snapshot larger than
    /// the global budget is returned but not retained.
    pub(super) fn remember<C: SnapshotContent>(&mut self, key: StateKey, content: C) -> Arc<str> {
        let borrowed = content.as_str();
        if borrowed.len() > MAX_TOTAL_SNAPSHOT_BYTES {
            return content.into_arc();
        }

        self.touch_target(&key);
        if let Some(front) = self.targets[0].versions.front()
            && front.content.as_ref() == borrowed
        {
            return Arc::clone(&front.content);
        }

        let content = content.into_arc();
        let byte_len = content.len();
        self.total_bytes += byte_len;
        self.targets[0].bytes += byte_len;
        self.targets[0].versions.push_front(SnapshotVersion {
            content: Arc::clone(&content),
        });
        while self.targets[0].versions.len() > MAX_VERSIONS_PER_TARGET {
            if let Some(evicted) = self.targets[0].versions.pop_back() {
                let evicted_len = evicted.content.len();
                self.total_bytes -= evicted_len;
                self.targets[0].bytes -= evicted_len;
            }
        }
        self.evict_to_budget();
        content
    }

    /// Return retained snapshots for a key in newest-first order.
    ///
    /// Returned arcs keep the snapshot bytes alive without copying their
    /// contents. Lookup does not change the global target LRU position.
    pub(super) fn snapshots(&self, key: &StateKey) -> Vec<Arc<str>> {
        let Some(index) = self.find_target(key) else {
            return Vec::new();
        };
        self.targets[index]
            .versions
            .iter()
            .map(|version| Arc::clone(&version.content))
            .collect()
    }

    #[cfg(test)]
    /// Return the newest retained snapshot for a key, if any.
    #[must_use]
    pub(super) fn latest(&self, key: &StateKey) -> Option<Arc<str>> {
        let index = self.find_target(key)?;
        self.targets[index]
            .versions
            .front()
            .map(|version| Arc::clone(&version.content))
    }

    #[cfg(test)]
    /// Return the number of currently retained target entries.
    #[must_use]
    pub(super) fn target_count(&self) -> usize {
        self.targets.len()
    }

    #[cfg(test)]
    /// Return the number of retained versions for one key.
    #[must_use]
    pub(super) fn version_count(&self, key: &StateKey) -> usize {
        self.find_target(key)
            .map_or(0, |index| self.targets[index].versions.len())
    }

    #[cfg(test)]
    /// Return the globally accounted snapshot bytes.
    #[must_use]
    pub(super) const fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    #[cfg(test)]
    /// Hash a normalized edit payload for bounded duplicate guards.
    #[must_use]
    pub(super) fn digest_payload(payload: &[u8]) -> [u8; 32] {
        digest_bytes(payload)
    }

    /// Check whether a payload repeats while its post-edit content is current.
    ///
    /// The caller supplies the live normalized text. Comparing its digest with
    /// the stored post-edit digest lets external file changes invalidate the
    /// guard without retaining a second copy of file content. This lookup does
    /// not change the global target LRU position.
    pub(super) fn guard_payload(
        &self,
        key: &StateKey,
        payload_digest: [u8; 32],
        current_content: &str,
    ) -> bool {
        let Some(index) = self.find_target(key) else {
            return false;
        };
        let current_digest = digest_bytes(current_content.as_bytes());
        self.targets[index]
            .applied_payload
            .as_ref()
            .is_some_and(|guard| {
                guard.payload_digest == payload_digest && guard.post_edit_digest == current_digest
            })
    }

    /// Record a successful normalized payload against its post-edit content.
    ///
    /// The payload and content are represented only by fixed-size digests. A
    /// successful mutation also starts a fresh no-op sequence for the target.
    pub(super) fn record_payload(
        &mut self,
        key: StateKey,
        payload_digest: [u8; 32],
        post_edit_content: &str,
    ) {
        self.ensure_target(&key);
        let Some(index) = self.find_target(&key) else {
            return;
        };
        self.targets[index].applied_payload = Some(AppliedPayload {
            payload_digest,
            post_edit_digest: digest_bytes(post_edit_content.as_bytes()),
        });
        self.targets[index].no_op = None;
    }

    /// Observe one no-op payload and enforce the two-soft-attempt threshold.
    pub(super) fn observe_noop(&mut self, key: StateKey, payload_digest: [u8; 32]) -> NoOpOutcome {
        self.ensure_target(&key);
        let Some(index) = self.find_target(&key) else {
            return NoOpOutcome::Allowed;
        };
        if let Some(sequence) = self.targets[index].no_op.as_mut()
            && sequence.payload_digest == payload_digest
        {
            sequence.count = sequence.count.saturating_add(1);
            return if sequence.count >= 3 {
                NoOpOutcome::RejectLoop
            } else {
                NoOpOutcome::Allowed
            };
        }

        self.targets[index].no_op = Some(NoOpSequence {
            payload_digest,
            count: 1,
        });
        NoOpOutcome::Allowed
    }

    #[cfg(test)]
    /// Clear the no-op sequence for a target without creating a new entry.
    pub(super) fn clear_noop(&mut self, key: &StateKey) {
        let Some(index) = self.find_target(key) else {
            return;
        };
        self.targets[index].no_op = None;
        self.remove_if_empty(index);
    }

    /// Clear duplicate and no-op markers after a non-raw read.
    ///
    /// Snapshot history remains available for stale recovery; only markers that
    /// describe a prior operation are reset.
    pub(super) fn reset_after_non_raw_read(&mut self, key: &StateKey) {
        let Some(index) = self.find_target(key) else {
            return;
        };
        self.targets[index].applied_payload = None;
        self.targets[index].no_op = None;
        self.remove_if_empty(index);
    }

    /// Find a target entry by its complete isolation key.
    #[must_use]
    fn find_target(&self, key: &StateKey) -> Option<usize> {
        self.targets.iter().position(|target| target.key == *key)
    }

    /// Promote one existing target entry to the newest LRU position.
    fn promote_target(&mut self, index: usize) {
        if index != 0 {
            let target = self.targets.remove(index);
            self.targets.insert(0, target);
        }
    }

    /// Ensure a key has a target entry without changing an existing entry's LRU position.
    fn ensure_target(&mut self, key: &StateKey) {
        if self.find_target(key).is_some() {
            return;
        }

        self.targets.insert(0, TargetState::new(key.clone()));
        if self.targets.len() > MAX_TARGETS
            && let Some(evicted) = self.targets.pop()
        {
            self.total_bytes -= evicted.byte_count();
        }
    }

    /// Touch a key, creating a bounded target entry when necessary.
    fn touch_target(&mut self, key: &StateKey) {
        if let Some(index) = self.find_target(key) {
            self.promote_target(index);
            return;
        }

        self.ensure_target(key);
    }

    /// Evict oldest retained versions until the global byte budget is met.
    fn evict_to_budget(&mut self) {
        while self.total_bytes > MAX_TOTAL_SNAPSHOT_BYTES {
            let mut evicted = false;
            for index in (0..self.targets.len()).rev() {
                if let Some(version) = self.targets[index].versions.pop_back() {
                    let version_len = version.content.len();
                    self.total_bytes -= version_len;
                    self.targets[index].bytes -= version_len;
                    self.remove_if_empty(index);
                    evicted = true;
                    break;
                }
            }
            if !evicted {
                // The accounting invariant makes this unreachable. Keeping the
                // loop bounded is safer than spinning if a future change breaks
                // that invariant.
                self.total_bytes = 0;
                break;
            }
        }
    }

    /// Remove an entry after its snapshots and guards have all been cleared.
    fn remove_if_empty(&mut self, index: usize) {
        if self.targets[index].is_empty() {
            self.targets.remove(index);
        }
    }
}

impl Default for SnapshotState {
    /// Create the default empty bounded snapshot state.
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed-array async mutation locks selected by filesystem identity.
pub(super) struct MutationLockShards {
    shards: [Mutex<()>; MUTATION_LOCK_SHARD_COUNT],
}

impl MutationLockShards {
    /// Create all lock shards eagerly with a compile-time bounded array.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            shards: std::array::from_fn(|_| Mutex::new(())),
        }
    }

    #[cfg(test)]
    /// Select the fixed shard assigned to a compatibility state key.
    #[must_use]
    pub(super) fn shard_index(&self, key: &StateKey) -> usize {
        (key.shard_hash() % MUTATION_LOCK_SHARD_COUNT as u64) as usize
    }

    /// Select the fixed shard assigned to a filesystem mutation identity.
    #[must_use]
    pub(super) fn shard_index_for_identity(&self, identity: &MutationIdentity) -> usize {
        (identity.shard_hash() % MUTATION_LOCK_SHARD_COUNT as u64) as usize
    }

    #[cfg(test)]
    /// Await the mutation lock for a compatibility state key.
    pub(super) async fn lock_for(&self, key: &StateKey) -> MutexGuard<'_, ()> {
        self.shards[self.shard_index(key)].lock().await
    }

    /// Await an identity lock while observing caller cancellation.
    ///
    /// # Parameters
    /// - `identity`: Session-independent filesystem identity for the target.
    /// - `cancel`: Token that aborts a wait before the lock is acquired.
    ///
    /// # Returns
    /// The held shard guard, or [`LockError::Cancelled`] when cancelled first.
    pub(super) async fn lock_for_identity(
        &self,
        identity: &MutationIdentity,
        cancel: &CancellationToken,
    ) -> Result<MutexGuard<'_, ()>, LockError> {
        if cancel.is_cancelled() {
            return Err(LockError::Cancelled);
        }
        let lock = &self.shards[self.shard_index_for_identity(identity)];
        tokio::select! {
            _ = cancel.cancelled() => Err(LockError::Cancelled),
            guard = lock.lock() => Ok(guard),
        }
    }
}

impl Default for MutationLockShards {
    /// Create the default fixed mutation-lock shard array.
    fn default() -> Self {
        Self::new()
    }
}

/// Normalize a path lexically without resolving filesystem links.
fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    let mut rooted = false;
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => {
                normalized.push(component.as_os_str());
                rooted = true;
            }
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop_normal = normalized
                    .components()
                    .next_back()
                    .is_some_and(|last| matches!(last, Component::Normal(_)));
                if can_pop_normal {
                    normalized.pop();
                } else if !rooted {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

/// Compute a fixed-size digest for a payload or normalized text.
fn digest_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Construct a compact key for state tests.
    fn test_key(session: Option<SessionId>, workdir: &str, target: &str) -> StateKey {
        StateKey::new(session, PathBuf::from(workdir), PathBuf::from(target))
    }

    /// Prove target, version, byte, and equal-version bounds.
    #[test]
    fn snapshot_limits_and_identical_versions() {
        let mut state = SnapshotState::new();
        for index in 0..=MAX_TARGETS {
            let key = test_key(None, "/work", &format!("/work/file-{index}"));
            state.remember(key, Arc::<str>::from("v"));
        }
        assert_eq!(state.target_count(), MAX_TARGETS);

        let key = test_key(None, "/work", "/work/versions");
        for index in 0..=MAX_VERSIONS_PER_TARGET {
            state.remember(key.clone(), Arc::<str>::from(format!("version-{index}")));
        }
        assert_eq!(state.version_count(&key), MAX_VERSIONS_PER_TARGET);

        let original = Arc::<str>::from("fusion");
        let canonical = state.remember(key.clone(), Arc::clone(&original));
        let fused = state.remember(key.clone(), Arc::<str>::from("fusion"));
        assert!(Arc::ptr_eq(&original, &canonical));
        assert!(Arc::ptr_eq(&original, &fused));
        assert_eq!(state.version_count(&key), MAX_VERSIONS_PER_TARGET);
        assert!(state.total_bytes() <= MAX_TOTAL_SNAPSHOT_BYTES);

        let mut byte_state = SnapshotState::new();
        let half_plus_one = MAX_TOTAL_SNAPSHOT_BYTES / 2 + 1;
        let first = Arc::<str>::from("a".repeat(half_plus_one));
        let second = Arc::<str>::from("b".repeat(half_plus_one));
        byte_state.remember(test_key(None, "/bytes", "/bytes/one"), Arc::clone(&first));
        byte_state.remember(test_key(None, "/bytes", "/bytes/two"), Arc::clone(&second));
        assert!(byte_state.total_bytes() <= MAX_TOTAL_SNAPSHOT_BYTES);

        let mut oversized_state = SnapshotState::new();
        let oversized = Arc::<str>::from("x".repeat(MAX_TOTAL_SNAPSHOT_BYTES + 1));
        let oversized_key = test_key(None, "/large", "/large/file");
        oversized_state.remember(oversized_key.clone(), oversized);
        assert_eq!(oversized_state.total_bytes(), 0);
        assert_eq!(oversized_state.version_count(&oversized_key), 0);
    }

    /// Prove only the current newest version fuses with an incoming snapshot.
    #[test]
    fn only_front_identical_version_fuses() {
        let key = test_key(None, "/work", "/work/file");
        let mut state = SnapshotState::new();
        for value in ["oldest", "older", "middle", "newest"] {
            state.remember(key.clone(), Arc::<str>::from(value));
        }
        let before = state.total_bytes();

        let older_duplicate = Arc::<str>::from("older");
        let returned = state.remember(key.clone(), Arc::clone(&older_duplicate));
        let versions = state.snapshots(&key);
        assert_eq!(versions.len(), MAX_VERSIONS_PER_TARGET);
        assert_eq!(versions[0].as_ref(), "older");
        assert_eq!(versions[1].as_ref(), "newest");
        assert_eq!(versions[2].as_ref(), "middle");
        assert_eq!(versions[3].as_ref(), "older");
        assert_eq!(state.total_bytes(), before + "older".len() - "oldest".len());
        assert!(Arc::ptr_eq(&returned, &older_duplicate));

        let front = state.remember(key.clone(), Arc::<str>::from("older"));
        assert!(Arc::ptr_eq(&front, &versions[0]));
        assert_eq!(state.total_bytes(), before + "older".len() - "oldest".len());
    }

    /// Prove snapshots, latest, and payload guards do not promote the LRU key.
    #[test]
    fn pure_lookups_do_not_promote_target_lru() {
        let keys: Vec<_> = (0..MAX_TARGETS)
            .map(|index| test_key(None, "/work", &format!("/work/file-{index}")))
            .collect();
        let oldest = keys[0].clone();
        let mut state = SnapshotState::new();
        for key in &keys {
            state.remember(key.clone(), Arc::<str>::from("snapshot"));
        }

        assert_eq!(state.snapshots(&oldest).len(), 1);
        assert_eq!(state.latest(&oldest).as_deref(), Some("snapshot"));
        let payload = SnapshotState::digest_payload(b"payload");
        state.record_payload(oldest.clone(), payload, "post-edit");
        assert!(state.guard_payload(&oldest, payload, "post-edit"));

        let newcomer = test_key(None, "/work", "/work/newcomer");
        state.remember(newcomer, Arc::<str>::from("new"));
        assert!(state.snapshots(&oldest).is_empty());
    }

    /// Prove complete session/workdir/target isolation and path normalization.
    #[test]
    fn snapshots_are_isolated_by_complete_state_key() {
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        let normalized = test_key(
            Some(session_a),
            "/work/project/./",
            "/work/project/src/../main.rs",
        );
        let equivalent = test_key(Some(session_a), "/work/project", "/work/project/main.rs");
        assert_eq!(normalized, equivalent);

        let other_session = test_key(Some(session_b), "/work/project", "/work/project/main.rs");
        let other_workdir = test_key(Some(session_a), "/work/other", "/work/project/main.rs");
        let other_target = test_key(Some(session_a), "/work/project", "/work/project/other.rs");

        let mut state = SnapshotState::new();
        state.remember(normalized.clone(), Arc::<str>::from("private"));
        let snapshots = state.snapshots(&equivalent);
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].as_ref(), "private");
        assert!(state.snapshots(&other_session).is_empty());
        assert!(state.snapshots(&other_workdir).is_empty());
        assert!(state.snapshots(&other_target).is_empty());
    }

    /// Prove payload guards, no-op thresholds, and non-raw reset behavior.
    #[test]
    fn guards_reset_after_non_raw_read() {
        let key = test_key(None, "/work", "/work/file");
        let payload = SnapshotState::digest_payload(b"replace:payload");
        let mut state = SnapshotState::new();
        state.record_payload(key.clone(), payload, "after");
        assert!(state.guard_payload(&key, payload, "after"));
        assert!(!state.guard_payload(&key, payload, "external-change"));

        assert_eq!(
            state.observe_noop(key.clone(), payload),
            NoOpOutcome::Allowed
        );
        assert_eq!(
            state.observe_noop(key.clone(), payload),
            NoOpOutcome::Allowed
        );
        assert_eq!(
            state.observe_noop(key.clone(), payload),
            NoOpOutcome::RejectLoop
        );
        state.clear_noop(&key);
        assert_eq!(
            state.observe_noop(key.clone(), payload),
            NoOpOutcome::Allowed
        );

        state.reset_after_non_raw_read(&key);
        assert!(!state.guard_payload(&key, payload, "after"));
        assert_eq!(state.observe_noop(key, payload), NoOpOutcome::Allowed);
    }

    /// Prove equivalent keys select one fixed async lock shard.
    #[tokio::test]
    async fn same_key_selects_same_fixed_shard() {
        let locks = MutationLockShards::new();
        let first = test_key(None, "/work/./project", "/work/project/../project/file");
        let equivalent = test_key(None, "/work/project", "/work/project/file");
        let first_index = locks.shard_index(&first);
        assert_eq!(first_index, locks.shard_index(&equivalent));
        assert!(first_index < MUTATION_LOCK_SHARD_COUNT);
        let guard = locks.lock_for(&first).await;
        drop(guard);
    }
}
