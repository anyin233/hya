//! Private native hashline runtime shared by filesystem coding tools.
//!
//! This module is intentionally not exported from `hya-tool`. The source-derived
//! behavior follows `pi-hashline-edit` 0.8.3 at git head
//! `ba7db9943d0f58499b24c1f6bd64722580f772a5` (MIT, tarball SHA-1
//! `8985f24c3493be375cc225a5522ed54de8daabc9`). Adapters own permissions,
//! events, and wire JSON; this module owns only pure hashline preparation,
//! bounded process-local state, and serialized filesystem mutations.

mod apply;
mod fs;
mod hash;
mod merge;
mod state;

use self::apply::ApplyOutcome;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use hya_proto::SessionId;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::MutexGuard;
use tokio_util::sync::CancellationToken;

/// Maximum UTF-8 bytes retained in one hashline diagnostic message.
const HASHLINE_ERROR_MESSAGE_BYTES: usize = 8 * 1024;
/// Maximum UTF-8 bytes retained in one hashline hint row.
const HASHLINE_ERROR_HINT_BYTES: usize = 512;
/// Maximum number of hint rows retained in one hashline error.
const HASHLINE_ERROR_HINT_COUNT: usize = 16;
/// Content-free marker appended when a diagnostic is bounded.
const HASHLINE_TRUNCATION_MARKER: &str = " ... [truncated]";

/// Bound text at a UTF-8 character boundary and mark omitted content.
///
/// # Parameters
/// - `value`: Text that may exceed the diagnostic budget.
/// - `max_bytes`: Maximum number of UTF-8 bytes to retain.
///
/// # Returns
/// A prefix with a content-free truncation marker when bytes were omitted.
fn bound_utf8_text(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    if max_bytes == 0 {
        return String::new();
    }
    let marker_bytes = HASHLINE_TRUNCATION_MARKER.len().min(max_bytes);
    if marker_bytes == max_bytes {
        return HASHLINE_TRUNCATION_MARKER[..marker_bytes].to_owned();
    }
    let mut prefix_bytes = max_bytes - marker_bytes;
    while prefix_bytes > 0 && !value.is_char_boundary(prefix_bytes) {
        prefix_bytes -= 1;
    }
    let mut bounded = String::with_capacity(max_bytes);
    bounded.push_str(&value[..prefix_bytes]);
    bounded.push_str(&HASHLINE_TRUNCATION_MARKER[..marker_bytes]);
    bounded
}

/// Bound every hint row and cap the number of rows retained.
///
/// # Parameters
/// - `hints`: Candidate recovery rows supplied by hashline matching.
///
/// # Returns
/// At most sixteen UTF-8-safe, content-free bounded hint rows.
fn bound_hint_rows(hints: Vec<String>) -> Vec<String> {
    hints
        .into_iter()
        .take(HASHLINE_ERROR_HINT_COUNT)
        .map(|hint| bound_utf8_text(&hint, HASHLINE_ERROR_HINT_BYTES))
        .collect()
}

/// Stable private error envelope returned by hashline preparation/application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct HashlineError {
    /// Machine-readable stable error code without brackets.
    pub(super) code: &'static str,
    /// Bounded human-readable diagnostic, excluding file contents.
    pub(super) message: String,
    /// Bounded recovery or disambiguation hints.
    pub(super) hints: Vec<String>,
}

impl HashlineError {
    /// Construct a content-free hashline error with no hints.
    pub(super) fn new(code: &'static str, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            code,
            message: bound_utf8_text(&message, HASHLINE_ERROR_MESSAGE_BYTES),
            hints: Vec::new(),
        }
    }

    /// Construct an error with bounded hints supplied by a caller.
    pub(super) fn with_hints(
        code: &'static str,
        message: impl Into<String>,
        hints: Vec<String>,
    ) -> Self {
        let message = message.into();
        Self {
            code,
            message: bound_utf8_text(&message, HASHLINE_ERROR_MESSAGE_BYTES),
            hints: bound_hint_rows(hints),
        }
    }

    /// Return the model-facing diagnostic with its stable bracketed code.
    pub(super) fn diagnostic(&self) -> String {
        let code_prefix = format!("[{}]", self.code);
        if self.message == code_prefix
            || self
                .message
                .strip_prefix(&code_prefix)
                .is_some_and(|rest| rest.starts_with(' '))
        {
            self.message.clone()
        } else {
            format!("{code_prefix} {}", self.message)
        }
    }

    /// Add one bounded recovery note without exposing file contents.
    pub(super) fn with_recovery_note(mut self, note: &str) -> Self {
        if note.is_empty() {
            return self;
        }
        let bounded_note = bound_utf8_text(note, HASHLINE_ERROR_MESSAGE_BYTES);
        let mut message = String::with_capacity(
            self.message
                .len()
                .saturating_add(1)
                .saturating_add(bounded_note.len()),
        );
        if !self.message.is_empty() {
            message.push_str(&self.message);
            message.push(' ');
        }
        message.push_str(&bounded_note);
        self.message = bound_utf8_text(&message, HASHLINE_ERROR_MESSAGE_BYTES);
        self
    }
}

impl fmt::Display for HashlineError {
    /// Render the bounded diagnostic while preserving its stable code.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.diagnostic())
    }
}

impl std::error::Error for HashlineError {}

/// Fixed runtime defaults shared by filesystem coding tools.
pub(super) const DEFAULT_HASH_LENGTH: usize = hash::DEFAULT_HASH_LENGTH;
/// Default maximum number of displayed lines in one text result.
pub(super) const DEFAULT_READ_LIMIT: usize = 2000;
/// Hard inline byte cap for a formatted text result.
pub(super) const MAX_READ_BYTES: usize = 50 * 1024;
/// Maximum Grep files staged before one successful call records snapshots.
pub(super) const MAX_SNAPSHOT_TARGETS: usize = state::MAX_TARGETS;
/// Maximum Grep snapshot bytes staged before one successful call commits state.
pub(super) const MAX_SNAPSHOT_BYTES: usize = state::MAX_TOTAL_SNAPSHOT_BYTES;
/// Maximum lock acquisition attempts when a target identity changes.
const MAX_LOCK_REVALIDATION_ATTEMPTS: usize = 3;

/// Bounded state and fixed mutation-lock holder shared by hashline adapters.
pub(super) struct HashlineRuntime {
    state: Mutex<state::SnapshotState>,
    locks: state::MutationLockShards,
}

