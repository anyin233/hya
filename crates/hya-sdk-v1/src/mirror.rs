use std::collections::BTreeMap;

use hya_api::v1 as pb;

/// In-memory transcript mirror folded from curated stream frames.
///
/// Start empty and apply [`apply`](Self::apply) to every live frame; on
/// `resync`, re-read the projection and use [`from_messages`](Self::from_messages)
/// to seed it, or replay durable events while continuing to apply live frames.
/// Folding rules (the v1 stream contract):
/// - Parts are keyed by id: a `partStarted` for a known part id (the durable
///   record of a part first seen live) does not add a second part.
/// - Live `partAppended` frames (`seq == 0`) grow a part only until the
///   text is durable (seeded from a read, or a durable text/reasoning `partStarted` /
///   `partReplaced` was applied); after that they are stale and ignored.
/// - Tool argument deltas remain live after the durable tool `partStarted`;
///   `toolStateChanged` supplies the full input and supersedes those deltas.
/// - On `resync`, incomplete parts become live-lost: clear partial tool input
///   and ignore later live appends until a durable replacement or full input.
/// - A seeded pending tool with empty input is also live-lost: its prefix is
///   unknown, so fragments cannot reconstruct it.
/// - `partReplaced` sets the part's whole text, superseding live deltas.
/// - `partDeleted` removes the part with that id from its message.
/// - `sessionReverted` (a revert or its undo) hides or restores whole
///   messages: [`apply`](Self::apply) returns `true` (re-seed).
/// - Durable frames at or below [`last_seq`](Self::last_seq) are ignored.
///   Seed with the watermark the read reflects; otherwise durable deltas it
///   already contains can apply again.
/// - `memberLifecycleChanged` patches known members by child session, retaining
///   task outcome and clearing archival on revival. These events belong to the
///   team-root stream. A mirror follows one session's sequence: consumers viewing
///   a nested parent refresh its member snapshot on root lifecycle notifications
///   instead of feeding a second session's watermark into this mirror.
#[derive(Default)]
pub struct V1SessionMirror {
    messages: BTreeMap<String, pb::MessageInfo>,
    /// Part ids whose text is durable (seeded or confirmed by the log).
    durable_parts: std::collections::BTreeSet<String>,
    /// Parts whose live prefix was lost before their durable content arrived.
    live_lost_parts: std::collections::BTreeSet<String>,
    /// Spawned subagents by member id.
    members: BTreeMap<String, pb::MemberInfo>,
    /// Highest applied sequence number.
    pub last_seq: u64,
}

impl V1SessionMirror {
    /// Seed a transcript from `ListMessages` at the supplied durable watermark.
    /// Pending tool calls with unknown input wait for full input instead of
    /// displaying argument suffixes received after the read.
    #[must_use]
    pub fn from_messages(messages: Vec<pb::MessageInfo>, last_seq: u64) -> Self {
        let mut mirror = Self {
            last_seq,
            ..Self::default()
        };
        for message in messages {
            for part in &message.parts {
                if matches!(&part.kind, Some(pb::part_info::Kind::ToolCall(tool))
                    if tool.state == pb::ToolExecutionState::Pending as i32
                        && tool.input_json.is_empty())
                {
                    mirror.live_lost_parts.insert(part.id.clone());
                } else {
                    mirror.durable_parts.insert(part.id.clone());
                }
            }
            mirror.messages.insert(message.id.clone(), message);
        }
        mirror
    }

