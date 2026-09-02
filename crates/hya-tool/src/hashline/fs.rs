//! Native text loading and atomic mutation support for hashline tools.
//!
//! The normalization and write rules in this module follow `pi-hashline-edit`
//! 0.8.3 (`ba7db9943d0f58499b24c1f6bd64722580f772a5`, tarball SHA-1
//! `8985f24c3493be375cc225a5522ed54de8daabc9`). The upstream package is
//! MIT-licensed; this private Rust seam deliberately keeps its filesystem
//! behavior independent from any JavaScript runtime.

use super::state::MutationIdentity;
use std::fmt;
use std::io::{self, ErrorKind};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const UTF8_BOM: &[u8; 3] = b"\xEF\xBB\xBF";
const MAX_SYMLINK_HOPS: usize = 40;
const TEMP_CREATE_ATTEMPTS: usize = 8;
const TEMP_MODE: u32 = 0o600;
/// Device and inode captured before a mutation begins.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PreparedPathIdentity {
    /// Device containing the prepared target inode.
    device: u64,
    /// Inode identifying the prepared target entry.
    inode: u64,
}

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

/// The line-ending style to use when materializing normalized text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LineEnding {
    /// A line terminated by a single LF byte.
    Lf,
    /// A line terminated by a CRLF byte pair.
    CrLf,
    /// A line terminated by a single CR byte.
    Cr,
}

impl LineEnding {
    /// Return the byte sequence used for one line ending.
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
            Self::Cr => b"\r",
        }
    }
}

/// Facts discovered while resolving and decoding one target file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LoadedText {
    /// The lexical path supplied by the caller.
    pub(super) requested_path: PathBuf,
    /// The final non-symlink path used for I/O and state identity.
    pub(super) path: PathBuf,
    /// Number of final source bytes loaded from the filesystem.
    pub(super) bytes: usize,
    /// Text with one BOM removed and all line endings normalized to LF.
    pub(super) text: String,
    /// Whether the source began with one UTF-8 BOM.
    pub(super) bom: bool,
    /// The style selected for subsequent writes.
    pub(super) ending: LineEnding,
    /// Whether more than one line-ending style was observed.
    pub(super) mixed_endings: bool,
    /// Whether invalid UTF-8 bytes were replaced with U+FFFD.
    pub(super) invalid_utf8: bool,
    /// Permission bits for an existing target, when the platform exposes them.
    pub(super) mode: Option<u32>,
    /// Whether the target is read-only on platforms without Unix mode bits.
    pub(super) readonly: bool,
    /// Number of directory entries pointing at the target inode.
    pub(super) nlink: u64,
    /// Whether the resolved target existed when this value was created.
    pub(super) existed: bool,
    /// Unix identity captured with the prepared bytes, when the target exists.
    #[cfg(unix)]
    prepared_identity: Option<PreparedPathIdentity>,

    /// Number of symlink hops used to reach [`Self::path`].
    pub(super) symlink_hops: u8,
    /// Whether at least one symlink was followed.
    pub(super) followed_symlink: bool,
    /// Whether a followed symlink ended at a missing target.
    pub(super) dangling: bool,
}

impl LoadedText {
    /// Construct empty facts for a new target before its first write.
    pub(super) fn new(path: &Path) -> Self {
        Self {
            requested_path: path.to_path_buf(),
            path: normalize_absolute_path(path),
            bytes: 0,
            text: String::new(),
            bom: false,
            ending: LineEnding::Lf,
            mixed_endings: false,
            invalid_utf8: false,
            mode: None,
            readonly: false,
            nlink: 0,
            existed: false,
            #[cfg(unix)]
            prepared_identity: None,

            symlink_hops: 0,
            followed_symlink: false,
            dangling: false,
        }
    }

    /// Return whether the target should be mutated in place to preserve an inode.
    pub(super) fn is_hard_link(&self) -> bool {
        self.nlink > 1
    }

    /// Return bounded human-readable warnings associated with the load.
    pub(super) fn warnings(&self) -> Vec<&'static str> {
        let mut warnings = Vec::with_capacity(2);
        if self.invalid_utf8 {
            warnings.push("Invalid UTF-8 was replaced with U+FFFD.");
        }
        if self.mixed_endings {
            warnings.push("Mixed line endings detected; preserving the first detected style.");
        }
        warnings
    }
}

/// Resolved target facts available before a target can be decoded.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ResolvedPath {
    /// Final path after lexical relative-link expansion.
    pub(super) path: PathBuf,
    /// Number of symlink hops followed.
    pub(super) symlink_hops: u8,
    /// Whether at least one symlink was followed.
    pub(super) followed_symlink: bool,
    /// Whether the final target is missing after following a symlink.
    pub(super) dangling: bool,
}

/// Result facts for one successful atomic write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct WriteOutcome {
    /// Final non-symlink path that was mutated.
    pub(super) path: PathBuf,
    /// Whether the target did not exist before the write.
    pub(super) created: bool,
    /// Whether the target inode was written in place.
    pub(super) hard_link: bool,
    /// Number of bytes written, including an emitted BOM and line endings.
    pub(super) bytes: usize,
}

/// Failure from an atomic write, distinguishing committed bytes from pre-commit I/O.
#[derive(Debug)]
pub(super) enum AtomicWriteError {
    /// The mutation did not commit.
    Io(io::Error),
    /// Rename committed but syncing its containing directory failed.
    Committed {
        /// Authoritative mutation facts that the caller must retain.
        outcome: WriteOutcome,
        /// Parent-directory synchronization failure.
        error: io::Error,
    },
}

