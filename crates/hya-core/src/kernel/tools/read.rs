use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hya_proto::ToolSchema;
use hya_tool::{Tool, ToolCtx, ToolError};
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
            "Read a UTF-8 text file or an image (PNG, JPEG, GIF, WebP).",
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
        if let Some(mime) = image_mime(&bytes) {
            return Ok(json!({
                "output": format!("Image file ({mime}, {} bytes).", bytes.len()),
                "attachments": [{
                    "type": "file",
                    "mime": mime,
                    "url": format!("data:{mime};base64,{}", STANDARD.encode(&bytes)),
                }],
            }));
        }
        let content = String::from_utf8(bytes).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )
        })?;
        Ok(json!({"content": content}))
    }
}

fn image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}
