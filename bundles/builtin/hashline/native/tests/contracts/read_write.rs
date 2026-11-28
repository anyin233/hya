//! Integration tests for public `read` and `write` contracts.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use hya_proto::SessionId;
use hya_tool::{Action, ToolError, ToolResultPolicy, cap_tool_output_with_policy};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::common::{TempDirGuard, allow, ctx_with, ctx_with_session, deny, tempdir};

const RECOVERY_SOURCE: &str =
    "alpha\nbeta\ngamma\ndelta\nfive\nsix\nseven\neight\nalpha\nbeta\ngamma\ndelta\n";
const RECOVERY_EXTERNAL: &str =
    "EXTERNAL\nbeta\ngamma\ndelta\nfive\nsix\nseven\neight\nalpha\nbeta\ngamma\ndelta\n";
const RECOVERY_FINAL: &str =
    "EXTERNAL\nbeta\ngamma\ndelta\nfive\nsix\nseven\neight\nalpha\nBETA\ngamma\ndelta\n";

fn assert_write_metadata(result: &Value, target: &Path, bytes: usize, existed: bool) {
    assert_eq!(result["ok"], true);
    assert_eq!(
        result["metadata"]["path"],
        target.to_string_lossy().as_ref()
    );
    assert_eq!(result["metadata"]["bytes"], bytes);
    assert_eq!(result["metadata"]["existed"], existed);
    assert_eq!(result["metadata"]["hardLink"], false);
    assert_eq!(result["metadata"]["executable"], false);
    assert!(result["metadata"]["warnings"].is_array());
    assert!(result["metadata"]["diagnostics"].is_object());
    assert_eq!(result["metadata"]["display"]["type"], "file");
    assert_eq!(
        result["metadata"]["display"]["path"],
        target.to_string_lossy().as_ref()
    );
}

fn assert_warning(result: &Value, fragment: &str) {
    let warnings = result["metadata"]["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .filter_map(Value::as_str)
            .any(|w| w.contains(fragment)),
        "warnings {warnings:?} missing {fragment:?}"
    );
}

async fn hashline_temp_files(directory: &Path) -> Vec<PathBuf> {
    let mut entries = tokio::fs::read_dir(directory).await.unwrap();
    let mut out = Vec::new();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with(".hya-hashline-") && n.ends_with(".tmp"))
        {
            out.push(path);
        }
    }
    out
}

#[test]
fn read_and_write_schemas_expose_canonical_closed_surfaces() {
    let registry = crate::hashline_registry();

    let read = registry.get("read").unwrap().schema().input_schema;
    let rprops = read["properties"].as_object().unwrap();
    assert_eq!(rprops.len(), 4);
    assert_eq!(rprops["path"]["type"], "string");
    assert!(!rprops.contains_key("filePath"));
    assert_eq!(rprops["offset"]["minimum"], 1);
    assert_eq!(rprops["limit"]["minimum"], 1);
    assert_eq!(rprops["raw"]["type"], "boolean");
    assert_eq!(read["required"], json!(["path"]));

    let write = registry.get("write").unwrap().schema().input_schema;
    let wprops = write["properties"].as_object().unwrap();
    assert_eq!(write["additionalProperties"], false);
    assert_eq!(write["required"], json!(["path", "content"]));
    assert_eq!(wprops.len(), 2);
    assert!(!wprops.contains_key("filePath"));
}

#[tokio::test]
async fn read_input_validation_path_aliases_and_bad_offset() {
    let dir = tempdir("read-input");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::write(dir.join("target.txt"), "first\nsecond\n")
        .await
        .unwrap();
    tokio::fs::write(dir.join("other.txt"), "other\n")
        .await
        .unwrap();
    let tool = crate::hashline_registry().get("read").unwrap();
    let ctx = ctx_with(vec![allow(Action::Read, "*")], dir);

    for valid in [
        json!({ "filePath": "target.txt", "path": "target.txt", "offset": 0, "limit": 2000 }),
        json!({ "path": "target.txt", "filePath": "" }),
        json!({ "path": "", "filePath": "target.txt" }),
    ] {
        let out = tool.execute(&ctx, valid).await.unwrap();
        assert_eq!(out["title"], "target.txt");
        assert!(out["content"].as_str().unwrap().contains("first"));
    }

    for bad in [
        json!({ "filePath": "other.txt", "path": "target.txt" }),
        json!({}),
        json!({ "path": "", "filePath": "" }),
        json!({ "path": "target.txt", "offset": null }),
        json!({ "path": "target.txt", "limit": null }),
        json!({ "path": "target.txt", "raw": null }),
        json!({ "path": "target.txt", "limit": 0 }),
    ] {
        assert!(
            matches!(
                tool.execute(&ctx, bad.clone()).await,
                Err(ToolError::Input(_))
            ),
            "expected typed input error for {bad}"
        );
    }

    let oob = tool
        .execute(&ctx, json!({ "path": "target.txt", "offset": 9 }))
        .await
        .unwrap_err();
    assert!(matches!(
        oob,
        ToolError::Input(msg) if msg.starts_with("[E_BAD_READ]") && msg.contains("Offset 9") && msg.contains("2 lines")
    ));
}