impl fmt::Display for AtomicWriteError {
    /// Render a bounded diagnostic without exposing file contents.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "atomic write failed: {error}"),
            Self::Committed { outcome, error } => write!(
                formatter,
                "atomic write committed at {} but parent sync failed: {error}",
                outcome.path.display()
            ),
        }
    }
}

impl std::error::Error for AtomicWriteError {
    /// Return the underlying I/O failure for typed write errors.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Committed { error, .. } => Some(error),
        }
    }
}

impl From<io::Error> for AtomicWriteError {
    /// Convert a pre-commit I/O error into the typed write error.
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Resolve a lexical path through at most forty symlink hops.
///
/// Resolution starts at an absolute root and inspects every existing path
/// component. A link target replaces that component while its remaining tail
/// is preserved, so links in parent directories cannot be skipped. A missing
/// component is returned as a dangling tail when a link was followed, which
/// lets callers distinguish a missing new file from a broken link. Cycles and
/// chains longer than the bounded hop count fail closed.
pub(super) async fn resolve_target(path: &Path) -> io::Result<ResolvedPath> {
    let mut current = normalize_absolute_path(path);
    let mut visited = Vec::with_capacity(MAX_SYMLINK_HOPS);
    let mut followed_symlink = false;
    let mut symlink_hops = 0usize;

    loop {
        match find_first_symlink(&current).await? {
            ResolveScan::Complete => {
                return Ok(ResolvedPath {
                    path: current,
                    symlink_hops: symlink_hops as u8,
                    followed_symlink,
                    dangling: false,
                });
            }
            ResolveScan::Missing => {
                return Ok(ResolvedPath {
                    path: current,
                    symlink_hops: symlink_hops as u8,
                    followed_symlink,
                    dangling: followed_symlink,
                });
            }
            ResolveScan::Symlink {
                path: symlink_path,
                target,
                suffix,
            } => {
                if visited.iter().any(|seen| seen == &symlink_path) {
                    return Err(io::Error::new(
                        ErrorKind::InvalidInput,
                        "symlink cycle detected while resolving target",
                    ));
                }
                if symlink_hops >= MAX_SYMLINK_HOPS {
                    return Err(io::Error::new(
                        ErrorKind::InvalidInput,
                        "symlink chain exceeds the 40-hop limit",
                    ));
                }
                visited.push(symlink_path.clone());

                let parent = symlink_path.parent().unwrap_or_else(|| Path::new("/"));
                let mut replacement = if target.is_absolute() {
                    target
                } else {
                    parent.join(target)
                };
                if !suffix.as_os_str().is_empty() {
                    replacement.push(suffix);
                }
                current = normalize_absolute_path(&replacement);
                followed_symlink = true;
                symlink_hops += 1;
            }
        }
    }
}

/// Result of scanning path components for the first symlink or missing tail.
enum ResolveScan {
    /// Every component exists and no symlink remains to be expanded.
    Complete,
    /// A component does not exist under the current candidate path.
    Missing,
    /// One symlink and the path tail that must follow its replacement.
    Symlink {
        /// Existing symlink component.
        path: PathBuf,
        /// Link target as stored by the filesystem.
        target: PathBuf,
        /// Components after the symlink component.
        suffix: PathBuf,
    },
}

/// Inspect each absolute component and return the first link or missing tail.
async fn find_first_symlink(path: &Path) -> io::Result<ResolveScan> {
    let components = path.components().collect::<Vec<_>>();
    let mut prefix = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        match component {
            Component::Prefix(prefix_component) => {
                prefix.push(prefix_component.as_os_str());
            }
            Component::RootDir => prefix.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                prefix.pop();
            }
            Component::Normal(name) => {
                prefix.push(name);
                let metadata = match tokio::fs::symlink_metadata(&prefix).await {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        return Ok(ResolveScan::Missing);
                    }
                    Err(error) => return Err(error),
                };
                if metadata.file_type().is_symlink() {
                    let target = tokio::fs::read_link(&prefix).await?;
                    let suffix = path_from_components(&components[index + 1..]);
                    return Ok(ResolveScan::Symlink {
                        path: prefix,
                        target,
                        suffix,
                    });
                }
            }
        }
    }
    Ok(ResolveScan::Complete)
}