/// Controls for one native text Read operation.
///
/// The requested path remains a separate argument because callers authorize it
/// lexically before the runtime resolves the mutation target.
pub(super) struct ReadOptions<'workdir> {
    /// Session scope used for snapshot isolation.
    pub(super) session: Option<SessionId>,
    /// Normalized working directory used in the snapshot identity.
    pub(super) workdir: &'workdir Path,
    /// One-based first line to format.
    pub(super) offset: usize,
    /// Maximum visible lines to format.
    pub(super) limit: usize,
    /// Return raw normalized text instead of hashline anchors.
    pub(super) raw: bool,
    /// Cancellation token checked around runtime I/O and formatting.
    pub(super) cancel: CancellationToken,
}

/// Text and display facts returned by a native Read operation.
pub(super) struct ReadResult {
    /// Lexical path used for display and the result title.
    pub(super) path: PathBuf,
    /// Bounded hashline or raw model output.
    pub(super) output: String,
    /// Unprefixed selected source text.
    pub(super) content: String,
    /// One-based first selected line.
    pub(super) line_start: usize,
    /// One-based last selected line, or zero for an empty selection.
    pub(super) line_end: usize,
    /// Number of visible lines in the complete normalized file.
    pub(super) total_lines: usize,
    /// Whether the result was limited by rows or bytes.
    pub(super) truncated: bool,
    /// Next one-based offset when more content is available.
    pub(super) next_offset: Option<usize>,
    /// Whether the first selected line exceeded the hashline byte budget.
    pub(super) first_line_exceeds_limit: bool,
    /// Bounded decoding and line-ending warnings.
    pub(super) warnings: Vec<String>,
}
/// Normalized text and filesystem facts prepared for one Grep file group.
pub(super) struct GrepText {
    /// Lexical path supplied by the adapter.
    pub(super) requested_path: PathBuf,
    /// Final non-symlink path used for reading and snapshot identity.
    pub(super) path: PathBuf,
    /// Normalized LF text loaded from the target.
    pub(super) text: String,
    /// Number of visible lines available for requested ranges.
    pub(super) total_lines: usize,
    /// Bounded decoding and line-ending warnings.
    pub(super) warnings: Vec<String>,
}

/// Text facts exposed to an adapter while a mutation lock remains held.
pub(super) struct MutationText {
    /// Normalized LF text loaded from the target.
    pub(super) text: String,
    /// Number of final bytes loaded from the filesystem.
    pub(super) bytes: usize,
    /// Whether the loaded target has more than one directory entry.
    pub(super) hard_link: bool,
    /// Bounded decoding and line-ending warnings.
    pub(super) warnings: Vec<String>,
}

/// A strict edit result prepared against one locked live snapshot.
pub(super) struct PreparedEdit {
    /// Normalized live text before this request.
    pub(super) original: String,
    /// Normalized desired text before formatting.
    pub(super) desired: String,
    /// Bounded non-fatal warnings.
    pub(super) warnings: Vec<String>,
    /// Content-free locations for valid no-op operations.
    pub(super) noop_locations: Vec<String>,
    /// Digest of the normalized operation payload.
    pub(super) payload_digest: [u8; 32],
    /// Whether exact historical replay was needed.
    pub(super) recovered: bool,
}

/// Final bounded hashline display region for a successful Edit.
pub(super) struct EditPreview {
    /// Fresh contextual anchors for the changed region.
    pub(super) output: String,
    /// Unprefixed changed-region text.
    pub(super) content: String,
    /// One-based first displayed line.
    pub(super) line_start: usize,
    /// One-based last displayed line.
    pub(super) line_end: usize,
    /// Number of visible lines in the final file.
    pub(super) total_lines: usize,
}

/// Facts returned after one successful atomic write.
pub(super) struct WriteResult {
    /// Resolved target path.
    pub(super) path: PathBuf,
    /// Whether the target was created by the write.
    pub(super) created: bool,
}

/// Failure from Read formatting, filesystem loading, or cancellation.
#[derive(Debug)]
pub(super) enum ReadRuntimeError {
    /// Filesystem loading failed before a result was available.
    Io(io::Error),
    /// Native hashline formatting rejected the request.
    Hashline(HashlineError),
    /// The caller cancelled before Read could commit state.
    Cancelled,
}

impl fmt::Display for ReadRuntimeError {
    /// Render the bounded Read runtime failure.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Hashline(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("operation cancelled"),
        }
    }
}

impl std::error::Error for ReadRuntimeError {}

/// Failure while beginning a cancellable filesystem mutation.
#[derive(Debug)]
pub(super) enum MutationBeginError {
    /// Resolution or identity discovery failed before a lock was held.
    Io(io::Error),
    /// The caller cancelled while waiting for or validating the lock.
    Cancelled,
}

impl fmt::Display for MutationBeginError {
    /// Render the bounded mutation-start failure.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Cancelled => formatter.write_str("operation cancelled"),
        }
    }
}

impl std::error::Error for MutationBeginError {}

/// Failure from an atomic mutation, distinguishing committed bytes.
#[derive(Debug)]
pub(super) enum MutationWriteError {
    /// The write failed before replacement committed.
    Io(io::Error),
    /// Replacement committed but parent-directory synchronization failed.
    Committed {
        /// Resolved path whose bytes are authoritative.
        path: PathBuf,
        /// Synchronization failure after the commit boundary.
        error: io::Error,
    },
}

impl fmt::Display for MutationWriteError {
    /// Render the bounded mutation failure without file contents.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Committed { path, error } => {
                write!(
                    formatter,
                    "write committed at {} but sync failed: {error}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for MutationWriteError {}

/// Opaque strict edit request retained inside the private runtime boundary.
pub(super) struct EditRequest {
    inner: apply::EditRequest,
}

impl EditRequest {
    /// Borrow the caller path used for lexical permission and target resolution.
    pub(super) fn path(&self) -> &str {
        &self.inner.path
    }
}

/// Resolved target and held mutation shard returned by lock acquisition.
struct LockedTarget<'runtime> {
    /// Path facts revalidated while the selected shard was held.
    resolved: fs::ResolvedPath,
    /// Fixed shard guard held through the caller's operation.
    _lock: MutexGuard<'runtime, ()>,
}

impl HashlineRuntime {
    /// Construct an empty runtime with fixed bounded state.
    #[must_use]
    pub(super) fn new() -> Self {
        Self {
            state: Mutex::new(state::SnapshotState::new()),
            locks: state::MutationLockShards::new(),
        }
    }

