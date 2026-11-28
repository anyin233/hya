//! Public `edit` & hashline contract, recovery, loop-guard, and concurrency tests.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hya_proto::SessionId;
use hya_tool::{Action, FormatterPlane, LspPlane, Tool, ToolCtx, ToolError, ToolRegistry};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::common::{
    FailingFormatter, FailingLsp, RecordingLsp, RewritingFormatter, TempDirGuard, allow, ctx_with,
    ctx_with_planes, deny, error_diagnostics, tempdir,
};

const ORIGINAL_WITH_NEWLINE: &str = "alpha\nbeta\ngamma\ndelta\n";

fn assert_input_code(result: Result<Value, ToolError>, code: &str) {
    match result.expect_err("expected typed input rejection") {
        ToolError::Input(msg) => {
            let prefix = format!("[{code}]");
            assert!(msg.contains(&prefix), "missing {prefix} in {msg:?}");
        }
        other => panic!("expected ToolError::Input([{code}]), got {other:?}"),
    }
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

fn anchor_for_line(read_result: &Value, line: usize) -> String {
    let output = read_result["output"].as_str().unwrap();
    output
        .lines()
        .find_map(|row| {
            let (prefix, _) = row.trim_start().split_once(':')?;
            let (num, hash) = prefix.split_once('#')?;
            (num.parse::<usize>().ok()? == line).then(|| format!("{line}#{hash}"))
        })
        .unwrap_or_else(|| panic!("no anchor for line {line} in {output:?}"))
}

async fn read_row(tool: &dyn Tool, ctx: &ToolCtx, path: &str, line: usize) -> String {
    let out = tool.execute(ctx, json!({ "path": path })).await.unwrap();
    let prefix = format!("{line}#");
    out["output"]
        .as_str()
        .unwrap()
        .lines()
        .find(|row| row.trim_start().starts_with(&prefix))
        .map(str::to_owned)
        .unwrap()
}

async fn append_lines(
    registry: &ToolRegistry,
    ctx: &ToolCtx,
    path: &str,
    lines: &[&str],
) -> Result<Value, ToolError> {
    registry
        .get("edit")
        .unwrap()
        .execute(
            ctx,
            json!({ "path": path, "edits": [{ "op": "append", "lines": lines }] }),
        )
        .await
}

async fn tmp_files(dir: &Path) -> Vec<PathBuf> {
    let mut entries = tokio::fs::read_dir(dir).await.unwrap();
    let mut out = Vec::new();
    while let Some(entry) = entries.next_entry().await.unwrap() {
        if entry.path().extension().is_some_and(|ext| ext == "tmp") {
            out.push(entry.path());
        }
    }
    out
}

#[test]
fn edit_schema_exposes_closed_hashline_ops_and_valid_example_anchors() {
    let schema = hya_tool_loader::builtins().get("edit").unwrap().schema();
    let text = format!("{} {}", schema.description, schema.input_schema);
    let mut examples = 0;
    for (index, _) in text.match_indices('#') {
        let digits = text[..index]
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .count();
        if digits == 0 {
            continue;
        }
        let hash: String = text[index + 1..]
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        examples += 1;
        assert!(
            hash.len() == 2 && hash.chars().all(|c| "ZPMQVRWSNKTXJBYH".contains(c)),
            "bad example anchor hash {hash:?}"
        );
    }
    assert!(examples > 0);

    let input = &schema.input_schema;
    let props = input["properties"].as_object().unwrap();
    assert_eq!(input["additionalProperties"], false);
    assert_eq!(input["required"], json!(["path", "edits"]));
    assert_eq!(props.len(), 2);
    for obsolete in [
        "filePath",
        "file_path",
        "oldString",
        "newString",
        "replaceAll",
    ] {
        assert!(!props.contains_key(obsolete));
    }
    let variants = props["edits"]["items"]["anyOf"].as_array().unwrap();
    let ops: BTreeSet<&str> = variants
        .iter()
        .map(|v| {
            assert_eq!(v["additionalProperties"], false);
            v["properties"]["op"]["enum"][0].as_str().unwrap()
        })
        .collect();
    assert_eq!(
        ops,
        BTreeSet::from(["append", "prepend", "replace", "replace_text"])
    );
}

#[tokio::test]
async fn edit_applies_table_driven_hashline_ops_and_replace_text_dialects() {
    let op_cases = [
        (
            "single replace",
            ORIGINAL_WITH_NEWLINE,
            json!({"path":"n.txt","edits":[{"op":"replace","pos":"2#JB","lines":["BETA"]}]}),
            "alpha\nBETA\ngamma\ndelta\n",
        ),
        (
            "range replace",
            ORIGINAL_WITH_NEWLINE,
            json!({"path":"n.txt","edits":[{"op":"replace","pos":"2#JB","end":"3#KJ","lines":["BETA","GAMMA"]}]}),
            "alpha\nBETA\nGAMMA\ndelta\n",
        ),
        (
            "range shrink",
            ORIGINAL_WITH_NEWLINE,
            json!({"path":"n.txt","edits":[{"op":"replace","pos":"2#JB","end":"3#KJ","lines":["BETA"]}]}),
            "alpha\nBETA\ndelta\n",
        ),
        (
            "anchored append",
            ORIGINAL_WITH_NEWLINE,
            json!({"path":"n.txt","edits":[{"op":"append","pos":"2#JB","lines":["ins"]}]}),
            "alpha\nbeta\nins\ngamma\ndelta\n",
        ),
        (
            "eof append",
            ORIGINAL_WITH_NEWLINE,
            json!({"path":"n.txt","edits":[{"op":"append","lines":["omega"]}]}),
            "alpha\nbeta\ngamma\ndelta\nomega\n",
        ),
        (
            "anchored prepend",
            ORIGINAL_WITH_NEWLINE,
            json!({"path":"n.txt","edits":[{"op":"prepend","pos":"3#KJ","lines":["ins"]}]}),
            "alpha\nbeta\nins\ngamma\ndelta\n",
        ),
        (
            "bof prepend",
            ORIGINAL_WITH_NEWLINE,
            json!({"path":"n.txt","edits":[{"op":"prepend","lines":["zero"]}]}),
            "zero\nalpha\nbeta\ngamma\ndelta\n",
        ),
        (
            "empty append",
            "",
            json!({"path":"n.txt","edits":[{"op":"append","lines":["first"]}]}),
            "first",
        ),
        (
            "empty prepend",
            "",
            json!({"path":"n.txt","edits":[{"op":"prepend","lines":["first"]}]}),
            "first",
        ),
    ];
    let tool = hya_tool_loader::builtins().get("edit").unwrap();
    for (name, initial, input, expected) in op_cases {
        let dir = tempdir("edit-ops");
        let _guard = TempDirGuard::new(&dir);
        tokio::fs::write(dir.join("n.txt"), initial).await.unwrap();
        let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
        tool.execute(&ctx, input)
            .await
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            tokio::fs::read_to_string(dir.join("n.txt")).await.unwrap(),
            expected,
            "{name}"
        );
    }

    let edits = json!([{"op": "replace_text", "oldText": "beta", "newText": "BETA"}]);
    let dialects = [
        json!({"path": "n.txt", "edits": edits.clone()}),
        json!({"path": "n.txt", "edits": serde_json::to_string(&edits).unwrap()}),
        json!({"file_path": "n.txt", "edits": edits}),
        json!({"path": "n.txt", "oldText": "beta", "newText": "BETA"}),
        json!({"path": "n.txt", "old_text": "beta", "new_text": "BETA"}),
    ];
    for input in dialects {
        let dir = tempdir("edit-dialect");
        let _guard = TempDirGuard::new(&dir);
        tokio::fs::write(dir.join("n.txt"), "alpha\nbeta\ngamma\n")
            .await
            .unwrap();
        let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
        tool.execute(&ctx, input).await.unwrap();
        assert_eq!(
            tokio::fs::read_to_string(dir.join("n.txt")).await.unwrap(),
            "alpha\nBETA\ngamma\n"
        );
    }
}