/// Build a relative path from components after an expanded symlink.
fn path_from_components(components: &[Component<'_>]) -> PathBuf {
    let mut path = PathBuf::new();
    for component in components {
        path.push(component.as_os_str());
    }
    path
}

/// Resolve a target's session-independent filesystem mutation identity.
///
/// Existing hard-linked regular files use Unix device and inode numbers so
/// every link alias selects the same lock shard. Missing and ordinary targets
/// use their normalized resolved path, keeping path creation deterministic.
///
/// # Parameters
/// - `resolved`: Target path already resolved through symlink components.
///
/// # Returns
/// The bounded mutation identity, or the metadata failure that prevented it.
pub(super) async fn mutation_identity(resolved: &ResolvedPath) -> io::Result<MutationIdentity> {
    match tokio::fs::metadata(&resolved.path).await {
        Ok(metadata) => {
            #[cfg(unix)]
            if metadata.is_file() && metadata_nlink(&metadata) > 1 {
                use std::os::unix::fs::MetadataExt;

                return Ok(MutationIdentity::Inode {
                    device: metadata.dev(),
                    inode: metadata.ino(),
                });
            }
            Ok(MutationIdentity::path(&resolved.path))
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            Ok(MutationIdentity::path(&resolved.path))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
/// Load, decode, and normalize one target while retaining write-preservation facts.
pub(super) async fn load_text(path: &Path) -> io::Result<LoadedText> {
    let resolved = resolve_target(path).await?;
    load_resolved_text(path, &resolved).await
}

/// Load and decode a path whose resolution was validated by the caller.
///
/// # Parameters
/// - `requested_path`: Lexical path to retain in result metadata.
/// - `resolved`: Resolved path and symlink facts to use without another lookup.
///
/// # Returns
/// Loaded normalized text and preservation facts for the validated target.
pub(super) async fn load_resolved_text(
    requested_path: &Path,
    resolved: &ResolvedPath,
) -> io::Result<LoadedText> {
    let mut file = tokio::fs::File::open(&resolved.path).await?;
    let metadata = file.metadata().await?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).await?;
    let (bom, payload) = strip_one_bom(&bytes);
    let (ending, mixed_endings) = classify_endings(payload);
    let invalid_utf8 = std::str::from_utf8(payload).is_err();
    let text = String::from_utf8_lossy(payload)
        .replace("\r\n", "\n")
        .replace('\r', "\n");

    Ok(LoadedText {
        requested_path: requested_path.to_path_buf(),
        path: resolved.path.clone(),
        bytes: bytes.len(),
        text,
        bom,
        ending,
        mixed_endings,
        invalid_utf8,
        mode: metadata_mode(&metadata),
        readonly: metadata.permissions().readonly(),
        nlink: metadata_nlink(&metadata),
        #[cfg(unix)]
        prepared_identity: Some(prepared_path_identity(&metadata)),

        existed: true,
        symlink_hops: resolved.symlink_hops,
        followed_symlink: resolved.followed_symlink,
        dangling: resolved.dangling,
    })
}

/// Encode normalized LF text with the source BOM and selected line-ending style.
pub(super) fn encode_text(text: &str, bom: bool, ending: LineEnding) -> Vec<u8> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let ending_bytes = ending.bytes();
    let additional = normalized.bytes().filter(|byte| *byte == b'\n').count()
        * ending_bytes.len().saturating_sub(1);
    let mut output = Vec::with_capacity(
        normalized
            .len()
            .saturating_add(additional)
            .saturating_add(usize::from(bom) * UTF8_BOM.len()),
    );
    if bom {
        output.extend_from_slice(UTF8_BOM);
    }
    if ending == LineEnding::Lf {
        output.extend_from_slice(normalized.as_bytes());
    } else {
        for byte in normalized.bytes() {
            if byte == b'\n' {
                output.extend_from_slice(ending_bytes);
            } else {
                output.push(byte);
            }
        }
    }
    output
}

/// Atomically write normalized text while preserving BOM, endings, mode, and links.
pub(super) async fn atomic_write(
    requested_path: &Path,
    text: &str,
    facts: &LoadedText,
) -> Result<WriteOutcome, AtomicWriteError> {
    // Revalidate both the lexical alias chain and the prepared inode. A changed
    // symlink must not redirect this mutation to either the old or new referent.
    revalidate_requested_target(requested_path, facts, "atomic write").await?;
    let target = facts.path.clone();
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(parent).await?;

    let metadata = revalidate_prepared_path(&target, facts, "atomic write").await?;
    if metadata.as_ref().is_some_and(std::fs::Metadata::is_dir) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "cannot atomically write a directory",
        )
        .into());
    }

    let bytes = encode_text(text, facts.bom, facts.ending);
    let created = metadata.is_none();
    let hard_link = metadata
        .as_ref()
        .is_some_and(|metadata| metadata_nlink(metadata) > 1);
    let mode = metadata.as_ref().and_then(metadata_mode).or(facts.mode);
    let readonly = metadata
        .as_ref()
        .map_or(facts.readonly, |metadata| metadata.permissions().readonly());
    // Open the directory before rename. A later sync failure then has a clear
    // committed-write boundary and cannot be mistaken for an untouched file.
    let parent_sync = if hard_link {
        None
    } else {
        open_parent_for_sync(parent).await?
    };

    let outcome = WriteOutcome {
        path: target,
        created,
        hard_link,
        bytes: bytes.len(),
    };
    if hard_link {
        match write_in_place(requested_path, &outcome.path, &bytes, facts).await {
            Ok(()) => {}
            Err(InPlaceWriteError::Io(error)) => return Err(AtomicWriteError::Io(error)),
            Err(InPlaceWriteError::Committed(error)) => {
                return Err(AtomicWriteError::Committed { outcome, error });
            }
        }
    } else {
        write_via_temp(requested_path, &outcome.path, &bytes, mode, readonly, facts).await?;
    }
    if let Some(parent_sync) = parent_sync
        && let Err(error) = parent_sync.sync_all().await
    {
        return Err(AtomicWriteError::Committed { outcome, error });
    }
    Ok(outcome)
}

/// Convert a path to an absolute, lexically normalized candidate.
fn normalize_absolute_path(path: &Path) -> PathBuf {
    let absolute = crate::lsp_path::absolutize(path);
    normalize_path(&absolute)
}

/// Normalize path components without allowing parent traversal above a root.
fn normalize_path(path: &Path) -> PathBuf {
    let absolute = path.is_absolute();
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !absolute {
                    normalized.push(component.as_os_str());
                }
            }
            Component::Normal(name) => normalized.push(name),
        }
    }
    normalized
}

/// Remove one leading UTF-8 BOM and report whether it was present.
fn strip_one_bom(bytes: &[u8]) -> (bool, &[u8]) {
    bytes
        .strip_prefix(UTF8_BOM)
        .map_or((false, bytes), |payload| (true, payload))
}

