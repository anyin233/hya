//! Integration tests for `ls`, `find`, `glob`, and `grep` (including hashline recovery).

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use hya_tool::{Action, Decision, PermissionInterceptor, Resource, ToolCtx, ToolError};
use serde_json::{Value, json};

use crate::common::{TempDirGuard, allow, ctx_with, deny, tempdir};

const MAX_GREP_LOGICAL_LINE_BYTES: usize = 1024 * 1024;
const MAX_GLOB_BYTES: usize = 4096;
const MAX_IGNORE_RULES: usize = 10_000;
const MAX_WARNING_BYTES: usize = 4096;

struct RecordingPermissionInterceptor {
    requests: Arc<Mutex<Vec<(Action, String)>>>,
}

#[async_trait::async_trait]
impl PermissionInterceptor for RecordingPermissionInterceptor {
    async fn intercept(
        &self,
        _session: Option<hya_proto::SessionId>,
        action: Action,
        resource: &Resource,
    ) -> Option<Decision> {
        let req = (action, resource.pattern());
        match self.requests.lock() {
            Ok(mut v) => v.push(req),
            Err(p) => p.into_inner().push(req),
        }
        Some(Decision::Reject { feedback: None })
    }
}

fn search_ctx(workdir: std::path::PathBuf) -> ToolCtx {
    ctx_with(
        vec![
            allow(Action::Read, "*"),
            allow(Action::Edit, "*"),
            allow(Action::Glob, "*"),
            allow(Action::Grep, "*"),
            allow(Action::ExternalDirectory, "*"),
        ],
        workdir,
    )
}

fn display_rows_for<'a>(result: &'a Value, suffix: &str) -> &'a [Value] {
    result["metadata"]["display"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["path"].as_str().is_some_and(|p| p.ends_with(suffix)))
        .unwrap_or_else(|| panic!("no group ending in {suffix:?}"))["rows"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap()
}

