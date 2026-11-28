//! Workspace listing, finding, and globbing behavior.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use hya_tool::{Action, ToolCtx, ToolError};
use serde_json::json;

use crate::common::{TempDirGuard, allow, ctx_with, deny, tempdir};

const MAX_GLOB_BYTES: usize = 4096;

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

#[tokio::test]
async fn ls_lists_entries_with_type_size_default_cwd_and_permission() {
    let dir = tempdir("ls-test");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::write(dir.join("a.txt"), "hello").await.unwrap();
    tokio::fs::create_dir_all(dir.join("sub")).await.unwrap();
    let ls = crate::coding_extras_registry().get("ls").unwrap();

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
    let find = crate::coding_extras_registry().get("find").unwrap();

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
    let glob = crate::coding_extras_registry().get("glob").unwrap();
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
    let glob = crate::coding_extras_registry().get("glob").unwrap();
    assert!(matches!(
        glob.execute(&ctx, json!({ "pattern": "*.txt", "path": nested }))
            .await,
        Err(ToolError::Permission(_))
    ));
}
