//! Terminal handoff generation over [`HostRuntime`] (ADR-0015).
//!
//! The terminal handoff is the only state an archived agent carries into its
//! next episode. Model-backed generation calls [`HostModelCaller::complete`];
//! any failure or absent model route falls back deterministically to a
//! projection-derived document flagged `degraded = true`.

use std::fmt::Write as _;

use hya_proto::{
    Message, MessageId, ModelRef, Part, PartId, PartProjection, Projection, Role, SessionId,
    UsagePurpose,
};
use hya_runtime_abi::{HostRuntime, ModelCallRequest};
use hya_tool::extension::{handoff_projection, team_projection};
use tokio_util::sync::CancellationToken;

/// Section structure of a terminal handoff document (ADR-0015).
pub const STATE_HANDOFF_TEMPLATE: &str = "\
Write a state handoff for an agent resuming this work in a fresh episode, \
under exactly these headings, keeping every heading even when its section is \
empty. Record only current state — never narrate how it came to be:

1. Goal - the task, in the parent's terms.
2. Current state - what exists and works right now.
3. Files and code - exact paths touched, and what changed or matters in each.
4. Decisions - choices in force, including what was ruled out.
5. Pending tasks - work explicitly requested and not yet done.
6. Next step - the single next action, or `none`.

Preserve exact file paths, identifiers, signatures, and command lines. Prefer \
terse bullets over paragraphs.";

/// Output token cap used when no explicit override is supplied.
pub const DEFAULT_HANDOFF_MAX_TOKENS: u32 = 4_096;

/// Settings for generation of terminal handoff documents.
#[derive(Clone, Debug)]
pub struct TerminalHandoffOptions {
    /// Optional model override (uses runtime default when `None`).
    pub model: Option<ModelRef>,
    /// Optional system prompt override.
    pub system: Option<String>,
    /// Maximum output tokens for model-backed handoff generation.
    pub max_output_tokens: u32,
}

impl Default for TerminalHandoffOptions {
    fn default() -> Self {
        Self {
            model: None,
            system: None,
            max_output_tokens: DEFAULT_HANDOFF_MAX_TOKENS,
        }
    }
}

/// Generated terminal handoff document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalHandoff {
    /// Monotonic episode generation anchoring on any prior handoff.
    pub generation: u32,
    /// Full six-section state document.
    pub doc: String,
    /// Whether deterministic projection fallback produced this document.
    pub degraded: bool,
}

/// Build the request message vector for model-backed terminal handoff generation:
/// the verbatim transcript plus one trailing prompt anchored on `previous_summary`.
#[must_use]
pub fn build_handoff_messages(
    messages: &[Message],
    previous_summary: Option<&str>,
) -> Vec<Message> {
    let mut prompt = String::new();
    if let Some(previous) = previous_summary.filter(|s| !s.trim().is_empty()) {
        let _ = write!(
            prompt,
            "<previous-summary>\n{previous}\n</previous-summary>\n\n"
        );
    }
    let _ = write!(prompt, "{STATE_HANDOFF_TEMPLATE}");
    let mut out = messages.to_vec();
    out.push(Message::User {
        id: MessageId::new(),
        parts: vec![Part::Text {
            id: PartId::new(),
            text: prompt,
        }],
    });
    out
}