#[tokio::test]
async fn read_paging_directory_listing_and_missing_suggestions() {
    let dir = tempdir("read-page");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::create_dir_all(dir.join("dir/sub"))
        .await
        .unwrap();
    tokio::fs::write(dir.join("dir/b.txt"), "b").await.unwrap();
    tokio::fs::write(dir.join("dir/a.txt"), "a").await.unwrap();
    tokio::fs::write(dir.join("notes.txt"), "alpha\nbeta\ngamma\ndelta\n")
        .await
        .unwrap();
    let config_toml = dir.join("config.toml");
    tokio::fs::write(&config_toml, "ok = true\n").await.unwrap();
    let missing = dir.join("config");

    let tool = crate::hashline_registry().get("read").unwrap();
    let ctx = ctx_with(vec![allow(Action::Read, "*")], dir);

    let paged = tool
        .execute(
            &ctx,
            json!({ "filePath": "notes.txt", "offset": 2, "limit": 2 }),
        )
        .await
        .unwrap();
    assert_eq!(paged["title"], "notes.txt");
    assert_eq!(paged["content"], "beta\ngamma");
    assert!(
        paged["output"]
            .as_str()
            .unwrap()
            .contains("<type>file</type>")
    );
    assert!(paged["output"].as_str().unwrap().contains("Use offset=4"));
    assert_eq!(paged["metadata"]["display"]["type"], "file");
    assert_eq!(paged["metadata"]["display"]["lineStart"], 2);
    assert_eq!(paged["metadata"]["display"]["lineEnd"], 3);
    assert_eq!(paged["metadata"]["display"]["totalLines"], 4);
    assert_eq!(paged["metadata"]["truncated"], true);
    assert_eq!(paged["metadata"]["nextOffset"], 4);
    assert_eq!(paged["metadata"]["preview"], "beta\ngamma");
    assert_eq!(paged["metadata"]["loaded"], json!([]));

    let listed = tool
        .execute(&ctx, json!({ "filePath": "dir", "offset": 1, "limit": 2 }))
        .await
        .unwrap();
    assert!(
        listed["output"]
            .as_str()
            .unwrap()
            .contains("<type>directory</type>")
    );
    assert_eq!(
        listed["metadata"]["display"]["entries"],
        json!(["sub/", "a.txt"])
    );
    assert_eq!(listed["metadata"]["display"]["totalEntries"], 3);
    assert_eq!(listed["metadata"]["display"]["truncated"], true);

    let err = tool.execute(&ctx, json!({ "filePath": "config" })).await;
    assert!(matches!(
        err,
        Err(ToolError::Other(msg)) if msg == format!(
            "File not found: {}\n\nDid you mean one of these?\n{}",
            missing.to_string_lossy(),
            config_toml.to_string_lossy()
        )
    ));
}

