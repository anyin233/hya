//! Standard `read` tool for `hya-core`.
//!
//! Reads UTF-8 text files, directories, images (PNG, JPEG, GIF, WebP), internal
//! handles (`artifact://`, `skill://`, `local://`), and dynamic custom URI
//! schemes registered on [`hya_tool::handle::HandleRouter`]. Enforces
//! [`hya_tool::PermissionPlane`], [`hya_tool::ProjectScope`], and coding output
//! capping with zero hashline or `channel://` coupling.

use std::path::Path;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use hya_proto::ToolSchema;
use hya_tool::handle::HandleError;
use hya_tool::media::{max_image_attachment_bytes, sniff_image_mime, validate_image_decode};
use hya_tool::path::display_path;
use hya_tool::{
    Action, Resource, Tool, ToolCtx, ToolError, ToolResultPolicy, cap_tool_output_with_policy,
    serialized_string_len,
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    MAX_CODING_OUTPUT_BYTES, assert_external_directory, check_cancel, closed_obj_schema,
    map_handle_error, relative_title, truncate_utf8,
};

const DEFAULT_DIRECTORY_LIMIT: usize = 200;

pub(super) struct Read;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }

    fn schema(&self) -> ToolSchema {
        closed_obj_schema(
            "read",
            concat!(
                "Read a UTF-8 text file, directory listing, or image (PNG, JPEG, GIF, WebP). ",
                "An image, when it is attached, is shown to you in the next step only; read it again to see it again.\n\n",
                "`path` also accepts a handle URI: `artifact://<id>`, `skill://<name>`, `local://<path>`, ",
                "or any registered custom URI scheme. Internal handles accept one projection: ",
                "`?lines=N`, `?lines=N-M`, `?head=N`, `?tail=N`, `?grep=<pattern>`, or `?q=<.dotted.path>`."
            ),
            json!({
                "path": {
                    "type": "string",
                    "description": "File or directory path (relative to the working directory or absolute) or handle URI"
                },
                "offset": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional 1-based starting line number (or entry number for directories)"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional maximum number of lines (or directory entries) to return"
                }
            }),
            &["path"],
        )
    }

    fn result_policy(&self) -> ToolResultPolicy {
        ToolResultPolicy::Coding
    }

    async fn execute(&self, ctx: &ToolCtx, input: Value) -> Result<Value, ToolError> {
        check_cancel(ctx)?;
        reject_null_fields(&input)?;
        let Args {
            path,
            offset,
            limit,
        } = super::args(input)?;
        if path.is_empty() {
            return Err(ToolError::Input(
                "read requires a non-empty `path` string".to_string(),
            ));
        }
        if offset == Some(0) {
            return Err(ToolError::Input(
                "offset must be a positive 1-based integer".to_string(),
            ));
        }
        if limit == Some(0) {
            return Err(ToolError::Input(
                "limit must be a positive integer".to_string(),
            ));
        }

        // Route internal (`artifact://`, `skill://`, `local://`) and dynamic
        // `UriSchemaHandler` URIs generically through `HandleRouter`.
        match ctx.handles().resolve_uri(ctx.session, &path).await {
            Ok(resolved) => {
                check_cancel(ctx)?;
                ctx.permission
                    .assert(Action::Read, Resource::Path(path.clone()))
                    .await?;
                check_cancel(ctx)?;
                return format_text_result(&path, &path, &resolved.body, offset, limit);
            }
            Err(HandleError::NotAHandle(_)) => {}
            Err(error) => return Err(map_handle_error(error)),
        }

        let resolved = super::resolve(&ctx.workdir, &path);
        assert_external_directory(ctx, &resolved, false).await?;
        check_cancel(ctx)?;
        ctx.permission
            .assert(Action::Read, Resource::Path(display_path(&resolved)))
            .await?;
        check_cancel(ctx)?;

        let meta = tokio::fs::metadata(&resolved).await?;
        check_cancel(ctx)?;
        let title = relative_title(&resolved, &ctx.workdir);
        if meta.is_dir() {
            return read_directory(&resolved, &title, offset, limit, ctx).await;
        }

        let bytes = tokio::fs::read(&resolved).await?;
        check_cancel(ctx)?;
        if let Some((mime, format)) = sniff_image_mime(&bytes) {
            let limit = max_image_attachment_bytes(mime);
            let size = bytes.len();
            if size > limit {
                return Ok(cap_tool_output_with_policy(
                    json!({
                        "title": title,
                        "output": format!(
                            "Image file ({mime}, {size} bytes). It is larger than the limit of {limit} \
                             bytes for an image in a tool result, so it is not attached. Downscale or \
                             crop it with a program and read the smaller file."
                        ),
                        "metadata": {
                            "path": display_path(&resolved),
                            "truncated": false,
                        },
                    }),
                    ToolResultPolicy::Coding,
                ));
            }
            let (bytes, decoded) = validate_image_decode(bytes, format)
                .await
                .map_err(|err| ToolError::Other(format!("image decode task failed: {err}")))?;
            if let Err(err) = decoded {
                return Ok(cap_tool_output_with_policy(
                    json!({
                        "title": title,
                        "output": format!(
                            "Image file ({mime}, {size} bytes). It does not decode as {mime} ({err}), \
                             so it is not attached. The file may be malformed or incomplete. \
                             Check the program that wrote it."
                        ),
                        "metadata": {
                            "path": display_path(&resolved),
                            "truncated": false,
                        },
                    }),
                    ToolResultPolicy::Coding,
                ));
            }
            return Ok(cap_tool_output_with_policy(
                json!({
                    "title": title,
                    "output": format!("Image file ({mime}, {size} bytes)."),
                    "metadata": {
                        "path": display_path(&resolved),
                        "truncated": false,
                    },
                    "attachments": [{
                        "type": "file",
                        "mime": mime,
                        "url": format!("data:{mime};base64,{}", STANDARD.encode(&bytes)),
                    }],
                }),
                ToolResultPolicy::Coding,
            ));
        }

        let content = String::from_utf8(bytes).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )
        })?;
        format_text_result(&title, &display_path(&resolved), &content, offset, limit)
    }
}

