//! Resource handles (`artifact://`, `skill://`, `local://`), query projections,
//! `read`/`write` handle integration, and external scheme extension dispatch.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use hya_proto::{ToolName, ToolSchema};
use hya_tool::handle::{
    ArtifactId, ArtifactStore, HandleError, HandleRef, HandleRouter, HandleScheme, Projection,
    SchemeBinding, SchemeDispatch, SchemeHandler, SchemeReadTool, SchemeRegistry,
    SchemeRegistryError, SchemeWriteTool,
};
use hya_tool::{
    Action, SkillCatalogEntry, SkillCatalogOrigin, SkillPlane, Tool, ToolCtx, ToolError,
    ToolResultPolicy,
};
use serde_json::{Value, json};

use crate::common::{TempDirGuard, allow, ctx_with, tempdir};

#[test]
fn handle_ref_parses_schemes_projections_and_rejects_paths_and_malformed() {
    for (text, scheme) in [
        ("artifact://198", HandleScheme::Artifact),
        ("skill://code-review", HandleScheme::Skill),
        ("local://notes/2026-09/plan.md", HandleScheme::Local),
    ] {
        let parsed: HandleRef = text.parse().unwrap();
        assert_eq!(parsed.scheme(), scheme);
        assert_eq!(parsed.projection(), &Projection::Whole);
        assert_eq!(parsed.to_string(), text);
    }

    for (text, expected) in [
        (
            "artifact://198?lines=10-20",
            Projection::Lines {
                start: 10,
                end: Some(20),
            },
        ),
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
                pattern: "error".into(),
            },
        ),
        (
            "artifact://1?q=.usage.input",
            Projection::Query {
                path: ".usage.input".into(),
            },
        ),
    ] {
        let parsed: HandleRef = text.parse().unwrap();
        assert_eq!(parsed.projection(), &expected, "{text}");
        assert_eq!(parsed.to_string(), text, "{text}");
    }

    assert!(HandleRef::looks_like_handle("artifact://198"));
    assert!(HandleRef::looks_like_handle("skill://x"));
    assert!(!HandleRef::looks_like_handle("src/main.rs"));
    assert!(!HandleRef::looks_like_handle("https://example.com"));

    // Plain paths, foreign schemes, malformed syntax, and traversal are rejected.
    for bad in [
        "src/main.rs",
        "/abs/path/file.txt",
        "./relative.txt",
        "../up.txt",
        "C:\\windows\\path.txt",
        "",
        "https://example.com/x",
        "file:///etc/passwd",
        "xd://resolve",
        "artifact://",
        "artifact:/198",
        "artifact://198?",
        "artifact://198?unknown=1",
        "artifact://198?lines=abc",
        "artifact://198?head=0",
        "artifact://198?lines=20-10",
        "local://../outside.txt",
        "local://notes/../../outside.txt",
        "artifact://..",
        "local:///absolute",
    ] {
        assert!(bad.parse::<HandleRef>().is_err(), "{bad} must fail");
    }
    assert!(matches!(
        "src/main.rs".parse::<HandleRef>(),
        Err(HandleError::NotAHandle(_))
    ));
}