/// Count line-ending styles and select the first style for mixed files.
fn classify_endings(bytes: &[u8]) -> (LineEnding, bool) {
    let mut counts = [0usize; 3];
    let mut first = None;
    let mut index = 0usize;
    while index < bytes.len() {
        let style = match bytes[index] {
            b'\r' if bytes.get(index + 1) == Some(&b'\n') => {
                index += 1;
                LineEnding::CrLf
            }
            b'\r' => LineEnding::Cr,
            b'\n' => LineEnding::Lf,
            _ => {
                index += 1;
                continue;
            }
        };
        let slot = match style {
            LineEnding::Lf => 0,
            LineEnding::CrLf => 1,
            LineEnding::Cr => 2,
        };
        counts[slot] = counts[slot].saturating_add(1);
        if first.is_none() {
            first = Some(style);
        }
        index += 1;
    }

    let Some(first) = first else {
        return (LineEnding::Lf, false);
    };
    let styles = counts.iter().filter(|count| **count > 0).count();
    (first, styles > 1)
}

/// Return Unix permission bits when the platform exposes them.
#[cfg(unix)]
fn metadata_mode(metadata: &std::fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;

    Some(metadata.permissions().mode() & 0o7777)
}

/// Return no Unix mode bits on platforms that do not expose them.
#[cfg(not(unix))]
fn metadata_mode(_metadata: &std::fs::Metadata) -> Option<u32> {
    None
}

/// Return the inode link count when the platform exposes it.
#[cfg(unix)]
fn metadata_nlink(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;

    metadata.nlink()
}

/// Treat non-Unix targets as single-link files.
#[cfg(not(unix))]
fn metadata_nlink(_metadata: &std::fs::Metadata) -> u64 {
    1
}

/// Capture the device and inode identifying one prepared Unix target.
#[cfg(unix)]
fn prepared_path_identity(metadata: &std::fs::Metadata) -> PreparedPathIdentity {
    use std::os::unix::fs::MetadataExt;

    PreparedPathIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

/// Return a contextual error when a prepared path now names another inode.
fn identity_changed_error(path: &Path, phase: &str) -> io::Error {
    io::Error::new(
        ErrorKind::InvalidInput,
        format!(
            "prepared target identity changed before {phase} at {}",
            path.display()
        ),
    )
}

/// Re-resolve the requested alias and reject a changed symlink chain before mutation.
///
/// # Parameters
/// - `requested_path`: Lexical path originally authorized by the adapter.
/// - `facts`: Prepared target facts coupled to the loaded bytes.
/// - `phase`: Destructive operation named in a contextual failure.
///
/// # Returns
/// Success only while the lexical request still resolves to the prepared target.
async fn revalidate_requested_target(
    requested_path: &Path,
    facts: &LoadedText,
    phase: &str,
) -> io::Result<()> {
    let resolved = resolve_target(requested_path).await?;
    if resolved.path != facts.path {
        return Err(identity_changed_error(requested_path, phase));
    }
    Ok(())
}

/// Revalidate the target identity captured when the bytes were loaded.
async fn revalidate_prepared_path(
    path: &Path,
    facts: &LoadedText,
    phase: &str,
) -> io::Result<Option<std::fs::Metadata>> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    #[cfg(unix)]
    {
        let actual = metadata.as_ref().map(prepared_path_identity);
        if facts.prepared_identity != actual {
            return Err(identity_changed_error(path, phase));
        }
    }
    Ok(metadata)
}

/// Refresh prepared filesystem facts after a successful committed write.
pub(super) async fn refresh_loaded_identity(facts: &mut LoadedText) -> io::Result<()> {
    let metadata = tokio::fs::metadata(&facts.path).await?;
    facts.existed = true;
    facts.mode = metadata_mode(&metadata);
    facts.readonly = metadata.permissions().readonly();
    facts.nlink = metadata_nlink(&metadata);
    #[cfg(unix)]
    {
        facts.prepared_identity = Some(prepared_path_identity(&metadata));
    }
    Ok(())
}
/// Failure from an in-place hard-link write after its open boundary.
enum InPlaceWriteError {
    /// The target could not be opened, so no commit boundary was crossed.
    Io(io::Error),
    /// Truncate/open succeeded and the inode may now contain changed bytes.
    Committed(io::Error),
}

/// Write bytes directly to a hard-linked inode and fsync the resulting file.
async fn write_in_place(
    requested_path: &Path,
    path: &Path,
    bytes: &[u8],
    facts: &LoadedText,
) -> Result<(), InPlaceWriteError> {
    revalidate_requested_target(requested_path, facts, "hard-link open")
        .await
        .map_err(InPlaceWriteError::Io)?;
    let _ = revalidate_prepared_path(path, facts, "hard-link open")
        .await
        .map_err(InPlaceWriteError::Io)?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .await
        .map_err(InPlaceWriteError::Io)?;
    #[cfg(unix)]
    {
        let metadata = file.metadata().await.map_err(InPlaceWriteError::Io)?;
        if facts.prepared_identity != Some(prepared_path_identity(&metadata)) {
            return Err(InPlaceWriteError::Io(identity_changed_error(
                path,
                "hard-link truncate",
            )));
        }
    }
    revalidate_requested_target(requested_path, facts, "hard-link truncate")
        .await
        .map_err(InPlaceWriteError::Io)?;
    let _ = revalidate_prepared_path(path, facts, "hard-link truncate")
        .await
        .map_err(InPlaceWriteError::Io)?;
    file.set_len(0)
        .await
        .map_err(InPlaceWriteError::Committed)?;
    file.write_all(bytes)
        .await
        .map_err(InPlaceWriteError::Committed)?;
    file.sync_all().await.map_err(InPlaceWriteError::Committed)
}