    /// Apply one live frame; returns `true` when a resync is required.
    pub fn apply(&mut self, frame: &pb::StreamFrame) -> bool {
        match &frame.frame {
            Some(pb::stream_frame::Frame::Resync(resync)) => {
                for message in self.messages.values_mut() {
                    for part in &mut message.parts {
                        if !self.durable_parts.contains(&part.id) {
                            self.live_lost_parts.insert(part.id.clone());
                            if let Some(pb::part_info::Kind::ToolCall(tool)) = &mut part.kind {
                                tool.input_json.clear();
                            }
                        }
                    }
                }
                self.last_seq = resync.last_seq;
                true
            }
            Some(pb::stream_frame::Frame::Event(event)) => {
                if event.seq != 0 {
                    if event.seq <= self.last_seq {
                        return false;
                    }
                    self.last_seq = event.seq;
                }
                // A revert (or its undo) hides or restores whole messages
                // the stream does not carry: re-read the transcript.
                if matches!(
                    event.payload,
                    Some(hya_api::v1::stream_event::Payload::SessionReverted(_))
                ) {
                    return true;
                }
                self.apply_event(event);
                false
            }
            None => false,
        }
    }

    fn apply_event(&mut self, event: &pb::StreamEvent) {
        use hya_api::v1::stream_event::Payload as P;
        let durable = event.seq != 0;
        match &event.payload {
            Some(P::MessageStarted(started)) => {
                let message = self.messages.entry(started.message.clone()).or_default();
                message.id.clone_from(&started.message);
                message.role = started.role;
                if message.session.is_empty() {
                    message.session.clone_from(&event.session);
                }
                if !started.agent.is_empty() {
                    message.agent.clone_from(&started.agent);
                }
                if !started.model.is_empty() {
                    message.model.clone_from(&started.model);
                }
                if message.time_created.is_none() {
                    message.time_created.clone_from(&event.time_recorded);
                }
                message.time_updated.clone_from(&event.time_recorded);
            }
            Some(P::MessageFinished(finished)) => {
                if let Some(message) = self.messages.get_mut(&finished.message) {
                    message.finish = finished.finish;
                    message.finish_cause = finished.cause;
                    message.time_updated.clone_from(&event.time_recorded);
                }
            }
            Some(P::PartStarted(started)) => {
                if durable && started.kind != "tool_call" {
                    self.durable_parts.insert(started.part.clone());
                }
                if let Some(message) = self.messages.get_mut(&started.message)
                    && !message.parts.iter().any(|part| part.id == started.part)
                {
                    let mut kind = empty_part_kind(&started.kind);
                    if let Some(pb::part_info::Kind::ToolCall(tool)) = kind.as_mut() {
                        tool.tool.clone_from(&started.tool);
                        tool.call_id.clone_from(&started.call_id);
                        tool.state = pb::ToolExecutionState::Pending as i32;
                    }
                    message.parts.push(pb::PartInfo {
                        id: started.part.clone(),
                        kind,
                    });
                }
            }
            Some(P::PartAppended(appended)) => {
                if !durable
                    && (self.durable_parts.contains(&appended.part)
                        || self.live_lost_parts.contains(&appended.part))
                {
                    return;
                }
                if let Some(text) = self.part_text_mut(&appended.message, &appended.part) {
                    text.push_str(&appended.text_delta);
                }
            }
            Some(P::PartReplaced(replaced)) => {
                if durable {
                    self.durable_parts.insert(replaced.part.clone());
                    self.live_lost_parts.remove(&replaced.part);
                }
                if let Some(text) = self.part_text_mut(&replaced.message, &replaced.part) {
                    text.clone_from(&replaced.text);
                }
            }
            Some(P::PartsAdded(added)) => {
                if let Some(message) = self.messages.get_mut(&added.message) {
                    for part in &added.parts {
                        if !message.parts.iter().any(|known| known.id == part.id) {
                            message.parts.push(part.clone());
                        }
                    }
                }
            }
            Some(P::PartDeleted(deleted)) => {
                if let Some(message) = self.messages.get_mut(&deleted.message) {
                    message.parts.retain(|part| part.id != deleted.part);
                }
                self.durable_parts.remove(&deleted.part);
                self.live_lost_parts.remove(&deleted.part);
            }
            Some(P::ToolStateChanged(changed)) => {
                if durable && !changed.input_json.is_empty() {
                    self.durable_parts.insert(changed.part.clone());
                    self.live_lost_parts.remove(&changed.part);
                }
                if let Some(tool) = self.tool_part_mut(&changed.message, &changed.part) {
                    fold_tool_state(tool, changed);
                }
            }
            Some(P::MemberUpdated(update)) => {
                let member = self.members.entry(update.member.clone()).or_default();
                fold_member(member, update);
            }
            Some(P::MemberLifecycleChanged(update)) => {
                for member in self
                    .members
                    .values_mut()
                    .filter(|member| member.child == update.child)
                {
                    member.handle.clone_from(&update.handle);
                    member.archived = Some(update.archived);
                    member.archive_reason.clone_from(&update.archive_reason);
                }
            }
            Some(P::ErrorReported(reported)) => {
                if let Some(message) = self.messages.get_mut(&reported.message) {
                    message.error = Some(pb::MessageError {
                        code: reported.code.clone(),
                        message: reported.error_message.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    /// The text buffer of a text/reasoning part or streaming tool input.
    fn part_text_mut(&mut self, message: &str, part: &str) -> Option<&mut String> {
        let part = self
            .messages
            .get_mut(message)?
            .parts
            .iter_mut()
            .find(|candidate| candidate.id == part)?;
        match part.kind.as_mut()? {
            pb::part_info::Kind::Text(text) => Some(&mut text.text),
            pb::part_info::Kind::Reasoning(reasoning) => Some(&mut reasoning.text),
            // Argument JSON fragments accumulate until the parsed input
            // (`toolStateChanged` RUNNING) replaces them.
            pb::part_info::Kind::ToolCall(tool) => Some(&mut tool.input_json),
            _ => None,
        }
    }

    /// The tool body of a tool-call part.
    fn tool_part_mut(&mut self, message: &str, part: &str) -> Option<&mut pb::ToolCallPart> {
        let part = self
            .messages
            .get_mut(message)?
            .parts
            .iter_mut()
            .find(|candidate| candidate.id == part)?;
        match part.kind.as_mut()? {
            pb::part_info::Kind::ToolCall(tool) => Some(tool),
            _ => None,
        }
    }

    /// Subagents spawned by the mirrored session, by member id, folded from
    /// `memberUpdated` frames (seed from `SessionInfo.members`).
    #[must_use]
    pub fn members(&self) -> Vec<&pb::MemberInfo> {
        self.members.values().collect()
    }

    /// Look up the current projection of a message touched by a stream update.
    #[must_use]
    pub fn message(&self, id: &str) -> Option<&pb::MessageInfo> {
        self.messages.get(id)
    }

    /// The mirrored transcript in append order.
    #[must_use]
    pub fn messages(&self) -> Vec<&pb::MessageInfo> {
        self.messages.values().collect()
    }
}

/// Apply a `toolStateChanged` frame: the state always, every other field
/// only when the frame carries it (a frame never clears known data).
fn fold_tool_state(tool: &mut pb::ToolCallPart, changed: &pb::ToolStateChanged) {
    tool.state = changed.state;
    for (target, value) in [
        (&mut tool.call_id, &changed.call_id),
        (&mut tool.tool, &changed.tool),
        (&mut tool.input_json, &changed.input_json),
        (&mut tool.output_json, &changed.output_json),
        (&mut tool.error_code, &changed.error_code),
        (&mut tool.error_message, &changed.error_message),
    ] {
        if !value.is_empty() {
            target.clone_from(value);
        }
    }
    if changed.duration_ms != 0 {
        tool.duration_ms = changed.duration_ms;
    }
}

/// Merge a `memberUpdated` frame into a member row: later frames carry only
/// what changed.
fn fold_member(member: &mut pb::MemberInfo, update: &pb::MemberInfo) {
    member.member.clone_from(&update.member);
    for (target, value) in [
        (&mut member.child, &update.child),
        (&mut member.agent, &update.agent),
        (&mut member.description, &update.description),
        (&mut member.summary, &update.summary),
        (&mut member.call_id, &update.call_id),
        (&mut member.handle, &update.handle),
    ] {
        if !value.is_empty() {
            target.clone_from(value);
        }
    }
    if update.status != 0 {
        member.status = update.status;
    }
    if update.depth != 0 {
        member.depth = update.depth;
    }
    if let Some(archived) = update.archived {
        member.archived = Some(archived);
        member.archive_reason.clone_from(&update.archive_reason);
    }
}

/// An empty part body for a `partStarted.kind` discriminator.
fn empty_part_kind(kind: &str) -> Option<pb::part_info::Kind> {
    match kind {
        "text" => Some(pb::part_info::Kind::Text(pb::TextPart::default())),
        "reasoning" => Some(pb::part_info::Kind::Reasoning(pb::ReasoningPart::default())),
        "tool_call" => Some(pb::part_info::Kind::ToolCall(pb::ToolCallPart::default())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use hya_api::v1::stream_event::Payload as P;

    fn frame(seq: u64, payload: P) -> pb::StreamFrame {
        pb::StreamFrame {
            frame: Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
                seq,
                session: "s".into(),
                time_recorded: None,
                payload: Some(payload),
            })),
        }
    }

    fn started(part: &str) -> P {
        P::PartStarted(pb::PartStarted {
            message: "m".into(),
            part: part.into(),
            kind: "text".into(),
            ..Default::default()
        })
    }

    fn appended(part: &str, delta: &str) -> P {
        P::PartAppended(pb::PartAppended {
            message: "m".into(),
            part: part.into(),
            text_delta: delta.into(),
        })
    }

    fn text_of(mirror: &V1SessionMirror) -> Vec<(String, String)> {
        mirror.messages()[0]
            .parts
            .iter()
            .map(|part| {
                let text = match part.kind.as_ref() {
                    Some(pb::part_info::Kind::Text(text)) => text.text.clone(),
                    _ => String::new(),
                };
                (part.id.clone(), text)
            })
            .collect()
    }

    /// A revert or its undo changes which messages exist: the mirror asks
    /// for a resync (re-read `ListMessages`) instead of guessing.
    #[test]
    fn a_session_revert_requires_a_resync() {
        let mut mirror = V1SessionMirror::default();
        let reverted = |undone| {
            P::SessionReverted(pb::SessionReverted {
                message_id: if undone { String::new() } else { "m".into() },
                undone,
                files: Vec::new(),
            })
        };
        assert!(mirror.apply(&frame(4, reverted(false))));
        assert_eq!(mirror.last_seq, 4);
        assert!(mirror.apply(&frame(5, reverted(true))));
        // Replayed durable frames stay no-ops.
        assert!(!mirror.apply(&frame(5, reverted(true))));
    }

    /// `partsAdded` appends a prompt's attachment parts once.
    #[test]
    fn parts_added_append_attachments_once() {
        let mut mirror = V1SessionMirror::default();
        mirror.apply(&frame(
            1,
            P::MessageStarted(pb::MessageStarted {
                message: "m".into(),
                role: pb::Role::User as i32,
                ..Default::default()
            }),
        ));
        let added = || {
            P::PartsAdded(pb::PartsAdded {
                message: "m".into(),
                parts: vec![pb::PartInfo {
                    id: "a".into(),
                    kind: Some(pb::part_info::Kind::Attachment(pb::AttachmentPart {
                        name: "pixel.png".into(),
                        mime: "image/png".into(),
                        size: 4,
                        ..Default::default()
                    })),
                }],
            })
        };
        mirror.apply(&frame(2, added()));
        mirror.apply(&frame(0, added()));
        let parts = &mirror.messages()[0].parts;
        assert_eq!(parts.len(), 1);
        assert!(matches!(
            parts[0].kind.as_ref(),
            Some(pb::part_info::Kind::Attachment(attachment)) if attachment.name == "pixel.png"
        ));
    }

    /// Live deltas build the part; the durable start for the same id does
    /// not add a second part.
    #[test]
    fn live_then_durable_part_started_folds_into_one_part() {
        let mut mirror = V1SessionMirror::default();
        mirror.apply(&frame(
            3,
            P::MessageStarted(pb::MessageStarted {
                message: "m".into(),
                role: pb::Role::Assistant as i32,
                ..Default::default()
            }),
        ));
        mirror.apply(&frame(0, started("p")));
        mirror.apply(&frame(0, appended("p", "Hel")));
        mirror.apply(&frame(0, appended("p", "lo")));
        assert_eq!(text_of(&mirror), vec![("p".into(), "Hello".into())]);
        mirror.apply(&frame(5, started("p")));
        assert_eq!(text_of(&mirror), vec![("p".into(), "Hello".into())]);
        let message = mirror.messages()[0];
        assert_eq!(message.id, "m");
        assert_eq!(message.role, pb::Role::Assistant as i32);
        // The durable full text replaces (never appends to) the live text;
        // a late live delta for the now-durable part is stale.
        mirror.apply(&frame(
            6,
            P::PartReplaced(pb::PartReplaced {
                message: "m".into(),
                part: "p".into(),
                text: "Hello, world".into(),
            }),
        ));
        mirror.apply(&frame(0, appended("p", "lo")));
        // An already-applied durable frame is ignored.
        mirror.apply(&frame(6, appended("p", "!!")));
        assert_eq!(text_of(&mirror), vec![("p".into(), "Hello, world".into())]);
        mirror.apply(&frame(
            7,
            P::ErrorReported(pb::ErrorReported {
                message: "m".into(),
                code: "provider_error".into(),
                error_message: "http status 400: nope".into(),
            }),
        ));
        mirror.apply(&frame(
            8,
            P::MessageFinished(pb::MessageFinished {
                message: "m".into(),
                finish: pb::FinishReason::Error as i32,
                usage: None,
                cause: pb::FinishCause::ProviderError as i32,
            }),
        ));
        let message = mirror.messages()[0];
        assert_eq!(message.finish, pb::FinishReason::Error as i32);
        assert_eq!(message.finish_cause, pb::FinishCause::ProviderError as i32);
        assert_eq!(
            message.error.as_ref().map(|error| error.message.as_str()),
            Some("http status 400: nope")
        );
        assert_eq!(mirror.last_seq, 8);
    }

    /// A deleted part leaves the message; its later live deltas add nothing.
    #[test]
    fn part_deleted_removes_the_part() {
        let mut mirror = V1SessionMirror::default();
        mirror.apply(&frame(
            1,
            P::MessageStarted(pb::MessageStarted {
                message: "m".into(),
                role: pb::Role::Assistant as i32,
                ..Default::default()
            }),
        ));
        mirror.apply(&frame(2, started("cut")));
        mirror.apply(&frame(3, started("kept")));
        mirror.apply(&frame(
            4,
            P::PartDeleted(pb::PartDeleted {
                message: "m".into(),
                part: "cut".into(),
            }),
        ));
        mirror.apply(&frame(0, appended("cut", "stale")));
        assert_eq!(text_of(&mirror), vec![("kept".into(), String::new())]);
    }

    /// A durable part is final: stale live deltas for it do not double it.
    #[test]
    fn live_deltas_for_a_durable_part_are_ignored() {
        let mut mirror = V1SessionMirror::default();
        mirror.apply(&frame(
            1,
            P::MessageStarted(pb::MessageStarted {
                message: "m".into(),
                role: pb::Role::Assistant as i32,
                ..Default::default()
            }),
        ));
        mirror.apply(&frame(2, started("p")));
        mirror.apply(&frame(
            3,
            P::PartReplaced(pb::PartReplaced {
                message: "m".into(),
                part: "p".into(),
                text: "Hello".into(),
            }),
        ));
        mirror.apply(&frame(0, appended("p", " stale")));
        assert_eq!(text_of(&mirror), vec![("p".into(), "Hello".into())]);
    }

    /// `messageStarted` carries the turn's own agent/model and the frame
    /// time seeds the message times; the finish moves the update time.
    #[test]
    fn message_started_attribution_and_times_fold_into_the_mirror() {
        // The well-known Timestamp type lives in a crate this one does not
        // name; reach it through the generated field.
        let at = |seconds: i64| {
            let mut recorded = pb::StreamEvent::default().time_recorded.unwrap_or_default();
            recorded.seconds = seconds;
            Some(recorded)
        };
        let mut mirror = V1SessionMirror::default();
        mirror.apply(&pb::StreamFrame {
            frame: Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
                seq: 1,
                session: "s".into(),
                time_recorded: at(10),
                payload: Some(P::MessageStarted(pb::MessageStarted {
                    message: "m".into(),
                    role: pb::Role::Assistant as i32,
                    agent: "hya-plan".into(),
                    model: "fake/beta".into(),
                })),
            })),
        });
        mirror.apply(&pb::StreamFrame {
            frame: Some(pb::stream_frame::Frame::Event(pb::StreamEvent {
                seq: 2,
                session: "s".into(),
                time_recorded: at(12),
                payload: Some(P::MessageFinished(pb::MessageFinished {
                    message: "m".into(),
                    finish: pb::FinishReason::Stop as i32,
                    usage: None,
                    cause: 0,
                })),
            })),
        });
        let message = mirror.messages()[0];
        assert_eq!(
            (message.agent.as_str(), message.model.as_str()),
            ("hya-plan", "fake/beta")
        );
        assert_eq!(message.time_created, at(10));
        assert_eq!(message.time_updated, at(12));
    }

    #[test]
    fn resync_discards_tool_argument_fragments_until_full_input() {
        tool_argument_resync(false);
    }

    #[test]
    fn resync_then_cancel_keeps_tool_input_empty() {
        tool_argument_resync(true);
    }

    fn tool_argument_resync(cancel: bool) {
        let mut mirror = V1SessionMirror::default();
        mirror.apply(&frame(
            1,
            P::MessageStarted(pb::MessageStarted {
                message: "m".into(),
                role: pb::Role::Assistant as i32,
                ..Default::default()
            }),
        ));
        mirror.apply(&frame(
            2,
            P::PartStarted(pb::PartStarted {
                message: "m".into(),
                part: "t".into(),
                kind: "tool_call".into(),
                tool: "read".into(),
                call_id: "c".into(),
            }),
        ));
        mirror.apply(&frame(0, appended("t", r#"{"path":"pre"#)));
        assert!(mirror.apply(&pb::StreamFrame {
            frame: Some(pb::stream_frame::Frame::Resync(pb::ResyncFrame {
                last_seq: 2,
            })),
        }));
        mirror.apply(&frame(0, appended("t", r#"fix"}"#)));
        if cancel {
            mirror.apply(&frame(
                3,
                P::ToolStateChanged(pb::ToolStateChanged {
                    message: "m".into(),
                    part: "t".into(),
                    state: pb::ToolExecutionState::Error as i32,
                    error_code: "cancelled".into(),
                    error_message: "cancelled".into(),
                    ..Default::default()
                }),
            ));
            mirror.apply(&frame(
                4,
                P::MessageFinished(pb::MessageFinished {
                    message: "m".into(),
                    finish: pb::FinishReason::Cancelled as i32,
                    ..Default::default()
                }),
            ));
        }
        let input = |mirror: &V1SessionMirror| match &mirror.messages()[0].parts[0].kind {
            Some(pb::part_info::Kind::ToolCall(tool)) => tool.input_json.clone(),
            _ => panic!("tool part"),
        };
        assert_eq!(input(&mirror), "");
        if !cancel {
            mirror.apply(&frame(
                3,
                P::ToolStateChanged(pb::ToolStateChanged {
                    message: "m".into(),
                    part: "t".into(),
                    state: pb::ToolExecutionState::Running as i32,
                    input_json: r#"{"path":"prefix"}"#.into(),
                    ..Default::default()
                }),
            ));
            mirror.apply(&frame(0, appended("t", "stale")));
            assert_eq!(input(&mirror), r#"{"path":"prefix"}"#);
        }
    }

    #[test]
    fn seeded_pending_tool_ignores_argument_suffix_until_full_input() {
        let mut mirror = V1SessionMirror::from_messages(
            vec![pb::MessageInfo {
                id: "m".into(),
                parts: vec![pb::PartInfo {
                    id: "t".into(),
                    kind: Some(pb::part_info::Kind::ToolCall(pb::ToolCallPart {
                        tool: "read".into(),
                        call_id: "c".into(),
                        state: pb::ToolExecutionState::Pending as i32,
                        ..Default::default()
                    })),
                }],
                ..Default::default()
            }],
            2,
        );
        mirror.apply(&frame(0, appended("t", r#"suffix"}"#)));
        let tool = |mirror: &V1SessionMirror| match &mirror.messages()[0].parts[0].kind {
            Some(pb::part_info::Kind::ToolCall(tool)) => tool.clone(),
            _ => panic!("tool part"),
        };
        assert_eq!(tool(&mirror).input_json, "");
        mirror.apply(&frame(
            3,
            P::ToolStateChanged(pb::ToolStateChanged {
                message: "m".into(),
                part: "t".into(),
                state: pb::ToolExecutionState::Running as i32,
                input_json: r#"{"path":"prefixsuffix"}"#.into(),
                ..Default::default()
            }),
        ));
        mirror.apply(&frame(0, appended("t", "stale")));
        assert_eq!(tool(&mirror).input_json, r#"{"path":"prefixsuffix"}"#);
        assert_eq!(tool(&mirror).call_id, "c");
    }

    #[test]
    fn tool_full_input_replaces_missed_fragments_and_ignores_late_deltas() {
        for missed_prefix in [false, true] {
            let mut mirror = V1SessionMirror::default();
            mirror.apply(&frame(
                1,
                P::MessageStarted(pb::MessageStarted {
                    message: "m".into(),
                    role: pb::Role::Assistant as i32,
                    ..Default::default()
                }),
            ));
            mirror.apply(&frame(
                2,
                P::PartStarted(pb::PartStarted {
                    message: "m".into(),
                    part: "t".into(),
                    kind: "tool_call".into(),
                    tool: "read".into(),
                    call_id: "c".into(),
                }),
            ));
            if !missed_prefix {
                mirror.apply(&frame(0, appended("t", "{\"path\":")));
            }
            mirror.apply(&frame(0, appended("t", "\"file\"}")));
            mirror.apply(&frame(
                3,
                P::ToolStateChanged(pb::ToolStateChanged {
                    message: "m".into(),
                    part: "t".into(),
                    state: pb::ToolExecutionState::Running as i32,
                    input_json: "{\"path\":\"file\"}".into(),
                    ..Default::default()
                }),
            ));
            mirror.apply(&frame(0, appended("t", "stale")));
            let Some(pb::part_info::Kind::ToolCall(tool)) = &mirror.messages()[0].parts[0].kind
            else {
                panic!("tool part");
            };
            assert_eq!(tool.input_json, "{\"path\":\"file\"}");
        }
    }

    /// A tool call streams into one part: start (tool, call id), argument
    /// fragments, the parsed input, then output and duration; members fold
    /// by id with later frames carrying only what changed.
    #[test]
    fn tool_call_frames_and_member_updates_fold_into_the_mirror() {
        let mut mirror = V1SessionMirror::default();
        mirror.apply(&frame(
            1,
            P::MessageStarted(pb::MessageStarted {
                message: "m".into(),
                role: pb::Role::Assistant as i32,
                ..Default::default()
            }),
        ));
        mirror.apply(&frame(
            2,
            P::PartStarted(pb::PartStarted {
                message: "m".into(),
                part: "t".into(),
                kind: "tool_call".into(),
                tool: "bash".into(),
                call_id: "c".into(),
            }),
        ));
        mirror.apply(&frame(
            0,
            P::PartAppended(pb::PartAppended {
                message: "m".into(),
                part: "t".into(),
                text_delta: "{\"command\":".into(),
            }),
        ));
        mirror.apply(&frame(
            0,
            P::PartAppended(pb::PartAppended {
                message: "m".into(),
                part: "t".into(),
                text_delta: "\"ls\"}".into(),
            }),
        ));
        let tool = |mirror: &V1SessionMirror| match mirror.messages()[0].parts[0].kind.clone() {
            Some(pb::part_info::Kind::ToolCall(tool)) => tool,
            other => panic!("expected a tool part, got {other:?}"),
        };
        let streaming = tool(&mirror);
        assert_eq!(
            (streaming.tool.as_str(), streaming.call_id.as_str()),
            ("bash", "c")
        );
        assert_eq!(streaming.input_json, "{\"command\":\"ls\"}");
        assert_eq!(streaming.state, pb::ToolExecutionState::Pending as i32);
        mirror.apply(&frame(
            5,
            P::ToolStateChanged(pb::ToolStateChanged {
                message: "m".into(),
                part: "t".into(),
                call_id: "c".into(),
                state: pb::ToolExecutionState::Running as i32,
                input_json: "{\"command\":\"ls\"}".into(),
                tool: "bash".into(),
                ..Default::default()
            }),
        ));
        mirror.apply(&frame(
            6,
            P::ToolStateChanged(pb::ToolStateChanged {
                message: "m".into(),
                part: "t".into(),
                call_id: "c".into(),
                state: pb::ToolExecutionState::Ok as i32,
                output_json: "\"a b\"".into(),
                duration_ms: 12,
                ..Default::default()
            }),
        ));
        let done = tool(&mirror);
        assert_eq!(done.state, pb::ToolExecutionState::Ok as i32);
        assert_eq!(done.input_json, "{\"command\":\"ls\"}");
        assert_eq!(done.output_json, "\"a b\"");
        assert_eq!(done.duration_ms, 12);

        mirror.apply(&frame(
            7,
            P::MemberUpdated(pb::MemberInfo {
                member: "mem".into(),
                child: "child".into(),
                agent: "hya-task".into(),
                description: "look".into(),
                status: pb::MemberStatus::Spawning as i32,
                call_id: "c".into(),
                depth: 1,
                ..Default::default()
            }),
        ));
        mirror.apply(&frame(
            8,
            P::MemberUpdated(pb::MemberInfo {
                member: "mem".into(),
                status: pb::MemberStatus::Done as i32,
                summary: "ok".into(),
                ..Default::default()
            }),
        ));
        let members = mirror.members();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].status, pb::MemberStatus::Done as i32);
        assert_eq!(
            (
                members[0].agent.as_str(),
                members[0].call_id.as_str(),
                members[0].summary.as_str()
            ),
            ("hya-task", "c", "ok")
        );
        for (seq, archived, reason) in [(9, true, "reported"), (10, false, "")] {
            mirror.apply(&frame(
                seq,
                P::MemberLifecycleChanged(pb::MemberLifecycleChanged {
                    child: "child".into(),
                    handle: "main/helper".into(),
                    archived,
                    archive_reason: reason.into(),
                }),
            ));
            let members = mirror.members();
            assert_eq!(members.len(), 1);
            assert_eq!(members[0].member, "mem");
            assert_eq!(members[0].handle, "main/helper");
            assert_eq!(members[0].archived, Some(archived));
            assert_eq!(members[0].archive_reason, reason);
            assert_eq!(members[0].status, pb::MemberStatus::Done as i32);
            assert_eq!(members[0].summary, "ok");
        }
        mirror.apply(&frame(
            11,
            P::MemberLifecycleChanged(pb::MemberLifecycleChanged {
                child: "unrelated".into(),
                archived: true,
                ..Default::default()
            }),
        ));
        assert_eq!(mirror.members().len(), 1);
    }
}
