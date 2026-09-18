//! Resolution of internal resource URLs to bodies.
//!
//! The router owns the scheme table. A [`HandleRef`] names a family and a
//! resource; the router turns that into bytes, runs any `artifact://` hook
//! chain, and finally applies the caller's projection — in that order, so a
//! hook always sees the whole body and `?head=40` always means the first forty
//! lines of what the caller will actually receive.

use std::path::PathBuf;

use sha2::{Digest, Sha256};

use super::HandleError;
use super::artifact::{ArtifactId, ArtifactStore, write_atomic};
use super::plane::ARTIFACT_DIR;
use super::reference::{HandleRef, HandleScheme, Projection};
use crate::SkillPlane;

/// Workspace-relative directory holding agent scratch payloads.
const LOCAL_DIR: &str = ".hya/local";

/// Subdirectory of the artifact root holding materialized projections.
const VIEW_DIR: &str = ".views";

/// A resolved handle body and what produced it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandleContent {
    /// Body after hooks and projection.
    pub body: String,
    /// Family that resolved it.
    pub scheme: HandleScheme,
    /// IANA media type of the body.
    pub media_type: String,
}

/// Resolves internal resource URLs for one session's working directory.
///
/// Holds no open handles, so tools take a clone per call.
#[derive(Clone, Default)]
pub struct HandleRouter {
    artifacts: ArtifactStore,
    local_root: PathBuf,
    workdir: PathBuf,
    skills: SkillPlane,
}

impl std::fmt::Debug for HandleRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandleRouter")
            .field("artifacts", &self.artifacts)
            .field("local_root", &self.local_root)
            .field("workdir", &self.workdir)
            .finish_non_exhaustive()
    }
}

impl HandleRouter {
    /// Route handles for a session rooted at `workdir`.
    #[must_use]
    pub fn new(workdir: impl Into<PathBuf>) -> Self {
        let workdir = workdir.into();
        Self {
            artifacts: ArtifactStore::new(workdir.join(ARTIFACT_DIR)),
            local_root: workdir.join(LOCAL_DIR),
            workdir,
            skills: SkillPlane::default(),
        }
    }

    /// Replace the artifact store, carrying whatever hook chain it holds.
    #[must_use]
    pub fn with_artifacts(mut self, artifacts: ArtifactStore) -> Self {
        self.artifacts = artifacts;
        self
    }

    /// Resolve `skill://` against an explicit catalog rather than discovery.
    #[must_use]
    pub fn with_skills(mut self, skills: SkillPlane) -> Self {
        self.skills = skills;
        self
    }

    /// Store that `artifact://` reads from and tools spill into.
    #[must_use]
    pub fn artifacts(&self) -> &ArtifactStore {
        &self.artifacts
    }

    /// Parse `text` as a handle and resolve it.
    ///
    /// # Errors
    /// Returns [`HandleError::NotAHandle`] when `text` carries no `scheme://`
    /// prefix, so a caller can fall back to treating it as a filesystem path.
    /// Otherwise propagates the errors of [`HandleRouter::resolve`].
    pub fn resolve_str(&self, text: &str) -> Result<HandleContent, HandleError> {
        self.resolve(&text.parse::<HandleRef>()?)
    }

    /// Resolve a parsed handle to its body.
    ///
    /// # Errors
    /// Returns [`HandleError::NotFound`] when the resource does not exist,
    /// [`HandleError::Io`] when it cannot be read, [`HandleError::Hook`] when an
    /// `artifact://` hook fails, and [`HandleError::MalformedQuery`] when the
    /// projection cannot be applied to the body.
    pub fn resolve(&self, reference: &HandleRef) -> Result<HandleContent, HandleError> {
        let (body, media_type) = match reference.scheme() {
            HandleScheme::Artifact => {
                let id = ArtifactId::parse(reference.path())?;
                let (meta, _) = self.artifacts.load(&id)?;
                (self.artifacts.resolve(&id)?, meta.media_type)
            }
            HandleScheme::Skill => (
                self.skills
                    .body(&self.workdir, reference.path())
                    .ok_or_else(|| HandleError::NotFound(reference.to_string()))?,
                "text/markdown".to_string(),
            ),
            HandleScheme::Local => (self.read_local(reference.path())?, "text/plain".to_string()),
        };
        Ok(HandleContent {
            body: apply_projection(&body, reference.projection())?,
            scheme: reference.scheme(),
            media_type,
        })
    }

