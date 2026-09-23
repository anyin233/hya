//! Terminal handoff generation (ADR-0015).
//!
//! The terminal handoff is the only state an archived agent carries into its
//! next episode. It is written once, at report time, by the subagent's
//! configured summarizer over the verbatim transcript with the state-only
//! template ([`STATE_HANDOFF_TEMPLATE`](crate::compaction)); each generation
//! anchors on its predecessor. Generation never fails: a missing summarizer, a
//! failed call, or engine-synthesized terminality falls back to a deterministic
//! projection-derived document flagged `degraded`.

use hya_proto::{Projection, SessionId, UsagePurpose};

use crate::compaction::{SummarizeOptions, UsageCollector};

use super::SessionEngine;
use super::summary::summary_messages;

/// A generated terminal handoff.
#[derive(Clone, Debug)]
pub struct TerminalHandoff {
    /// Episode generation; anchors on the previous document.
    pub generation: u32,
    /// Full six-section state document.
    pub doc: String,
    /// A deterministic projection-derived fallback produced this document.
    pub degraded: bool,
}

impl SessionEngine {
    /// Generate the terminal handoff for `session`. See the module docs for
    /// the degradation contract.
    pub async fn terminal_handoff(&self, session: SessionId) -> TerminalHandoff {
        let Ok(projection) = self.read_projection(session).await else {
            return TerminalHandoff {
                generation: 1,
                doc: degraded_handoff_doc(None),
                degraded: true,
            };
        };
        let previous = projection.session.handoff.clone();
        let generation = previous
            .as_ref()
            .map_or(1, |handoff| handoff.generation.saturating_add(1));
        let previous_doc = previous.map(|handoff| handoff.doc);
        if let Some(summarizer) = self.summarizer.clone()
            && let Ok(messages) = summary_messages(&projection)
        {
            let usage = UsageCollector::default();
            let options = SummarizeOptions {
                handoff: true,
                state_only: true,
                previous_summary: previous_doc,
                max_output_tokens: Some(self.compaction.summary_max_tokens),
                usage: Some(usage.clone()),
                ..SummarizeOptions::default()
            };
            let written = summarizer.summarize(&messages, options).await;
            // The terminal handoff is a summarizer call billed to this session.
            self.record_side_call_usage(None, session, UsagePurpose::Compaction, &usage)
                .await;
            if let Ok(doc) = written {
                return TerminalHandoff {
                    generation,
                    doc,
                    degraded: false,
                };
            }
        }
        TerminalHandoff {
            generation,
            doc: degraded_handoff_doc(Some(&projection)),
            degraded: true,
        }
    }
}

/// Deterministic degraded handoff from the projection alone. Placeholders stay
/// explicit — a degraded document is honest about what it does not know.
fn degraded_handoff_doc(projection: Option<&Projection>) -> String {
    let mut doc = String::from("1. Goal - ");
    let mut current = String::from("(degraded handoff: unknown)");
    let mut pending = String::from("(degraded handoff: unknown)");
    if let Some(projection) = projection {
        for message in projection.session.messages.iter().rev() {
            let text = message
                .parts
                .iter()
                .filter_map(|part| match part {
                    hya_proto::PartProjection::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            if text.trim().is_empty() {
                continue;
            }
            if message.role == hya_proto::Role::Assistant && current.starts_with("(degraded") {
                current = text.chars().take(400).collect();
            } else if message.role == hya_proto::Role::User && pending.starts_with("(degraded") {
                pending = text.chars().take(200).collect();
            }
            if !current.starts_with("(degraded") && !pending.starts_with("(degraded") {
                break;
            }
        }
    }
    let _ = std::fmt::Write::write_fmt(
        &mut doc,
        format_args!("the task of this agent's current episode.\n2. Current state - {current}\n"),
    );
    let _ = std::fmt::Write::write_fmt(
        &mut doc,
        format_args!(
            "3. Files and code - (degraded handoff: unknown)\n4. Decisions - (degraded handoff: unknown)\n"
        ),
    );
    let _ = std::fmt::Write::write_fmt(
        &mut doc,
        format_args!("5. Pending tasks - {pending}\n6. Next step - none\n"),
    );
    doc
}

impl SessionEngine {
    /// Deterministic degraded handoff, bypassing the summarizer entirely
    /// (`archive`, drain, and budget-kill paths must never wait on a model call).
    pub async fn degraded_terminal_handoff(&self, session: SessionId) -> TerminalHandoff {
        match self.read_projection(session).await {
            Ok(projection) => {
                let generation = projection
                    .session
                    .handoff
                    .as_ref()
                    .map_or(1, |handoff| handoff.generation.saturating_add(1));
                TerminalHandoff {
                    generation,
                    doc: degraded_handoff_doc(Some(&projection)),
                    degraded: true,
                }
            }
            Err(_) => TerminalHandoff {
                generation: 1,
                doc: degraded_handoff_doc(None),
                degraded: true,
            },
        }
    }
}