/// Write bytes through an exclusive same-directory temporary file and rename.
async fn write_via_temp(
    requested_path: &Path,
    target: &Path,
    bytes: &[u8],
    mode: Option<u32>,
    readonly: bool,
    facts: &LoadedText,
) -> io::Result<()> {
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    let (temporary_path, mut file) = create_temp_file(parent).await?;
    let mut cleanup = TempCleanup::new(temporary_path.clone());

    let result = async {
        file.write_all(bytes).await?;
        set_temp_permissions(&temporary_path, mode, readonly).await?;
        file.sync_all().await?;
        drop(file);
        revalidate_requested_target(requested_path, facts, "atomic rename").await?;
        let _ = revalidate_prepared_path(target, facts, "atomic rename").await?;
        tokio::fs::rename(&temporary_path, target).await
    }
    .await;

    match result {
        Ok(()) => {
            cleanup.disarm();
            Ok(())
        }
        Err(error) => Err(cleanup.with_error(error).await),
    }
}

/// Create a mode-0600 temporary file with bounded collision retries.
async fn create_temp_file(parent: &Path) -> io::Result<(PathBuf, tokio::fs::File)> {
    for _ in 0..TEMP_CREATE_ATTEMPTS {
        let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let name = format!(".hya-hashline-{}-{id}.tmp", std::process::id());
        let path = parent.join(name);
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(TEMP_MODE);
        }
        match options.open(&path).await {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        ErrorKind::AlreadyExists,
        "could not allocate a unique temporary hashline file",
    ))
}

/// Apply exact target permissions to the temporary file before the rename.
async fn set_temp_permissions(path: &Path, mode: Option<u32>, _readonly: bool) -> io::Result<()> {
    let mut permissions = tokio::fs::metadata(path).await?.permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        permissions.set_mode(mode.unwrap_or(TEMP_MODE));
    }
    #[cfg(not(unix))]
    {
        permissions.set_readonly(_readonly);
    }
    tokio::fs::set_permissions(path, permissions).await
}

/// Open a parent directory before rename so sync failures are post-commit typed.
#[cfg(unix)]
async fn open_parent_for_sync(parent: &Path) -> io::Result<Option<tokio::fs::File>> {
    Ok(Some(tokio::fs::File::open(parent).await?))
}

/// Skip directory pre-open where the platform cannot fsync directories.
#[cfg(not(unix))]
async fn open_parent_for_sync(_parent: &Path) -> io::Result<Option<tokio::fs::File>> {
    Ok(None)
}

/// Own one temporary path until the rename succeeds or cleanup completes.
struct TempCleanup {
    path: Option<PathBuf>,
}

impl TempCleanup {
    /// Track a newly created temporary path.
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// Stop cleanup after the path has been renamed successfully.
    fn disarm(&mut self) {
        self.path = None;
    }

    /// Remove the temporary path and preserve the primary failure context.
    async fn with_error(&mut self, error: io::Error) -> io::Error {
        let cleanup = self.cleanup().await;
        match cleanup {
            Ok(()) => error,
            Err(cleanup_error) => io::Error::new(
                error.kind(),
                format!("{error}; temporary-file cleanup failed: {cleanup_error}"),
            ),
        }
    }