#[tokio::test]
async fn edit_rejects_malformed_requests_copied_prefixes_and_invalid_operations() {
    let dir = tempdir("edit-err");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::write(dir.join("notes.txt"), ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    tokio::fs::write(dir.join("dup.txt"), "alpha\nbeta\nbeta\n")
        .await
        .unwrap();
    tokio::fs::write(dir.join("ovl.txt"), "ababa")
        .await
        .unwrap();
    let ctx = ctx_with(vec![allow(Action::Edit, "*")], dir.clone());
    let tool = hya_tool_loader::builtins().get("edit").unwrap();

    let cases = [
        (
            json!({"filePath":"notes.txt","oldString":"beta","newString":"BETA"}),
            "E_BAD_REQUEST",
        ),
        (
            json!({"path":"notes.txt","file_path":"notes.txt","edits":[]}),
            "E_BAD_REQUEST",
        ),
        (
            json!({"path":"notes.txt","oldText":"beta","new_text":"BETA"}),
            "E_BAD_REQUEST",
        ),
        (json!({"path":"notes.txt","oldText":"beta"}), "E_BAD_OP"),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace_text","oldText":"b","newText":"B"}],"oldText":"b","newText":"B"}),
            "E_BAD_REQUEST",
        ),
        (json!({"path":7,"edits":[]}), "E_BAD_REQUEST"),
        (json!({"path":"","edits":[]}), "E_BAD_REQUEST"),
        (json!({"path":"notes.txt","edits":true}), "E_BAD_REQUEST"),
        (
            json!({"path":"notes.txt","edits":[{"op":1,"pos":"2#JB","lines":["B"]}]}),
            "E_BAD_REQUEST",
        ),
        (
            json!({"path":"notes.txt","edits":[{"pos":"2#JB","lines":["B"]}]}),
            "E_BAD_OP",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"2#JB","lines":"B"}]}),
            "E_BAD_REQUEST",
        ),
        (
            json!({"path":"notes.txt","edits":[],"replaceAll":true}),
            "E_BAD_REQUEST",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"2#JB","lines":["B"],"extra":true}]}),
            "E_BAD_REQUEST",
        ),
        (
            json!({"path":"notes.txt","oldText":9,"newText":"B"}),
            "E_BAD_REQUEST",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"2#JB","lines":["2#JB:beta"]}]}),
            "E_INVALID_PATCH",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"2#JB","lines":["- 2    beta"]}]}),
            "E_INVALID_PATCH",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"2#JB","lines":["+ 2#JB:beta"]}]}),
            "E_INVALID_PATCH",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"2#JB:gamma","lines":["B"]}]}),
            "E_STALE_ANCHOR",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"2:beta","lines":["B"]}]}),
            "E_BAD_REF",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"5#JB","lines":["B"]}]}),
            "E_RANGE_OOB",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"3#KJ","end":"2#JB","lines":["B"]}]}),
            "E_BAD_OP",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"1#KT","end":"2#JB","lines":["A"]},{"op":"replace","pos":"2#JB","end":"3#KJ","lines":["B"]}]}),
            "E_EDIT_CONFLICT",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"append","pos":"2#JB","lines":["A"]},{"op":"append","pos":"2#JB","lines":["B"]}]}),
            "E_EDIT_CONFLICT",
        ),
        (
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"1#KT","end":"4#PX","lines":[]}]}),
            "E_WOULD_EMPTY",
        ),
        (
            json!({"path":"notes.txt","oldText":"missing","newText":"B"}),
            "E_NO_MATCH",
        ),
        (
            json!({"path":"dup.txt","oldText":"beta","newText":"B"}),
            "E_MULTI_MATCH",
        ),
        (
            json!({"path":"ovl.txt","oldText":"aba","newText":"x"}),
            "E_MULTI_MATCH",
        ),
        (
            json!({"path":"notes.txt","oldText":"","newText":"x"}),
            "E_BAD_OP",
        ),
    ];
    for (input, code) in cases {
        assert_input_code(tool.execute(&ctx, input).await, code);
    }

    // Missing file fails cleanly without creating target or temp files.
    let missing = tool
        .execute(
            &ctx,
            json!({"path":"missing.txt","edits":[{"op":"append","lines":["x"]}]}),
        )
        .await;
    assert!(missing.is_err());
    assert!(!dir.join("missing.txt").exists());
    assert!(tmp_files(&dir).await.is_empty());
}