    /// Parse a strict edit request through the private native boundary.
    pub(super) fn parse_edit_request(&self, input: Value) -> Result<EditRequest, HashlineError> {
        let _ = self;
        apply::parse_edit_request(input).map(|inner| EditRequest { inner })
    }

    fn apply_hashline_edits(
        &self,
        content: &str,
        request: &EditRequest,
    ) -> Result<apply::ApplyOutcome, HashlineError> {
        let _ = self;
        apply::apply_hashline_edits(content, &request.inner)
    }

    /// Load, format, and optionally snapshot one text Read operation.
    pub(super) async fn read_text(
        &self,
        requested_path: &Path,
        options: ReadOptions<'_>,
    ) -> Result<ReadResult, ReadRuntimeError> {
        let ReadOptions {
            session,
            workdir,
            offset,
            limit,
            raw,
            cancel,
        } = options;
        if cancel.is_cancelled() {
            return Err(ReadRuntimeError::Cancelled);
        }
        let locked = self
            .acquire_locked_target(requested_path, &cancel, false)
            .await
            .map_err(|error| match error {
                MutationBeginError::Io(error) => ReadRuntimeError::Io(error),
                MutationBeginError::Cancelled => ReadRuntimeError::Cancelled,
            })?;
        let loaded = fs::load_resolved_text(requested_path, &locked.resolved)
            .await
            .map_err(ReadRuntimeError::Io)?;
        if cancel.is_cancelled() {
            return Err(ReadRuntimeError::Cancelled);
        }
        let formatted =
            format_read(&loaded.text, offset, limit, raw).map_err(ReadRuntimeError::Hashline)?;
        if cancel.is_cancelled() {
            return Err(ReadRuntimeError::Cancelled);
        }
        let key = state::StateKey::new(session, workdir, &loaded.path);
        let warnings = bounded_warnings(loaded.warnings().into_iter().map(str::to_owned));
        if !raw {
            self.reset_and_remember(&key, &loaded.text);
        }
        Ok(ReadResult {
            path: loaded.requested_path,
            output: formatted.output,
            content: formatted.content,
            line_start: formatted.line_start,
            line_end: formatted.line_end,
            total_lines: formatted.total_lines,
            truncated: formatted.truncated,
            next_offset: formatted.next_offset,
            first_line_exceeds_limit: formatted.first_line_exceeds_limit,
            warnings,
        })
    }
    /// Load normalized Grep text through the shared resolved-target policy.
    pub(super) async fn load_text_for_grep(
        &self,
        requested_path: &Path,
        cancel: CancellationToken,
    ) -> Result<GrepText, ReadRuntimeError> {
        if cancel.is_cancelled() {
            return Err(ReadRuntimeError::Cancelled);
        }
        let locked = self
            .acquire_locked_target(requested_path, &cancel, false)
            .await
            .map_err(|error| match error {
                MutationBeginError::Io(error) => ReadRuntimeError::Io(error),
                MutationBeginError::Cancelled => ReadRuntimeError::Cancelled,
            })?;
        let loaded = fs::load_resolved_text(requested_path, &locked.resolved)
            .await
            .map_err(ReadRuntimeError::Io)?;
        if cancel.is_cancelled() {
            return Err(ReadRuntimeError::Cancelled);
        }
        let total_lines = hash::visible_line_count(&loaded.text);
        let warnings = bounded_warnings(loaded.warnings().into_iter().map(str::to_owned));
        Ok(GrepText {
            requested_path: loaded.requested_path,
            path: loaded.path,
            text: loaded.text,
            total_lines,
            warnings,
        })
    }

    /// Format merged one-based Grep ranges with contextual hashline neighbors.
    ///
    /// Ranges must be sorted, inclusive, and already merged by the adapter.
    /// The file is scanned without retaining an all-lines index, and the
    /// cancellation token is checked in bounded byte chunks and for every
    /// logical line.
    pub(super) fn format_hashline_ranges(
        &self,
        text: &str,
        ranges: &[(usize, usize)],
        cancel: &CancellationToken,
    ) -> Result<String, ReadRuntimeError> {
        let _ = self;
        if ranges.is_empty() {
            return Ok(String::new());
        }
        let mut newline_count = 0usize;
        for chunk in text.as_bytes().chunks(64 * 1024) {
            if cancel.is_cancelled() {
                return Err(ReadRuntimeError::Cancelled);
            }
            newline_count =
                newline_count.saturating_add(chunk.iter().filter(|byte| **byte == b'\n').count());
        }
        let total_lines = if text.is_empty() {
            0
        } else {
            newline_count.saturating_add(usize::from(!text.ends_with('\n')))
        };
        let mut previous_end = 0usize;
        for &(start, end) in ranges {
            if start == 0 || end < start || end > total_lines || start <= previous_end {
                return Err(ReadRuntimeError::Hashline(HashlineError::new(
                    "E_RANGE_OOB",
                    format!(
                        "Cannot format line range {start}-{end} for {total_lines} visible lines."
                    ),
                )));
            }
            previous_end = end;
        }

        let mut output = String::new();
        let mut range_index = 0usize;
        let mut previous = "";
        let mut lines = text.split('\n').take(total_lines).peekable();
        for line_number in 1..=total_lines {
            if cancel.is_cancelled() {
                return Err(ReadRuntimeError::Cancelled);
            }
            let Some(current) = lines.next() else {
                return Err(ReadRuntimeError::Hashline(HashlineError::new(
                    "E_INTERNAL",
                    "Grep line scan ended before the validated visible-line count.",
                )));
            };
            let next = lines.peek().copied().unwrap_or("");
            while range_index < ranges.len() && line_number > ranges[range_index].1 {
                range_index += 1;
            }
            if range_index >= ranges.len() {
                break;
            }
            let (start, end) = ranges[range_index];
            if line_number >= start {
                if line_number == start && range_index > 0 {
                    if output.len().saturating_add("\n    ...\n".len()) > MAX_READ_BYTES {
                        return Err(ReadRuntimeError::Hashline(HashlineError::new(
                            "E_OUTPUT_LIMIT",
                            "Grep hashline output exceeded the bounded display budget.",
                        )));
                    }
                    output.push_str("\n    ...\n");
                }
                let hash = hash::compute_hash_from_context_with_width(
                    previous,
                    current,
                    next,
                    DEFAULT_HASH_LENGTH,
                )
                .map_err(ReadRuntimeError::Hashline)?;
                let line_number_width = end.to_string().len();
                let prefix = format!(
                    "{:>line_number_width$}#{hash}:",
                    line_number,
                    line_number_width = line_number_width
                );
                let separator_len = usize::from(line_number > start);
                if output
                    .len()
                    .saturating_add(separator_len)
                    .saturating_add(prefix.len())
                    .saturating_add(current.len())
                    > MAX_READ_BYTES
                {
                    return Err(ReadRuntimeError::Hashline(HashlineError::new(
                        "E_OUTPUT_LIMIT",
                        "Grep hashline output exceeded the bounded display budget.",
                    )));
                }
                if line_number > start {
                    output.push('\n');
                }
                output.push_str(&prefix);
                output.push_str(current);
            }
            previous = current;
        }
        Ok(output)
    }