#[tokio::test]
async fn read_raw_bom_invalid_utf8_empty_and_binary_files() {
    let dir = tempdir("read-formats");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::write(dir.join("raw.txt"), "alpha\nbeta\n")
        .await
        .unwrap();
    tokio::fs::write(dir.join("bom.txt"), b"\xEF\xBB\xBFhello\n")
        .await
        .unwrap();
    tokio::fs::write(dir.join("invalid.txt"), b"alpha\xFFbeta\n")
        .await
        .unwrap();
    tokio::fs::write(dir.join("empty.txt"), b"").await.unwrap();
    let binary = dir.join("payload.bin");
    tokio::fs::write(&binary, "plain text but binary extension\n")
        .await
        .unwrap();
    let tool = crate::hashline_registry().get("read").unwrap();
    let ctx = ctx_with(vec![allow(Action::Read, "*")], dir);

    let raw = tool
        .execute(&ctx, json!({ "path": "raw.txt", "raw": true }))
        .await
        .unwrap();
    assert_eq!(raw["output"], "alpha\nbeta");
    assert_eq!(raw["content"], "alpha\nbeta");

    let bom = tool
        .execute(&ctx, json!({ "path": "bom.txt" }))
        .await
        .unwrap();
    assert_eq!(bom["content"], "hello");
    assert_eq!(bom["metadata"]["display"]["text"], "hello");
    assert!(bom["output"].as_str().unwrap().contains("1#"));

    let invalid = tool
        .execute(&ctx, json!({ "path": "invalid.txt" }))
        .await
        .unwrap();
    assert_eq!(invalid["content"], "alpha\u{fffd}beta");
    assert_warning(&invalid, "Invalid UTF-8");

    let empty = tool
        .execute(&ctx, json!({ "path": "empty.txt" }))
        .await
        .unwrap();
    assert_eq!(empty["content"], "");
    assert_eq!(empty["metadata"]["display"]["totalLines"], 0);
    assert!(empty["output"].as_str().unwrap().contains("File is empty"));

    let bin_err = tool.execute(&ctx, json!({ "path": "payload.bin" })).await;
    assert!(matches!(
        bin_err,
        Err(ToolError::Other(msg)) if msg == format!("Cannot read binary file: {}", binary.to_string_lossy())
    ));
}

#[tokio::test]
async fn read_image_attachments_malformed_and_over_budget() {
    let dir = tempdir("read-img");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::write(
        dir.join("image.png"),
        [
            137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1,
            8, 0, 0, 0, 0, 58, 126, 155, 85, 0, 0, 0, 10, 73, 68, 65, 84, 120, 156, 99, 96, 0, 0,
            0, 2, 0, 1, 72, 175, 164, 113, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
        ],
    )
    .await
    .unwrap();
    tokio::fs::write(dir.join("bad.data"), b"\x89PNG\r\n\x1a\n")
        .await
        .unwrap();
    let mut big_png = vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    big_png.resize(hya_tool::MAX_CODING_ATTACHMENT_BYTES * 3 / 4, 0);
    tokio::fs::write(dir.join("big.png"), &big_png)
        .await
        .unwrap();

    let tool = crate::hashline_registry().get("read").unwrap();
    let ctx = ctx_with(vec![allow(Action::Read, "*")], dir);

    let ok = tool
        .execute(&ctx, json!({ "path": "image.png" }))
        .await
        .unwrap();
    assert_eq!(ok["output"], "Image file (image/png, 67 bytes).");
    assert_eq!(ok["attachments"][0]["mime"], "image/png");
    assert_eq!(
        ok["attachments"][0]["url"],
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAAAAAA6fptVAAAACklEQVR4nGNgAAAAAgABSK+kcQAAAABJRU5ErkJggg=="
    );

    let bad = tool
        .execute(&ctx, json!({ "path": "bad.data" }))
        .await
        .unwrap();
    assert_eq!(bad["metadata"]["preview"], "Image could not be decoded");
    assert!(bad.get("attachments").is_none());
    assert!(
        bad["output"]
            .as_str()
            .unwrap()
            .starts_with("Malformed or incomplete image (image/png, 8 bytes). Decoding failed:")
    );

    let big = tool
        .execute(&ctx, json!({ "path": "big.png" }))
        .await
        .unwrap();
    assert!(big.get("attachments").is_none());
    assert!(
        big["output"]
            .as_str()
            .unwrap()
            .contains("larger than the limit of")
    );
}