#[test]
fn handle_router_resolves_spills_projections_local_skills_and_write_targets() {
    let dir = tempdir("handle-router");
    let _guard = TempDirGuard::new(&dir);
    let skill_dir = dir.join("skills/code-review");
    let skills = SkillPlane::from_snapshot(Arc::new(vec![SkillCatalogEntry {
        name: "code-review".into(),
        description: "Review a diff".into(),
        content: "Look for unhandled errors.\n".into(),
        allowed_tools: Vec::new(),
        model: None,
        path: skill_dir.join("SKILL.md"),
        dir: skill_dir,
        origin: SkillCatalogOrigin::Filesystem,
        disable_model_invocation: false,
    }]));
    let router = HandleRouter::new(&dir).with_skills(skills);

    // Spill & retrieve + line/grep projections.
    let body = (1..=10)
        .map(|n| format!("line {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    let meta = router
        .artifacts()
        .store("bash", "text/plain", body.as_bytes())
        .unwrap();
    let h = meta.handle();
    assert_eq!(router.resolve(&h.parse().unwrap()).unwrap().body, body);
    assert_eq!(
        router
            .resolve(&format!("{h}?head=2").parse().unwrap())
            .unwrap()
            .body,
        "line 1\nline 2\n"
    );
    assert_eq!(
        router
            .resolve(&format!("{h}?tail=2").parse().unwrap())
            .unwrap()
            .body,
        "line 9\nline 10\n"
    );
    assert_eq!(
        router
            .resolve(&format!("{h}?lines=3-4").parse().unwrap())
            .unwrap()
            .body,
        "line 3\nline 4\n"
    );
    assert_eq!(
        router
            .resolve(&format!("{h}?lines=9").parse().unwrap())
            .unwrap()
            .body,
        "line 9\nline 10\n"
    );
    assert_eq!(
        router
            .resolve(&format!("{h}?grep=line 1$").parse().unwrap())
            .unwrap()
            .body,
        "line 1\n"
    );

    // JSON query projection.
    let jbody =
        json!({ "usage": { "input": 1234 }, "items": ["alpha", "beta"], "stdout": "raw\ntext" })
            .to_string();
    let jh = router
        .artifacts()
        .store("bash", "application/json", jbody.as_bytes())
        .unwrap()
        .handle();
    assert_eq!(
        router
            .resolve(&format!("{jh}?q=.usage.input").parse().unwrap())
            .unwrap()
            .body,
        "1234"
    );
    assert_eq!(
        router
            .resolve(&format!("{jh}?q=.items.1").parse().unwrap())
            .unwrap()
            .body,
        "beta"
    );
    assert_eq!(
        router
            .resolve(&format!("{jh}?q=.stdout").parse().unwrap())
            .unwrap()
            .body,
        "raw\ntext"
    );

    // Skill resolution and missing resources.
    assert!(
        router
            .resolve(&"skill://code-review".parse().unwrap())
            .unwrap()
            .body
            .contains("unhandled errors")
    );
    for missing in ["artifact://0-0-0", "local://nope.md", "skill://nope"] {
        assert!(
            router
                .resolve(&missing.parse().unwrap())
                .unwrap_err()
                .to_string()
                .contains("not found")
        );
    }

    // ArtifactId allowlist.
    for bad in ["", "a/b", "a\\b", "..", "a.b", "a b"] {
        assert!(ArtifactId::parse(bad).is_err());
    }
    assert!(ArtifactId::parse("1758196800000-42-7").is_ok());

    // Local write target round-trip & refusals (read-only schemes, projections, symlink escape).
    let target = router
        .write_target(&"local://notes/plan.md".parse().unwrap())
        .unwrap();
    assert!(target.starts_with(dir.join(".hya/local")) && target.ends_with("notes/plan.md"));
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "# plan\n").unwrap();
    assert_eq!(
        router
            .resolve(&"local://notes/plan.md".parse().unwrap())
            .unwrap()
            .body,
        "# plan\n"
    );

    for ro in ["artifact://abc123", "skill://code-review"] {
        assert!(matches!(
            router.write_target(&ro.parse().unwrap()),
            Err(HandleError::NotWritable(_))
        ));
    }
    assert!(matches!(
        router.write_target(&"local://notes.md?head=3".parse().unwrap()),
        Err(HandleError::MalformedQuery(_))
    ));
    #[cfg(unix)]
    {
        let outside = dir.join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, dir.join(".hya/local/escape")).unwrap();
        assert!(matches!(
            router.write_target(&"local://escape/owned.txt".parse().unwrap()),
            Err(HandleError::MalformedPath(_))
        ));
    }
}

#[tokio::test]
async fn read_and_write_tools_resolve_handles_and_preserve_plain_paths() {
    let dir = tempdir("handle-rw");
    let _guard = TempDirGuard::new(&dir);
    std::fs::write(dir.join("notes.txt"), "plain file\n").unwrap();
    let store = ArtifactStore::new(dir.join(".hya/tool-output"));
    let handle = format!(
        "artifact://{}",
        store
            .store("bash", "text/plain", b"one\ntwo\nthree\nfour\n")
            .unwrap()
            .id
    );
    let ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        dir.clone(),
    );
    let reg = hya_tool_loader::builtins();
    let (read, write) = (reg.get("read").unwrap(), reg.get("write").unwrap());

    let out = read
        .execute(&ctx, json!({ "path": &handle }))
        .await
        .unwrap();
    assert!(
        out["output"].as_str().unwrap().contains("one")
            && out["output"].as_str().unwrap().contains("four")
    );

    let sliced = read
        .execute(&ctx, json!({ "path": format!("{handle}?head=2") }))
        .await
        .unwrap();
    let stxt = sliced["output"].as_str().unwrap();
    assert!(stxt.contains("one") && stxt.contains("two") && !stxt.contains("four"));

    let plain = read
        .execute(&ctx, json!({ "path": "notes.txt" }))
        .await
        .unwrap();
    assert!(plain["output"].as_str().unwrap().contains("plain file"));

    assert!(
        read.execute(&ctx, json!({ "path": "https://example.com/x" }))
            .await
            .unwrap_err()
            .to_string()
            .contains("unknown handle scheme")
    );
    assert!(
        read.execute(&ctx, json!({ "path": "artifact://does-not-exist" }))
            .await
            .unwrap_err()
            .to_string()
            .contains("artifact://does-not-exist")
    );

    write
        .execute(
            &ctx,
            json!({ "path": "local://notes/plan.md", "content": "step one\nstep two\n" }),
        )
        .await
        .unwrap();
    assert!(dir.join(".hya/local/notes/plan.md").exists());
    let local_out = read
        .execute(&ctx, json!({ "path": "local://notes/plan.md" }))
        .await
        .unwrap();
    assert!(local_out["output"].as_str().unwrap().contains("step two"));

    let ro_err = write
        .execute(&ctx, json!({ "path": &handle, "content": "overwrite" }))
        .await
        .unwrap_err();
    assert!(ro_err.to_string().contains("read-only"));
    assert!(
        read.execute(&ctx, json!({ "path": &handle }))
            .await
            .unwrap()["output"]
            .as_str()
            .unwrap()
            .contains("one")
    );
}

