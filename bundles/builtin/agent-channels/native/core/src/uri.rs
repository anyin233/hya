//! Native ABI v2 [`UriSchemaHandler`] for `channel://<id>[?last=N]`.
//!
//! Registered into `HandleRouter` so BOTH standard `hya-bare` `read` AND
//! `hya/hashline` `read` resolve `channel://` handles uniformly with zero
//! hardcoded `ChannelUriHandler` in `hya-engine`.

use std::sync::Arc;

use async_trait::async_trait;
use hya_runtime_abi::{HostError, UriContent, UriResolveCtx, UriSchemaHandler};
use serde_json::json;

use crate::service::AgentChannelsService;

/// ABI v2 [`UriSchemaHandler`] resolving `channel://<id>[?last=N]` through
/// [`AgentChannelsService`] / [`hya_runtime_abi::HostRuntime`].
pub struct ChannelUriSchemaHandler {
    service: Arc<AgentChannelsService>,
}

impl ChannelUriSchemaHandler {
    /// Construct a `channel://` URI handler backed by `service`.
    #[must_use]
    pub fn new(service: Arc<AgentChannelsService>) -> Self {
        Self { service }
    }
}

/// Parse `channel://<id>[?last=N]` into `(channel_id, Option<last>)`.
///
/// # Errors
/// Returns [`HostError::InvalidInput`] if the URI does not carry the `channel://`
/// scheme prefix or has an empty channel id.
pub fn parse_channel_uri(uri: &str) -> Result<(String, Option<usize>), HostError> {
    let rest = uri
        .strip_prefix("channel://")
        .ok_or_else(|| HostError::InvalidInput(format!("invalid channel URI `{uri}`")))?;
    let (id, query) = match rest.split_once('?') {
        Some((id, query)) => (id, Some(query)),
        None => (rest, None),
    };
    let last = query.and_then(|text| {
        text.split('&')
            .find_map(|pair| pair.strip_prefix("last="))
            .and_then(|v| v.parse::<usize>().ok())
    });
    let id = id.trim().to_string();
    if id.is_empty() {
        return Err(HostError::InvalidInput(
            "channel:// URI requires a non-empty channel id".to_string(),
        ));
    }
    Ok((id, last))
}

#[async_trait]
impl UriSchemaHandler for ChannelUriSchemaHandler {
    fn scheme(&self) -> &str {
        "channel"
    }

    async fn read(&self, ctx: UriResolveCtx<'_>, uri: &str) -> Result<UriContent, HostError> {
        let (channel, last) = parse_channel_uri(uri)?;
        let session = ctx.session.ok_or_else(|| {
            HostError::Unavailable(
                "channel:// reads are only available inside a running team".to_string(),
            )
        })?;
        let owned_runtime;
        let runtime = match ctx.runtime {
            Some(rt) => rt,
            None => {
                owned_runtime = self.service.runtime().map_err(|_| {
                    HostError::Unavailable(
                        "channel:// reads are only available inside a running team".to_string(),
                    )
                })?;
                owned_runtime.as_ref()
            }
        };
        let read = AgentChannelsService::read_channel_history(
            runtime,
            ctx.actor_claim,
            session,
            &channel,
            last,
        )
        .await?;
        let count = read.messages.len();
        let rendered: Vec<String> = read
            .messages
            .iter()
            .map(|(from, body)| format!("[{from}] {body}"))
            .collect();
        let body = if rendered.is_empty() {
            format!("#{} has no messages yet.", read.channel)
        } else {
            rendered.join("\n")
        };
        let output = match read.warning.as_deref() {
            Some(warning) => format!("[warning] {warning}\n{body}"),
            None => body,
        };
        let tail = if read.unread_remaining > 0 {
            format!(
                "\n\n{} unread message(s) remain in your inbox — run `list_channel` \
                 for per-channel counts.",
                read.unread_remaining
            )
        } else {
            String::new()
        };
        Ok(UriContent {
            body: format!("{output}{tail}"),
            media_type: "text/plain".to_string(),
            metadata: Some(json!({
                "type": "channel",
                "channel": read.channel,
                "count": count,
                "unreadRemaining": read.unread_remaining,
                "warning": read.warning,
            })),
        })
    }
}
