//! Integration tests for `hya-tool`: internal resource URL resolution.
//!
//! Covers spill-and-retrieve and the query projections every scheme shares.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use hya_tool::SkillPlane;
use hya_tool::handle::{ArtifactId, HandleError, HandleRef, HandleRouter};

fn tempdir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("hya-handle-{tag}-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The core loop: a tool spills a large body and reports a handle; retrieving
/// the handle returns exactly what was spilled.
#[test]
fn spilled_output_resolves_through_its_handle() {
    let dir = tempdir("spill");
    let router = HandleRouter::new(&dir);
    let body = "line one\nline two\n".repeat(500);

    let meta = router
        .artifacts()
        .store("bash", "text/plain", body.as_bytes())
        .unwrap();

    assert_eq!(meta.tool, "bash");
    assert_eq!(meta.bytes as usize, body.len());
    assert!(meta.handle().starts_with("artifact://"));

    let resolved = router.resolve(&meta.handle().parse().unwrap()).unwrap();
    assert_eq!(resolved.body, body, "retrieval must be byte-exact");
    assert_eq!(resolved.media_type, "text/plain");
}

#[test]
fn projections_slice_the_resolved_body() {
    let dir = tempdir("projection");
    let router = HandleRouter::new(&dir);
    let body = (1..=10)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let meta = router
        .artifacts()
        .store("bash", "text/plain", body.as_bytes())
        .unwrap();
    let handle = meta.handle();

    let head = router
        .resolve(&format!("{handle}?head=2").parse().unwrap())
        .unwrap();
    assert_eq!(head.body, "line 1\nline 2\n");

    let tail = router
        .resolve(&format!("{handle}?tail=2").parse().unwrap())
        .unwrap();
    assert_eq!(tail.body, "line 9\nline 10\n");

    let range = router
        .resolve(&format!("{handle}?lines=3-4").parse().unwrap())
        .unwrap();
    assert_eq!(range.body, "line 3\nline 4\n");

    let open = router
        .resolve(&format!("{handle}?lines=9").parse().unwrap())
        .unwrap();
    assert_eq!(open.body, "line 9\nline 10\n");

    let grep = router
        .resolve(&format!("{handle}?grep=line 1$").parse().unwrap())
        .unwrap();
    assert_eq!(grep.body, "line 1\n");
}

#[test]
fn json_projection_selects_a_field() {
    let dir = tempdir("json");
    let router = HandleRouter::new(&dir);
    let body = serde_json::json!({
        "usage": { "input": 1234 },
        "items": ["alpha", "beta"],
        "stdout": "raw text\nwith newlines"
    })
    .to_string();
    let meta = router
        .artifacts()
        .store("bash", "application/json", body.as_bytes())
        .unwrap();
    let handle = meta.handle();

    assert_eq!(
        router
            .resolve(&format!("{handle}?q=.usage.input").parse().unwrap())
            .unwrap()
            .body,
        "1234"
    );
    assert_eq!(
        router
            .resolve(&format!("{handle}?q=.items.1").parse().unwrap())
            .unwrap()
            .body,
        "beta",
        "array segments index by number"
    );
    assert_eq!(
        router
            .resolve(&format!("{handle}?q=.stdout").parse().unwrap())
            .unwrap()
            .body,
        "raw text\nwith newlines",
        "a selected string is returned raw, not re-escaped"
    );
}

#[test]
fn local_scheme_reads_agent_scratch() {
    let dir = tempdir("local");
    let local = dir.join(".hya/local/notes");
    std::fs::create_dir_all(&local).unwrap();
    std::fs::write(local.join("plan.md"), "# plan\nstep one\n").unwrap();

    let router = HandleRouter::new(&dir);
    let content = router
        .resolve(&"local://notes/plan.md".parse().unwrap())
        .unwrap();
    assert_eq!(content.body, "# plan\nstep one\n");
}

#[test]
fn write_target_resolves_a_local_payload_path() {
    let dir = tempdir("local-write");
    let router = HandleRouter::new(&dir);

    let reference: HandleRef = "local://notes/plan.md".parse().unwrap();
    let target = router.write_target(&reference).unwrap();

    // write_target keeps the lexical spelling of the router's own root: the
    // canonical prefix would leak the resolved symlink into callers' lexical
    // permission boundaries.
    let root = dir.join(".hya/local");
    assert!(
        target.starts_with(&root),
        "a scratch payload lands under the local root: {}",
        target.display()
    );
    assert!(target.ends_with("notes/plan.md"));

    // The target need not exist yet. Writing it must make the same handle
    // resolve, which is the whole point of handing the path back.
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "# plan\n").unwrap();
    assert_eq!(
        router
            .resolve(&"local://notes/plan.md".parse().unwrap())
            .unwrap()
            .body,
        "# plan\n"
    );
}

