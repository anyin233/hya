//! Terminal handoff generation (ADR-0015).
//!
//! The terminal handoff is the only state an archived agent carries into its
//! next episode. It is written once, at report time, by the subagent's
//! configured summarizer over the ended episode transcript with the state-only
//! template ([`STATE_HANDOFF_TEMPLATE`](crate::compaction)); each generation
//! anchors on its predecessor. Generation never fails: a missing summarizer, a
//! failed call, or engine-synthesized terminality falls back to a deterministic
//! projection-derived document flagged `degraded`.

use hya_proto::{Projection, SessionId, UsagePurpose};

use crate::compaction::{SummarizeOptions, UsageCollector};

use super::SessionEngine;
use super::summary::{episode_messages, summary_messages};

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
    pub async fn terminal_handoff(
        &self,
        session: SessionId,
    ) -> Result<TerminalHandoff, crate::CoreError> {
        let _lease = self
            .begin_turn(session, &tokio_util::sync::CancellationToken::new())
            .await?
            .ok_or(crate::CoreError::Cancelled)?;
        Ok(self.terminal_handoff_claimed(session, None).await)
    }

    /// The caller holds the turn lease through handoff publication.
    pub(crate) async fn terminal_handoff_claimed(
        &self,
        session: SessionId,
        actor_claim: Option<&hya_proto::ActorClaim>,
    ) -> TerminalHandoff {
        let Ok(projection) = self.read_projection_shared(session).await else {
            return TerminalHandoff {
                generation: 1,
                doc: degraded_handoff_doc(None, &[]),
                degraded: true,
            };
        };
        let previous = projection.session.handoff.clone();
        let generation = previous
            .as_ref()
            .map_or(1, |handoff| handoff.generation.saturating_add(1));
        let previous_doc = previous.map(|handoff| handoff.doc);
        let degraded_doc = prepare_degraded_handoff_doc(Some(&projection));
        let summarizer = self.summarizer.clone();
        let messages = summarizer
            .as_ref()
            .and_then(|_| summary_messages(&projection).ok());
        drop(projection);
        if let Some(summarizer) = summarizer
            && let Some(messages) = messages
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
            self.record_side_call_usage(actor_claim, session, UsagePurpose::Compaction, &usage)
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
            doc: degraded_doc(&self.unread_handoff_mail(session).await),
            degraded: true,
        }
    }
}

/// The ended episode contributes goal/state/pending; only durable files and
/// decisions carry forward from the previous generation.
fn degraded_handoff_doc(projection: Option<&Projection>, unread: &[String]) -> String {
    prepare_degraded_handoff_doc(projection)(unread)
}

// Keep only fallback text while the model runs; unread mail is still read afterwards.
fn prepare_degraded_handoff_doc(
    projection: Option<&Projection>,
) -> impl FnOnce(&[String]) -> String + use<> {
    use hya_proto::{PartProjection, Role};
    let messages = projection.map(episode_messages).unwrap_or_default();
    let texts: Vec<_> = messages
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
        .collect();
    let carry = |number: u8| {
        let Some(previous) = projection.and_then(|projection| projection.session.handoff.as_ref())
        else {
            return "unknown".into();
        };
        let prefix = format!("{number}. ");
        let next = format!("\n{}. ", number + 1);
        let Some(start) = previous.doc.find(&prefix) else {
            return "unknown".into();
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
        .map(|(_, text)| text.clone());
    let current = texts
        .iter()
        .rev()
        .find(|(role, _)| *role == Role::Assistant)
        .map_or_else(
            || "no work done in this episode".into(),
            |(_, text)| text.clone(),
        );
    let pending = texts
        .iter()
        .rev()
        .find(|(role, _)| *role == Role::User)
        .map_or_else(|| "none".into(), |(_, text)| text.clone());
    move |unread| {
        let goal = goal
            .as_deref()
            .or_else(|| unread.first().map(String::as_str))
            .unwrap_or("no work requested in this episode");
        let pending = if unread.is_empty() {
            pending
        } else {
            unread.join("\n")
        };
        format!(
            "1. Goal - {goal}\n2. Current state - {current}\n3. Files and code - {}\n4. Decisions - {}\n5. Pending tasks - {pending}\n6. Next step - none\n",
            files, decisions
        )
    }
}

impl SessionEngine {
    async fn unread_handoff_mail(&self, session: SessionId) -> Vec<String> {
        let Ok(root) = self.team_root(session).await else {
            return Vec::new();
        };
        let Ok(projection) = self.read_projection_shared(root).await else {
            return Vec::new();
        };
        let Some(entry) = projection
            .team
            .roster
            .values()
            .find(|entry| entry.session == session)
        else {
            return Vec::new();
        };
        projection
            .team
            .unread_incoming_mail(&entry.handle, entry.resident_cursor)
            .map(|mail| format!("[mail from {}] {}", mail.from, mail.body))
            .collect()
    }

    /// Deterministic degraded handoff, bypassing the summarizer entirely
    /// (`archive`, drain, and budget-kill paths must never wait on a model call).
    pub async fn degraded_terminal_handoff(&self, session: SessionId) -> TerminalHandoff {
        match self.read_projection_shared(session).await {
            Ok(projection) => {
                let generation = projection
                    .session
                    .handoff
                    .as_ref()
                    .map_or(1, |handoff| handoff.generation.saturating_add(1));
                TerminalHandoff {
                    generation,
                    doc: degraded_handoff_doc(
                        Some(&projection),
                        &self.unread_handoff_mail(session).await,
                    ),
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
}