#[tokio::test]
async fn edit_fuzzy_hint_suspicious_warnings_and_authorized_missing_hash_context() {
    let dir = tempdir("edit-warn");
    let _guard = TempDirGuard::new(&dir);
    let registry = hya_tool_loader::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(
        vec![
            allow(Action::Read, "*"),
            allow(Action::Edit, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        dir.clone(),
    );

    // Fuzzy typographic-quote hint succeeds with warning.
    let quotes = dir.join("quotes.txt");
    tokio::fs::write(&quotes, "before\nprintln!(“old”)\nafter\n")
        .await
        .unwrap();
    let stale_anchor = read_row(read.as_ref(), &ctx, "quotes.txt", 2).await;
    tokio::fs::write(&quotes, "before\nprintln!(\"old\")\nafter\n")
        .await
        .unwrap();
    let fuzzy = edit
        .execute(&ctx, json!({"path":"quotes.txt","edits":[{"op":"replace","pos":stale_anchor,"lines":["replacement"]}]}))
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&quotes).await.unwrap(),
        "before\nreplacement\nafter\n"
    );
    assert_warning(&fuzzy, "fuzzy");

    // Suspicious insertion warnings (adjacent duplicate, bare hash prefix, \uDDDD).
    for (op, expected_warn) in [
        (
            json!({"op":"append","pos":"2#JB","lines":["gamma"]}),
            "duplicate",
        ),
        (
            json!({"op":"append","lines":["JB: copied content"]}),
            "hash and ':'",
        ),
        (json!({"op":"append","lines":[r"\uDDDD"]}), r"\uDDDD"),
    ] {
        tokio::fs::write(dir.join("w.txt"), ORIGINAL_WITH_NEWLINE)
            .await
            .unwrap();
        let out = edit
            .execute(&ctx, json!({"path":"w.txt","edits":[op]}))
            .await
            .unwrap();
        assert_warning(&out, expected_warn);
    }

    // Missing-hash anchors return live context ONLY when authorized and non-binary.
    let target = dir.join("big.txt");
    let original = format!(
        "{}{}",
        "row\r\n".repeat(1096),
        "alpha\r\nbeta\r\ngamma\r\ndelta\r\n"
    );
    std::fs::write(&target, &original).unwrap();
    let req =
        |p: &Path, pos: &str| json!({"path":p,"edits":[{"op":"replace","pos":pos,"lines":["x"]}]});
    for pos in ["1098#", "1098#:copied", "1098", "1098:copied"] {
        let err = edit
            .execute(&ctx, req(&target, pos))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("[E_BAD_REF]")
                && err.contains("\"1098#<missing>\"")
                && err.contains("1098#JB:beta")
        );
        assert!(!err.contains("copied"));
    }
    let oob = edit
        .execute(&ctx, req(&target, "9999#"))
        .await
        .unwrap_err()
        .to_string();
    assert!(oob.contains("File has 1100 visible lines; line 9999 is out of range."));

    let outside_dir = tempdir("edit-out-hash");
    let _out_guard = TempDirGuard::new(&outside_dir);
    let outside = outside_dir.join("outside.txt");
    std::fs::write(&outside, &original).unwrap();
    let binary = dir.join("binary");
    std::fs::write(&binary, b"private\0content").unwrap();
    let denied = ctx_with(vec![deny(Action::Read, "*")], dir.clone());
    let plain = edit
        .execute(&ctx, req(&dir.join("missing"), "1#"))
        .await
        .unwrap_err()
        .to_string();
    assert!(plain.contains("[E_BAD_REF]") && !plain.contains("Current anchors"));
    for (c, p) in [(&ctx, binary), (&ctx, outside), (&denied, target)] {
        assert_eq!(
            edit.execute(c, req(&p, "1#"))
                .await
                .unwrap_err()
                .to_string(),
            plain
        );
    }
}

