//! Spilled tool output: the store behind `artifact://`.
//!
//! A tool that produces more output than belongs in a transcript writes the
//! bytes here and puts `artifact://<id>` in its result. The body stays on disk
//! at full fidelity; the model spends tokens on it only when it asks.
//!
//! Writes are staged under a private temporary name and published by rename, so
//! a reader never observes a half-written artifact and an interrupted call
//! leaves no partial file behind.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;

use serde::{Deserialize, Serialize};

use super::HandleError;

/// Disambiguates artifacts written within the same millisecond by one process.
static NEXT_ARTIFACT_SEQ: AtomicU64 = AtomicU64::new(0);

/// Suffix of the sidecar file holding an artifact's metadata.
const META_SUFFIX: &str = ".meta.json";

/// Identity of one spilled payload.
///
/// The character set is a deliberate allowlist rather than a traversal check:
/// an id becomes a filename, and permitting only `[A-Za-z0-9_-]` means no
/// separator, prefix, or encoding trick can address anything but an artifact.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactId(String);

impl ArtifactId {
    /// Validate an id received from a handle.
    ///
    /// # Errors
    /// Returns [`HandleError::MalformedPath`] for an empty id or one holding a
    /// character outside `[A-Za-z0-9_-]`.
    pub fn parse(text: &str) -> Result<Self, HandleError> {
        if text.is_empty()
            || !text
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(HandleError::MalformedPath(text.to_string()));
        }
        Ok(Self(text.to_string()))
    }

    /// Id text, which is also the artifact's filename.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ArtifactId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a stored artifact is, independent of its bytes.
///
/// Persisted beside the body so a hook can discriminate on the producing tool
/// or the payload type without parsing the body first.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactMeta {
    /// Identity, and the body's filename.
    pub id: ArtifactId,
    /// Canonical name of the tool that produced the payload.
    pub tool: String,
    /// IANA media type of the body; `text/plain` when the tool knows no better.
    pub media_type: String,
    /// Size of the stored body in bytes.
    pub bytes: u64,
    /// Unix milliseconds at which the artifact was published.
    pub created_ms: u64,
}

impl ArtifactMeta {
    /// Handle that retrieves this artifact.
    #[must_use]
    pub fn handle(&self) -> String {
        format!("artifact://{}", self.id)
    }
}

/// User-supplied post-processing for a retrieved artifact body.
///
/// Hooks run on **retrieval**, never on write. The stored bytes stay the
/// authoritative capture, so a hook that is wrong — or that someone changes
/// their mind about — can never have destroyed the original output.
///
/// Applicable hooks are chained in registration order, each transforming the
/// previous one's result, which is what lets "strip noise, then extract the
/// failing assertion" compose out of two independent hooks.
pub trait ArtifactHook: Send + Sync {
    /// Hook name, used to attribute a failure back to its registration.
    fn name(&self) -> &str;

    /// Whether this hook should run for `meta`. Defaults to every artifact.
    fn applies_to(&self, meta: &ArtifactMeta) -> bool {
        let _ = meta;
        true
    }

    /// Transform `body`, or return `None` to pass it through untouched.
    ///
    /// # Errors
    /// Return [`HandleError::Hook`] to fail the retrieval. The failure is
    /// surfaced rather than swallowed: a silently skipped hook looks exactly
    /// like a hook that ran and found nothing to do.
    fn post_process(
        &self,
        meta: &ArtifactMeta,
        body: String,
    ) -> Result<Option<String>, HandleError>;
}

/// Store for spilled tool output under one root directory.
///
/// Cloning shares the hook chain; the store owns no open handles, so a clone
/// per tool call is cheap.
#[derive(Clone, Default)]
pub struct ArtifactStore {
    root: PathBuf,
    hooks: Vec<Arc<dyn ArtifactHook>>,
}

impl std::fmt::Debug for ArtifactStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactStore")
            .field("root", &self.root)
            .field("hooks", &self.hook_names())
            .finish()
    }
}