    /// Record a successfully rendered Grep file snapshot under its state key.
    pub(super) fn remember_grep_snapshot(
        &self,
        session: Option<SessionId>,
        workdir: &Path,
        target: &Path,
        text: &str,
    ) {
        let key = state::StateKey::new(session, workdir, target);
        self.with_state(|state| {
            state.remember(key, text);
        });
    }

    /// Resolve, lock, and revalidate one target with bounded identity retries.
    async fn acquire_locked_target(
        &self,
        requested_path: &Path,
        cancel: &CancellationToken,
        allow_dangling_missing: bool,
    ) -> Result<LockedTarget<'_>, MutationBeginError> {
        for attempt in 0..MAX_LOCK_REVALIDATION_ATTEMPTS {
            if cancel.is_cancelled() {
                return Err(MutationBeginError::Cancelled);
            }
            let resolved = fs::resolve_target(requested_path)
                .await
                .map_err(MutationBeginError::Io)?;
            if resolved.dangling && !allow_dangling_missing {
                return Err(MutationBeginError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    "cannot mutate a dangling symlink target",
                )));
            }
            let identity = fs::mutation_identity(&resolved)
                .await
                .map_err(MutationBeginError::Io)?;
            let lock = self
                .locks
                .lock_for_identity(&identity, cancel)
                .await
                .map_err(|_| MutationBeginError::Cancelled)?;
            if cancel.is_cancelled() {
                drop(lock);
                return Err(MutationBeginError::Cancelled);
            }
            let revalidated = fs::resolve_target(requested_path)
                .await
                .map_err(MutationBeginError::Io)?;
            if revalidated.dangling && !allow_dangling_missing {
                drop(lock);
                return Err(MutationBeginError::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    "cannot mutate a dangling symlink target",
                )));
            }
            let revalidated_identity = fs::mutation_identity(&revalidated)
                .await
                .map_err(MutationBeginError::Io)?;
            if identity == revalidated_identity {
                return Ok(LockedTarget {
                    resolved: revalidated,
                    _lock: lock,
                });
            }
            drop(lock);
            if attempt + 1 == MAX_LOCK_REVALIDATION_ATTEMPTS {
                return Err(MutationBeginError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "target identity changed while acquiring mutation lock",
                )));
            }
        }
        Err(MutationBeginError::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            "target identity changed while acquiring mutation lock",
        )))
    }

    /// Resolve one requested target and acquire its fixed mutation lock.
    pub(super) async fn begin_mutation(
        &self,
        requested_path: &Path,
        session: Option<SessionId>,
        workdir: &Path,
        cancel: CancellationToken,
    ) -> Result<HashlineMutation<'_>, MutationBeginError> {
        let locked = self
            .acquire_locked_target(requested_path, &cancel, false)
            .await?;
        let key = state::StateKey::new(session, workdir, &locked.resolved.path);
        Ok(HashlineMutation {
            runtime: self,
            key,
            requested_path: requested_path.to_path_buf(),
            target: locked.resolved.path.clone(),
            resolved: locked.resolved,
            _lock: locked._lock,
            original: None,
        })
    }

    /// Resolve and lock a Write target, including a missing symlink referent.
    pub(super) async fn begin_write(
        &self,
        requested_path: &Path,
        session: Option<SessionId>,
        workdir: &Path,
        cancel: CancellationToken,
    ) -> Result<HashlineMutation<'_>, MutationBeginError> {
        let locked = self
            .acquire_locked_target(requested_path, &cancel, true)
            .await?;
        let key = state::StateKey::new(session, workdir, &locked.resolved.path);
        Ok(HashlineMutation {
            runtime: self,
            key,
            requested_path: requested_path.to_path_buf(),
            target: locked.resolved.path.clone(),
            resolved: locked.resolved,
            _lock: locked._lock,
            original: None,
        })
    }

    /// Execute one short state operation while recovering a poisoned mutex.
    fn with_state<T>(&self, operation: impl FnOnce(&mut state::SnapshotState) -> T) -> T {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operation(&mut state)
    }

    /// Clear edit guards and retain one normalized non-raw Read snapshot.
    fn reset_and_remember(&self, key: &state::StateKey, content: &str) {
        self.with_state(|state| {
            state.reset_after_non_raw_read(key);
            state.remember(key.clone(), content);
        });
    }

    /// Return newest-first snapshots for one complete isolated target key.
    fn snapshots(&self, key: &state::StateKey) -> Vec<Arc<str>> {
        self.with_state(|state| state.snapshots(key))
    }

    /// Return whether one payload is a successful duplicate on current bytes.
    fn guard_payload(
        &self,
        key: &state::StateKey,
        payload_digest: [u8; 32],
        content: &str,
    ) -> bool {
        self.with_state(|state| state.guard_payload(key, payload_digest, content))
    }

    /// Observe one no-op payload and return its bounded loop decision.
    fn observe_noop(&self, key: state::StateKey, payload_digest: [u8; 32]) -> state::NoOpOutcome {
        self.with_state(|state| state.observe_noop(key, payload_digest))
    }

    /// Record original/final snapshots and the successful payload digest.
    fn record_final(
        &self,
        key: &state::StateKey,
        original: &str,
        final_text: &str,
        digest: [u8; 32],
    ) {
        self.with_state(|state| {
            state.remember(key.clone(), original);
            state.remember(key.clone(), final_text);
            state.record_payload(key.clone(), digest, final_text);
        });
    }
    /// Remember Write's original and final text without a payload guard.
    fn record_write(&self, key: &state::StateKey, original: &str, final_text: &str) {
        self.with_state(|state| {
            state.reset_after_non_raw_read(key);
            state.remember(key.clone(), original);
            state.remember(key.clone(), final_text);
        });
    }
}