#[tokio::test]
async fn edit_diff_bom_crlf_large_preview_omission_permissions_and_cancel() {
    let dir = tempdir("edit-meta");
    let _guard = TempDirGuard::new(&dir);
    let outside_dir = tempdir("edit-meta-out");
    let _out_guard = TempDirGuard::new(&outside_dir);
    let registry = hya_tool_loader::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(
        vec![
            allow(Action::Read, "*"),
            allow(Action::Edit, "*"),
            deny(Action::ExternalDirectory, "*"),
        ],
        dir.clone(),
    );

    // Strict diff/filediff metadata.
    let target = dir.join("notes.txt");
    tokio::fs::write(&target, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    let out = edit
        .execute(
            &ctx,
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":"2#JB","lines":["THREE"]}]}),
        )
        .await
        .unwrap();
    assert_eq!(out["title"], "notes.txt");
    assert_eq!(out["metadata"]["filediff"]["additions"], 1);
    assert_eq!(out["metadata"]["filediff"]["deletions"], 1);
    assert!(out["metadata"]["diff"].as_str().unwrap().contains("-beta"));
    assert!(out["metadata"]["diff"].as_str().unwrap().contains("+THREE"));

    // Preserves BOM and CRLF.
    tokio::fs::write(
        dir.join("bom.txt"),
        b"\xEF\xBB\xBFalpha\nbeta\ngamma\ndelta\n",
    )
    .await
    .unwrap();
    edit.execute(
        &ctx,
        json!({"path":"bom.txt","edits":[{"op":"replace","pos":"2#JB","lines":["BETA"]}]}),
    )
    .await
    .unwrap();
    assert_eq!(
        tokio::fs::read(dir.join("bom.txt")).await.unwrap(),
        b"\xEF\xBB\xBFalpha\nBETA\ngamma\ndelta\n"
    );

    tokio::fs::write(dir.join("crlf.txt"), b"alpha\r\nbeta\r\ngamma\r\ndelta\r\n")
        .await
        .unwrap();
    edit.execute(&ctx, json!({"path":"crlf.txt","edits":[{"op":"replace","pos":"2#JB","end":"3#KJ","lines":["BETA","GAMMA"]}]}))
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(dir.join("crlf.txt")).await.unwrap(),
        b"alpha\r\nBETA\r\nGAMMA\r\ndelta\r\n"
    );

    // Large changed region omits unbounded display text and requests re-read.
    let large_body = (1..=20)
        .map(|l| format!("line-{l}"))
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(dir.join("large.txt"), format!("{large_body}\n"))
        .await
        .unwrap();
    let start = read_row(read.as_ref(), &ctx, "large.txt", 1).await;
    let end = read_row(read.as_ref(), &ctx, "large.txt", 20).await;
    let repl: Vec<_> = (1..=20)
        .map(|l| format!("r-{l}-{}", "x".repeat(4096)))
        .collect();
    let large = edit
        .execute(&ctx, json!({"path":"large.txt","edits":[{"op":"replace","pos":start,"end":end,"lines":repl}]}))
        .await
        .unwrap();
    assert_eq!(large["metadata"]["display"]["text"], "");
    assert_eq!(large["metadata"]["display"]["truncated"], true);
    assert!(large["output"].as_str().unwrap().contains("Re-read"));

    // ExternalDirectory denied & pre-cancelled do not mutate.
    let out_file = outside_dir.join("ext.txt");
    tokio::fs::write(&out_file, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    assert!(matches!(
        edit.execute(
            &ctx,
            json!({"path":out_file,"edits":[{"op":"replace","pos":"2#JB","lines":["X"]}]})
        )
        .await,
        Err(ToolError::Permission(_))
    ));
    assert_eq!(
        tokio::fs::read_to_string(&out_file).await.unwrap(),
        ORIGINAL_WITH_NEWLINE
    );

    let cancel = CancellationToken::new();
    cancel.cancel();
    let cctx = ctx_with_planes(
        vec![allow(Action::Edit, "*")],
        dir.clone(),
        FormatterPlane::default(),
        LspPlane::default(),
        cancel,
        None,
    );
    assert!(matches!(
        edit.execute(
            &cctx,
            json!({"path":"notes.txt","edits":[{"op":"append","lines":["X"]}]})
        )
        .await,
        Err(ToolError::Cancelled)
    ));
}

#[tokio::test]
async fn edit_formatter_lsp_encoding_normalization_and_hard_link_metadata() {
    let dir = tempdir("edit-fmt");
    let _guard = TempDirGuard::new(&dir);
    let registry = hya_tool_loader::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();

    // 1) `hya/hashline` delegates post-edit formatting and LSP diagnostics to hooks
    // rather than invoking `ctx.formatter` or `ctx.lsp` directly inside `edit`.
    let main_rs = dir.join("main.rs");
    tokio::fs::write(&main_rs, ORIGINAL_WITH_NEWLINE)
        .await
        .unwrap();
    let diagnostics = error_diagnostics(&main_rs, "bad edit");
    let touches = Arc::new(Mutex::new(Vec::new()));
    let ctx = ctx_with_planes(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        dir.clone(),
        FormatterPlane::new(Arc::new(RewritingFormatter)),
        LspPlane::new(Arc::new(RecordingLsp {
            touches: Arc::clone(&touches),
            diagnostics,
        })),
        CancellationToken::new(),
        None,
    );
    let anchor = read_row(read.as_ref(), &ctx, "main.rs", 2).await;
    let out = edit
        .execute(
            &ctx,
            json!({"path":"main.rs","edits":[{"op":"replace","pos":anchor,"lines":["BETA"]}]}),
        )
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read_to_string(&main_rs).await.unwrap(),
        "alpha\nBETA\ngamma\ndelta\n"
    );
    assert!(out["output"].as_str().unwrap().contains(":BETA"));
    assert_eq!(out["metadata"]["diagnostics"], json!({}));
    assert!(touches.lock().await.is_empty());

    // 2) Hard-link in-place update preserves sibling visibility and diff metadata.
    #[cfg(unix)]
    {
        let hl = dir.join("hl.txt");
        let sib = dir.join("hl-sib.txt");
        tokio::fs::write(&hl, ORIGINAL_WITH_NEWLINE).await.unwrap();
        std::fs::hard_link(&hl, &sib).unwrap();
        let a = read_row(read.as_ref(), &ctx, "hl.txt", 2).await;
        let res = edit
            .execute(
                &ctx,
                json!({"path":"hl.txt","edits":[{"op":"replace","pos":a,"lines":["BETA","EXTRA"]}]}),
            )
            .await
            .unwrap();
        assert_eq!(res["metadata"]["hardLink"], true);
        assert_eq!(res["metadata"]["filediff"]["additions"], 2);
        assert_eq!(res["metadata"]["filediff"]["deletions"], 1);
        assert_eq!(
            tokio::fs::read(&sib).await.unwrap(),
            b"alpha\nBETA\nEXTRA\ngamma\ndelta\n"
        );
    }
}