/// `artifact://` is a real family whose bytes are deliberately immutable, so it
/// must report read-only rather than sending the caller looking for a typo.
#[test]
fn write_target_refuses_a_read_only_scheme() {
    let dir = tempdir("write-readonly");
    let router = HandleRouter::new(&dir);

    for handle in ["artifact://abc123", "skill://code-review"] {
        let reference: HandleRef = handle.parse().unwrap();
        let error = router.write_target(&reference).unwrap_err();
        assert!(
            matches!(error, HandleError::NotWritable(_)),
            "{handle} must report read-only: {error}"
        );
    }
}

#[test]
fn write_target_refuses_a_projection() {
    let dir = tempdir("write-projection");
    let router = HandleRouter::new(&dir);

    let reference: HandleRef = "local://notes.md?head=3".parse().unwrap();
    let error = router.write_target(&reference).unwrap_err();
    assert!(
        matches!(error, HandleError::MalformedQuery(_)),
        "a projection selects part of a body to read and means nothing on a write: {error}"
    );
}

/// Parsing rejects `..` and absolute paths, so the only way out of the root is a
/// symlink inside it — the case parsing cannot see.
#[cfg(unix)]
#[test]
fn write_target_refuses_a_symlink_escaping_the_scratch_root() {
    let dir = tempdir("write-escape");
    let outside = dir.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let root = dir.join(".hya/local");
    std::fs::create_dir_all(&root).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();

    let router = HandleRouter::new(&dir);
    let reference: HandleRef = "local://escape/owned.txt".parse().unwrap();
    let error = router.write_target(&reference).unwrap_err();
    assert!(
        matches!(error, HandleError::MalformedPath(_)),
        "a symlink out of the root must be refused: {error}"
    );
}

#[test]
fn skill_scheme_resolves_a_catalog_body() {
    let dir = tempdir("skill");
    let skill_dir = dir.join("skills/code-review");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: code-review\ndescription: Review a diff\n---\nLook for unhandled errors.\n",
    )
    .unwrap();

    let router = HandleRouter::new(&dir).with_skills(SkillPlane::new(vec![dir.join("skills")]));
    let content = router
        .resolve(&"skill://code-review".parse().unwrap())
        .unwrap();
    assert!(
        content.body.contains("unhandled errors"),
        "skill body must resolve: {}",
        content.body
    );
}

#[test]
fn a_missing_resource_is_a_not_found_error() {
    let dir = tempdir("missing");
    let router = HandleRouter::new(&dir);

    let error = router
        .resolve(&"artifact://0-0-0".parse().unwrap())
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");

    let error = router
        .resolve(&"local://nope.md".parse().unwrap())
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");

    let error = router
        .resolve(&"skill://nope".parse().unwrap())
        .unwrap_err();
    assert!(error.to_string().contains("not found"), "{error}");
}

/// An artifact id becomes a filename, so it is an allowlist rather than a
/// traversal check: nothing with a separator can ever address a file.
#[test]
fn artifact_ids_reject_anything_that_is_not_an_id() {
    for bad in ["", "a/b", "a\\b", "..", "a.b", "a b"] {
        assert!(
            ArtifactId::parse(bad).is_err(),
            "{bad:?} must not parse as an artifact id"
        );
    }
    assert!(ArtifactId::parse("1758196800000-42-7").is_ok());
}

/// A handle that resolves nothing must not be confused with a filesystem path;
/// the router reports it as not-a-handle so callers can fall back to path
/// handling, which is what keeps `read`/`write`/`grep`/`bash` unchanged.
#[test]
fn a_plain_path_is_reported_as_not_a_handle() {
    let dir = tempdir("plain");
    let router = HandleRouter::new(&dir);

    let error = match "src/main.rs".parse::<HandleRef>() {
        Err(error) => error,
        Ok(reference) => router.resolve(&reference).unwrap_err(),
    };
    assert!(
        matches!(error, hya_tool::handle::HandleError::NotAHandle(_)),
        "{error}"
    );
}
