use std::io::Cursor;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hya_proto::ToolSchema;
use hya_tool::{MAX_CODING_ATTACHMENT_BYTES, Tool, ToolCtx, ToolError};
use image::{ImageFormat, ImageReader};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) struct Read;

#[derive(Deserialize)]
struct Args {
    path: String,
}

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }

    fn schema(&self) -> ToolSchema {
        super::schema(
            "read",
            "Read a UTF-8 text file or an image (PNG, JPEG, GIF, WebP). An image, when it is attached, is shown to you in the next step only; read it again to see it again.",
            json!({
                "type": "object",
                "properties": {"path": {"type": "string", "description": "File path, relative to the working directory or absolute"}},
                "required": ["path"],
            }),
        )
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        let Args { path } = super::args(input)?;
        let bytes = tokio::fs::read(super::resolve(&ctx.workdir, &path)).await?;
        if let Some((mime, format)) = image_mime(&bytes) {
            let prefix = format!("data:{mime};base64,");
            let empty = json!([{"type": "file", "mime": mime, "url": prefix}]);
            // Padded base64 uses four bytes for every three bytes of file data.
            let limit = (MAX_CODING_ATTACHMENT_BYTES - empty.to_string().len()) / 4 * 3;
            let size = bytes.len();
            if size > limit {
                return Ok(json!({
                    "output": format!(
                        "Image file ({mime}, {size} bytes). It is larger than the limit of {limit} \
                         bytes for an image in a tool result, so it is not attached. Downscale or \
                         crop it with a program and read the smaller file."
                    ),
                }));
            }
            let (bytes, decoded) = tokio::task::spawn_blocking(move || {
                let decoded = ImageReader::with_format(Cursor::new(&bytes), format)
                    .decode()
                    .map(drop);
                (bytes, decoded)
            })
            .await
            .map_err(|err| ToolError::Other(format!("image decode task failed: {err}")))?;
            if let Err(err) = decoded {
                return Ok(json!({
                    "output": format!(
                        "Image file ({mime}, {size} bytes). It does not decode as {mime} ({err}), \
                         so it is not attached. The file may be malformed or incomplete. \
                         Check the program that wrote it."
                    ),
                }));
            }
            return Ok(json!({
                "output": format!("Image file ({mime}, {size} bytes)."),
                "attachments": [{
                    "type": "file",
                    "mime": mime,
                    "url": format!("{prefix}{}", STANDARD.encode(&bytes)),
                }],
            }));
        }
        let content = String::from_utf8(bytes).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )
        })?;
        Ok(json!({"output": content}))
    }
}

fn image_mime(bytes: &[u8]) -> Option<(&'static str, ImageFormat)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", ImageFormat::Png))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(("image/jpeg", ImageFormat::Jpeg))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("image/gif", ImageFormat::Gif))
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(("image/webp", ImageFormat::WebP))
    } else {
        None
    }
}