fn display_paths(result: &Value) -> Vec<&str> {
    result["metadata"]["display"]["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["path"].as_str().unwrap())
        .collect()
}

fn grep_anchor(result: &Value, line: usize, text: &str) -> String {
    result["output"]
        .as_str()
        .unwrap()
        .lines()
        .find_map(|row| {
            let (prefix, content) = row.trim_start().split_once(':')?;
            let (num, hash) = prefix.split_once('#')?;
            if num.parse::<usize>().ok()? == line && !hash.is_empty() && content == text {
                Some(prefix.to_owned())
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("no grep anchor for line {line}"))
}

#[tokio::test]
async fn ls_lists_entries_with_type_size_default_cwd_and_permission() {
    let dir = tempdir("ls-test");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::write(dir.join("a.txt"), "hello").await.unwrap();
    tokio::fs::create_dir_all(dir.join("sub")).await.unwrap();
    let ls = hya_tool_loader::builtins().get("ls").unwrap();

    let ctx = ctx_with(vec![allow(Action::Read, "/**")], dir.clone());
    let out = ls.execute(&ctx, json!({ "path": dir })).await.unwrap();
    let entries = out["entries"].as_array().unwrap();
    assert_eq!(
        entries.iter().find(|e| e["name"] == "a.txt").unwrap()["size"],
        5
    );
    assert_eq!(
        entries.iter().find(|e| e["name"] == "sub").unwrap()["type"],
        "dir"
    );

    let default_out = ls.execute(&ctx, json!({})).await.unwrap();
    assert!(
        default_out["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "a.txt")
    );

    let denied = ctx_with(vec![deny(Action::Read, "/**")], dir.clone());
    assert!(ls.execute(&denied, json!({ "path": dir })).await.is_err());
}

#[tokio::test]
async fn find_matches_relative_absolute_and_blocks_external_paths() {
    let dir = tempdir("find-test");
    let _guard = TempDirGuard::new(&dir);
    let outside = tempdir("find-out");
    let _out_guard = TempDirGuard::new(&outside);
    tokio::fs::create_dir_all(dir.join("src/sub"))
        .await
        .unwrap();
    tokio::fs::write(dir.join("src/a.rs"), "fn a(){}")
        .await
        .unwrap();
    tokio::fs::write(dir.join("src/sub/b.txt"), "bb")
        .await
        .unwrap();
    tokio::fs::write(dir.join("c.txt"), "ccc").await.unwrap();
    tokio::fs::write(outside.join("secret.txt"), "s")
        .await
        .unwrap();
    let find = hya_tool_loader::builtins().get("find").unwrap();

    let ctx = ctx_with(
        vec![
            allow(Action::Glob, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        dir.clone(),
    );
    let rel = find
        .execute(&ctx, json!({ "pattern": "*", "path": "src" }))
        .await
        .unwrap();
    let mut rel_paths: Vec<_> = rel["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["path"].as_str().unwrap())
        .collect();
    rel_paths.sort();
    assert_eq!(rel_paths.len(), 2);
    assert!(rel_paths[0].ends_with("src/a.rs") && rel_paths[1].ends_with("src/sub/b.txt"));

    let abs = find
        .execute(&ctx, json!({ "pattern": "*.rs", "path": dir }))
        .await
        .unwrap();
    assert_eq!(abs["results"].as_array().unwrap().len(), 1);
    assert_eq!(abs["results"][0]["size"], 8);

    for bad_path in [outside.to_string_lossy().into_owned(), "../".to_string()] {
        assert!(matches!(
            find.execute(&ctx, json!({ "pattern": "*", "path": bad_path }))
                .await,
            Err(ToolError::Permission(_))
        ));
    }
    let denied = ctx_with(vec![deny(Action::Glob, "*")], dir);
    assert!(
        find.execute(&denied, json!({ "pattern": "*.rs" }))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn glob_matching_negated_classes_cap_boundary_cancellation_and_validation() {
    let dir = tempdir("glob-main");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::create_dir_all(dir.join("src")).await.unwrap();
    tokio::fs::write(dir.join("src/main.rs"), "fn main() {}\n")
        .await
        .unwrap();
    for name in ["a.txt", "b.txt", "c.txt"] {
        tokio::fs::write(dir.join(name), "x\n").await.unwrap();
    }
    let glob = hya_tool_loader::builtins().get("glob").unwrap();
    let ctx = search_ctx(dir.clone());

    // OpenCode output shape & rejects file target.
    let out = glob
        .execute(&ctx, json!({ "pattern": "*.rs", "path": "src" }))
        .await
        .unwrap();
    assert_eq!(out["title"], "src");
    assert_eq!(out["metadata"]["count"], 1);
    assert_eq!(out["metadata"]["truncated"], false);
    assert!(
        out["output"]
            .as_str()
            .unwrap()
            .replace('\\', "/")
            .ends_with("src/main.rs")
    );

    let file_err = glob
        .execute(&ctx, json!({ "pattern": "*.rs", "path": "src/main.rs" }))
        .await
        .unwrap_err();
    assert!(
        file_err
            .to_string()
            .contains("glob path must be a directory")
    );

    // Negated character classes `[!a]` and `[^a]`.
    for pat in ["[!a].txt", "[^a].txt"] {
        let res = glob.execute(&ctx, json!({ "pattern": pat })).await.unwrap();
        assert_eq!(res["metadata"]["count"], 2);
        let paths = res["paths"].as_array().unwrap();
        assert!(
            paths
                .iter()
                .all(|p| !p.as_str().unwrap().ends_with("a.txt"))
        );
    }

    // Closed input & over-budget pattern rejection.
    for bad in [
        json!({ "pattern": "*.rs", "unknown": true }),
        json!({ "pattern": "*.rs", "path": null }),
        json!({ "pattern": "x".repeat(MAX_GLOB_BYTES + 1) }),
    ] {
        assert!(matches!(
            glob.execute(&ctx, bad).await,
            Err(ToolError::Input(_))
        ));
    }

    // Cancellation.
    let cancel_ctx = search_ctx(dir);
    cancel_ctx.cancel.cancel();
    assert!(matches!(
        glob.execute(&cancel_ctx, json!({ "pattern": "**/*.txt" }))
            .await,
        Err(ToolError::Cancelled)
    ));

    // Exact 100 vs 101 cap boundary.
    for (count, truncated) in [(100usize, false), (101usize, true)] {
        let cap_dir = tempdir("glob-cap");
        let _cg = TempDirGuard::new(&cap_dir);
        for i in 0..count {
            tokio::fs::write(cap_dir.join(format!("f-{i:03}.txt")), "x")
                .await
                .unwrap();
        }
        let res = glob
            .execute(&search_ctx(cap_dir), json!({ "pattern": "*.txt" }))
            .await
            .unwrap();
        assert_eq!(res["total"], count);
        assert_eq!(res["metadata"]["count"], count.min(100));
        assert_eq!(res["metadata"]["truncated"], truncated);
    }
}

#[tokio::test]
async fn glob_authorizes_external_path_before_metadata_probe() {
    let dir = tempdir("glob-ext");
    let _guard = TempDirGuard::new(&dir);
    let outside_root = tempdir("glob-out");
    let _out_guard = TempDirGuard::new(&outside_root);
    let nested = outside_root.join("nested");
    tokio::fs::create_dir_all(&nested).await.unwrap();
    tokio::fs::write(nested.join("secret.txt"), "secret\n")
        .await
        .unwrap();

    let ctx = ctx_with(
        vec![
            allow(Action::Glob, "*"),
            deny(
                Action::ExternalDirectory,
                &format!("{}/*", outside_root.to_string_lossy()),
            ),
            allow(
                Action::ExternalDirectory,
                &format!("{}/*", nested.to_string_lossy()),
            ),
        ],
        dir,
    );
    let glob = hya_tool_loader::builtins().get("glob").unwrap();
    assert!(matches!(
        glob.execute(&ctx, json!({ "pattern": "*.txt", "path": nested }))
            .await,
        Err(ToolError::Permission(_))
    ));
}

#[tokio::test]
async fn grep_schema_input_validation_and_context_clamping() {
    let tool = hya_tool_loader::builtins().get("grep").unwrap();
    let schema = tool.schema().input_schema;
    let props = schema["properties"].as_object().unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["required"], json!(["pattern"]));
    assert_eq!(
        props.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "context",
            "glob",
            "ignoreCase",
            "limit",
            "literal",
            "path",
            "pattern"
        ])
    );
    let desc = props["context"]["description"].as_str().unwrap();
    assert!(desc.contains("0–5") && desc.contains("clamped"));
    assert_eq!(
        (
            props["limit"]["minimum"].as_u64(),
            props["limit"]["maximum"].as_u64()
        ),
        (Some(1), Some(200))
    );

    let dir = tempdir("grep-val");
    let _guard = TempDirGuard::new(&dir);
    let body: String = (1..=20)
        .map(|n| {
            if n == 10 {
                "needle\n".into()
            } else {
                format!("line {n}\n")
            }
        })
        .collect();
    tokio::fs::write(dir.join("notes.txt"), body).await.unwrap();
    let ctx = search_ctx(dir);

    for bad in [
        json!({ "pattern": "needle", "unexpected": true }),
        json!({ "pattern": "needle", "limit": 0 }),
        json!({ "pattern": "needle", "limit": 201 }),
        json!({ "pattern": "[unterminated" }),
        json!({ "pattern": "needle", "path": "notes.txt", "glob": "x".repeat(MAX_GLOB_BYTES + 1) }),
    ] {
        assert!(matches!(
            tool.execute(&ctx, bad).await,
            Err(ToolError::Input(_))
        ));
    }

    let high = tool
        .execute(&ctx, json!({ "pattern": "needle", "context": 8 }))
        .await
        .unwrap();
    assert!(
        high["output"]
            .as_str()
            .unwrap()
            .contains("context clamped to 5")
    );
    assert!(
        high["output"].as_str().unwrap().contains("line 5")
            && !high["output"].as_str().unwrap().contains("line 16")
    );

    let neg = tool
        .execute(&ctx, json!({ "pattern": "needle", "context": -2 }))
        .await
        .unwrap();
    assert!(
        neg["output"]
            .as_str()
            .unwrap()
            .contains("context clamped to 0")
    );
    assert!(!neg["output"].as_str().unwrap().contains("line 9"));

    let ok = tool
        .execute(&ctx, json!({ "pattern": "needle", "context": 2 }))
        .await
        .unwrap();
    assert!(!ok["output"].as_str().unwrap().contains("clamped"));
}

#[tokio::test]
async fn grep_matching_modes_globs_deterministic_order_and_no_matches() {
    let dir = tempdir("grep-match");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::create_dir_all(dir.join("src/nested"))
        .await
        .unwrap();
    tokio::fs::write(dir.join("src/top.rs"), "needle top\n")
        .await
        .unwrap();
    tokio::fs::write(dir.join("src/nested/deep.rs"), "needle deep\n")
        .await
        .unwrap();
    tokio::fs::write(dir.join("src/readme.md"), "needle docs\n")
        .await
        .unwrap();
    tokio::fs::write(dir.join("cases.txt"), "needle.*\nNEEDLE.*\nneedleX\n")
        .await
        .unwrap();
    let tool = hya_tool_loader::builtins().get("grep").unwrap();
    let ctx = search_ctx(dir);

    // Regex case-sensitivity + literal + single-file target.
    let sens = tool
        .execute(&ctx, json!({ "pattern": "^needle", "path": "cases.txt" }))
        .await
        .unwrap();
    let insens = tool
        .execute(
            &ctx,
            json!({ "pattern": "^needle", "path": "cases.txt", "ignoreCase": true }),
        )
        .await
        .unwrap();
    assert_eq!(
        (
            sens["metadata"]["matches"].as_u64(),
            insens["metadata"]["matches"].as_u64()
        ),
        (Some(2), Some(3))
    );

    let lit = tool
        .execute(&ctx, json!({ "pattern": "needle.*", "path": "cases.txt", "literal": true, "ignoreCase": true }))
        .await
        .unwrap();
    assert_eq!(lit["metadata"]["matches"], 2);
    assert!(!lit["output"].as_str().unwrap().contains("needleX"));

    // Recursive glob filter & deterministic lexical file order.
    let nested = tool
        .execute(
            &ctx,
            json!({ "pattern": "needle", "path": "src", "glob": "**/*.rs" }),
        )
        .await
        .unwrap();
    let paths = display_paths(&nested);
    assert_eq!(paths.len(), 2);
    assert!(paths[0].ends_with("nested/deep.rs") && paths[1].ends_with("top.rs"));

    // Exact no-match envelope.
    let none = tool
        .execute(&ctx, json!({ "pattern": "absent", "path": "cases.txt" }))
        .await
        .unwrap();
    assert_eq!(none["output"], "No matches found for absent.");
    assert_eq!(none["metadata"]["display"]["groups"], json!([]));
}

#[tokio::test]
async fn grep_context_merging_separators_and_limit_boundaries() {
    let dir = tempdir("grep-ctx");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::write(
        dir.join("merge.txt"),
        "one\nneedle-two\nthree\nneedle-four\nfive\nsix\n",
    )
    .await
    .unwrap();
    let sep_src = (1..=10)
        .map(|l| match l {
            2 => "needle-2".into(),
            8 => "needle-8".into(),
            _ => format!("l-{l}"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(dir.join("sep.txt"), sep_src)
        .await
        .unwrap();
    let fifty_one = (1..=51)
        .map(|l| format!("hit-{l}"))
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(dir.join("many.txt"), fifty_one)
        .await
        .unwrap();

    let tool = hya_tool_loader::builtins().get("grep").unwrap();
    let ctx = search_ctx(dir);

    let merged = tool
        .execute(
            &ctx,
            json!({ "pattern": "needle", "path": "merge.txt", "context": 1 }),
        )
        .await
        .unwrap();
    assert_eq!(display_rows_for(&merged, "merge.txt").len(), 5);
    assert_eq!(
        merged["output"]
            .as_str()
            .unwrap()
            .lines()
            .filter(|l| l.trim() == "...")
            .count(),
        0
    );

    let disjoint = tool
        .execute(
            &ctx,
            json!({ "pattern": "needle", "path": "sep.txt", "context": 1 }),
        )
        .await
        .unwrap();
    let lines: Vec<_> = display_rows_for(&disjoint, "sep.txt")
        .iter()
        .map(|r| r["line"].as_u64().unwrap())
        .collect();
    assert_eq!(lines, vec![1, 2, 3, 7, 8, 9]);
    assert_eq!(
        disjoint["output"]
            .as_str()
            .unwrap()
            .lines()
            .filter(|l| l.trim() == "...")
            .count(),
        1
    );

    // Default 50-match cap, exact limit vs limit-1.
    let def = tool
        .execute(&ctx, json!({ "pattern": "hit" }))
        .await
        .unwrap();
    assert_eq!(def["metadata"]["matches"], 50);
    assert_eq!(def["metadata"]["truncated"], true);

    let exact = tool
        .execute(
            &ctx,
            json!({ "pattern": "hit", "path": "many.txt", "limit": 51 }),
        )
        .await
        .unwrap();
    assert_eq!(exact["metadata"]["truncated"], false);
}

#[tokio::test]
async fn grep_ignore_negated_classes_binary_skip_and_over_budget_warnings() {
    let tool = hya_tool_loader::builtins().get("grep").unwrap();

    // 1) .gitignore, .ignore with negated classes, binary skip, and dangling symlink skip.
    {
        let dir = tempdir("grep-ign");
        let _guard = TempDirGuard::new(&dir);
        tokio::fs::write(dir.join(".gitignore"), "ignored.txt\n[!a].txt\n")
            .await
            .unwrap();
        tokio::fs::write(dir.join(".ignore"), "[^a].log\n")
            .await
            .unwrap();
        for f in ["a.txt", "b.txt", "a.log", "b.log", "ignored.txt"] {
            tokio::fs::write(dir.join(f), "needle\n").await.unwrap();
        }
        tokio::fs::write(dir.join("bin.dat"), b"needle\0bin\n")
            .await
            .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("missing", dir.join("dangling.txt")).unwrap();
        let res = tool
            .execute(&search_ctx(dir), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        let paths = display_paths(&res);
        assert_eq!(paths.len(), 2);
        assert!(paths.iter().any(|p| p.ends_with("a.txt")));
        assert!(paths.iter().any(|p| p.ends_with("a.log")));
    }

    // 2) Caller glob negated classes `[!a]` / `[^a]`.
    {
        let dir = tempdir("grep-neg");
        let _guard = TempDirGuard::new(&dir);
        for f in ["a.txt", "b.txt", "c.txt"] {
            tokio::fs::write(dir.join(f), "needle\n").await.unwrap();
        }
        let ctx = search_ctx(dir);
        for g in ["[!a].txt", "[^a].txt"] {
            let res = tool
                .execute(&ctx, json!({ "pattern": "needle", "glob": g }))
                .await
                .unwrap();
            assert_eq!(res["metadata"]["matches"], 2);
            assert!(display_paths(&res).iter().all(|p| !p.ends_with("a.txt")));
        }
    }

    // 3) Over-budget >10k ignore rules + >1MB logical line produce bounded warnings without aborting.
    {
        let dir = tempdir("grep-budgets");
        let _guard = TempDirGuard::new(&dir);
        let rules = (0..=MAX_IGNORE_RULES)
            .map(|i| format!("absent-{i}.txt"))
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(dir.join(".gitignore"), rules)
            .await
            .unwrap();
        let mut oversized = vec![b'x'; MAX_GREP_LOGICAL_LINE_BYTES + 1];
        oversized.extend_from_slice(b"\nnormal NEEDLE line\n");
        tokio::fs::write(dir.join("oversized.txt"), oversized)
            .await
            .unwrap();

        let res = tool
            .execute(&search_ctx(dir), json!({ "pattern": "NEEDLE" }))
            .await
            .unwrap();
        assert_eq!(res["metadata"]["matches"], 1);
        let warnings = res["metadata"]["warnings"].as_array().unwrap();
        assert!(warnings.len() >= 2);
        assert!(
            warnings
                .iter()
                .all(|w| w.as_str().unwrap().len() <= MAX_WARNING_BYTES)
        );
    }

    // 4) Over-budget 60KB rendered match marks displayTruncated explicitly.
    {
        let dir = tempdir("grep-trunc");
        let _guard = TempDirGuard::new(&dir);
        tokio::fs::write(
            dir.join("huge.txt"),
            format!("needle-{}", "x".repeat(60 * 1024)),
        )
        .await
        .unwrap();
        let res = tool
            .execute(&search_ctx(dir), json!({ "pattern": "needle" }))
            .await
            .unwrap();
        assert_eq!(res["metadata"]["matches"], 1);
        assert_eq!(res["metadata"]["displayTruncated"], true);
        assert!(res["output"].as_str().unwrap().contains("omitted"));
        assert!(res["output"].as_str().unwrap().len() <= 50 * 1024);
    }
}

#[tokio::test]
async fn grep_hashline_rows_recovery_to_edit_and_cancellation_isolation() {
    let dir = tempdir("grep-hl");
    let _guard = TempDirGuard::new(&dir);
    let registry = hya_tool_loader::builtins();
    let grep = registry.get("grep").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = search_ctx(dir.clone());

    // 1) Hashline rows + isMatch flags + stale recovery from Grep-emitted anchor.
    let source = (1..=12)
        .map(|l| match l {
            3 => "needle-noop".into(),
            10 => "needle-target".into(),
            _ => format!("line-{l}"),
        })
        .collect::<Vec<_>>()
        .join("\n");
    let target = dir.join("rec.txt");
    tokio::fs::write(&target, &source).await.unwrap();
    let g = grep
        .execute(
            &ctx,
            json!({ "pattern": "needle", "path": "rec.txt", "context": 1 }),
        )
        .await
        .unwrap();
    let rows = display_rows_for(&g, "rec.txt");
    assert_eq!(rows[0]["isMatch"], false);
    assert_eq!(rows[1]["isMatch"], true);
    let stale_noop = grep_anchor(&g, 3, "needle-noop");
    let target_anchor = grep_anchor(&g, 10, "needle-target");

    let mut changed: Vec<_> = (1..=12).map(|l| format!("line-{l}")).collect();
    changed[1] = "external-far-change".into();
    changed[2] = "needle-noop".into();
    changed[9] = "needle-target".into();
    tokio::fs::write(&target, changed.join("\n")).await.unwrap();

    let edited = edit
        .execute(
            &ctx,
            json!({
                "path": "rec.txt",
                "edits": [
                    { "op": "replace", "pos": stale_noop, "lines": ["needle-noop"] },
                    { "op": "replace", "pos": target_anchor, "lines": ["needle-replaced"] }
                ]
            }),
        )
        .await
        .unwrap();
    assert!(
        edited["output"]
            .as_str()
            .unwrap()
            .contains("Recovered stale anchors")
    );
    assert!(
        tokio::fs::read_to_string(&target)
            .await
            .unwrap()
            .contains("needle-replaced")
    );

    // 2) Pre-cancelled Grep returns Cancelled and does not seed recovery snapshot.
    let cancel_file = dir.join("cancel.txt");
    tokio::fs::write(&cancel_file, "alpha\nbeta\ngamma\ndelta\n")
        .await
        .unwrap();
    let fresh_reg = hya_tool_loader::builtins();
    let cctx = search_ctx(dir.clone());
    cctx.cancel.cancel();
    assert!(matches!(
        fresh_reg
            .get("grep")
            .unwrap()
            .execute(&cctx, json!({ "pattern": "beta", "path": "cancel.txt" }))
            .await,
        Err(ToolError::Cancelled)
    ));
    tokio::fs::write(&cancel_file, "external\nbeta\ngamma\ndelta\n")
        .await
        .unwrap();
    let err = fresh_reg
        .get("edit")
        .unwrap()
        .execute(&ctx, json!({ "path": "cancel.txt", "edits": [{ "op": "replace", "pos": "2#JB", "lines": ["X"] }] }))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("[E_STALE_ANCHOR]") && !err.contains("Recovery attempted"));
}

#[tokio::test]
async fn grep_external_permission_resource_is_file_directory_blind() {
    let dir = tempdir("grep-blind");
    let _guard = TempDirGuard::new(&dir);
    let outside = tempdir("grep-blind-out");
    let _out_guard = TempDirGuard::new(&outside);
    let outside_file = outside.join("outside.txt");
    let outside_dir = outside.join("outside-dir");
    tokio::fs::write(&outside_file, "needle\n").await.unwrap();
    tokio::fs::create_dir(&outside_dir).await.unwrap();
    let grep = hya_tool_loader::builtins().get("grep").unwrap();

    let mut asked = Vec::new();
    for target in [&outside_file, &outside_dir] {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut ctx = ctx_with(vec![allow(Action::Grep, "*")], dir.clone());
        ctx.permission =
            ctx.permission
                .clone()
                .with_interceptor(Arc::new(RecordingPermissionInterceptor {
                    requests: Arc::clone(&requests),
                }));
        let err = grep
            .execute(&ctx, json!({ "pattern": "needle", "path": target }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Permission(_)));
        let list = requests.lock().unwrap().clone();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, Action::ExternalDirectory);
        asked.push(list[0].1.clone());
    }
    assert_eq!(
        asked[0], asked[1],
        "external resource must not reveal file vs directory"
    );
}