/// Deterministic projection-derived fallback handoff document.
#[must_use]
pub fn degraded_handoff_doc(projection: Option<&Projection>, unread: &[String]) -> String {
    let texts: Vec<(Role, String)> = projection
        .map(|proj| {
            proj.session
                .messages
                .iter()
                .map(|message| {
                    let text = message
                        .parts
                        .iter()
                        .filter_map(|part| match part {
                            PartProjection::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<String>();
                    (message.role, text)
                })
                .filter(|(_, text)| !text.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();

    let carry = |number: u8| {
        let Some(previous) = projection.and_then(handoff_projection) else {
            return "unknown".to_string();
        };
        let prefix = format!("{number}. ");
        let next = format!("\n{}. ", number + 1);
        let Some(start) = previous.doc.find(&prefix) else {
            return "unknown".to_string();
        };
        let section = previous.doc[start..]
            .split(&next)
            .next()
            .unwrap_or_default();
        let content = section
            .split_once(" - ")
            .map_or(section, |(_, text)| text)
            .trim();
        format!("(from generation {}) {content}", previous.generation)
    };

    let files = carry(3);
    let decisions = carry(4);
    let goal = texts
        .iter()
        .find(|(role, _)| *role == Role::User)
        .map(|(_, text)| text.as_str())
        .or_else(|| unread.first().map(String::as_str))
        .unwrap_or("no work requested in this episode");
    let current = texts
        .iter()
        .rev()
        .find(|(role, _)| *role == Role::Assistant)
        .map_or("no work done in this episode", |(_, text)| text.as_str());
    let pending_owned;
    let pending = if unread.is_empty() {
        texts
            .iter()
            .rev()
            .find(|(role, _)| *role == Role::User)
            .map_or("none", |(_, text)| text.as_str())
    } else {
        pending_owned = unread.join("\n");
        pending_owned.as_str()
    };

    format!(
        "1. Goal - {goal}\n2. Current state - {current}\n3. Files and code - {files}\n4. Decisions - {decisions}\n5. Pending tasks - {pending}\n6. Next step - none\n"
    )
}

/// Convert projected messages into verbatim wire [`Message`]s for handoff calls.
fn projection_to_messages(projection: &Projection) -> Vec<Message> {
    projection
        .session
        .messages
        .iter()
        .filter_map(|msg| {
            let text: String = msg
                .parts
                .iter()
                .filter_map(|part| match part {
                    PartProjection::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if text.trim().is_empty() {
                return None;
            }
            let part = Part::Text {
                id: PartId::new(),
                text,
            };
            match msg.role {
                Role::User => Some(Message::User {
                    id: msg.id,
                    parts: vec![part],
                }),
                Role::Assistant => Some(Message::Assistant {
                    id: msg.id,
                    agent: msg
                        .agent
                        .clone()
                        .unwrap_or_else(|| hya_proto::AgentName::new("hya-task")),
                    model: msg
                        .model
                        .clone()
                        .unwrap_or_else(|| hya_proto::ModelRef::new("unknown/model")),
                    parts: vec![part],
                    last_step_start: None,
                    last_round: None,
                    finish: None,
                    tokens: None,
                }),
                Role::System => None,
            }
        })
        .collect()
}

/// Collect unread incoming mail for `session` from its team root projection.
async fn unread_handoff_mail(runtime: &dyn HostRuntime, session: SessionId) -> Vec<String> {
    let Ok(root) = crate::service::MultiAgentService::team_root(runtime, session).await else {
        return Vec::new();
    };
    let Ok(projection) = runtime.session_store().read_projection(root).await else {
        return Vec::new();
    };
    let team = team_projection(&projection);
    let Some(entry) = team.roster.values().find(|entry| entry.session == session) else {
        return Vec::new();
    };
    team.unread_incoming_mail(&entry.handle, entry.resident_cursor)
        .map(|mail| format!("[mail from {}] {}", mail.from, mail.body))
        .collect()
}

/// Generate a terminal handoff via [`HostModelCaller::complete`], falling back
/// deterministically to [`degraded_handoff_doc`] on any error.
pub async fn generate_terminal_handoff(
    runtime: &dyn HostRuntime,
    session: SessionId,
    options: TerminalHandoffOptions,
    cancel: CancellationToken,
) -> TerminalHandoff {
    let Ok(projection) = runtime.session_store().read_projection(session).await else {
        return TerminalHandoff {
            generation: 1,
            doc: degraded_handoff_doc(None, &[]),
            degraded: true,
        };
    };
    let previous = handoff_projection(&projection);
    let generation = previous
        .as_ref()
        .map_or(1, |handoff| handoff.generation.saturating_add(1));
    let previous_doc = previous.map(|handoff| handoff.doc);
    let transcript = projection_to_messages(&projection);
    let request = ModelCallRequest {
        model: options.model.or_else(|| projection.session.model.clone()),
        system: options.system,
        messages: build_handoff_messages(&transcript, previous_doc.as_deref()),
        tools: Vec::new(),
        temperature: Some(0.0),
        max_tokens: Some(options.max_output_tokens),
        reasoning_effort: None,
        json_schema: None,
        purpose: Some(UsagePurpose::Compaction),
    };
    if let Ok(reply) = runtime.model_caller().complete(request, cancel).await
        && !reply.text.trim().is_empty()
    {
        return TerminalHandoff {
            generation,
            doc: reply.text,
            degraded: false,
        };
    }
    let unread = unread_handoff_mail(runtime, session).await;
    TerminalHandoff {
        generation,
        doc: degraded_handoff_doc(Some(&projection), &unread),
        degraded: true,
    }
}

/// Deterministic degraded terminal handoff that never invokes a model route.
pub async fn degraded_terminal_handoff(
    runtime: &dyn HostRuntime,
    session: SessionId,
) -> TerminalHandoff {
    match runtime.session_store().read_projection(session).await {
        Ok(projection) => {
            let generation = handoff_projection(&projection)
                .map_or(1, |handoff| handoff.generation.saturating_add(1));
            let unread = unread_handoff_mail(runtime, session).await;
            TerminalHandoff {
                generation,
                doc: degraded_handoff_doc(Some(&projection), &unread),
                degraded: true,
            }
        }
        Err(_) => TerminalHandoff {
            generation: 1,
            doc: degraded_handoff_doc(None, &[]),
            degraded: true,
        },
    }
}
