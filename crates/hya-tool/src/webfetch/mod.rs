mod html;

use std::time::Duration;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hya_proto::{ToolName, ToolSchema};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::permission::{Action, Resource};
use crate::tool::{Tool, ToolCtx, ToolError};

const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;
const DEFAULT_TIMEOUT_SECS: f64 = 30.0;
const MAX_TIMEOUT_SECS: f64 = 120.0;

pub(crate) struct WebFetchTool;

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum WebFetchFormat {
    Text,
    #[default]
    Markdown,
    Html,
}

#[derive(Deserialize)]
struct WebFetchInput {
    url: String,
    #[serde(default)]
    format: WebFetchFormat,
    timeout: Option<f64>,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "webfetch"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: ToolName::new("webfetch"),
            description: "Fetch content from an HTTP(S) URL as text, markdown, or raw HTML."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch content from"
                    },
                    "format": {
                        "type": "string",
                        "enum": ["text", "markdown", "html"],
                        "description": "The output format. Defaults to markdown."
                    },
                    "timeout": {
                        "type": "number",
                        "description": "Optional timeout in seconds, capped at 120."
                    }
                },
                "required": ["url"]
            }),
            output_schema: None,
        }
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let input: WebFetchInput =
            serde_json::from_value(input).map_err(|e| ToolError::Input(e.to_string()))?;
        let url = parse_url(&input.url)?;
        ctx.permission
            .assert(Action::WebFetch, Resource::Url(url.as_str().to_string()))
            .await?;

        let timeout_secs = input
            .timeout
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(0.0, MAX_TIMEOUT_SECS);
        let timeout = Duration::from_secs_f64(timeout_secs);
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .user_agent("hya")
            .build()
            .map_err(|e| ToolError::Other(e.to_string()))?;
        let response = client
            .get(url.clone())
            .header(reqwest::header::ACCEPT, accept_header(input.format))
            .send()
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            return Err(ToolError::Other(format!(
                "webfetch request failed with HTTP {status}"
            )));
        }
        if let Some(length) = response.content_length()
            && length > u64::try_from(MAX_RESPONSE_BYTES).unwrap_or(u64::MAX)
        {
            return Err(ToolError::Other(
                "response too large (exceeds 5MB limit)".to_string(),
            ));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = response
            .bytes()
            .await
            .map_err(|e| ToolError::Other(e.to_string()))?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(ToolError::Other(
                "response too large (exceeds 5MB limit)".to_string(),
            ));
        }
        if let Some(mime) = image_attachment_mime(&content_type) {
            return Ok(json!({
                "title": format!("{} ({content_type})", url.as_str()),
                "output": "Image fetched successfully",
                "metadata": {},
                "attachments": [{
                    "type": "file",
                    "mime": mime,
                    "url": format!("data:{mime};base64,{}", STANDARD.encode(&bytes)),
                }],
            }));
        }
        let content = String::from_utf8_lossy(&bytes);
        let output = match input.format {
            WebFetchFormat::Html => content.into_owned(),
            WebFetchFormat::Text => {
                if is_html(&content_type) {
                    html::to_text(&content)
                } else {
                    content.into_owned()
                }
            }
            WebFetchFormat::Markdown => {
                if is_html(&content_type) {
                    html::to_markdown(&content)
                } else {
                    content.into_owned()
                }
            }
        };

        Ok(json!({
            "url": url.as_str(),
            "status": status.as_u16(),
            "content_type": content_type,
            "output": output,
        }))
    }
}

fn parse_url(raw: &str) -> Result<reqwest::Url, ToolError> {
    let url = reqwest::Url::parse(raw).map_err(|e| ToolError::Input(e.to_string()))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        _ => Err(ToolError::Input(
            "url must start with http:// or https://".to_string(),
        )),
    }
}

const fn accept_header(format: WebFetchFormat) -> &'static str {
    match format {
        WebFetchFormat::Markdown => "text/markdown, text/plain;q=0.9, text/html;q=0.8, */*;q=0.1",
        WebFetchFormat::Text => "text/plain, text/markdown;q=0.9, text/html;q=0.8, */*;q=0.1",
        WebFetchFormat::Html => "text/html, application/xhtml+xml;q=0.9, */*;q=0.1",
    }
}

fn is_html(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("text/html"))
}

fn image_attachment_mime(content_type: &str) -> Option<String> {
    let mime = content_type
        .split(';')
        .next()
        .map(str::trim)?
        .to_ascii_lowercase();
    matches!(
        mime.as_str(),
        "image/jpeg" | "image/png" | "image/gif" | "image/webp"
    )
    .then_some(mime)
}
