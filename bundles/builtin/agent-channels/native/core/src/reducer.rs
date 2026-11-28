//! Durable `Event::Extension` projection reducer for `hya/agent-channels`.

use hya_proto::{MailEndpoint, Projection};
use hya_runtime_abi::{ExtensionEventContext, HostError, ProjectionReducer};
use hya_tool::extension::{BUNDLE_CHANNELS, ChannelsExtensionState, ExtensionEvent, MailMessage};
use serde_json::Value;

/// Decode folded `hya/agent-channels` extension state from a [`Projection`].
#[must_use]
pub fn decode_channels_projection(projection: &Projection) -> ChannelsExtensionState {
    projection
        .extensions
        .get(BUNDLE_CHANNELS)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

/// ABI v2 [`ProjectionReducer`] folding `hya/agent-channels` extension events
/// (`channel_created`, `channel_joined`, `mail_sent`, `mail_consumed`) into
/// `projection.extensions["hya/agent-channels"]`.
#[derive(Debug, Default)]
pub struct AgentChannelsProjectionReducer;

impl ProjectionReducer for AgentChannelsProjectionReducer {
    fn bundle_id(&self) -> &str {
        BUNDLE_CHANNELS
    }

    fn initial_state(&self) -> Value {
        serde_json::to_value(ChannelsExtensionState::default()).unwrap_or(Value::Null)
    }

    fn apply(&self, state: &mut Value, event: &ExtensionEventContext) -> Result<(), HostError> {
        let Some(ext) =
            ExtensionEvent::from_parts(event.session, BUNDLE_CHANNELS, &event.kind, &event.payload)
        else {
            return Ok(());
        };
        let mut channels: ChannelsExtensionState = if state.is_null() {
            ChannelsExtensionState::default()
        } else {
            serde_json::from_value(state.clone()).unwrap_or_default()
        };

        match ext {
            ExtensionEvent::ChannelJoined { .. } => {}
            ExtensionEvent::ChannelCreated {
                channel,
                kind,
                unit,
                members,
                ..
            } => {
                let channel_state = channels.channels.entry(channel).or_default();
                channel_state.kind = kind;
                if unit.is_some() {
                    channel_state.unit = unit;
                }
                channel_state.members.extend(members);
            }
            ExtensionEvent::MailSent {
                from,
                to,
                kind,
                body,
                ..
            } => {
                let message = MailMessage {
                    from,
                    to: to.clone(),
                    kind,
                    body,
                };
                let index = channels.mail.len();
                channels.mail.push(message);
                match to {
                    MailEndpoint::Handle(handle) => {
                        channels.inboxes.entry(handle).or_default().push(index);
                    }
                    MailEndpoint::Channel(channel) => {
                        let channel_state = channels.channels.entry(channel).or_default();
                        channel_state.log.push(index);
                        let members: Vec<String> = channel_state.members.iter().cloned().collect();
                        for member in members {
                            channels.inboxes.entry(member).or_default().push(index);
                        }
                    }
                }
            }
            ExtensionEvent::MailConsumed {
                handle, through, ..
            } => {
                let cursor = channels.cursors.entry(handle).or_insert(0);
                *cursor = (*cursor).max(through);
            }
            _ => {}
        }

        *state = serde_json::to_value(&channels)
            .map_err(|error| HostError::Internal(format!("serialize channels state: {error}")))?;
        Ok(())
    }
}
