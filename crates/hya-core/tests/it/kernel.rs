//! Direct kernel tool checks.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::{path::Path, sync::Arc, time::Duration};

use hya_core::kernel;
use hya_proto::context::tool_result_text;
use hya_tool::{Action, Mode, PermissionPlane, PermissionRules, Rule, Tool, ToolCtx, ToolError};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn tool_ctx(workdir: &Path, cancel: &CancellationToken) -> ToolCtx {
    let (permission, _rx) = PermissionPlane::new(PermissionRules::new(vec![
        Rule::new(Action::Read, "*", Mode::Allow),
        Rule::new(Action::Edit, "*", Mode::Allow),
        Rule::new(Action::Bash, "*", Mode::Allow),
        Rule::new(Action::Grep, "*", Mode::Allow),
    ]));
    ToolCtx::new(workdir.to_path_buf(), permission, cancel.clone())
}

fn builtin_tool(name: &str) -> Arc<dyn Tool> {
    kernel::tools::builtin()
        .into_iter()
        .find(|tool| tool.name() == name)
        .unwrap()
}

#[tokio::test]
async fn read_verbatim_and_literal_edit_validation() {
    let dir = tempfile::tempdir().unwrap();
    let sample = "first line\n\"quoted\" and a \\ backslash\n";
    std::fs::write(dir.path().join("text.txt"), sample).unwrap();
    let ctx = tool_ctx(dir.path(), &CancellationToken::new());
    let read = builtin_tool("read");
    let result = read
        .execute(&ctx, json!({"path": "text.txt"}))
        .await
        .unwrap();
    assert_eq!(tool_result_text(&result), sample);
    assert_eq!(result["output"], sample);
    assert_eq!(result["title"], "text.txt");
    assert_eq!(result["metadata"]["truncated"], false);

    let edit = builtin_tool("edit");
    for (content, old, new, reason) in [
        ("", "", "x", "non-empty"),
        ("aaa", "aa", "x", "overlapping"),
        ("ééé", "éé", "x", "overlapping"),
        ("hello\n", "hello", "hello", "identical"),
    ] {
        let path = dir.path().join("edit.txt");
        std::fs::write(&path, content).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1_000_000_000)),
        )
        .unwrap();
        let mtime = file.metadata().unwrap().modified().unwrap();
        let err = edit
            .execute(&ctx, json!({"path": "edit.txt", "old": old, "new": new}))
            .await
            .expect_err("invalid literal edit rejected");
        assert!(
            matches!(err, ToolError::Input(ref msg) if msg.contains(reason)),
            "{err:?}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), mtime);
    }
}

fn encoded_test_image(format: image::ImageFormat) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(2, 2, image::Rgb([255, 0, 0]));
    let mut encoded = std::io::Cursor::new(Vec::new());
    image.write_to(&mut encoded, format).unwrap();
    encoded.into_inner()
}

#[tokio::test]
async fn read_image_validation_and_valid_formats() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use image::ImageFormat;

    let dir = tempfile::tempdir().unwrap();
    let read = builtin_tool("read");
    let ctx = tool_ctx(dir.path(), &CancellationToken::new());
    let run_read = |bytes: &[u8]| {
        std::fs::write(dir.path().join("image.data"), bytes).unwrap();
        read.execute(&ctx, json!({"path": "image.data"}))
    };

    // Valid formats attach original bytes.
    for (format, mime) in [
        (ImageFormat::Png, "image/png"),
        (ImageFormat::Jpeg, "image/jpeg"),
        (ImageFormat::Gif, "image/gif"),
        (ImageFormat::WebP, "image/webp"),
    ] {
        let bytes = encoded_test_image(format);
        let result = run_read(&bytes).await.unwrap();
        assert_eq!(
            result["output"],
            format!("Image file ({mime}, {} bytes).", bytes.len())
        );
        assert_eq!(
            result["attachments"],
            json!([{
                "type": "file",
                "mime": mime,
                "url": format!("data:{mime};base64,{}", STANDARD.encode(&bytes)),
            }])
        );
    }

    // Corrupt/truncated images report decode errors without attachments.
    let mut corrupt_png = encoded_test_image(ImageFormat::Png);
    assert_eq!(&corrupt_png[37..41], b"IDAT");
    corrupt_png[41] = 0;
    let mut trunc_jpeg = encoded_test_image(ImageFormat::Jpeg);
    trunc_jpeg.truncate(32);
    for (bytes, mime) in [
        (corrupt_png.as_slice(), "image/png"),
        (b"\x89PNG\r\n\x1a\n".as_slice(), "image/png"),
        (trunc_jpeg.as_slice(), "image/jpeg"),
    ] {
        let result = run_read(bytes).await.unwrap();
        assert!(result.get("attachments").is_none());
        let notice = tool_result_text(&result);
        assert!(notice.starts_with(&format!(
            "Image file ({mime}, {} bytes). It does not decode as {mime} (",
            bytes.len()
        )));
        assert!(notice.contains("), so it is not attached."));
    }
}