    /// Explicitly remove the temporary path, tolerating an already-renamed file.
    async fn cleanup(&mut self) -> io::Result<()> {
        let Some(path) = self.path.take() else {
            return Ok(());
        };
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

impl Drop for TempCleanup {
    /// Make a best-effort synchronous fallback cleanup if async cleanup is interrupted.
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_TEST_ID: AtomicU64 = AtomicU64::new(0);

    /// Create an isolated temporary directory for filesystem seam tests.
    fn test_directory() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        let id = NEXT_TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "hya-hashline-fs-{timestamp}-{}-{id}",
            std::process::id()
        ));
        if let Err(error) = std::fs::create_dir_all(&path) {
            panic!("create test directory: {error}");
        }
        path
    }

    /// Remove a filesystem seam test directory without masking its assertions.
    fn remove_test_directory(path: &Path) {
        if let Err(error) = std::fs::remove_dir_all(path) {
            panic!("remove test directory {}: {error}", path.display());
        }
    }

    /// Verify BOM removal, replacement decoding, normalization, and first-style selection.
    #[tokio::test]
    async fn load_tracks_bom_invalid_utf8_and_mixed_endings() {
        let directory = test_directory();
        let path = directory.join("mixed.txt");
        if let Err(error) = std::fs::write(&path, b"\xEF\xBB\xBFone\r\ntwo\rthree\n\xFF") {
            panic!("write fixture: {error}");
        }

        let loaded = match load_text(&path).await {
            Ok(loaded) => loaded,
            Err(error) => panic!("load fixture: {error}"),
        };
        assert!(loaded.bom);
        assert_eq!(loaded.ending, LineEnding::CrLf);
        assert!(loaded.mixed_endings);
        assert!(loaded.invalid_utf8);
        assert_eq!(loaded.text, "one\ntwo\nthree\n�");
        assert_eq!(loaded.warnings().len(), 2);
        remove_test_directory(&directory);
    }

    /// Verify a dangling link is resolved and then reported as a read failure.
    #[cfg(unix)]
    #[tokio::test]
    async fn dangling_symlink_is_bounded_and_not_silently_followed() {
        use std::os::unix::fs::symlink;

        let directory = test_directory();
        let link = directory.join("dangling");
        if let Err(error) = symlink("missing.txt", &link) {
            panic!("create dangling symlink: {error}");
        }
        let resolved = match resolve_target(&link).await {
            Ok(resolved) => resolved,
            Err(error) => panic!("resolve dangling symlink: {error}"),
        };
        assert!(resolved.followed_symlink);
        assert!(resolved.dangling);
        let error = match load_text(&link).await {
            Ok(_) => panic!("dangling symlink unexpectedly loaded"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ErrorKind::NotFound);
        remove_test_directory(&directory);
    }

    /// Verify relative and absolute directory links expand before their file tail.
    #[cfg(unix)]
    #[tokio::test]
    async fn intermediate_relative_and_absolute_symlinks_resolve() {
        use std::os::unix::fs::symlink;

        let directory = test_directory();
        let real_directory = directory.join("real");
        if let Err(error) = std::fs::create_dir_all(&real_directory) {
            panic!("create real directory: {error}");
        }
        if let Err(error) = std::fs::write(real_directory.join("file.txt"), b"content\n") {
            panic!("write real file: {error}");
        }

        let relative_link = directory.join("relative-dir");
        if let Err(error) = symlink("real", &relative_link) {
            panic!("create relative directory symlink: {error}");
        }
        let relative_target = relative_link.join("file.txt");
        let relative_resolved = match resolve_target(&relative_target).await {
            Ok(resolved) => resolved,
            Err(error) => panic!("resolve relative directory symlink: {error}"),
        };
        assert_eq!(relative_resolved.path, real_directory.join("file.txt"));
        assert_eq!(relative_resolved.symlink_hops, 1);

        let absolute_link = directory.join("absolute-dir");
        if let Err(error) = symlink(&real_directory, &absolute_link) {
            panic!("create absolute directory symlink: {error}");
        }
        let absolute_target = absolute_link.join("file.txt");
        let absolute_resolved = match resolve_target(&absolute_target).await {
            Ok(resolved) => resolved,
            Err(error) => panic!("resolve absolute directory symlink: {error}"),
        };
        assert_eq!(absolute_resolved.path, real_directory.join("file.txt"));
        assert_eq!(absolute_resolved.symlink_hops, 1);
        remove_test_directory(&directory);
    }

    /// Verify a missing intermediate link target retains the unresolved tail.
    #[cfg(unix)]
    #[tokio::test]
    async fn dangling_intermediate_symlink_preserves_tail() {
        use std::os::unix::fs::symlink;

        let directory = test_directory();
        let link = directory.join("dangling-dir");
        if let Err(error) = symlink("missing-dir", &link) {
            panic!("create dangling intermediate symlink: {error}");
        }
        let requested = link.join("tail.txt");
        let resolved = match resolve_target(&requested).await {
            Ok(resolved) => resolved,
            Err(error) => panic!("resolve dangling intermediate symlink: {error}"),
        };
        assert!(resolved.dangling);
        assert_eq!(resolved.symlink_hops, 1);
        assert_eq!(
            resolved.path,
            directory.join("missing-dir").join("tail.txt")
        );
        remove_test_directory(&directory);
    }

    /// Verify forty links resolve and the forty-first link is rejected.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_hops_allow_exact_bound_and_reject_next() {
        use std::os::unix::fs::symlink;

        let directory = test_directory();
        let target = directory.join("target.txt");
        if let Err(error) = std::fs::write(&target, b"target\n") {
            panic!("write hop target: {error}");
        }
        for index in (0..MAX_SYMLINK_HOPS).rev() {
            let link = directory.join(format!("chain-{index}"));
            let next = if index + 1 == MAX_SYMLINK_HOPS {
                target.clone()
            } else {
                directory.join(format!("chain-{}", index + 1))
            };
            if let Err(error) = symlink(next, link) {
                panic!("create forty-hop symlink {index}: {error}");
            }
        }
        let resolved = match resolve_target(&directory.join("chain-0")).await {
            Ok(resolved) => resolved,
            Err(error) => panic!("resolve forty-hop chain: {error}"),
        };
        assert_eq!(resolved.symlink_hops as usize, MAX_SYMLINK_HOPS);
        assert_eq!(resolved.path, target);

        let long_directory = directory.join("long");
        if let Err(error) = std::fs::create_dir_all(&long_directory) {
            panic!("create forty-one-hop directory: {error}");
        }
        let long_target = long_directory.join("target.txt");
        if let Err(error) = std::fs::write(&long_target, b"target\n") {
            panic!("write forty-one-hop target: {error}");
        }
        for index in (0..=MAX_SYMLINK_HOPS).rev() {
            let link = long_directory.join(format!("chain-{index}"));
            let next = if index == MAX_SYMLINK_HOPS {
                long_target.clone()
            } else {
                long_directory.join(format!("chain-{}", index + 1))
            };
            if let Err(error) = symlink(next, link) {
                panic!("create forty-one-hop symlink {index}: {error}");
            }
        }
        let error = match resolve_target(&long_directory.join("chain-0")).await {
            Ok(_) => panic!("forty-one-hop chain unexpectedly resolved"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("40-hop"));
        remove_test_directory(&directory);
    }

    /// Verify a symlink cycle fails with a typed invalid-input filesystem error.
    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_cycle_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = test_directory();
        let first = directory.join("first");
        let second = directory.join("second");
        if let Err(error) = symlink("second", &first) {
            panic!("create first symlink: {error}");
        }
        if let Err(error) = symlink("first", &second) {
            panic!("create second symlink: {error}");
        }
        let error = match resolve_target(&first).await {
            Ok(_) => panic!("symlink cycle unexpectedly resolved"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("cycle"));
        remove_test_directory(&directory);
    }

    /// Verify hard links keep their inode and observe the in-place replacement.
    #[cfg(unix)]
    #[tokio::test]
    async fn hard_link_is_written_in_place() {
        use std::os::unix::fs::MetadataExt;

        let directory = test_directory();
        let path = directory.join("target.txt");
        let link = directory.join("link.txt");
        if let Err(error) = std::fs::write(&path, b"old\n") {
            panic!("write target: {error}");
        }
        if let Err(error) = std::fs::hard_link(&path, &link) {
            panic!("create hard link: {error}");
        }
        let before = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.ino(),
            Err(error) => panic!("stat target: {error}"),
        };
        let facts = match load_text(&path).await {
            Ok(facts) => facts,
            Err(error) => panic!("load target: {error}"),
        };
        let outcome = match atomic_write(&path, "new\n", &facts).await {
            Ok(outcome) => outcome,
            Err(error) => panic!("write target: {error}"),
        };
        let after = match std::fs::metadata(&path) {
            Ok(metadata) => metadata.ino(),
            Err(error) => panic!("stat target after write: {error}"),
        };
        assert!(outcome.hard_link);
        assert_eq!(before, after);
        let linked_contents = match std::fs::read_to_string(&link) {
            Ok(contents) => contents,
            Err(error) => panic!("read linked target: {error}"),
        };
        assert_eq!(linked_contents, "new\n");
        remove_test_directory(&directory);
    }

    /// Verify restrictive existing modes survive a replacement and new files start at 0600.
    #[cfg(unix)]
    #[tokio::test]
    async fn mode_is_preserved_and_new_file_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let directory = test_directory();
        let existing = directory.join("restricted.txt");
        if let Err(error) = std::fs::write(&existing, b"old") {
            panic!("write existing fixture: {error}");
        }
        let mut permissions = match std::fs::metadata(&existing) {
            Ok(metadata) => metadata.permissions(),
            Err(error) => panic!("stat existing fixture: {error}"),
        };
        permissions.set_mode(0o400);
        if let Err(error) = std::fs::set_permissions(&existing, permissions) {
            panic!("restrict existing fixture: {error}");
        }
        let facts = match load_text(&existing).await {
            Ok(facts) => facts,
            Err(error) => panic!("load existing fixture: {error}"),
        };
        if let Err(error) = atomic_write(&existing, "new", &facts).await {
            panic!("replace existing fixture: {error}");
        }
        let existing_mode = match std::fs::metadata(&existing) {
            Ok(metadata) => metadata.permissions().mode() & 0o7777,
            Err(error) => panic!("stat replaced fixture: {error}"),
        };
        assert_eq!(existing_mode, 0o400);

        let created = directory.join("created.txt");
        let new_facts = LoadedText::new(&created);
        if let Err(error) = atomic_write(&created, "new", &new_facts).await {
            panic!("write new fixture: {error}");
        }
        let created_mode = match std::fs::metadata(&created) {
            Ok(metadata) => metadata.permissions().mode() & 0o7777,
            Err(error) => panic!("stat new fixture: {error}"),
        };
        assert_eq!(created_mode, TEMP_MODE);
        remove_test_directory(&directory);
    }

    /// Verify explicit temporary cleanup removes a failed-write path.
    #[tokio::test]
    async fn failed_temp_cleanup_removes_leftover() {
        let directory = test_directory();
        let path = directory.join("leftover.tmp");
        if let Err(error) = std::fs::write(&path, b"temporary") {
            panic!("write temporary fixture: {error}");
        }
        let mut cleanup = TempCleanup::new(path.clone());
        let error = io::Error::other("primary failure");
        let returned = cleanup.with_error(error).await;
        assert_eq!(returned.kind(), ErrorKind::Other);
        assert!(!path.exists());
        remove_test_directory(&directory);
    }

    /// Verify a committed parent-sync failure exposes the authoritative outcome.
    #[test]
    fn committed_write_error_keeps_outcome() {
        let outcome = WriteOutcome {
            path: PathBuf::from("target.txt"),
            created: false,
            hard_link: false,
            bytes: 4,
        };
        let error = AtomicWriteError::Committed {
            outcome,
            error: io::Error::other("sync failed"),
        };
        match error {
            AtomicWriteError::Committed { outcome, error } => {
                assert_eq!(outcome.path, PathBuf::from("target.txt"));
                assert_eq!(outcome.bytes, 4);
                assert_eq!(error.kind(), ErrorKind::Other);
            }
            AtomicWriteError::Io(_) => panic!("committed error changed variant"),
        }
    }

    /// Reject a pathname replacement between load and atomic commit.
    #[cfg(unix)]
    #[tokio::test]
    async fn atomic_write_rejects_regular_inode_swap_before_replace() {
        use std::os::unix::fs::MetadataExt;

        let directory = test_directory();
        let target = directory.join("target.txt");
        let swapped = directory.join("swapped.txt");
        if let Err(error) = std::fs::write(&target, b"original\n") {
            panic!("write original fixture: {error}");
        }
        let facts = match load_text(&target).await {
            Ok(facts) => facts,
            Err(error) => panic!("load original fixture: {error}"),
        };

        // Keep both inodes alive while preparing the replacement. Renaming
        // the distinct same-directory file then swaps the pathname without
        // relying on allocator or inode-number reuse after unlink.
        let original_identity = std::fs::metadata(&target)
            .unwrap_or_else(|error| panic!("stat original fixture: {error}"));
        if let Err(error) = std::fs::write(&swapped, b"swapped\n") {
            panic!("write swapped fixture: {error}");
        }
        let swapped_identity = std::fs::metadata(&swapped)
            .unwrap_or_else(|error| panic!("stat swapped fixture: {error}"));
        assert_ne!(
            (original_identity.dev(), original_identity.ino()),
            (swapped_identity.dev(), swapped_identity.ino()),
            "the prepared and swapped pathnames must identify different inodes"
        );
        if let Err(error) = std::fs::rename(&swapped, &target) {
            panic!("replace prepared pathname: {error}");
        }

        let result = atomic_write(&target, "replacement\n", &facts).await;
        match result {
            Err(AtomicWriteError::Io(error)) => {
                assert_eq!(error.kind(), ErrorKind::InvalidInput);
            }
            Err(AtomicWriteError::Committed { .. }) => {
                panic!("pathname identity rejection must precede commit")
            }
            Ok(outcome) => panic!(
                "pathname swap unexpectedly committed through {}",
                outcome.path.display()
            ),
        }
        assert_eq!(
            std::fs::read(&target)
                .unwrap_or_else(|error| panic!("read swapped target fixture: {error}")),
            b"swapped\n"
        );
        remove_test_directory(&directory);
    }

    /// Reject a symlink retarget between prepared load and atomic replacement.
    #[cfg(unix)]
    #[tokio::test]
    async fn atomic_write_rejects_symlink_alias_swap_before_replace() {
        use std::os::unix::fs::symlink;

        let directory = test_directory();
        let first = directory.join("first.txt");
        let second = directory.join("second.txt");
        let link = directory.join("target.txt");
        if let Err(error) = std::fs::write(&first, b"first\n") {
            panic!("write first fixture: {error}");
        }
        if let Err(error) = std::fs::write(&second, b"second\n") {
            panic!("write second fixture: {error}");
        }
        if let Err(error) = symlink("first.txt", &link) {
            panic!("create initial symlink: {error}");
        }
        let facts = match load_text(&link).await {
            Ok(facts) => facts,
            Err(error) => panic!("load symlink fixture: {error}"),
        };
        if let Err(error) = std::fs::remove_file(&link) {
            panic!("remove initial symlink: {error}");
        }
        if let Err(error) = symlink("second.txt", &link) {
            panic!("retarget symlink: {error}");
        }

        let result = atomic_write(&link, "replacement\n", &facts).await;
        match result {
            Err(AtomicWriteError::Io(error)) => {
                assert_eq!(error.kind(), ErrorKind::InvalidInput);
            }
            Err(AtomicWriteError::Committed { .. }) => {
                panic!("symlink identity rejection must precede commit")
            }
            Ok(outcome) => panic!(
                "symlink swap unexpectedly committed through {}",
                outcome.path.display()
            ),
        }
        assert_eq!(
            std::fs::read(&first)
                .unwrap_or_else(|error| panic!("read first symlink fixture: {error}")),
            b"first\n"
        );
        assert_eq!(
            std::fs::read(&second)
                .unwrap_or_else(|error| panic!("read second symlink fixture: {error}")),
            b"second\n"
        );
        remove_test_directory(&directory);
    }

    /// Reject a hard-link inode swap before the destructive truncate/open.
    #[cfg(unix)]
    #[tokio::test]
    async fn atomic_write_rejects_hard_link_inode_swap_before_truncate() {
        use std::os::unix::fs::MetadataExt;

        let directory = test_directory();
        let target = directory.join("target.txt");
        let original_alias = directory.join("original-alias.txt");
        let swapped_source = directory.join("swapped-source.txt");
        let swapped_alias = directory.join("swapped-alias.txt");
        if let Err(error) = std::fs::write(&target, b"original\n") {
            panic!("write original fixture: {error}");
        }
        if let Err(error) = std::fs::hard_link(&target, &original_alias) {
            panic!("link original fixture: {error}");
        }
        let original_inode = std::fs::metadata(&target)
            .unwrap_or_else(|error| panic!("stat original fixture: {error}"))
            .ino();
        let facts = match load_text(&target).await {
            Ok(facts) => facts,
            Err(error) => panic!("load hard-linked fixture: {error}"),
        };

        if let Err(error) = std::fs::write(&swapped_source, b"swapped\n") {
            panic!("write swapped source: {error}");
        }
        if let Err(error) = std::fs::hard_link(&swapped_source, &swapped_alias) {
            panic!("link swapped source: {error}");
        }
        let swapped_inode = std::fs::metadata(&swapped_source)
            .unwrap_or_else(|error| panic!("stat swapped source: {error}"))
            .ino();
        assert_ne!(original_inode, swapped_inode);
        if let Err(error) = std::fs::remove_file(&target) {
            panic!("remove prepared hard-link pathname: {error}");
        }
        if let Err(error) = std::fs::hard_link(&swapped_source, &target) {
            panic!("retarget pathname to swapped inode: {error}");
        }

        let result = atomic_write(&target, "replacement\n", &facts).await;
        match result {
            Err(AtomicWriteError::Io(error)) => {
                assert_eq!(error.kind(), ErrorKind::InvalidInput);
            }
            Err(AtomicWriteError::Committed { .. }) => {
                panic!("hard-link identity rejection must precede truncate")
            }
            Ok(outcome) => panic!(
                "hard-link pathname swap unexpectedly committed through {}",
                outcome.path.display()
            ),
        }

        let final_original_inode = std::fs::metadata(&original_alias)
            .unwrap_or_else(|error| panic!("stat original alias after rejection: {error}"))
            .ino();
        let final_target_inode = std::fs::metadata(&target)
            .unwrap_or_else(|error| panic!("stat swapped target after rejection: {error}"))
            .ino();
        assert_eq!(final_original_inode, original_inode);
        assert_eq!(final_target_inode, swapped_inode);
        assert_eq!(
            std::fs::read(&original_alias)
                .unwrap_or_else(|error| panic!("read original alias after rejection: {error}")),
            b"original\n"
        );
        assert_eq!(
            std::fs::read(&swapped_source)
                .unwrap_or_else(|error| panic!("read swapped source after rejection: {error}")),
            b"swapped\n"
        );
        assert_eq!(
            std::fs::read(&swapped_alias)
                .unwrap_or_else(|error| panic!("read swapped alias after rejection: {error}")),
            b"swapped\n"
        );
        assert_eq!(
            std::fs::read(&target)
                .unwrap_or_else(|error| panic!("read swapped target after rejection: {error}")),
            b"swapped\n"
        );
        remove_test_directory(&directory);
    }
}