#[tokio::test]
async fn edit_post_commit_formatter_and_lsp_failures_and_cancellation_reconcile() {
    let dir = tempdir("edit-post");
    let _guard = TempDirGuard::new(&dir);
    let registry = hya_tool_loader::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let base_ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        dir.clone(),
    );

    // Attached failing formatter/LSP planes do not abort `edit`, leave no temp
    // residue, and duplicate-edit guard fires on identical replay.
    tokio::fs::write(dir.join("fail.txt"), "before\n")
        .await
        .unwrap();
    let a_fail = read_row(read.as_ref(), &base_ctx, "fail.txt", 1).await;
    let fctx_fail = ctx_with_planes(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        dir.clone(),
        FormatterPlane::new(Arc::new(FailingFormatter(
            "forced formatter failure".into(),
        ))),
        LspPlane::new(Arc::new(FailingLsp)),
        CancellationToken::new(),
        None,
    );
    let req = json!({"path":"fail.txt","edits":[{"op":"replace","pos":a_fail,"lines":["after"]}]});
    edit.execute(&fctx_fail, req.clone()).await.unwrap();
    assert_eq!(
        tokio::fs::read(dir.join("fail.txt")).await.unwrap(),
        b"after\n"
    );
    assert!(tmp_files(&dir).await.is_empty());
    assert_input_code(edit.execute(&base_ctx, req).await, "E_DUPLICATE_EDIT");
}

#[tokio::test]
async fn read_emits_pinned_hashline_anchors_and_chains_to_edit() {
    let dir = tempdir("hl-chain");
    let _guard = TempDirGuard::new(&dir);
    tokio::fs::write(dir.join("notes.txt"), "alpha\nbeta\ngamma\ndelta\n")
        .await
        .unwrap();
    let registry = hya_tool_loader::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        dir.clone(),
    );

    let before = read
        .execute(&ctx, json!({ "path": "notes.txt" }))
        .await
        .unwrap();
    let out = before["output"].as_str().unwrap();
    // Pinned pi-hashline-edit 0.8.3 golden vectors; trailing newline adds no row 5.
    for row in ["1#KT:alpha", "2#JB:beta", "3#KJ:gamma", "4#PX:delta"] {
        assert!(out.contains(row), "missing {row}");
    }
    assert!(!out.lines().any(|l| l.starts_with("5#")));

    let beta = anchor_for_line(&before, 2);
    let edited = edit
        .execute(
            &ctx,
            json!({"path":"notes.txt","edits":[{"op":"replace","pos":beta,"lines":["BETA"]}]}),
        )
        .await
        .unwrap();
    assert_eq!(edited["metadata"]["classification"], "applied");
    let after = read
        .execute(&ctx, json!({ "path": "notes.txt" }))
        .await
        .unwrap();
    let fresh = anchor_for_line(&after, 2);
    assert_ne!(fresh, beta);
    assert!(edited["output"].as_str().unwrap().contains(&fresh));
}