    /// Resolve `reference` to a file holding the body a reader should present.
    ///
    /// A handle needing no transformation resolves to the stored bytes
    /// themselves. A hook chain or a projection produces *different* bytes, so
    /// those are materialized once into a content-addressed view beside the
    /// store and reused on the next identical request.
    ///
    /// Returning a path rather than a string is the point of the method: a
    /// caller that already knows how to present a file — line numbering, paging,
    /// truncation notices — presents a handle with that same code, instead of
    /// growing a second implementation of those rules that can drift from the
    /// first.
    ///
    /// # Errors
    /// As [`HandleRouter::resolve`], plus [`HandleError::Io`] when a view cannot
    /// be written.
    pub fn resolve_to_path(&self, reference: &HandleRef) -> Result<PathBuf, HandleError> {
        if let Some(path) = self.untransformed_body_path(reference) {
            return Ok(path);
        }
        let body = self.resolve(reference)?.body;
        self.materialize(reference, &body)
    }

    /// The stored file behind `reference`, when opening it gives exactly what
    /// [`HandleRouter::resolve`] would have returned.
    ///
    /// `None` for anything the router would transform, and for `skill://`, whose
    /// bodies come from a catalog rather than from a path this router knows.
    fn untransformed_body_path(&self, reference: &HandleRef) -> Option<PathBuf> {
        if reference.projection() != &Projection::Whole {
            return None;
        }
        match reference.scheme() {
            HandleScheme::Artifact if !self.artifacts.has_hooks() => {
                let id = ArtifactId::parse(reference.path()).ok()?;
                let path = self.artifacts.body_path_of(&id);
                path.exists().then_some(path)
            }
            HandleScheme::Local => self.local_path(reference.path()).ok(),
            HandleScheme::Artifact | HandleScheme::Skill => None,
        }
    }

    /// Write `body` to a content-addressed view and return its path.
    ///
    /// Keyed by both the handle text and the body digest, so an identical
    /// request reuses the file while a hook whose result changed never reads a
    /// stale one.
    fn materialize(&self, reference: &HandleRef, body: &str) -> Result<PathBuf, HandleError> {
        let mut digest = Sha256::new();
        digest.update(reference.to_string().as_bytes());
        digest.update([0]);
        digest.update(body.as_bytes());
        let name = format!("{:x}", digest.finalize());
        // An ArtifactId cannot begin with `.`, so a view can never shadow one.
        let path = self.artifacts.root().join(VIEW_DIR).join(&name[..32]);
        if !path.exists() {
            write_atomic(&path, body.as_bytes())?;
        }
        Ok(path)
    }

    /// Read a scratch payload, refusing anything that escapes the local root.
    fn read_local(&self, path: &str) -> Result<String, HandleError> {
        Ok(std::fs::read_to_string(self.local_path(path)?)?)
    }

    /// Resolve a scratch payload's path, refusing anything outside the root.
    ///
    /// [`HandleRef`] already rejects `..` and absolute paths, so this guards the
    /// case parsing cannot see: a symlink inside the root pointing out of it.
    fn local_path(&self, path: &str) -> Result<PathBuf, HandleError> {
        let target = self.local_root.join(path);
        if !target.exists() {
            return Err(HandleError::NotFound(format!("local://{path}")));
        }
        let resolved = target.canonicalize()?;
        let root = self
            .local_root
            .canonicalize()
            .unwrap_or_else(|_| self.local_root.clone());
        if !resolved.starts_with(&root) {
            return Err(HandleError::MalformedPath(path.to_string()));
        }
        // Return the lexical spelling so callers keep lexical permission
        // boundaries even when the root sits behind a symlink.
        Ok(target)
    }