fn reject_null_fields(input: &Value) -> Result<(), ToolError> {
    let Some(object) = input.as_object() else {
        return Ok(());
    };
    for field in ["offset", "limit"] {
        if object.get(field).is_some_and(Value::is_null) {
            return Err(ToolError::Input(format!("{field} must not be null")));
        }
    }
    Ok(())
}

async fn read_directory(
    dir: &Path,
    title: &str,
    offset: Option<usize>,
    limit: Option<usize>,
    ctx: &ToolCtx,
) -> Result<Value, ToolError> {
    let mut entries = Vec::new();
    let mut reader = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = reader.next_entry().await? {
        check_cancel(ctx)?;
        let mut name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type().await.is_ok_and(|ft| ft.is_dir()) {
            name.push('/');
        }
        entries.push(name);
    }
    entries.sort();
    let total = entries.len();
    let start = offset.unwrap_or(1);
    if total > 0 && start > total {
        return Err(ToolError::Input(format!(
            "offset {start} is out of range for directory ({total} entries)"
        )));
    }
    let page_limit = limit.unwrap_or(DEFAULT_DIRECTORY_LIMIT);
    let selected: Vec<String> = entries
        .into_iter()
        .skip(start.saturating_sub(1))
        .take(page_limit)
        .collect();
    let shown = selected.len();
    let end = if shown == 0 {
        0
    } else {
        start.saturating_add(shown).saturating_sub(1)
    };
    let truncated = end < total;
    let next_offset = truncated.then(|| end.saturating_add(1));
    let mut output = selected.join("\n");
    if truncated {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&format!(
            "[Showing entries {start}-{end} of {total}. Use offset={} to continue.]",
            end.saturating_add(1)
        ));
    }
    Ok(cap_tool_output_with_policy(
        json!({
            "title": title,
            "output": output,
            "metadata": {
                "path": display_path(dir),
                "type": "directory",
                "entries": selected,
                "totalEntries": total,
                "offset": start,
                "truncated": truncated,
                "nextOffset": next_offset,
            },
        }),
        ToolResultPolicy::Coding,
    ))
}