#[tokio::test]
async fn stale_recovery_raw_read_conflict_and_no_history_guidance() {
    let dir = tempdir("hl-recover");
    let _guard = TempDirGuard::new(&dir);
    let registry = hya_tool_loader::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        dir.clone(),
    );

    // 1) Far stale recovery succeeds using non-raw Read snapshot.
    let base: Vec<_> = (1..=12).map(|l| format!("line-{l}")).collect();
    tokio::fs::write(dir.join("history.txt"), base.join("\n"))
        .await
        .unwrap();
    let r1 = read
        .execute(&ctx, json!({"path":"history.txt"}))
        .await
        .unwrap();
    let stale3 = anchor_for_line(&r1, 3);
    let fresh10 = anchor_for_line(&r1, 10);
    let mut ext = base.clone();
    ext[1] = "EXTERNAL_FAR_CHANGE".into();
    tokio::fs::write(dir.join("history.txt"), ext.join("\n"))
        .await
        .unwrap();
    let _ = read
        .execute(&ctx, json!({"path":"history.txt"}))
        .await
        .unwrap();
    let rec = edit
        .execute(
            &ctx,
            json!({
                "path": "history.txt",
                "edits": [
                    {"op":"replace","pos":stale3,"lines":["line-3"]},
                    {"op":"replace","pos":fresh10,"lines":["LINE-10-EDITED"]}
                ]
            }),
        )
        .await
        .unwrap();
    assert!(
        rec["output"]
            .as_str()
            .unwrap()
            .contains("Recovered stale anchors")
    );
    ext[9] = "LINE-10-EDITED".into();
    assert_eq!(
        tokio::fs::read_to_string(dir.join("history.txt"))
            .await
            .unwrap(),
        ext.join("\n")
    );

    // 2) Raw Read does NOT seed recovery snapshot.
    tokio::fs::write(dir.join("raw.txt"), "alpha\nbeta\ngamma\ndelta")
        .await
        .unwrap();
    read.execute(&ctx, json!({"path":"raw.txt","raw":true}))
        .await
        .unwrap();
    tokio::fs::write(dir.join("raw.txt"), "alpha\nbeta\nRAW_SECRET\ndelta")
        .await
        .unwrap();
    let raw_err = edit
        .execute(
            &ctx,
            json!({"path":"raw.txt","edits":[{"op":"replace","pos":"2#JB","lines":["X"]}]}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        raw_err.contains("[E_STALE_ANCHOR]")
            && !raw_err.contains("Recovery attempted:")
            && !raw_err.contains("RAW_SECRET")
    );

    // 3) Conflicting change on same line reports conflict guidance without leaking secrets.
    tokio::fs::write(
        dir.join("conflict.txt"),
        "one\ntwo\nthree\nfour\nfive\nsix\nseven\neight",
    )
    .await
    .unwrap();
    let rc = read
        .execute(&ctx, json!({"path":"conflict.txt"}))
        .await
        .unwrap();
    let a4 = anchor_for_line(&rc, 4);
    tokio::fs::write(
        dir.join("conflict.txt"),
        "one\ntwo\nthree\nCONFLICT_SECRET\nfive\nsix\nseven\neight",
    )
    .await
    .unwrap();
    let cerr = edit
        .execute(&ctx, json!({"path":"conflict.txt","edits":[{"op":"replace","pos":a4,"lines":["REPL_SECRET"]}]}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        cerr.contains("[E_STALE_ANCHOR]")
            && cerr.contains("Recovery attempted:")
            && !cerr.contains("CONFLICT_SECRET")
            && !cerr.contains("REPL_SECRET")
    );

    // 4) Anchor from another file reports no matching history without leaking content.
    tokio::fs::write(dir.join("target.txt"), "t1\nt2\nt3\nt4")
        .await
        .unwrap();
    tokio::fs::write(dir.join("other.txt"), "o1\no2\no3\no4")
        .await
        .unwrap();
    let _ = read
        .execute(&ctx, json!({"path":"target.txt"}))
        .await
        .unwrap();
    let ro = read
        .execute(&ctx, json!({"path":"other.txt"}))
        .await
        .unwrap();
    let no_hist = edit
        .execute(&ctx, json!({"path":"target.txt","edits":[{"op":"replace","pos":anchor_for_line(&ro, 2),"lines":["SECRET"]}]}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        no_hist.contains("[E_STALE_ANCHOR]")
            && no_hist.contains("do not match any recent read")
            && !no_hist.contains("SECRET")
    );
}

#[tokio::test]
async fn duplicate_and_noop_guards_reset_on_read_and_survive_failed_or_cancelled_read() {
    let dir = tempdir("hl-guards");
    let _guard = TempDirGuard::new(&dir);
    let registry = hya_tool_loader::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        dir.clone(),
    );

    // 1) Duplicate guard survives bad Read and resets on valid non-raw Read.
    tokio::fs::write(dir.join("dup.txt"), "alpha\nbeta")
        .await
        .unwrap();
    let payload = json!({"path":"dup.txt","edits":[{"op":"append","lines":["MARKER_SECRET"]}]});
    edit.execute(&ctx, payload.clone()).await.unwrap();
    assert_input_code(
        read.execute(&ctx, json!({"path":"dup.txt","offset":9}))
            .await,
        "E_BAD_READ",
    );
    let dup_err = edit
        .execute(&ctx, payload.clone())
        .await
        .unwrap_err()
        .to_string();
    assert!(dup_err.contains("[E_DUPLICATE_EDIT]") && !dup_err.contains("MARKER_SECRET"));
    read.execute(&ctx, json!({"path":"dup.txt"})).await.unwrap();
    edit.execute(&ctx, payload).await.unwrap();
    assert_eq!(
        tokio::fs::read_to_string(dir.join("dup.txt"))
            .await
            .unwrap()
            .matches("MARKER_SECRET")
            .count(),
        2
    );

    // 2) Two soft noops then E_NOOP_LOOP; survives bad Read and resets on valid non-raw Read.
    tokio::fs::write(dir.join("noop.txt"), "alpha\nbeta\ngamma")
        .await
        .unwrap();
    let rn = read
        .execute(&ctx, json!({"path":"noop.txt"}))
        .await
        .unwrap();
    let noop_req = json!({"path":"noop.txt","edits":[{"op":"replace","pos":anchor_for_line(&rn, 2),"lines":["beta"]}]});
    assert_eq!(
        edit.execute(&ctx, noop_req.clone()).await.unwrap()["metadata"]["classification"],
        "noop"
    );
    assert_eq!(
        edit.execute(&ctx, noop_req.clone()).await.unwrap()["metadata"]["classification"],
        "noop"
    );
    assert_input_code(
        read.execute(&ctx, json!({"path":"noop.txt","offset":9}))
            .await,
        "E_BAD_READ",
    );
    assert_input_code(edit.execute(&ctx, noop_req.clone()).await, "E_NOOP_LOOP");
    read.execute(&ctx, json!({"path":"noop.txt"}))
        .await
        .unwrap();
    assert_eq!(
        edit.execute(&ctx, noop_req).await.unwrap()["metadata"]["classification"],
        "noop"
    );

    // 3) Out-of-range Read & pre-cancelled Read do not add recovery history.
    let lines: Vec<_> = (1..=12).map(|l| format!("line-{l}")).collect();
    let original = lines.join("\n");
    tokio::fs::write(dir.join("src.txt"), &original)
        .await
        .unwrap();
    let src_read = read.execute(&ctx, json!({"path":"src.txt"})).await.unwrap();
    let a2 = anchor_for_line(&src_read, 2);
    let a10 = anchor_for_line(&src_read, 10);
    let mut changed = lines;
    changed[0] = "changed-1".into();
    let ext = changed.join("\n");

    for (file, cancel_read) in [("bad-hist.txt", false), ("cancel-hist.txt", true)] {
        tokio::fs::write(dir.join(file), &original).await.unwrap();
        if cancel_read {
            let cancel = CancellationToken::new();
            cancel.cancel();
            let cctx = ctx_with_planes(
                vec![allow(Action::Read, "*")],
                dir.clone(),
                FormatterPlane::default(),
                LspPlane::default(),
                cancel,
                None,
            );
            assert!(matches!(
                read.execute(&cctx, json!({"path":file})).await,
                Err(ToolError::Cancelled)
            ));
        } else {
            assert_input_code(
                read.execute(&ctx, json!({"path":file,"offset":13})).await,
                "E_BAD_READ",
            );
        }
        tokio::fs::write(dir.join(file), &ext).await.unwrap();
        let err = edit
            .execute(
                &ctx,
                json!({"path":file,"edits":[{"op":"replace","pos":a2,"lines":["line-2"]},{"op":"replace","pos":a10,"lines":["edited"]}]}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("[E_STALE_ANCHOR]") && err.contains("do not match any recent read"));
    }
}

#[tokio::test]
async fn snapshots_are_isolated_by_session_workdir_and_target() {
    let dir1 = tempdir("hl-iso-a");
    let dir2 = tempdir("hl-iso-b");
    let _g1 = TempDirGuard::new(&dir1);
    let _g2 = TempDirGuard::new(&dir2);
    let base = "one\ntwo\nthree\nfour\nfive";
    tokio::fs::write(dir1.join("target.txt"), base)
        .await
        .unwrap();
    tokio::fs::write(dir1.join("sibling.txt"), base)
        .await
        .unwrap();
    tokio::fs::write(
        dir2.join("target.txt"),
        "one\nOTHER_SECRET\nthree\nfour\nfive",
    )
    .await
    .unwrap();

    let registry = hya_tool_loader::builtins();
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let s_a = SessionId::new();
    let s_b = SessionId::new();
    let rules = vec![allow(Action::Read, "*"), allow(Action::Edit, "*")];
    let ctx_a = ctx_with_planes(
        rules.clone(),
        dir1.clone(),
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        Some(s_a),
    );
    let r = read
        .execute(&ctx_a, json!({"path":"target.txt"}))
        .await
        .unwrap();
    let a3 = anchor_for_line(&r, 3);

    tokio::fs::write(
        dir1.join("target.txt"),
        "one\nSESS_SECRET\nthree\nfour\nfive",
    )
    .await
    .unwrap();
    tokio::fs::write(
        dir1.join("sibling.txt"),
        "one\nSIB_SECRET\nthree\nfour\nfive",
    )
    .await
    .unwrap();

    let ctx_b = ctx_with_planes(
        rules.clone(),
        dir1.clone(),
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        Some(s_b),
    );
    let ctx_other_dir = ctx_with_planes(
        rules,
        dir2,
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        Some(s_a),
    );

    for (ctx, path) in [
        (&ctx_b, "target.txt"),
        (&ctx_a, "sibling.txt"),
        (&ctx_other_dir, "target.txt"),
    ] {
        let msg = edit
            .execute(
                ctx,
                json!({"path":path,"edits":[{"op":"replace","pos":a3,"lines":["X"]}]}),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(msg.contains("[E_STALE_ANCHOR]") && msg.contains("do not match any recent read"));
        assert!(!msg.contains("SECRET"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn edit_follows_relative_symlink_and_preserves_link_identity() {
    use std::os::unix::fs::symlink;

    let dir = tempdir("hl-symlink");
    let _guard = TempDirGuard::new(&dir);
    let target = dir.join("real.txt");
    let link = dir.join("relative.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    symlink("real.txt", &link).unwrap();

    let registry = hya_tool_loader::builtins();
    let ctx = ctx_with(
        vec![allow(Action::Read, "*"), allow(Action::Edit, "*")],
        dir,
    );
    let read = registry.get("read").unwrap();
    let edit = registry.get("edit").unwrap();
    let anchor = read_row(read.as_ref(), &ctx, "relative.txt", 1).await;
    edit.execute(
        &ctx,
        json!({"path":"relative.txt","edits":[{"op":"replace","pos":anchor,"lines":["after"]}]}),
    )
    .await
    .unwrap();
    assert_eq!(tokio::fs::read_to_string(&target).await.unwrap(), "after\n");
    assert!(
        std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[tokio::test]
async fn same_resolved_file_serializes_mutations_across_sessions() {
    let dir = tempdir("hl-ser-sess");
    let _guard = TempDirGuard::new(&dir);
    let target = dir.join("shared.txt");
    tokio::fs::write(&target, "root\n").await.unwrap();
    let registry = Arc::new(hya_tool_loader::builtins());
    let rules = vec![allow(Action::Read, "*"), allow(Action::Edit, "*")];

    let mut handles = Vec::new();
    for idx in 0..8 {
        let r = Arc::clone(&registry);
        let ctx = ctx_with_planes(
            rules.clone(),
            dir.clone(),
            FormatterPlane::default(),
            LspPlane::default(),
            CancellationToken::new(),
            Some(SessionId::new()),
        );
        let line = format!("session-{idx}");
        handles.push(tokio::spawn(async move {
            append_lines(r.as_ref(), &ctx, "shared.txt", &[line.as_str()]).await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    let final_text = tokio::fs::read_to_string(&target).await.unwrap();
    let lines: BTreeSet<&str> = final_text.lines().collect();
    assert_eq!(lines.len(), 9);
    assert!(lines.contains("root"));
    for idx in 0..8 {
        assert!(lines.contains(format!("session-{idx}").as_str()));
    }
    assert!(tmp_files(&dir).await.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn hard_link_aliases_serialize_mutations_by_inode() {
    use std::os::unix::fs::MetadataExt;

    let dir = tempdir("hl-ser-ino");
    let _guard = TempDirGuard::new(&dir);
    let target = dir.join("target.txt");
    let alias = dir.join("alias.txt");
    tokio::fs::write(&target, "root\n").await.unwrap();
    std::fs::hard_link(&target, &alias).unwrap();
    let inode = std::fs::metadata(&target).unwrap().ino();
    let registry = Arc::new(hya_tool_loader::builtins());
    let rules = vec![allow(Action::Read, "*"), allow(Action::Edit, "*")];

    let mut handles = Vec::new();
    for idx in 0..8 {
        let r = Arc::clone(&registry);
        let ctx = ctx_with_planes(
            rules.clone(),
            dir.clone(),
            FormatterPlane::default(),
            LspPlane::default(),
            CancellationToken::new(),
            Some(SessionId::new()),
        );
        let path = if idx % 2 == 0 {
            "target.txt"
        } else {
            "alias.txt"
        };
        let line = format!("alias-{idx}");
        handles.push(tokio::spawn(async move {
            append_lines(r.as_ref(), &ctx, path, &[line.as_str()]).await
        }));
    }
    for handle in handles {
        handle.await.unwrap().unwrap();
    }

    assert_eq!(std::fs::metadata(&target).unwrap().ino(), inode);
    let final_text = tokio::fs::read_to_string(&alias).await.unwrap();
    let lines: BTreeSet<&str> = final_text.lines().collect();
    assert_eq!(lines.len(), 9);
    assert!(lines.contains("root"));
    for idx in 0..8 {
        assert!(lines.contains(format!("alias-{idx}").as_str()));
    }
}

#[tokio::test]
async fn read_observes_consistent_snapshots_alongside_edits() {
    let dir = tempdir("hl-read-wait");
    let _guard = TempDirGuard::new(&dir);
    let target = dir.join("final.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    let registry = Arc::new(hya_tool_loader::builtins());
    let rules = vec![allow(Action::Read, "*"), allow(Action::Edit, "*")];
    let edit_ctx = ctx_with_planes(
        rules.clone(),
        dir.clone(),
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        Some(SessionId::new()),
    );
    let read_ctx = ctx_with_planes(
        rules,
        dir,
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        Some(SessionId::new()),
    );

    append_lines(registry.as_ref(), &edit_ctx, "final.txt", &["edit"])
        .await
        .unwrap();
    let read_res = registry
        .get("read")
        .unwrap()
        .execute(&read_ctx, json!({"path":"final.txt"}))
        .await
        .unwrap();
    assert_eq!(read_res["content"].as_str(), Some("before\nedit"));
}

#[cfg(unix)]
#[tokio::test]
async fn waiting_symlink_edit_reresolves_after_retarget() {
    use std::os::unix::fs::symlink;

    let dir = tempdir("hl-retarget");
    let _guard = TempDirGuard::new(&dir);
    let old_target = dir.join("old.txt");
    let new_target = dir.join("new.txt");
    let link = dir.join("current.txt");
    tokio::fs::write(&old_target, "old\n").await.unwrap();
    tokio::fs::write(&new_target, "new\n").await.unwrap();
    symlink("old.txt", &link).unwrap();

    let registry = Arc::new(hya_tool_loader::builtins());
    let sid = Some(SessionId::new());
    let rules = vec![allow(Action::Read, "*"), allow(Action::Edit, "*")];
    let c1 = ctx_with_planes(
        rules.clone(),
        dir.clone(),
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        sid,
    );
    let c2 = ctx_with_planes(
        rules,
        dir,
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        sid,
    );

    append_lines(registry.as_ref(), &c1, "current.txt", &["first"])
        .await
        .unwrap();
    std::fs::remove_file(&link).unwrap();
    symlink("new.txt", &link).unwrap();
    append_lines(registry.as_ref(), &c2, "current.txt", &["second"])
        .await
        .unwrap();

    assert_eq!(tokio::fs::read(&old_target).await.unwrap(), b"old\nfirst\n");
    assert_eq!(
        tokio::fs::read(&new_target).await.unwrap(),
        b"new\nsecond\n"
    );
}

#[tokio::test]
async fn cancelled_waiting_edit_returns_cancelled_without_commit() {
    let dir = tempdir("hl-cancel-wait");
    let _guard = TempDirGuard::new(&dir);
    let target = dir.join("cancel.txt");
    tokio::fs::write(&target, "before\n").await.unwrap();
    let registry = Arc::new(hya_tool_loader::builtins());
    let session = SessionId::new();
    let rules = vec![allow(Action::Read, "*"), allow(Action::Edit, "*")];
    let c1 = ctx_with_planes(
        rules.clone(),
        dir.clone(),
        FormatterPlane::default(),
        LspPlane::default(),
        CancellationToken::new(),
        Some(session),
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    let c2 = ctx_with_planes(
        rules,
        dir,
        FormatterPlane::default(),
        LspPlane::default(),
        cancel,
        Some(session),
    );

    append_lines(registry.as_ref(), &c1, "cancel.txt", &["first"])
        .await
        .unwrap();
    let second_res = append_lines(registry.as_ref(), &c2, "cancel.txt", &["cancelled"]).await;
    assert!(matches!(second_res, Err(ToolError::Cancelled)));
    assert_eq!(tokio::fs::read(&target).await.unwrap(), b"before\nfirst\n");
}