struct RecordingTool {
    name: String,
    body: String,
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new(self.name.clone()),
            description: "recording".into(),
            input_schema: json!({"type": "object"}),
            output_schema: None,
        }
    }
    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Coding
    }
    async fn execute(&self, _ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if let Some(r) = input.get("reference").and_then(Value::as_str) {
            self.seen.lock().unwrap().push(r.to_string());
        }
        Ok(json!({ "output": self.body }))
    }
}

#[tokio::test]
async fn scheme_registry_and_read_write_dispatch() {
    let mut reg = SchemeRegistry::new();
    reg.register(
        "db",
        SchemeBinding::new("plugin:vecdb", "vecdb__query", false),
    )
    .unwrap();
    assert_eq!(
        reg.resolve("db").map(SchemeBinding::canonical_tool),
        Some("vecdb__query")
    );
    assert!(reg.resolve("nosuch").is_none());

    for protected in ["artifact", "skill", "local"] {
        assert_eq!(
            SchemeRegistry::new()
                .register(protected, SchemeBinding::new("plugin:x", "x__y", false))
                .unwrap_err(),
            SchemeRegistryError::ProtectedScheme(protected.into())
        );
    }
    let conflict = reg
        .register("db", SchemeBinding::new("plugin:second", "second__q", true))
        .unwrap_err();
    assert!(
        conflict.to_string().contains("plugin:first")
            || conflict.to_string().contains("plugin:vecdb")
    );

    assert!(SchemeRegistry::is_valid_scheme_token("vec-db_2"));
    for bad in ["d", "", "has__sep", "no space", "no.dot"] {
        assert!(!SchemeRegistry::is_valid_scheme_token(bad));
    }

    let db_owner = Arc::new(RecordingTool {
        name: "vecdb__query".into(),
        body: "rows".into(),
        seen: Mutex::default(),
    });
    let kv_owner = Arc::new(RecordingTool {
        name: "kv__put".into(),
        body: "stored".into(),
        seen: Mutex::default(),
    });
    let dispatch = SchemeDispatch::new(
        [
            (
                "db".into(),
                SchemeHandler::new(
                    SchemeBinding::new("plugin:vecdb", "vecdb__query", false),
                    db_owner.clone(),
                ),
            ),
            (
                "kv".into(),
                SchemeHandler::new(
                    SchemeBinding::new("plugin:kv", "kv__put", true),
                    kv_owner.clone(),
                ),
            ),
        ]
        .into_iter()
        .collect(),
    );
    let builtins = hya_tool_loader::builtins();
    let read = SchemeReadTool::new(builtins.resolve("read").unwrap().tool, dispatch.clone());
    let write = SchemeWriteTool::new(builtins.resolve("write").unwrap().tool, dispatch);

    let dir = tempdir("scheme-dispatch");
    let _guard = TempDirGuard::new(&dir);
    std::fs::write(dir.join("plain.txt"), "ordinary").unwrap();
    let ctx = ctx_with(vec![allow(Action::Read, "*")], dir);

    assert_eq!(
        read.execute(&ctx, json!({ "path": "db://x/y?head=2" }))
            .await
            .unwrap()["output"],
        "rows"
    );
    assert_eq!(
        db_owner.seen.lock().unwrap().as_slice(),
        &["db://x/y?head=2"]
    );

    let err = write
        .execute(&ctx, json!({ "path": "db://x/y", "content": "row" }))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("db:// is read-only"));

    assert_eq!(
        write
            .execute(&ctx, json!({ "path": "kv://bucket/key", "content": "row" }))
            .await
            .unwrap()["output"],
        "stored"
    );
    assert_eq!(
        kv_owner.seen.lock().unwrap().as_slice(),
        &["kv://bucket/key"]
    );

    // Unregistered scheme & plain path pass through to inner read.
    assert!(
        read.execute(&ctx, json!({ "path": "unknown://x/y" }))
            .await
            .unwrap_err()
            .to_string()
            .contains("unknown handle scheme")
    );
    assert!(
        read.execute(&ctx, json!({ "path": "plain.txt" }))
            .await
            .unwrap()
            .to_string()
            .contains("ordinary")
    );
}