#[tokio::test]
async fn read_caps_long_lines_fifty_kb_oversized_first_line_and_escape_heavy_rows() {
    const CAP: usize = 50 * 1024;
    let dir = tempdir("read-caps");
    let _guard = TempDirGuard::new(&dir);
    let tool = crate::hashline_registry().get("read").unwrap();
    let ctx = ctx_with(vec![allow(Action::Read, "*")], dir.clone());

    // 1) Individual line > 2000 chars truncates in legacy filePath mode.
    tokio::fs::write(dir.join("long.txt"), format!("{}\n", "a".repeat(2001)))
        .await
        .unwrap();
    let long = tool
        .execute(&ctx, json!({ "filePath": "long.txt" }))
        .await
        .unwrap();
    let expected_long = format!("{}... (line truncated to 2000 chars)", "a".repeat(2000));
    assert_eq!(long["content"], expected_long);
    assert_eq!(long["metadata"]["truncated"], false);

    // 2) 50KB cap stops before tail lines without leaking them.
    let mut lines = (0..60).map(|_| "a".repeat(1000)).collect::<Vec<_>>();
    lines.push("TAIL_SECRET".to_string());
    tokio::fs::write(dir.join("large.txt"), lines.join("\n"))
        .await
        .unwrap();
    let large = tool
        .execute(&ctx, json!({ "path": "large.txt", "limit": 2000 }))
        .await
        .unwrap();
    assert_eq!(large["metadata"]["truncated"], true);
    assert!(large["output"].as_str().unwrap().len() <= CAP);
    assert!(!large["output"].as_str().unwrap().contains("TAIL_SECRET"));
    assert!(!large["content"].as_str().unwrap().contains("TAIL_SECRET"));

    // 3) Oversized first line (>50KB) doesn't leak secret; raw mode keeps continuation; near-cap keeps wrapper.
    let secret = "OVERSIZE_LINE_SECRET";
    tokio::fs::write(
        dir.join("oversize.txt"),
        format!("{secret}{}\nsecond-line", "x".repeat(CAP)),
    )
    .await
    .unwrap();
    let over = tool
        .execute(&ctx, json!({ "path": "oversize.txt" }))
        .await
        .unwrap();
    assert!(
        over["output"]
            .as_str()
            .unwrap()
            .contains("exceeds 51200 bytes")
    );
    assert!(!over["output"].as_str().unwrap().contains(secret));
    assert_eq!(over["content"], "");

    let over_raw = tool
        .execute(&ctx, json!({ "path": "oversize.txt", "raw": true }))
        .await
        .unwrap();
    assert!(over_raw["output"].as_str().unwrap().len() <= CAP);
    assert!(
        over_raw["output"]
            .as_str()
            .unwrap()
            .contains("Use offset=2 to continue.")
    );
    assert_eq!(over_raw["metadata"]["nextOffset"], 2);

    tokio::fs::write(dir.join("near-cap.txt"), "x".repeat(CAP - 10))
        .await
        .unwrap();
    let near = tool
        .execute(&ctx, json!({ "path": "near-cap.txt" }))
        .await
        .unwrap();
    assert!(
        near["output"]
            .as_str()
            .unwrap()
            .contains("Hashline output requires full lines")
    );
    assert!(near["output"].as_str().unwrap().ends_with("</content>"));
    assert!(near["metadata"]["nextOffset"].is_null());

    // 4) Escape-heavy rows stay complete after Coding policy cap.
    let payload = "\"\\\t\u{0001}".repeat(16);
    let source = (0..640)
        .map(|_| payload.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(dir.join("escaped.txt"), &source)
        .await
        .unwrap();
    let raw_esc = tool
        .execute(&ctx, json!({ "path": "escaped.txt", "limit": 640 }))
        .await
        .unwrap();
    let capped = cap_tool_output_with_policy(raw_esc, ToolResultPolicy::Coding);
    let output = capped["output"].as_str().unwrap();
    assert!(output.ends_with("</content>"));
    let body = output
        .split_once("<content>\n")
        .unwrap()
        .1
        .strip_suffix("\n</content>")
        .unwrap();
    let (rows_text, cont) = body.rsplit_once("\n\n[Showing lines ").unwrap();
    assert!(cont.contains("Use offset=") && cont.ends_with(']'));
    for (idx, row) in rows_text.split('\n').enumerate() {
        let (anchor, text) = row.split_once(':').unwrap();
        let (line, hash) = anchor.trim().split_once('#').unwrap();
        assert_eq!(line.parse::<usize>().unwrap(), idx + 1);
        assert_eq!(hash.len(), 2);
        assert_eq!(text, payload);
    }
    for v in [
        &capped["output"],
        &capped["content"],
        &capped["metadata"]["display"]["text"],
    ] {
        assert!(serde_json::to_vec(v).unwrap().len() <= CAP);
    }
}

#[tokio::test]
async fn read_authorizes_external_path_lexically_before_metadata_probe() {
    let dir = tempdir("read-ext");
    let _guard = TempDirGuard::new(&dir);
    let outside_root = tempdir("read-outside");
    let _out_guard = TempDirGuard::new(&outside_root);
    let nested = outside_root.join("nested");
    tokio::fs::create_dir_all(&nested).await.unwrap();
    tokio::fs::write(nested.join("secret.txt"), "secret\n")
        .await
        .unwrap();

    let ctx = ctx_with(
        vec![
            allow(Action::Read, "*"),
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
    let tool = crate::hashline_registry().get("read").unwrap();
    let res = tool.execute(&ctx, json!({ "path": nested })).await;
    assert!(matches!(res, Err(ToolError::Permission(_))));
}

#[tokio::test]
async fn write_creates_overwrites_bounds_display_and_enforces_input_and_permissions() {
    let dir = tempdir("write-basic");
    let _guard = TempDirGuard::new(&dir);
    let outside = tempdir("write-outside");
    let _out_guard = TempDirGuard::new(&outside);
    let tool = crate::hashline_registry().get("write").unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());

    // Rejects removed `filePath` without mutating.
    assert!(matches!(
        tool.execute(
            &ctx,
            json!({ "filePath": "legacy.txt", "content": "nope\n" })
        )
        .await,
        Err(ToolError::Input(_))
    ));
    assert!(!dir.join("legacy.txt").exists());

    // Enforces Edit and ExternalDirectory permissions before mutating.
    let denied = ctx_with(vec![deny(Action::Edit, "*")], dir.clone());
    assert!(matches!(
        tool.execute(&denied, json!({ "path": "blocked.txt", "content": "x" }))
            .await,
        Err(ToolError::Permission(_))
    ));
    let no_ext = ctx_with(
        vec![
            allow(Action::Edit, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        dir.clone(),
    );
    let outside_file = outside.join("out.txt");
    assert!(matches!(
        tool.execute(&no_ext, json!({ "path": outside_file, "content": "x" }))
            .await,
        Err(ToolError::Permission(_))
    ));
    assert!(!outside_file.exists());

    // Creates parents, reports metadata, overwrites, and bounds large displays.
    let target = dir.join("src/generated/config.txt");
    let created = tool
        .execute(
            &ctx,
            json!({ "path": "src/generated/config.txt", "content": "hello\n" }),
        )
        .await
        .unwrap();
    assert_write_metadata(&created, &target, 6, false);
    assert_eq!(created["output"], "Wrote file successfully.");
    assert!(hashline_temp_files(&dir).await.is_empty());

    let updated = tool
        .execute(
            &ctx,
            json!({ "path": "src/generated/config.txt", "content": "new\nline" }),
        )
        .await
        .unwrap();
    assert_eq!(updated["metadata"]["existed"], true);
    assert_eq!(updated["metadata"]["bytes"], 8);
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"new\nline");

    let large = tool
        .execute(
            &ctx,
            json!({ "path": "large.txt", "content": "x\n".repeat(30_000) }),
        )
        .await
        .unwrap();
    assert!(large["output"].as_str().unwrap().len() <= 50 * 1024);
    assert!(large["metadata"]["display"]["text"].as_str().unwrap().len() <= 50 * 1024);
    assert_eq!(large["metadata"]["display"]["truncated"], true);
    assert_eq!(
        tokio::fs::metadata(dir.join("large.txt"))
            .await
            .unwrap()
            .len(),
        large["metadata"]["bytes"].as_u64().unwrap()
    );
}

#[tokio::test]
async fn write_strips_unambiguous_hashline_block_and_preserves_encodings() {
    let dir = tempdir("write-enc");
    let _guard = TempDirGuard::new(&dir);
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
    let tool = crate::hashline_registry().get("write").unwrap();

    // Unambiguous hashline block stripped; ambiguous preserved verbatim.
    let stripped = tool
        .execute(
            &ctx,
            json!({
                "path": "dest.txt",
                "content": "[dest.txt#ABCD]\n1#KT:alpha\n2#JB:beta\n3#KJ:gamma\n4#PX:delta"
            }),
        )
        .await
        .unwrap();
    assert!(stripped["output"].as_str().unwrap().contains("stripped"));
    assert_eq!(
        tokio::fs::read(dir.join("dest.txt")).await.unwrap(),
        b"alpha\nbeta\ngamma\ndelta"
    );

    let ambiguous = "[amb.txt#ABCD]\n1#KT:alpha\nplain line";
    tool.execute(&ctx, json!({ "path": "amb.txt", "content": ambiguous }))
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(dir.join("amb.txt")).await.unwrap(),
        ambiguous.as_bytes()
    );

    // Existing BOM, incoming BOM, CRLF, invalid UTF-8 warning, mixed line endings warning.
    tokio::fs::write(dir.join("bom.txt"), b"\xEF\xBB\xBFold\n")
        .await
        .unwrap();
    tool.execute(&ctx, json!({ "path": "bom.txt", "content": "new\n" }))
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(dir.join("bom.txt")).await.unwrap(),
        b"\xEF\xBB\xBFnew\n"
    );

    tool.execute(
        &ctx,
        json!({ "path": "in-bom.txt", "content": "\u{feff}created\n" }),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read(dir.join("in-bom.txt")).await.unwrap(),
        b"\xEF\xBB\xBFcreated\n"
    );

    tokio::fs::write(dir.join("crlf.txt"), b"old\r\nline\r\n")
        .await
        .unwrap();
    tool.execute(
        &ctx,
        json!({ "path": "crlf.txt", "content": "new\nline\n" }),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read(dir.join("crlf.txt")).await.unwrap(),
        b"new\r\nline\r\n"
    );

    tokio::fs::write(dir.join("inv.txt"), b"old\n\xFF\n")
        .await
        .unwrap();
    let inv = tool
        .execute(&ctx, json!({ "path": "inv.txt", "content": "new\n" }))
        .await
        .unwrap();
    assert_warning(&inv, "Invalid UTF-8");

    tokio::fs::write(dir.join("mix.txt"), b"old\r\nline\nlast\r")
        .await
        .unwrap();
    let mix = tool
        .execute(
            &ctx,
            json!({ "path": "mix.txt", "content": "new\nvalue\n" }),
        )
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(dir.join("mix.txt")).await.unwrap(),
        b"new\r\nvalue\r\n"
    );
    assert_warning(&mix, "Mixed line endings");
}