fn format_text_result(
    title: &str,
    display_target: &str,
    content: &str,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<Value, ToolError> {
    let lines = split_lines_preserving_count(content);
    let total_lines = lines.len();
    let line_start = offset.unwrap_or(1);
    if total_lines > 0 && line_start > total_lines {
        return Err(ToolError::Input(format!(
            "offset {line_start} is out of range for this file ({total_lines} lines)"
        )));
    }

    // Fast path: full verbatim read within the serialized coding output budget.
    let is_unpaged = offset.is_none() && limit.is_none();
    if is_unpaged && serialized_string_len(content) <= MAX_CODING_OUTPUT_BYTES {
        let line_end = total_lines;
        return Ok(cap_tool_output_with_policy(
            json!({
                "title": title,
                "output": content,
                "metadata": {
                    "path": display_target,
                    "lineStart": if total_lines == 0 { 0 } else { 1 },
                    "lineEnd": line_end,
                    "totalLines": total_lines,
                    "truncated": false,
                    "nextOffset": Value::Null,
                },
            }),
            ToolResultPolicy::Coding,
        ));
    }

    if total_lines == 0 {
        return Ok(cap_tool_output_with_policy(
            json!({
                "title": title,
                "output": "",
                "metadata": {
                    "path": display_target,
                    "lineStart": 0,
                    "lineEnd": 0,
                    "totalLines": 0,
                    "truncated": false,
                    "nextOffset": Value::Null,
                },
            }),
            ToolResultPolicy::Coding,
        ));
    }

    let start_idx = line_start.saturating_sub(1);
    let max_lines = limit.unwrap_or(total_lines.saturating_sub(start_idx));
    let slice = &lines[start_idx..total_lines.min(start_idx.saturating_add(max_lines))];
    let line_limited = start_idx.saturating_add(slice.len()) < total_lines;

    // Reserve room for outer JSON quotes and the worst-case serialized footer so
    // `cap_tool_output_with_policy` preserves the continuation notice intact.
    let worst_footer = format!(
        "\n\n[Showing lines {line_start}-{total_lines} of {total_lines} ({MAX_CODING_OUTPUT_BYTES} byte limit). Use offset={} to continue.]",
        total_lines.saturating_add(1)
    );
    let worst_footer_bytes = serialized_string_len(&worst_footer).saturating_sub(2);
    let body_budget = MAX_CODING_OUTPUT_BYTES
        .saturating_sub(2)
        .saturating_sub(worst_footer_bytes);

    let mut kept_count = 0usize;
    let mut used_bytes = 0usize;
    let mut byte_limited = false;
    for line in slice {
        let sep = if kept_count > 0 { 2 } else { 0 };
        let line_bytes = serialized_string_len(line).saturating_sub(2);
        let next_bytes = used_bytes.saturating_add(sep).saturating_add(line_bytes);
        if next_bytes > body_budget {
            byte_limited = true;
            break;
        }
        used_bytes = next_bytes;
        kept_count += 1;
    }

    let (body, line_end) = if kept_count == 0 {
        // First selected line alone exceeds budget: emit a UTF-8 safe prefix
        // whose JSON-serialized width fits within `body_budget`.
        byte_limited = true;
        let mut prefix = truncate_utf8(slice[0], body_budget);
        while !prefix.is_empty() && serialized_string_len(&prefix).saturating_sub(2) > body_budget {
            let mut end = prefix.len().saturating_sub(1);
            while end > 0 && !prefix.is_char_boundary(end) {
                end -= 1;
            }
            prefix.truncate(end);
        }
        (prefix, line_start)
    } else {
        let joined = slice[..kept_count].join("\n");
        (
            joined,
            line_start.saturating_add(kept_count).saturating_sub(1),
        )
    };

    let truncated = byte_limited || line_limited || line_end < total_lines;
    let next_offset = (line_end < total_lines).then(|| line_end.saturating_add(1));
    let mut output = body;
    if truncated {
        let reason = if byte_limited {
            format!(" ({MAX_CODING_OUTPUT_BYTES} byte limit)")
        } else {
            String::new()
        };
        let continuation = match next_offset {
            Some(next) => format!(
                "[Showing lines {line_start}-{line_end} of {total_lines}{reason}. Use offset={next} to continue.]"
            ),
            None => format!("[Showing lines {line_start}-{line_end} of {total_lines}{reason}.]"),
        };
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str(&continuation);
    }

    Ok(cap_tool_output_with_policy(
        json!({
            "title": title,
            "output": output,
            "metadata": {
                "path": display_target,
                "lineStart": line_start,
                "lineEnd": line_end,
                "totalLines": total_lines,
                "truncated": truncated,
                "nextOffset": next_offset,
            },
        }),
        ToolResultPolicy::Coding,
    ))
}

fn split_lines_preserving_count(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if content.ends_with('\n') {
        lines.pop();
    }
    lines
}
