//! Parsing and rendering of internal resource URLs.
//!
//! These handles index the agent's own resources — spilled tool output, skill
//! bodies, scratch payloads. They are deliberately *not* a filesystem path
//! syntax: `read`, `write`, `grep` and `bash` keep treating every plain path
//! exactly as before, and a handle only means something to the tools that opt
//! into resolving one.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_tool::handle::{HandleRef, HandleScheme, Projection};

#[test]
fn round_trips_scheme_and_path() {
    let parsed: HandleRef = "artifact://198".parse().unwrap();
    assert_eq!(parsed.scheme(), HandleScheme::Artifact);
    assert_eq!(parsed.path(), "198");
    assert_eq!(parsed.to_string(), "artifact://198");
}

#[test]
fn every_shipped_scheme_parses() {
    for (text, scheme) in [
        ("artifact://198", HandleScheme::Artifact),
        ("skill://code-review", HandleScheme::Skill),
        ("local://notes/plan.md", HandleScheme::Local),
    ] {
        let parsed: HandleRef = text.parse().unwrap();
        assert_eq!(parsed.scheme(), scheme, "{text}");
        assert_eq!(parsed.to_string(), text, "{text}");
    }
}

#[test]
fn nested_path_survives_the_round_trip() {
    let parsed: HandleRef = "local://notes/2026-09/plan.md".parse().unwrap();
    assert_eq!(parsed.path(), "notes/2026-09/plan.md");
    assert_eq!(parsed.to_string(), "local://notes/2026-09/plan.md");
}

#[test]
fn query_parses_into_a_projection() {
    let parsed: HandleRef = "artifact://198?lines=10-20".parse().unwrap();
    assert_eq!(parsed.path(), "198");
    assert_eq!(
        parsed.projection(),
        &Projection::Lines {
            start: 10,
            end: Some(20)
        }
    );
    assert_eq!(parsed.to_string(), "artifact://198?lines=10-20");
}

#[test]
fn each_projection_form_parses() {
    let cases = [
        (
            "artifact://1?lines=5",
            Projection::Lines {
                start: 5,
                end: None,
            },
        ),
        ("artifact://1?head=40", Projection::Head { lines: 40 }),
        ("artifact://1?tail=40", Projection::Tail { lines: 40 }),
        (
            "artifact://1?grep=error",
            Projection::Grep {
                pattern: "error".to_string(),
            },
        ),
        (
            "artifact://1?q=.usage.input",
            Projection::Query {
                path: ".usage.input".to_string(),
            },
        ),
    ];
    for (text, expected) in cases {
        let parsed: HandleRef = text.parse().unwrap();
        assert_eq!(parsed.projection(), &expected, "{text}");
        assert_eq!(parsed.to_string(), text, "{text}");
    }
}

#[test]
fn no_query_means_the_whole_body() {
    let parsed: HandleRef = "artifact://198".parse().unwrap();
    assert_eq!(parsed.projection(), &Projection::Whole);
}

/// The constraint the user set: introducing these URLs must not change how the
/// filesystem tools see ordinary paths. A path is never silently a handle.
#[test]
fn plain_filesystem_paths_are_not_handles() {
    for text in [
        "src/main.rs",
        "/abs/path/file.txt",
        "./relative.txt",
        "../up.txt",
        "C:\\windows\\path.txt",
        "file.txt",
        "",
        "no-scheme-here",
    ] {
        assert!(
            text.parse::<HandleRef>().is_err(),
            "{text} must not parse as a handle"
        );
    }
}

/// A URL that is not one of ours stays not ours, so a fetched `https://` URL is
/// never mistaken for an agent resource.
#[test]
fn foreign_schemes_are_rejected() {
    for text in [
        "https://example.com/x",
        "file:///etc/passwd",
        "xd://resolve",
        "agent://Main",
    ] {
        assert!(
            text.parse::<HandleRef>().is_err(),
            "{text} must not parse as a shipped handle"
        );
    }
}

#[test]
fn malformed_handles_are_rejected() {
    for text in [
        "artifact://",
        "artifact:/198",
        "artifact:198",
        "://198",
        "artifact://198?",
        "artifact://198?unknown=1",
        "artifact://198?lines=",
        "artifact://198?lines=abc",
        "artifact://198?head=0",
        "artifact://198?lines=20-10",
    ] {
        assert!(
            text.parse::<HandleRef>().is_err(),
            "{text} must be rejected"
        );
    }
}

/// Path traversal through a handle must not escape the store root.
#[test]
fn traversal_is_rejected() {
    for text in [
        "local://../outside.txt",
        "local://notes/../../outside.txt",
        "artifact://..",
        "local:///absolute",
    ] {
        assert!(
            text.parse::<HandleRef>().is_err(),
            "{text} must be rejected as traversal"
        );
    }
}

#[test]
fn detects_handles_without_committing_to_one() {
    assert!(HandleRef::looks_like_handle("artifact://198"));
    assert!(HandleRef::looks_like_handle("skill://x"));
    assert!(!HandleRef::looks_like_handle("src/main.rs"));
    assert!(!HandleRef::looks_like_handle("https://example.com"));
}