#[cfg(unix)]
#[tokio::test]
async fn write_unix_modes_shebang_hard_link_and_symlink() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    let dir = tempdir("write-unix");
    let _guard = TempDirGuard::new(&dir);
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
    let tool = crate::hashline_registry().get("write").unwrap();

    // New file gets mode 0600; existing 0640 is preserved; shebang sets execute bits.
    let new_file = dir.join("new.txt");
    let res = tool
        .execute(&ctx, json!({ "path": "new.txt", "content": "new\n" }))
        .await
        .unwrap();
    assert_eq!(
        std::fs::metadata(&new_file).unwrap().permissions().mode() & 0o7777,
        0o600
    );
    assert_eq!(res["metadata"]["executable"], false);

    let mode_file = dir.join("existing.txt");
    tokio::fs::write(&mode_file, "old\n").await.unwrap();
    let mut perms = std::fs::metadata(&mode_file).unwrap().permissions();
    perms.set_mode(0o640);
    std::fs::set_permissions(&mode_file, perms).unwrap();
    tool.execute(&ctx, json!({ "path": "existing.txt", "content": "new\n" }))
        .await
        .unwrap();
    assert_eq!(
        std::fs::metadata(&mode_file).unwrap().permissions().mode() & 0o7777,
        0o640
    );

    let script = dir.join("run.sh");
    let sh = tool
        .execute(
            &ctx,
            json!({ "path": "run.sh", "content": "#!/bin/sh\necho ok\n" }),
        )
        .await
        .unwrap();
    assert_ne!(
        std::fs::metadata(&script).unwrap().permissions().mode() & 0o111,
        0
    );
    assert_eq!(sh["metadata"]["executable"], true);

    // Hard link updates in place; symlink follows resolved target while keeping link.
    let hl_a = dir.join("hl-a.txt");
    let hl_b = dir.join("hl-b.txt");
    tokio::fs::write(&hl_a, "old\n").await.unwrap();
    std::fs::hard_link(&hl_a, &hl_b).unwrap();
    let ino = std::fs::metadata(&hl_a).unwrap().ino();
    let hl = tool
        .execute(&ctx, json!({ "path": "hl-a.txt", "content": "new\n" }))
        .await
        .unwrap();
    assert_eq!(std::fs::metadata(&hl_a).unwrap().ino(), ino);
    assert_eq!(tokio::fs::read(&hl_b).await.unwrap(), b"new\n");
    assert_eq!(hl["metadata"]["hardLink"], true);

    let real = dir.join("real.txt");
    let link = dir.join("link.txt");
    tokio::fs::write(&real, "old\n").await.unwrap();
    symlink("real.txt", &link).unwrap();
    let sl = tool
        .execute(&ctx, json!({ "path": "link.txt", "content": "new\n" }))
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&real).await.unwrap(), b"new\n");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(sl["metadata"]["path"], real.to_string_lossy().as_ref());
}