impl ArtifactStore {
    /// Store artifacts under `root`, creating it lazily on first write.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            hooks: Vec::new(),
        }
    }

    /// Append a post-processing hook to the retrieval chain.
    #[must_use]
    pub fn with_hook(mut self, hook: Arc<dyn ArtifactHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    /// Directory holding the artifacts.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Names of the registered hooks, in chain order.
    #[must_use]
    pub fn hook_names(&self) -> Vec<&str> {
        self.hooks.iter().map(|hook| hook.name()).collect()
    }

    /// Whether any hook would transform a retrieved body.
    ///
    /// Lets a caller decide between reading the stored bytes directly and paying
    /// for a resolve that runs the chain.
    #[must_use]
    pub fn has_hooks(&self) -> bool {
        !self.hooks.is_empty()
    }

    /// Where the body for `id` is stored, whether or not it exists yet.
    ///
    /// Exposed so a reader that wants the raw artifact can open it in place
    /// rather than copying the bytes through memory into a second file.
    #[must_use]
    pub fn body_path_of(&self, id: &ArtifactId) -> PathBuf {
        self.body_path(id)
    }

    /// Write `body` and return the metadata naming it.
    ///
    /// # Errors
    /// Returns [`HandleError::Io`] when the root cannot be created or the body
    /// cannot be written and published.
    pub fn store(
        &self,
        tool: &str,
        media_type: &str,
        body: &[u8],
    ) -> Result<ArtifactMeta, HandleError> {
        let created_ms = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis()),
        )
        .unwrap_or(u64::MAX);
        let seq = NEXT_ARTIFACT_SEQ.fetch_add(1, Ordering::Relaxed);
        let id = ArtifactId(format!("{created_ms}-{}-{seq}", std::process::id()));
        let meta = ArtifactMeta {
            id: id.clone(),
            tool: tool.to_string(),
            media_type: media_type.to_string(),
            bytes: u64::try_from(body.len()).unwrap_or(u64::MAX),
            created_ms,
        };
        // Body first: a reader that finds metadata always finds the payload it
        // describes, and an interrupted write leaves an unreferenced body
        // rather than a handle pointing at nothing.
        write_atomic(&self.body_path(&id), body)?;
        let encoded = serde_json::to_vec(&meta)
            .map_err(|e| HandleError::MalformedPath(format!("encode artifact metadata: {e}")))?;
        write_atomic(&self.meta_path(&id), &encoded)?;
        Ok(meta)
    }

    /// Read the stored metadata and raw body, before any hook runs.
    ///
    /// # Errors
    /// Returns [`HandleError::NotFound`] when no artifact carries `id`, or
    /// [`HandleError::Io`] when it exists but cannot be read.
    pub fn load(&self, id: &ArtifactId) -> Result<(ArtifactMeta, String), HandleError> {
        let body_path = self.body_path(id);
        if !body_path.exists() {
            return Err(HandleError::NotFound(format!("artifact://{id}")));
        }
        let body = std::fs::read_to_string(&body_path)?;
        let meta = self.load_meta(id, &body);
        Ok((meta, body))
    }

    /// Read an artifact and run the applicable hook chain over it.
    ///
    /// # Errors
    /// Propagates [`ArtifactHook::post_process`] failures, plus the errors of
    /// [`ArtifactStore::load`]. A failing hook never modifies what is on disk.
    pub fn resolve(&self, id: &ArtifactId) -> Result<String, HandleError> {
        let (meta, mut body) = self.load(id)?;
        for hook in &self.hooks {
            if !hook.applies_to(&meta) {
                continue;
            }
            if let Some(next) = hook.post_process(&meta, body.clone())? {
                body = next;
            }
        }
        Ok(body)
    }

    fn body_path(&self, id: &ArtifactId) -> PathBuf {
        self.root.join(id.as_str())
    }

    fn meta_path(&self, id: &ArtifactId) -> PathBuf {
        self.root.join(format!("{id}{META_SUFFIX}"))
    }

    /// Stored metadata, or a description derived from the body.
    ///
    /// A body with no readable sidecar is still a usable artifact, so the
    /// fallback keeps retrieval working instead of failing the whole handle for
    /// a missing annotation.
    fn load_meta(&self, id: &ArtifactId, body: &str) -> ArtifactMeta {
        std::fs::read(self.meta_path(id))
            .ok()
            .and_then(|raw| serde_json::from_slice::<ArtifactMeta>(&raw).ok())
            .unwrap_or_else(|| ArtifactMeta {
                id: id.clone(),
                tool: String::new(),
                media_type: "text/plain".to_string(),
                bytes: u64::try_from(body.len()).unwrap_or(u64::MAX),
                created_ms: 0,
            })
    }
}

/// Write `bytes` to `path` so a reader sees either the old file or the new one.
pub(super) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), HandleError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("artifact");
    let staged = dir.join(format!(".{name}.tmp"));
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&staged)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&staged, path)?;
    Ok(())
}