impl Default for HashlineRuntime {
    /// Construct the default empty hashline runtime.
    fn default() -> Self {
        Self::new()
    }
}

/// Locked filesystem mutation session used by the Edit and Write adapters.
pub(super) struct HashlineMutation<'runtime> {
    runtime: &'runtime HashlineRuntime,
    key: state::StateKey,
    requested_path: PathBuf,
    target: PathBuf,
    resolved: fs::ResolvedPath,
    _lock: MutexGuard<'runtime, ()>,
    original: Option<fs::LoadedText>,
}

impl<'runtime> HashlineMutation<'runtime> {
    /// Load the resolved target and retain its preservation facts.
    pub(super) async fn load_current(&mut self) -> io::Result<MutationText> {
        let loaded = fs::load_resolved_text(&self.requested_path, &self.resolved).await?;
        if loaded.path != self.target {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "edit target changed while resolving symlinks",
            ));
        }
        self.original = Some(loaded.clone());
        Ok(mutation_text(&loaded))
    }

    /// Load an existing target or initialize empty facts for a Write create.
    pub(super) async fn load_current_or_empty(&mut self) -> io::Result<MutationText> {
        let loaded = match fs::load_resolved_text(&self.requested_path, &self.resolved).await {
            Ok(loaded) => loaded,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut empty = fs::LoadedText::new(&self.target);
                empty.requested_path = self.requested_path.clone();
                empty.path = self.target.clone();
                empty.symlink_hops = self.resolved.symlink_hops;
                empty.followed_symlink = self.resolved.followed_symlink;
                empty.dangling = self.resolved.dangling;
                empty
            }
            Err(error) => return Err(error),
        };
        self.original = Some(loaded.clone());
        Ok(mutation_text(&loaded))
    }

    /// Validate, apply, and recover one strict request against live bytes.
    pub(super) fn prepare(&self, request: &EditRequest) -> Result<PreparedEdit, HashlineError> {
        let Some(loaded) = self.original.as_ref() else {
            return Err(HashlineError::new(
                "E_INTERNAL",
                "edit target was not loaded before applying the request",
            ));
        };
        let payload_digest = digest_edit_request(request);
        if self
            .runtime
            .guard_payload(&self.key, payload_digest, &loaded.text)
        {
            return Err(HashlineError::new(
                "E_DUPLICATE_EDIT",
                "This exact edit payload was already applied while the file remained unchanged.",
            ));
        }

        let mut recovered = false;
        let outcome = match self.runtime.apply_hashline_edits(&loaded.text, request) {
            Ok(outcome) => outcome,
            Err(stale) if stale.code == "E_STALE_ANCHOR" => {
                let (outcome, did_recover) = self.recover_stale(request, &loaded.text, stale)?;
                recovered = did_recover;
                outcome
            }
            Err(error) => return Err(error),
        };

        let mut warnings = bounded_warnings(loaded.warnings().into_iter().map(str::to_owned));
        warnings.extend(bound_warning_list(outcome.warnings));
        if recovered {
            warnings.push(
                "Recovered stale anchors by replaying the edit against a compatible recent read."
                    .to_string(),
            );
        }
        warnings = bounded_warnings(warnings);

        let mut noop_locations = outcome
            .noop_edits
            .into_iter()
            .map(|noop| noop.location)
            .collect::<Vec<_>>();
        noop_locations.truncate(16);
        if outcome.content == loaded.text
            && self.runtime.observe_noop(self.key.clone(), payload_digest)
                == state::NoOpOutcome::RejectLoop
        {
            return Err(HashlineError::new(
                "E_NOOP_LOOP",
                "Repeated no-op edits detected; re-read the file before sending another edit.",
            ));
        }

        Ok(PreparedEdit {
            original: loaded.text.clone(),
            desired: outcome.content,
            warnings,
            noop_locations,
            payload_digest,
            recovered,
        })
    }

    /// Replay a stale request newest-first and merge the first exact candidate.
    fn recover_stale(
        &self,
        request: &EditRequest,
        live: &str,
        stale: HashlineError,
    ) -> Result<(ApplyOutcome, bool), HashlineError> {
        const NO_HISTORY_NOTE: &str = "(Your anchors do not match any recent read of this file — they may be from a stale context or copied incorrectly. Re-read before editing.)";
        const CONFLICT_NOTE: &str = "(Recovery attempted: your anchors match an older read of this file, but replaying that edit conflicts with changes made since. Re-read to get current anchors.)";

        let mut candidate_applied = false;
        for base in self.runtime.snapshots(&self.key) {
            let Ok(mut candidate) = self.runtime.apply_hashline_edits(&base, request) else {
                continue;
            };
            candidate_applied = true;
            let Ok(merged) = merge::merge(&base, &candidate.content, live) else {
                continue;
            };
            let changed = hash::compute_changed_line_range(live, &merged);
            candidate.first_changed_line = changed.map(|range| range.0);
            candidate.last_changed_line = changed.map(|range| range.1);
            candidate.content = merged;
            return Ok((candidate, true));
        }
        let note = if candidate_applied {
            CONFLICT_NOTE
        } else {
            NO_HISTORY_NOTE
        };
        Err(stale.with_recovery_note(note))
    }

    /// Refresh the target identity after bytes have crossed the commit boundary.
    async fn refresh_after_commit(
        &mut self,
        outcome: &fs::WriteOutcome,
    ) -> Result<(), MutationWriteError> {
        let Some(facts) = self.original.as_mut() else {
            return Err(MutationWriteError::Committed {
                path: outcome.path.clone(),
                error: io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "mutation facts were unavailable after a committed write",
                ),
            });
        };
        fs::refresh_loaded_identity(facts)
            .await
            .map_err(|error| MutationWriteError::Committed {
                path: outcome.path.clone(),
                error,
            })
    }

    /// Write normalized text using the original target's BOM and line ending.
    pub(super) async fn commit(&mut self, text: &str) -> Result<WriteResult, MutationWriteError> {
        let outcome = {
            let Some(facts) = self.original.as_ref() else {
                return Err(MutationWriteError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "edit target was not loaded before committing",
                )));
            };
            match fs::atomic_write(&self.requested_path, text, facts).await {
                Ok(outcome) => outcome,
                Err(fs::AtomicWriteError::Io(error)) => {
                    return Err(MutationWriteError::Io(error));
                }
                Err(fs::AtomicWriteError::Committed { outcome, error }) => {
                    return Err(MutationWriteError::Committed {
                        path: outcome.path,
                        error,
                    });
                }
            }
        };
        self.refresh_after_commit(&outcome).await?;
        Ok(WriteResult {
            path: outcome.path,
            created: outcome.created,
        })
    }

    /// Commit Write content while merging one incoming BOM with source facts.
    pub(super) async fn commit_write(
        &mut self,
        content: &str,
    ) -> Result<WriteResult, MutationWriteError> {
        let (incoming_bom, content) = content
            .strip_prefix('\u{feff}')
            .map_or((false, content), |stripped| (true, stripped));
        let outcome = {
            let Some(facts) = self.original.as_mut() else {
                return Err(MutationWriteError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "write target was not loaded before committing",
                )));
            };
            facts.bom |= incoming_bom;
            match fs::atomic_write(&self.requested_path, content, facts).await {
                Ok(outcome) => outcome,
                Err(fs::AtomicWriteError::Io(error)) => {
                    return Err(MutationWriteError::Io(error));
                }
                Err(fs::AtomicWriteError::Committed { outcome, error }) => {
                    return Err(MutationWriteError::Committed {
                        path: outcome.path,
                        error,
                    });
                }
            }
        };
        self.refresh_after_commit(&outcome).await?;
        Ok(WriteResult {
            path: outcome.path,
            created: outcome.created,
        })
    }

    /// Restore the original BOM and line-ending style after formatter output.
    pub(super) async fn restore_after_formatter(&mut self) -> Result<(), MutationWriteError> {
        let outcome = {
            let Some(original) = self.original.as_ref() else {
                return Err(MutationWriteError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "edit target was not loaded before formatter synchronization",
                )));
            };
            let current = fs::load_resolved_text(&self.target, &self.resolved)
                .await
                .map_err(MutationWriteError::Io)?;
            if current.path != self.target {
                return Err(MutationWriteError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "edit target changed while restoring formatter output",
                )));
            }
            if current.bom == original.bom
                && current.ending == original.ending
                && !current.mixed_endings
                && !current.invalid_utf8
            {
                return Ok(());
            }
            match fs::atomic_write(&self.requested_path, &current.text, original).await {
                Ok(outcome) => outcome,
                Err(fs::AtomicWriteError::Io(error)) => {
                    return Err(MutationWriteError::Io(error));
                }
                Err(fs::AtomicWriteError::Committed { outcome, error }) => {
                    return Err(MutationWriteError::Committed {
                        path: outcome.path,
                        error,
                    });
                }
            }
        };
        self.refresh_after_commit(&outcome).await
    }

    /// Reload final bytes without changing state, for formatter and LSP order.
    pub(super) async fn reload(&mut self) -> io::Result<MutationText> {
        let loaded = fs::load_resolved_text(&self.target, &self.resolved).await?;
        if loaded.path != self.target {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "edit target changed while reloading final bytes",
            ));
        }
        Ok(mutation_text(&loaded))
    }

    /// Reload final bytes and reconcile state after a committed failure.
    pub(super) async fn reload_and_record(
        &mut self,
        payload_digest: [u8; 32],
    ) -> io::Result<MutationText> {
        let final_text = self.reload().await?;
        self.record_final(&final_text.text, payload_digest);
        Ok(final_text)
    }

    /// Record final snapshots and the successful duplicate guard.
    pub(super) fn record_final(&self, final_text: &str, payload_digest: [u8; 32]) {
        let Some(original) = self.original.as_ref() else {
            return;
        };
        self.runtime
            .record_final(&self.key, &original.text, final_text, payload_digest);
    }
    /// Record Write's original and final snapshots without an edit digest.
    pub(super) fn record_write(&self, final_text: &MutationText) {
        let Some(original) = self.original.as_ref() else {
            return;
        };
        self.runtime
            .record_write(&self.key, &original.text, &final_text.text);
    }

    /// Produce a fresh bounded contextual anchor region for final bytes.
    pub(super) fn preview(
        &self,
        original: &str,
        final_text: &str,
    ) -> Result<Option<EditPreview>, HashlineError> {
        let Some((first, last)) = hash::compute_changed_line_range(original, final_text) else {
            return Ok(None);
        };
        let lines = hash::visible_line_slices(final_text);
        let Some((start, end)) = compute_affected_line_range(Some(first), Some(last), lines.len())
        else {
            return Ok(None);
        };
        let output =
            hash::format_hashline_region_with_width(&lines, start, end, DEFAULT_HASH_LENGTH)?;
        Ok(Some(EditPreview {
            output,
            content: lines[start - 1..end].join("\n"),
            line_start: start,
            line_end: end,
            total_lines: lines.len(),
        }))
    }

    /// Return the resolved target path while the mutation remains locked.
    pub(super) fn target_path(&self) -> &Path {
        &self.target
    }
}