#[tokio::test]
async fn write_reports_no_diagnostics_and_leaves_no_temp_files() {
    let dir = tempdir("write-hooks");
    let _guard = TempDirGuard::new(&dir);
    let tool = crate::hashline_registry().get("write").unwrap();

    // A hashline `write` reports no diagnostics of its own: post-edit
    // formatting and LSP belong to `tool.execute.after` hooks.
    let target = dir.join("main.rs");
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
    let ok = tool
        .execute(&ctx, json!({ "path": "main.rs", "content": "raw\n" }))
        .await
        .unwrap();
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"raw\n");
    assert_eq!(ok["metadata"]["diagnostics"], json!({}));
    assert!(hashline_temp_files(&dir).await.is_empty());
}

#[tokio::test]
async fn write_cancellation_and_snapshot_recovery_lifecycle() {
    let dir = tempdir("write-snap");
    let _guard = TempDirGuard::new(&dir);
    let registry = crate::hashline_registry();
    let write = registry.get("write").unwrap();
    let edit = registry.get("edit").unwrap();

    // 1) Pre-cancelled Write neither mutates nor seeds recovery snapshot.
    let s1 = SessionId::new();
    let pre_cancel = CancellationToken::new();
    pre_cancel.cancel();
    let cctx = ctx_with_session(
        vec![allow(Action::Edit, "*")],
        dir.clone(),
        pre_cancel,
        Some(s1),
    );
    assert!(matches!(
        write
            .execute(
                &cctx,
                json!({ "path": "cancelled.txt", "content": RECOVERY_SOURCE })
            )
            .await,
        Err(ToolError::Cancelled)
    ));
    assert!(!dir.join("cancelled.txt").exists());
    tokio::fs::write(dir.join("cancelled.txt"), RECOVERY_EXTERNAL)
        .await
        .unwrap();
    let ectx1 = ctx_with_session(
        vec![allow(Action::Edit, "*")],
        dir.clone(),
        CancellationToken::new(),
        Some(s1),
    );
    let stale = edit
        .execute(
            &ectx1,
            json!({
                "path": "cancelled.txt",
                "edits": [{"op": "replace", "pos": "2#JB", "lines": ["BETA"]}]
            }),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(stale.contains("E_STALE_ANCHOR"));

    // 2) Normal Write records snapshot for stale recovery.
    let file = "snap.txt";
    let sid = SessionId::new();
    let wctx = ctx_with_session(
        vec![allow(Action::Edit, "*")],
        dir.clone(),
        CancellationToken::new(),
        Some(sid),
    );
    write
        .execute(&wctx, json!({ "path": file, "content": RECOVERY_SOURCE }))
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(dir.join(file)).await.unwrap(),
        RECOVERY_SOURCE.as_bytes()
    );

    tokio::fs::write(dir.join(file), RECOVERY_EXTERNAL)
        .await
        .unwrap();
    let ectx = ctx_with_session(
        vec![allow(Action::Edit, "*")],
        dir.clone(),
        CancellationToken::new(),
        Some(sid),
    );
    let recovered = edit
        .execute(
            &ectx,
            json!({
                "path": file,
                "edits": [
                    {"op": "replace", "pos": "2#JB", "lines": ["beta"]},
                    {"op": "replace", "pos": "10#JB", "lines": ["BETA"]}
                ]
            }),
        )
        .await
        .unwrap();
    assert_eq!(recovered["metadata"]["recovered"], true);
    assert_eq!(
        tokio::fs::read(dir.join(file)).await.unwrap(),
        RECOVERY_FINAL.as_bytes()
    );
}