    /// Resolve where a `local://` payload should be written.
    ///
    /// Only `local://` is writable. `artifact://` is an immutable capture —
    /// spilling is only safe *because* the stored bytes stay authoritative — and
    /// `skill://` is a catalog view rather than a file this router owns.
    ///
    /// # Errors
    /// Returns [`HandleError::NotWritable`] for any other scheme,
    /// [`HandleError::MalformedQuery`] when a projection is present, since a
    /// projection selects part of a body to read and means nothing on a write,
    /// [`HandleError::MalformedPath`] when the target escapes the root, and
    /// [`HandleError::Io`] when the root cannot be created.
    pub fn write_target(&self, reference: &HandleRef) -> Result<PathBuf, HandleError> {
        if reference.scheme() != HandleScheme::Local {
            return Err(HandleError::NotWritable(reference.scheme()));
        }
        if reference.projection() != &Projection::Whole {
            return Err(HandleError::MalformedQuery(reference.to_string()));
        }
        std::fs::create_dir_all(&self.local_root)?;
        let root = self.local_root.canonicalize()?;
        // Return the lexical spelling: callers keep permission boundaries
        // lexical (write.rs asserts against the un-resolved workdir), and a
        // canonical prefix would make the target look external whenever the
        // workdir itself sits behind a symlink (macOS /var -> /private/var).
        let target = self.local_root.join(reference.path());
        // The target need not exist yet, so containment is checked against the
        // nearest ancestor that does: that is the directory a symlink could
        // redirect, and the one the write would actually land under.
        let anchor = target
            .ancestors()
            .skip(1)
            .find(|candidate| candidate.exists())
            .ok_or_else(|| HandleError::MalformedPath(reference.path().to_string()))?;
        if !anchor.canonicalize()?.starts_with(&root) {
            return Err(HandleError::MalformedPath(reference.path().to_string()));
        }
        Ok(target)
    }
}

/// Slice `body` per `projection`.
fn apply_projection(body: &str, projection: &Projection) -> Result<String, HandleError> {
    match projection {
        Projection::Whole => Ok(body.to_string()),
        Projection::Lines { start, end } => {
            let skip = start.saturating_sub(1);
            let take = end.map_or(usize::MAX, |end| end.saturating_sub(skip));
            Ok(join_lines(body.lines().skip(skip).take(take)))
        }
        Projection::Head { lines } => Ok(join_lines(body.lines().take(*lines))),
        Projection::Tail { lines } => {
            let total = body.lines().count();
            Ok(join_lines(body.lines().skip(total.saturating_sub(*lines))))
        }
        Projection::Grep { pattern } => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| HandleError::MalformedQuery(format!("grep={pattern}: {e}")))?;
            Ok(join_lines(body.lines().filter(|line| re.is_match(line))))
        }
        Projection::Query { path } => project_json(body, path),
    }
}

fn join_lines<'a>(lines: impl Iterator<Item = &'a str>) -> String {
    let mut out = String::new();
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// Select a field from a JSON body by dotted path, indexing arrays by number.
fn project_json(body: &str, path: &str) -> Result<String, HandleError> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| HandleError::MalformedQuery(format!("q={path}: body is not JSON: {e}")))?;
    let mut current = &value;
    for segment in path.split('.').filter(|s| !s.is_empty()) {
        current = match current {
            serde_json::Value::Object(map) => map
                .get(segment)
                .ok_or_else(|| HandleError::NotFound(format!("q={path}")))?,
            serde_json::Value::Array(items) => {
                let index: usize = segment
                    .parse()
                    .map_err(|_| HandleError::MalformedQuery(format!("q={path}")))?;
                items
                    .get(index)
                    .ok_or_else(|| HandleError::NotFound(format!("q={path}")))?
            }
            _ => return Err(HandleError::NotFound(format!("q={path}"))),
        };
    }
    // A selected string is returned raw: `?q=.stdout` should yield the output,
    // not the output wrapped in quotes and re-escaped.
    Ok(match current {
        serde_json::Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    })
}