/// Convert loaded filesystem facts into adapter-safe text facts.
fn mutation_text(loaded: &fs::LoadedText) -> MutationText {
    MutationText {
        text: loaded.text.clone(),
        bytes: loaded.bytes,
        hard_link: loaded.is_hard_link(),
        warnings: bounded_warnings(loaded.warnings().into_iter().map(str::to_owned)),
    }
}

/// Return a deterministic digest for normalized edit operations.
fn digest_edit_request(request: &EditRequest) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"hya.hashline.edit/v1\0");
    for edit in &request.inner.edits {
        match edit {
            apply::HashlineEdit::Replace { pos, end, lines } => {
                digest.update(b"replace\0");
                digest_anchor(&mut digest, pos);
                if let Some(end) = end {
                    digest.update(b"end\0");
                    digest_anchor(&mut digest, end);
                } else {
                    digest.update(b"no-end\0");
                }
                digest_lines(&mut digest, lines);
            }
            apply::HashlineEdit::Append { pos, lines } => {
                digest.update(b"append\0");
                if let Some(pos) = pos {
                    digest_anchor(&mut digest, pos);
                } else {
                    digest.update(b"eof\0");
                }
                digest_lines(&mut digest, lines);
            }
            apply::HashlineEdit::Prepend { pos, lines } => {
                digest.update(b"prepend\0");
                if let Some(pos) = pos {
                    digest_anchor(&mut digest, pos);
                } else {
                    digest.update(b"bof\0");
                }
                digest_lines(&mut digest, lines);
            }
            apply::HashlineEdit::ReplaceText { old_text, new_text } => {
                digest.update(b"replace-text\0");
                digest_field(&mut digest, old_text.as_bytes());
                digest_field(&mut digest, new_text.as_bytes());
            }
        }
    }
    digest.finalize().into()
}

