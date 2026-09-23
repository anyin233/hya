//! Bundle API endpoint vocabulary: HTTP methods, scopes, and path templates.
//!
//! A bundle with an explicit `extensions.process` may register its own HTTP
//! endpoints (manifest `apis:`). Each endpoint is one `(method, scope, path
//! template)` triple. The grammar and the matching rules live here so that
//! prepare (which rejects bad or ambiguous declarations) and the runtime
//! (which routes concrete request paths) share one implementation.
//!
//! Path template grammar:
//!
//! - a leading `/` followed by 1..=[`MAX_API_PATH_SEGMENTS`] segments separated
//!   by single `/` (no empty segments, no trailing `/`), at most
//!   [`MAX_API_PATH_BYTES`] bytes in total;
//! - a segment is either a literal `[A-Za-z0-9._-]+` (but not `.` or `..`) or
//!   a parameter `{name}` whose name is `[A-Za-z_][A-Za-z0-9_]*` (at most
//!   [`MAX_API_PARAM_NAME_BYTES`] bytes) and unique within the template;
//! - no wildcards and no partial-segment parameters (`/a{b}` is rejected).
//!
//! Two templates *overlap* when some concrete path matches both: same segment
//! count and, position by position, equal literals or at least one parameter.
//! Prepare rejects overlapping templates under the same method and scope, so
//! every concrete path resolves to at most one endpoint per method.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

/// Maximum UTF-8 byte length of a path template.
pub const MAX_API_PATH_BYTES: usize = 256;
/// Maximum number of segments in a path template.
pub const MAX_API_PATH_SEGMENTS: usize = 16;
/// Maximum byte length of a path parameter name.
pub const MAX_API_PARAM_NAME_BYTES: usize = 64;
/// Maximum byte length of a concrete request path (below the bundle mount).
pub const MAX_API_REQUEST_PATH_BYTES: usize = 4096;

/// HTTP method a bundle endpoint answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ApiMethod {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
    /// `PUT`.
    Put,
    /// `PATCH`.
    Patch,
    /// `DELETE`.
    Delete,
}

impl ApiMethod {
    /// Every supported method, in canonical order.
    pub const ALL: [Self; 5] = [Self::Get, Self::Post, Self::Put, Self::Patch, Self::Delete];

    /// Upper-case wire name (`GET`, `POST`, …).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }

    /// Parse an upper-case method name; anything else is `None`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|method| method.as_str() == name)
    }
}

impl fmt::Display for ApiMethod {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Where a bundle endpoint is mounted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiScope {
    /// `/v1/sessions/{session}/bundles/{bundle}/…`: bound to one existing
    /// session, whose data the process may read through its capability.
    Session,
    /// `/v1/bundles/{bundle}/api/…`: not tied to any session.
    Global,
}

impl ApiScope {
    /// Lower-case wire name (`session` or `global`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Global => "global",
        }
    }

    /// Parse a lower-case scope name; anything else is `None`.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        [Self::Session, Self::Global]
            .into_iter()
            .find(|scope| scope.as_str() == name)
    }
}

impl fmt::Display for ApiScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One segment of a parsed path template.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApiTemplateSegment {
    /// A literal segment compared byte-for-byte.
    Literal(String),
    /// A `{name}` parameter matching any one non-empty segment.
    Param(String),
}

/// A validated endpoint path template such as `/items/{id}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiPathTemplate {
    raw: String,
    segments: Vec<ApiTemplateSegment>,
}

impl ApiPathTemplate {
    /// Parse and validate `template` against the grammar in the module docs.
    ///
    /// # Errors
    /// A human-readable reason when the template is not canonical.
    pub fn parse(template: &str) -> Result<Self, String> {
        if template.len() > MAX_API_PATH_BYTES {
            return Err(format!("must be at most {MAX_API_PATH_BYTES} bytes"));
        }
        let Some(rest) = template.strip_prefix('/') else {
            return Err("must start with `/`".to_string());
        };
        if rest.is_empty() {
            return Err("must name at least one segment".to_string());
        }
        let mut segments = Vec::new();
        let mut params = BTreeSet::new();
        for segment in rest.split('/') {
            if segments.len() == MAX_API_PATH_SEGMENTS {
                return Err(format!(
                    "must have at most {MAX_API_PATH_SEGMENTS} segments"
                ));
            }
            if segment.is_empty() {
                return Err("must not contain empty segments or a trailing `/`".to_string());
            }
            if let Some(name) = segment
                .strip_prefix('{')
                .and_then(|inner| inner.strip_suffix('}'))
            {
                if !is_param_name(name) {
                    return Err(format!(
                        "parameter `{{{name}}}` must be named `[A-Za-z_][A-Za-z0-9_]*` \
                         (at most {MAX_API_PARAM_NAME_BYTES} bytes)"
                    ));
                }
                if !params.insert(name) {
                    return Err(format!("parameter `{{{name}}}` appears more than once"));
                }
                segments.push(ApiTemplateSegment::Param(name.to_string()));
            } else if is_literal_segment(segment) {
                segments.push(ApiTemplateSegment::Literal(segment.to_string()));
            } else {
                return Err(format!(
                    "segment `{segment}` must be a `[A-Za-z0-9._-]+` literal (not `.` or `..`) \
                     or a whole-segment `{{name}}` parameter"
                ));
            }
        }
        Ok(Self {
            raw: template.to_string(),
            segments,
        })
    }

    /// The template text as declared (already canonical).
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Parsed segments.
    #[must_use]
    pub fn segments(&self) -> &[ApiTemplateSegment] {
        &self.segments
    }

