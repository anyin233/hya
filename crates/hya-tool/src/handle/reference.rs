//! Parsing and rendering of internal resource URLs.

use std::fmt;
use std::str::FromStr;

use super::HandleError;

/// Resource families addressable by an internal URL.
///
/// Deliberately a closed set. An unknown scheme is an error rather than a
/// pass-through, so a typo surfaces immediately instead of resolving to
/// something surprising, and a foreign URL (`https://`, `file://`) can never be
/// mistaken for an agent-owned resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HandleScheme {
    /// Spilled tool output, addressed by artifact id.
    Artifact,
    /// A skill body, addressed by skill name.
    Skill,
    /// Agent scratch payload, addressed by a store-relative path.
    Local,
}

impl HandleScheme {
    /// Scheme text as it appears before `://`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Artifact => "artifact",
            Self::Skill => "skill",
            Self::Local => "local",
        }
    }

    /// Parse a scheme, rejecting anything not shipped.
    fn parse(text: &str) -> Result<Self, HandleError> {
        match text {
            "artifact" => Ok(Self::Artifact),
            "skill" => Ok(Self::Skill),
            "local" => Ok(Self::Local),
            other => Err(HandleError::UnknownScheme(other.to_string())),
        }
    }
}

impl fmt::Display for HandleScheme {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A slice of a resolved body, requested through the handle's query string.
///
/// Projections are applied by the router *after* any hook chain, so a hook sees
/// the whole body and the caller's `?head=40` still means the first forty lines
/// of what the hook produced.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Projection {
    /// The entire body.
    #[default]
    Whole,
    /// An inclusive 1-based line range, open-ended when `end` is absent.
    Lines {
        /// First line, 1-based.
        start: usize,
        /// Last line, inclusive.
        end: Option<usize>,
    },
    /// The first `lines` lines.
    Head {
        /// Line count.
        lines: usize,
    },
    /// The last `lines` lines.
    Tail {
        /// Line count.
        lines: usize,
    },
    /// Lines matching a regular expression.
    Grep {
        /// Pattern, compiled on resolve.
        pattern: String,
    },
    /// A field selected from a JSON body by dotted path.
    Query {
        /// Dotted path such as `.usage.input`.
        path: String,
    },
}

impl Projection {
    /// Parse one `key=value` query pair.
    fn parse(query: &str) -> Result<Self, HandleError> {
        let (key, value) = query
            .split_once('=')
            .ok_or_else(|| HandleError::MalformedQuery(query.to_string()))?;
        if value.is_empty() {
            return Err(HandleError::MalformedQuery(query.to_string()));
        }
        match key {
            "lines" => Self::parse_lines(value),
            "head" => Ok(Self::Head {
                lines: parse_count(value)?,
            }),
            "tail" => Ok(Self::Tail {
                lines: parse_count(value)?,
            }),
            "grep" => Ok(Self::Grep {
                pattern: value.to_string(),
            }),
            "q" => Ok(Self::Query {
                path: value.to_string(),
            }),
            other => Err(HandleError::UnknownQueryKey(other.to_string())),
        }
    }

    /// Parse `lines=N` or `lines=N-M`.
    fn parse_lines(value: &str) -> Result<Self, HandleError> {
        let Some((start, end)) = value.split_once('-') else {
            return Ok(Self::Lines {
                start: parse_count(value)?,
                end: None,
            });
        };
        let start = parse_count(start)?;
        let end = parse_count(end)?;
        if end < start {
            return Err(HandleError::MalformedQuery(format!("lines={value}")));
        }
        Ok(Self::Lines {
            start,
            end: Some(end),
        })
    }

    /// Query string for this projection, empty for [`Projection::Whole`].
    fn query(&self) -> String {
        match self {
            Self::Whole => String::new(),
            Self::Lines {
                start,
                end: Some(end),
            } => format!("?lines={start}-{end}"),
            Self::Lines { start, end: None } => format!("?lines={start}"),
            Self::Head { lines } => format!("?head={lines}"),
            Self::Tail { lines } => format!("?tail={lines}"),
            Self::Grep { pattern } => format!("?grep={pattern}"),
            Self::Query { path } => format!("?q={path}"),
        }
    }
}

/// Parse a positive line count. Zero is rejected: `?head=0` asks for nothing,
/// which is a mistake rather than an empty projection.
fn parse_count(value: &str) -> Result<usize, HandleError> {
    let count: usize = value
        .parse()
        .map_err(|_| HandleError::MalformedQuery(value.to_string()))?;
    if count == 0 {
        return Err(HandleError::MalformedQuery(value.to_string()));
    }
    Ok(count)
}

/// A parsed internal resource URL: `scheme://path[?projection]`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HandleRef {
    scheme: HandleScheme,
    path: String,
    projection: Projection,
}

impl HandleRef {
    /// Build a reference to a whole resource.
    ///
    /// # Errors
    /// Returns [`HandleError::MalformedPath`] when `path` is empty, absolute, or
    /// contains a `..` component.
    pub fn new(scheme: HandleScheme, path: impl Into<String>) -> Result<Self, HandleError> {
        let path = path.into();
        validate_path(&path)?;
        Ok(Self {
            scheme,
            path,
            projection: Projection::Whole,
        })
    }

    /// Resource family.
    #[must_use]
    pub const fn scheme(&self) -> HandleScheme {
        self.scheme
    }

    /// Scheme-relative resource path.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Requested slice of the body.
    #[must_use]
    pub const fn projection(&self) -> &Projection {
        &self.projection
    }

    /// Replace the projection, keeping scheme and path.
    #[must_use]
    pub fn with_projection(mut self, projection: Projection) -> Self {
        self.projection = projection;
        self
    }

    /// Whether `text` carries one of the shipped schemes.
    ///
    /// Cheap enough to call on every argument of a filesystem tool. It answers
    /// only "is this ours"; a true answer still has to `parse` to be used, and a
    /// false answer means the caller treats the text exactly as it did before —
    /// which is what keeps `read`, `write`, `grep` and `bash` unchanged for
    /// ordinary paths.
    #[must_use]
    pub fn looks_like_handle(text: &str) -> bool {
        text.split_once("://")
            .is_some_and(|(scheme, _)| HandleScheme::parse(scheme).is_ok())
    }
}

/// Reject empty, absolute, or traversing paths before they reach a store.
///
/// Every scheme resolves under a root directory, so a `..` component is the
/// difference between reading an artifact and reading the user's home. Rejecting
/// at parse time means no resolver can forget the check.
fn validate_path(path: &str) -> Result<(), HandleError> {
    if path.is_empty() {
        return Err(HandleError::MalformedPath(path.to_string()));
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return Err(HandleError::MalformedPath(path.to_string()));
    }
    if path.split(['/', '\\']).any(|part| part == "..") {
        return Err(HandleError::MalformedPath(path.to_string()));
    }
    Ok(())
}

impl FromStr for HandleRef {
    type Err = HandleError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let (scheme, rest) = text
            .split_once("://")
            .ok_or_else(|| HandleError::NotAHandle(text.to_string()))?;
        let scheme = HandleScheme::parse(scheme)?;
        let (path, projection) = match rest.split_once('?') {
            Some((path, query)) => (path, Projection::parse(query)?),
            None => (rest, Projection::Whole),
        };
        validate_path(path)?;
        Ok(Self {
            scheme,
            path: path.to_string(),
            projection,
        })
    }
}

impl fmt::Display for HandleRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}://{}{}",
            self.scheme,
            self.path,
            self.projection.query()
        )
    }
}