/// Add one length-delimited field to the payload digest.
fn digest_field(digest: &mut Sha256, field: &[u8]) {
    digest.update((field.len() as u64).to_be_bytes());
    digest.update(field);
}

/// Add one parsed anchor and hint to the payload digest.
fn digest_anchor(digest: &mut Sha256, anchor: &hash::Anchor) {
    digest.update((anchor.line as u64).to_be_bytes());
    digest_field(digest, anchor.hash.as_bytes());
    if let Some(hint) = &anchor.text_hint {
        digest.update(b"hint\0");
        digest_field(digest, hint.as_bytes());
    } else {
        digest.update(b"no-hint\0");
    }
}

/// Add an ordered string-line array to the payload digest.
fn digest_lines(digest: &mut Sha256, lines: &[String]) {
    digest.update((lines.len() as u64).to_be_bytes());
    for line in lines {
        digest_field(digest, line.as_bytes());
    }
}

/// Bound warning strings while retaining deterministic warning order.
fn bounded_warnings<I>(warnings: I) -> Vec<String>
where
    I: IntoIterator<Item = String>,
{
    bound_warning_list(warnings.into_iter().collect())
}

/// Bound an owned warning list by count and byte length.
fn bound_warning_list(mut warnings: Vec<String>) -> Vec<String> {
    const MAX_WARNINGS: usize = 32;
    const MAX_WARNING_BYTES: usize = 1024;
    warnings.truncate(MAX_WARNINGS);
    for warning in &mut warnings {
        if warning.len() > MAX_WARNING_BYTES {
            let mut keep = MAX_WARNING_BYTES;
            while keep > 0 && !warning.is_char_boundary(keep) {
                keep -= 1;
            }
            warning.truncate(keep);
        }
    }
    warnings
}