    /// Whether some concrete path matches both `self` and `other`.
    #[must_use]
    pub fn overlaps(&self, other: &Self) -> bool {
        self.segments.len() == other.segments.len()
            && self
                .segments
                .iter()
                .zip(&other.segments)
                .all(|pair| match pair {
                    (ApiTemplateSegment::Literal(left), ApiTemplateSegment::Literal(right)) => {
                        left == right
                    }
                    _ => true,
                })
    }

    /// Match percent-decoded request `segments`, returning the parameter
    /// bindings on success. Parameters never match an empty segment.
    #[must_use]
    pub fn match_segments(&self, segments: &[String]) -> Option<BTreeMap<String, String>> {
        if segments.len() != self.segments.len() {
            return None;
        }
        let mut params = BTreeMap::new();
        for (template, actual) in self.segments.iter().zip(segments) {
            match template {
                ApiTemplateSegment::Literal(literal) if literal == actual => {}
                ApiTemplateSegment::Param(name) if !actual.is_empty() => {
                    params.insert(name.clone(), actual.clone());
                }
                _ => return None,
            }
        }
        Some(params)
    }
}

impl fmt::Display for ApiPathTemplate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.raw)
    }
}

/// Split a concrete request path (below the bundle mount, for example
/// `/items/a%2Fb`) into percent-decoded segments.
///
/// Empty segments are kept (they never match a template), so `/items/` is a
/// well-formed path that simply resolves to no endpoint.
///
/// # Errors
/// A reason when the path is empty, lacks the leading `/`, is longer than
/// [`MAX_API_REQUEST_PATH_BYTES`], carries a query or fragment, or holds an
/// invalid percent escape or non-UTF-8 bytes after decoding.
pub fn split_request_path(path: &str) -> Result<Vec<String>, String> {
    if path.len() > MAX_API_REQUEST_PATH_BYTES {
        return Err(format!(
            "path must be at most {MAX_API_REQUEST_PATH_BYTES} bytes"
        ));
    }
    let Some(rest) = path.strip_prefix('/') else {
        return Err("path must start with `/`".to_string());
    };
    if path.contains(['?', '#']) {
        return Err("path must not contain `?` or `#`".to_string());
    }
    rest.split('/').map(percent_decode).collect()
}

fn percent_decode(segment: &str) -> Result<String, String> {
    let bytes = segment.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes.get(index + 1).and_then(|byte| hex_value(*byte));
            let low = bytes.get(index + 2).and_then(|byte| hex_value(*byte));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(format!(
                    "invalid percent escape in path segment `{segment}`"
                ));
            };
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| format!("path segment `{segment}` is not UTF-8 after percent-decoding"))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_param_name(name: &str) -> bool {
    name.len() <= MAX_API_PARAM_NAME_BYTES
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn is_literal_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(text: &str) -> ApiPathTemplate {
        match ApiPathTemplate::parse(text) {
            Ok(template) => template,
            Err(error) => panic!("{text}: {error}"),
        }
    }

    #[test]
    fn templates_parse_and_match_with_decoded_params() {
        let items = template("/items/{id}/tags/{tag}");
        let segments = match split_request_path("/items/a%2Fb/tags/x%20y") {
            Ok(segments) => segments,
            Err(error) => panic!("{error}"),
        };
        let params = items.match_segments(&segments);
        assert_eq!(
            params,
            Some(BTreeMap::from([
                ("id".to_string(), "a/b".to_string()),
                ("tag".to_string(), "x y".to_string()),
            ]))
        );
        assert_eq!(
            items.match_segments(&["items".into(), String::new(), "tags".into(), "x".into()]),
            None,
            "a parameter never matches an empty segment"
        );
        assert_eq!(
            template("/usage").match_segments(&["usage".into()]),
            Some(BTreeMap::new())
        );
    }

    #[test]
    fn invalid_templates_are_rejected() {
        for bad in [
            "",
            "/",
            "usage",
            "/a/",
            "/a//b",
            "/a/{}",
            "/a/{1x}",
            "/a/{x}/{x}",
            "/a{b}",
            "/a/*",
            "/a/..",
            "/a/.",
            "/a b",
            "/a/%20",
        ] {
            assert!(
                ApiPathTemplate::parse(bad).is_err(),
                "{bad:?} must be rejected"
            );
        }
        let deep = "/a".repeat(MAX_API_PATH_SEGMENTS + 1);
        assert!(ApiPathTemplate::parse(&deep).is_err());
        let long = format!("/{}", "a".repeat(MAX_API_PATH_BYTES));
        assert!(ApiPathTemplate::parse(&long).is_err());
    }

    #[test]
    fn overlap_is_decided_position_by_position() {
        assert!(template("/a/{x}").overlaps(&template("/a/b")));
        assert!(template("/{x}/b").overlaps(&template("/a/{y}")));
        assert!(!template("/a/b").overlaps(&template("/a/c")));
        assert!(!template("/a/{x}").overlaps(&template("/a/{x}/c")));
    }

    #[test]
    fn request_paths_reject_bad_escapes() {
        assert!(split_request_path("/a/%zz").is_err());
        assert!(split_request_path("/a/%ff").is_err(), "not UTF-8");
        assert!(split_request_path("a").is_err());
        assert_eq!(
            split_request_path("/a/"),
            Ok(vec!["a".to_string(), String::new()])
        );
    }

    #[test]
    fn methods_and_scopes_round_trip_their_wire_names() {
        for method in ApiMethod::ALL {
            assert_eq!(ApiMethod::parse(method.as_str()), Some(method));
        }
        assert_eq!(ApiMethod::parse("get"), None);
        assert_eq!(ApiScope::parse("global"), Some(ApiScope::Global));
    }
}