/// Append bounded warnings while keeping the complete output under the cap.
pub(super) fn append_bounded_notices(mut output: String, warnings: &[String]) -> (String, bool) {
    if warnings.is_empty() {
        return (output, false);
    }
    let notice = format!(
        "\n\n<hashline_warnings>\n{}\n</hashline_warnings>",
        warnings
            .iter()
            .map(|warning| format!("- {warning}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    if output.len().saturating_add(notice.len()) <= MAX_READ_BYTES {
        output.push_str(&notice);
        return (output, false);
    }
    let available = MAX_READ_BYTES.saturating_sub(notice.len());
    let mut keep = available.min(output.len());
    while keep > 0 && !output.is_char_boundary(keep) {
        keep -= 1;
    }
    if keep < output.len() {
        keep = output[..keep].rfind('\n').unwrap_or(keep);
    }
    output.truncate(keep);
    output.push_str(&notice);
    (output, true)
}

/// Bound adapter-added wrappers without retaining an oversized model payload.
pub(super) fn bound_output(mut output: String) -> String {
    if output.len() <= MAX_READ_BYTES {
        return output;
    }
    let mut keep = MAX_READ_BYTES;
    while keep > 0 && !output.is_char_boundary(keep) {
        keep -= 1;
    }
    output.truncate(keep);
    output
}

/// Compute the changed result range plus bounded context for fresh anchors.
#[must_use]
pub(super) fn compute_affected_line_range(
    first_changed_line: Option<usize>,
    last_changed_line: Option<usize>,
    result_line_count: usize,
) -> Option<(usize, usize)> {
    compute_affected_line_range_with_limits(
        first_changed_line,
        last_changed_line,
        result_line_count,
        2,
        12,
    )
}

/// Compute an affected range with explicit context and output budgets.
#[must_use]
pub(super) fn compute_affected_line_range_with_limits(
    first_changed_line: Option<usize>,
    last_changed_line: Option<usize>,
    result_line_count: usize,
    context_lines: usize,
    max_output_lines: usize,
) -> Option<(usize, usize)> {
    let (Some(first), Some(last)) = (first_changed_line, last_changed_line) else {
        return None;
    };
    let start = first.saturating_sub(context_lines).max(1);
    let end = last.saturating_add(context_lines).min(result_line_count);
    if end < start || end.saturating_sub(start).saturating_add(1) > max_output_lines {
        return None;
    }
    Some((start, end))
}

/// Format normalized text using one-based offset/limit and optional raw mode.
///
/// Hashes are computed against all visible lines even when only a slice is
/// returned. The terminal empty split sentinel is never counted or rendered.
pub(super) fn format_read(
    text: &str,
    offset: usize,
    limit: usize,
    raw: bool,
) -> Result<ReadFormat, HashlineError> {
    if offset == 0 || limit == 0 {
        return Err(HashlineError::new(
            "E_BAD_READ",
            "offset and limit must be positive integers",
        ));
    }

    let lines = hash::visible_line_slices(text);
    let total_lines = lines.len();
    if total_lines == 0 {
        if offset != 1 {
            return Err(HashlineError::new(
                "E_BAD_READ",
                format!(
                    "Offset {offset} is beyond end of file (0 lines total). The file is empty. Use offset=1 to read from the start."
                ),
            ));
        }
        return Ok(ReadFormat {
            output:
                "File is empty. Use edit with prepend or append and omit pos to insert content."
                    .to_string(),
            content: String::new(),
            line_start: offset,
            line_end: 0,
            total_lines,
            truncated: false,
            next_offset: None,
            first_line_exceeds_limit: false,
        });
    }
    if offset > total_lines {
        return Err(HashlineError::new(
            "E_BAD_READ",
            format!(
                "Offset {offset} is beyond end of file ({total_lines} lines total). Use offset=1 to read from the start."
            ),
        ));
    }

    let start_index = offset - 1;
    let uncapped_requested_end = start_index.saturating_add(limit).min(total_lines);
    let requested_end = start_index
        .saturating_add(limit.min(DEFAULT_READ_LIMIT))
        .min(total_lines);
    let selected = &lines[start_index..requested_end];
    let line_number_width = uncapped_requested_end.to_string().len();
    let mut emitted_count = 0usize;
    let mut truncated_by_bytes = false;
    let mut rendered = String::new();
    let mut content = String::new();
    for (position, line) in selected.iter().enumerate() {
        let line_number = offset + position;
        let hash_prefix = if raw {
            None
        } else {
            let hash =
                hash::compute_line_hash_with_width(&lines, line_number - 1, DEFAULT_HASH_LENGTH)?;
            Some(format!(
                "{:>line_number_width$}#{hash}:",
                line_number,
                line_number_width = line_number_width
            ))
        };
        let candidate_len = hash_prefix
            .as_ref()
            .map_or(line.len(), |prefix| prefix.len().saturating_add(line.len()));

        if candidate_len > MAX_READ_BYTES && emitted_count == 0 {
            if raw {
                let mut prefix_end = MAX_READ_BYTES.min(line.len());
                while prefix_end > 0 && !line.is_char_boundary(prefix_end) {
                    prefix_end -= 1;
                }
                let prefix = &line[..prefix_end];
                rendered.push_str(prefix);
                content.push_str(prefix);
                emitted_count = 1;
                truncated_by_bytes = true;
                break;
            }
            return Ok(ReadFormat {
                output: format!(
                    "[Line {offset} exceeds {MAX_READ_BYTES} bytes. Hashline output requires full lines; cannot compute hashes for a truncated preview.]"
                ),
                content: String::new(),
                line_start: offset,
                line_end: offset,
                total_lines,
                truncated: true,
                next_offset: None,
                first_line_exceeds_limit: true,
            });
        }

        let additional = candidate_len.saturating_add(usize::from(emitted_count > 0));
        if rendered.len().saturating_add(additional) > MAX_READ_BYTES {
            truncated_by_bytes = true;
            break;
        }
        if emitted_count > 0 {
            rendered.push('\n');
            content.push('\n');
        }
        if let Some(prefix) = hash_prefix {
            rendered.push_str(&prefix);
        }
        rendered.push_str(line);
        content.push_str(line);
        emitted_count += 1;
    }

    let line_end = if emitted_count == 0 {
        offset.saturating_sub(1)
    } else {
        offset + emitted_count - 1
    };
    let truncated_by_lines = requested_end < total_lines;
    let truncated = truncated_by_bytes || truncated_by_lines;
    let next_offset = truncated.then_some(line_end.saturating_add(1));
    if truncated {
        let reason = if truncated_by_bytes {
            format!(" ({MAX_READ_BYTES} byte limit)")
        } else {
            String::new()
        };
        rendered.push_str(&format!(
            "\n\n[Showing lines {offset}-{line_end} of {total_lines}{reason}. Use offset={} to continue.]",
            line_end.saturating_add(1)
        ));
    }

    Ok(ReadFormat {
        output: rendered,
        content,
        line_start: offset,
        line_end,
        total_lines,
        truncated,
        next_offset,
        first_line_exceeds_limit: false,
    })
}

/// Bounded metadata and text generated for one normalized Read slice.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReadFormat {
    /// Formatted model-facing text, either hashline or raw mode.
    pub(super) output: String,
    /// Unprefixed selected source lines for structured display metadata.
    pub(super) content: String,
    /// One-based first selected line, or one for an empty file.
    pub(super) line_start: usize,
    /// One-based last selected line, or zero when no visible lines exist.
    pub(super) line_end: usize,
    /// Number of visible lines in the complete normalized file.
    pub(super) total_lines: usize,
    /// Whether a continuation notice or oversize guard applies.
    pub(super) truncated: bool,
    /// Next one-based offset when another slice is available.
    pub(super) next_offset: Option<usize>,
    /// Whether the first selected line exceeded the hashline byte budget.
    pub(super) first_line_exceeds_limit: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_uncapped_end_width_when_line_cap_applies() {
        let text = (1..=2001)
            .map(|line| format!("line{line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let formatted = format_read(&text, 1, 4000, false)
            .unwrap_or_else(|error| panic!("{}", error.diagnostic()));
        assert_eq!(formatted.line_end, 2000);
        assert_eq!(formatted.next_offset, Some(2001));
        assert!(formatted.output.starts_with("   1#"));
        assert!(formatted.output.contains("Showing lines 1-2000 of 2001"));
    }

    #[test]
    fn hashline_error_constructors_bound_diagnostic_and_hint_rows() {
        const MESSAGE_BYTES: usize = 8 * 1024;
        const HINT_COUNT: usize = 16;
        const HINT_ROW_BYTES: usize = 512;

        let oversized_message = format!("prefix-{}-SECRET_MESSAGE_TAIL", "é".repeat(MESSAGE_BYTES));
        let error = HashlineError::new("E_BAD_REF", oversized_message);
        assert_eq!(error.code, "E_BAD_REF");
        assert!(
            error.message.len() <= MESSAGE_BYTES,
            "HashlineError::new must cap raw message bytes: {}",
            error.message.len()
        );
        assert!(
            !error.message.contains("SECRET_MESSAGE_TAIL"),
            "truncated diagnostics must not retain the oversized tail"
        );
        let diagnostic = error.diagnostic();
        assert!(diagnostic.starts_with("[E_BAD_REF] "));
        assert!(
            diagnostic.to_ascii_lowercase().contains("truncat"),
            "bounded diagnostics must carry a content-free truncation marker: {diagnostic:?}"
        );
        assert!(
            diagnostic.len() <= MESSAGE_BYTES + "[E_BAD_REF] ".len(),
            "the stable code is outside the raw message budget: {}",
            diagnostic.len()
        );

        let hints = (0..HINT_COUNT * 2)
            .map(|index| {
                format!(
                    "hint-{index}-{}-SECRET_HINT_TAIL",
                    "ß".repeat(HINT_ROW_BYTES)
                )
            })
            .collect();
        let error = HashlineError::with_hints("E_STALE_ANCHOR", "bounded", hints);
        assert_eq!(error.code, "E_STALE_ANCHOR");
        assert!(
            error.hints.len() <= HINT_COUNT,
            "HashlineError::with_hints must cap hint count: {}",
            error.hints.len()
        );
        for hint in &error.hints {
            assert!(
                hint.len() <= HINT_ROW_BYTES,
                "each hint row must be byte-bounded: {}",
                hint.len()
            );
            assert!(
                !hint.contains("SECRET_HINT_TAIL"),
                "truncated hint rows must not retain oversized content"
            );
        }
    }
}
