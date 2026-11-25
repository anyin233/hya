use super::{ChannelProjection, MessageError, Projection, RosterEntry, TeamProjection};
use crate::event::{Envelope, Event};
use crate::ids::{ActorEpoch, MemberId, MessageId, PartId, SessionId, ToolCallId};
use crate::mail::{ChannelKind, MailEndpoint, MailKind};
use crate::message::{
    FinishCause, FinishReason, MemberRunStatus, Role, RosterStatus, SubagentMode, TokenUsage,
};
use crate::model::{AgentName, ModelRef};
use crate::scope;

mod member_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::ids::EventSeq;

    fn env(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event,
        }
    }

    #[test]
    fn folds_member_lifecycle_into_projection() {
        let parent = SessionId::new();
        let child = SessionId::new();
        let member = MemberId::new();
        let mut p = Projection::default();
        p.apply(&env(
            1,
            Event::MemberSpawned {
                session: parent,
                member,
                child: Some(child),
                subagent_type: AgentName::new("explore"),
                description: "scan routing".to_string(),
                depth: 1,
                directive: "Scan the routing layer and report every handler".to_string(),
                tool_call: Some(ToolCallId::new()),
            },
        ));
        p.apply(&env(
            2,
            Event::MemberStatusChanged {
                session: parent,
                member,
                status: MemberRunStatus::Running,
            },
        ));
        assert_eq!(p.session.members.len(), 1);
        assert_eq!(p.session.members[0].status, MemberRunStatus::Running);
        assert_eq!(p.session.members[0].child, Some(child));
        assert_eq!(
            p.session.members[0].subagent_type,
            AgentName::new("explore")
        );
        // The spawn edge carries the member's purpose and its originating call.
        assert_eq!(
            p.session.members[0].directive,
            "Scan the routing layer and report every handler"
        );
        assert!(p.session.members[0].tool_call.is_some());

        p.apply(&env(
            3,
            Event::MemberFinished {
                session: parent,
                member,
                status: MemberRunStatus::Done,
                summary: "found it".to_string(),
                child: Some(child),
            },
        ));
        assert_eq!(p.session.members.len(), 1, "same member upserts, not dupes");
        assert_eq!(p.session.members[0].status, MemberRunStatus::Done);
        assert_eq!(p.session.members[0].summary, "found it");

        // Idempotent by seq: replaying an older seq is a no-op.
        p.apply(&env(
            2,
            Event::MemberStatusChanged {
                session: parent,
                member,
                status: MemberRunStatus::Running,
            },
        ));
        assert_eq!(p.session.members[0].status, MemberRunStatus::Done);
    }
}

mod team_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::ids::EventSeq;

    fn env(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event,
        }
    }

    /// The full team event log for the invariant: two agents join `#build`, the
    /// root posts to it, plus one direct message between the two. Reused by both
    /// the fold test and the replay test so they are provably reconstructing the
    /// SAME state.
    ///
    /// Written in the **post-scoping** emitter shape: registrations carry a real
    /// `parent`, and every handle / channel on the wire is already canonical.
    fn build_log(root: SessionId) -> Vec<Envelope> {
        let alice = SessionId::new();
        let bob = SessionId::new();
        vec![
            env(
                1,
                Event::AgentRegistered {
                    session: root,
                    agent_session: root,
                    handle: "main".to_string(),
                    parent: None,
                    agent_type: AgentName::new("build"),
                    mode: SubagentMode::Transient,
                },
            ),
            env(
                2,
                Event::AgentRegistered {
                    session: root,
                    agent_session: alice,
                    handle: "reviewer-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("reviewer"),
                    mode: SubagentMode::Resident,
                },
            ),
            env(
                3,
                Event::AgentRegistered {
                    session: root,
                    agent_session: bob,
                    handle: "reviewer-2".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("reviewer"),
                    mode: SubagentMode::Resident,
                },
            ),
            env(
                4,
                Event::ChannelCreated {
                    session: root,
                    channel: "main#build".to_string(),
                    kind: ChannelKind::Group,
                    unit: Some("main".into()),
                    members: vec!["main/reviewer-1".to_string()],
                },
            ),
            env(
                5,
                Event::ChannelCreated {
                    session: root,
                    channel: "main#build".to_string(),
                    kind: ChannelKind::Group,
                    unit: Some("main".into()),
                    members: vec!["main/reviewer-2".to_string()],
                },
            ),
            env(
                6,
                Event::MailSent {
                    session: root,
                    from: "main".to_string(),
                    to: MailEndpoint::Channel("main#build".to_string()),
                    kind: MailKind::Announcement,
                    body: "ship it".to_string(),
                },
            ),
            env(
                7,
                Event::MailSent {
                    session: root,
                    from: scope::HARNESS_HANDLE.to_string(),
                    to: MailEndpoint::Handle("main/reviewer-2".to_string()),
                    kind: MailKind::Message,
                    body: "psst".to_string(),
                },
            ),
        ]
    }

    #[test]
    fn channel_mail_folds_into_every_subscriber_inbox_and_survives_replay() {
        let root = SessionId::new();
        let log = build_log(root);

        // Apply incrementally.
        let mut live = Projection::default();
        for e in &log {
            live.apply(e);
        }

        // The channel post reached BOTH subscribers' inboxes...
        let inbox_of = |p: &Projection, handle: &str| -> Vec<String> {
            p.team
                .inboxes
                .get(handle)
                .map(|msgs| {
                    msgs.iter()
                        .map(|index| p.team.mail[*index].body.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(
            inbox_of(&live, "main/reviewer-1"),
            vec!["ship it".to_string()]
        );
        assert_eq!(
            inbox_of(&live, "main/reviewer-2"),
            vec!["ship it".to_string(), "psst".to_string()],
            "reviewer-2 sees the channel post AND the direct message, in order"
        );
        // ...and the channel keeps its own ordered log, under its unit-qualified
        // key. The bare name is NOT a key: two units may each own a `#build`.
        assert!(!live.team.channels.contains_key("build"));
        let channel = live
            .team
            .channels
            .get("main#build")
            .expect("channel exists under its unit-qualified key");
        assert_eq!(channel.members.len(), 2);
        assert_eq!(channel.log.len(), 1);
        assert_eq!(live.team.mail[channel.log[0]].body, "ship it");
        assert_eq!(live.team.mail.len(), 2);
        let snapshot = String::from_utf8(live.encode_snapshot().unwrap()).unwrap();
        assert_eq!(snapshot.matches("\"body\":\"ship it\"").count(), 1);
        // Roster bound the root plus both members, keyed by canonical path.
        assert_eq!(live.team.roster.len(), 3);
        assert!(live.team.roster.contains_key("main"));
        assert!(live.team.roster.contains_key("main/reviewer-1"));
        assert!(live.team.roster.contains_key("main/reviewer-2"));

        // A fresh replay from the same log reconstructs identical team state.
        let replayed = Projection::from_events(&log);
        assert_eq!(
            replayed.team, live.team,
            "replay must reconstruct inboxes + channels + roster exactly"
        );
    }

    /// Two sibling units each own a `#build`. Because the channel key is
    /// unit-qualified, a post in one unit must not touch the other's log or any
    /// of its members' inboxes (task 08-07, AC4).
    ///
    /// ```text
    /// main
    /// ├── lead-1 ── worker-1   #build (lead-1's)
    /// └── lead-2 ── worker-7   #build (lead-2's)
    /// ```
    #[test]
    fn same_channel_name_in_two_units_never_cross_talks() {
        let root = SessionId::new();
        let mut seq = 0;
        let mut log = Vec::new();
        let mut push = |event: Event, seq: &mut u64| {
            *seq += 1;
            log.push(env(*seq, event));
        };

        push(
            Event::AgentRegistered {
                session: root,
                agent_session: root,
                handle: "main".to_string(),
                parent: None,
                agent_type: AgentName::new("build"),
                mode: SubagentMode::Transient,
            },
            &mut seq,
        );
        for (leaf, parent) in [
            ("lead-1", "main"),
            ("lead-2", "main"),
            ("worker-1", "main/lead-1"),
            ("worker-7", "main/lead-2"),
        ] {
            push(
                Event::AgentRegistered {
                    session: root,
                    agent_session: SessionId::new(),
                    handle: leaf.to_string(),
                    parent: Some(parent.to_string()),
                    agent_type: AgentName::new("worker"),
                    mode: SubagentMode::Resident,
                },
                &mut seq,
            );
        }

        // Each unit's members join THEIR unit's #build.
        for (unit, member) in [
            ("main/lead-1", "main/lead-1/worker-1"),
            ("main/lead-2", "main/lead-2/worker-7"),
        ] {
            push(
                Event::ChannelCreated {
                    session: root,
                    channel: format!("{unit}#build"),
                    kind: ChannelKind::Group,
                    unit: Some("main".into()),
                    members: vec![member.to_string()],
                },
                &mut seq,
            );
        }

        // lead-1 posts to its own #build only.
        push(
            Event::MailSent {
                session: root,
                from: "main/lead-1".to_string(),
                to: MailEndpoint::Channel("main/lead-1#build".to_string()),
                kind: MailKind::Message,
                body: "unit one only".to_string(),
            },
            &mut seq,
        );

        let projection = Projection::from_events(&log);

        // Two distinct channels share the name `build`.
        assert_eq!(projection.team.channels.len(), 2);
        let left = projection
            .team
            .channels
            .get("main/lead-1#build")
            .expect("lead-1 owns a #build");
        let right = projection
            .team
            .channels
            .get("main/lead-2#build")
            .expect("lead-2 owns a SEPARATE #build");

        assert_eq!(left.log.len(), 1, "the post landed in lead-1's channel");
        assert_eq!(projection.team.mail[left.log[0]].body, "unit one only");
        assert!(right.log.is_empty(), "lead-2's #build must be untouched");

        // Only the posting unit's member received it.
        let inbox = |handle: &str| -> Vec<String> {
            projection
                .team
                .inboxes
                .get(handle)
                .map(|msgs| {
                    msgs.iter()
                        .map(|index| projection.team.mail[*index].body.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        assert_eq!(
            inbox("main/lead-1/worker-1"),
            vec!["unit one only".to_string()]
        );
        assert_eq!(
            inbox("main/lead-2/worker-7"),
            Vec::<String>::new(),
            "a sibling unit's worker must never see another unit's channel post"
        );
    }

    #[test]
    fn handle_resolution_uses_only_the_callers_existing_dm() {
        let mut team = TeamProjection::default();
        for (id, parent, child) in [
            ("DM-11111111", "main", "main/lead"),
            ("DM-22222222", "main/lead", "main/lead/worker"),
            ("DM-33333333", "main/other", "main/other/worker"),
        ] {
            team.channels.insert(
                id.into(),
                ChannelProjection {
                    kind: ChannelKind::Dm,
                    members: [parent.into(), child.into()].into(),
                    ..Default::default()
                },
            );
        }
        for alias in ["worker", "main/lead/worker"] {
            assert_eq!(
                team.resolve_dm("main/lead", alias).as_deref(),
                Some("DM-22222222")
            );
        }
        for alias in ["^", "^parent", "", "lead", "main/lead"] {
            assert_eq!(
                team.resolve_dm("main/lead/worker", alias).as_deref(),
                Some("DM-22222222")
            );
        }
        for alias in ["main/other/worker", "missing", "main/lead"] {
            assert_eq!(team.resolve_dm("main/lead", alias), None);
        }
        // Roster rows alone cannot create an address or grant membership.
        for (handle, mode) in [
            ("main", SubagentMode::Transient),
            ("main/worker", SubagentMode::Resident),
        ] {
            team.roster.insert(
                handle.into(),
                RosterEntry {
                    handle: handle.into(),
                    session: SessionId::new(),
                    agent_type: AgentName::new("worker"),
                    mode,
                    status: RosterStatus::Idle,
                    current_task: None,
                    resident_cursor: 0,
                    heartbeat_ms: 0,
                    resident_work: None,
                },
            );
        }
        assert_eq!(team.resolve_dm("main", "worker"), None);
        assert_eq!(team.resolve_dm("main", "main/worker"), None);
        assert_eq!(team.resolve_dm("main/lead/worker", "main"), None);
    }

    /// The reducer builds a real tree from `parent`, and the resulting paths obey
    /// the scope rule the rest of the system enforces.
    #[test]
    fn nested_registration_folds_into_scoped_paths() {
        let root = SessionId::new();
        let log = vec![
            env(
                1,
                Event::AgentRegistered {
                    session: root,
                    agent_session: root,
                    handle: "main".to_string(),
                    parent: None,
                    agent_type: AgentName::new("build"),
                    mode: SubagentMode::Transient,
                },
            ),
            env(
                2,
                Event::AgentRegistered {
                    session: root,
                    agent_session: SessionId::new(),
                    handle: "lead-1".to_string(),
                    parent: Some("main".to_string()),
                    agent_type: AgentName::new("lead"),
                    mode: SubagentMode::Resident,
                },
            ),
            env(
                3,
                Event::AgentRegistered {
                    session: root,
                    agent_session: SessionId::new(),
                    handle: "worker-1".to_string(),
                    parent: Some("main/lead-1".to_string()),
                    agent_type: AgentName::new("worker"),
                    mode: SubagentMode::Resident,
                },
            ),
        ];
        let projection = Projection::from_events(&log);

        let keys: Vec<&str> = projection.team.roster.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["main", "main/lead-1", "main/lead-1/worker-1"]);
    }
}

mod context_status_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::ids::EventSeq;
    use crate::tokens::{TokenAccountingMode, TokenSource};

    fn env(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event,
        }
    }

    #[test]
    fn folds_latest_context_status_per_session() {
        let session = SessionId::new();
        let mut p = Projection::default();
        p.apply(&env(
            1,
            Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: "/tmp".to_string(),
                project: None,
                kind: crate::SessionKind::Project,
            },
        ));
        assert!(
            p.session.context_status.is_none(),
            "a session that has not streamed yet reports nothing"
        );

        p.apply(&env(
            2,
            Event::ContextStatus {
                session,
                tokens: 12_000,
                source: TokenSource::Estimate,
                mode: TokenAccountingMode::Auto,
                threshold: 150_000,
            },
        ));
        let first = p.session.context_status.unwrap();
        assert_eq!(first.tokens, 12_000);
        assert_eq!(first.source, TokenSource::Estimate);
        assert_eq!(first.mode, TokenAccountingMode::Auto);
        assert_eq!(first.threshold, 150_000);

        // A later round replaces the figure; clients read one current value.
        p.apply(&env(
            3,
            Event::ContextStatus {
                session,
                tokens: 9_000,
                source: TokenSource::Provider,
                mode: TokenAccountingMode::Auto,
                threshold: 150_000,
            },
        ));
        let latest = p.session.context_status.unwrap();
        assert_eq!(latest.tokens, 9_000);
        assert_eq!(latest.source, TokenSource::Provider);

        // Replay of an older seq is a no-op, matching every other fold.
        p.apply(&env(
            2,
            Event::ContextStatus {
                session,
                tokens: 12_000,
                source: TokenSource::Estimate,
                mode: TokenAccountingMode::Auto,
                threshold: 150_000,
            },
        ));
        assert_eq!(p.session.context_status.unwrap().tokens, 9_000);
    }
}

mod compaction_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::CompactionStrategy;
    use crate::ids::EventSeq;

    fn env(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event,
        }
    }

    #[test]
    fn compaction_marker_precedes_retained_in_flight_message() {
        let session = SessionId::new();
        let user = MessageId::new();
        let assistant = MessageId::new();
        let marker = MessageId::new();
        let projection = Projection::from_events(&[
            env(
                1,
                Event::SessionCreated {
                    session,
                    parent: None,
                    agent: AgentName::new("build"),
                    model: ModelRef::new("fake"),
                    workdir: "/tmp".to_string(),
                    project: None,
                    kind: crate::SessionKind::Project,
                },
            ),
            env(
                2,
                Event::MessageStarted {
                    session,
                    message: user,
                    role: Role::User,
                    agent: None,
                    model: None,
                },
            ),
            env(
                3,
                Event::MessageStarted {
                    session,
                    message: assistant,
                    role: Role::Assistant,
                    agent: Some(AgentName::new("build")),
                    model: Some(ModelRef::new("fake")),
                },
            ),
            env(
                4,
                Event::MessageStarted {
                    session,
                    message: marker,
                    role: Role::System,
                    agent: None,
                    model: None,
                },
            ),
            env(
                5,
                Event::ContextCompacted {
                    session,
                    message: marker,
                    strategy: CompactionStrategy::LocalSummarizer,
                    from_message: user,
                    to_message: user,
                    folded_count: 1,
                    input_tokens_est: 100,
                    threshold: 75,
                    step_cut: None,
                },
            ),
        ]);

        let ids: Vec<MessageId> = projection
            .session
            .messages
            .iter()
            .map(|message| message.id)
            .collect();
        assert_eq!(ids, vec![user, marker, assistant]);
    }

    /// A log written before step cuts existed carries no `step_cut` field; it
    /// must decode to `None` and fold exactly as it always did.
    #[test]
    fn context_compacted_without_step_cut_decodes_as_a_message_fold() {
        let session = SessionId::new();
        let (marker, from, to) = (MessageId::new(), MessageId::new(), MessageId::new());
        let json = serde_json::json!({
            "type": "context_compacted",
            "session": session,
            "message": marker,
            "strategy": "local_summarizer",
            "from_message": from,
            "to_message": to,
            "folded_count": 2,
            "input_tokens_est": 100,
            "threshold": 75,
        })
        .to_string();
        let event: Event = serde_json::from_str(&json).unwrap();
        assert_eq!(
            event,
            Event::ContextCompacted {
                session,
                message: marker,
                strategy: CompactionStrategy::LocalSummarizer,
                from_message: from,
                to_message: to,
                folded_count: 2,
                input_tokens_est: 100,
                threshold: 75,
                step_cut: None,
            }
        );
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::from_str::<serde_json::Value>(&json).unwrap(),
            "a message fold still writes the old shape"
        );
    }

    struct Turn {
        session: SessionId,
        user: MessageId,
        notice: MessageId,
        assistant: MessageId,
        parts: Vec<PartId>,
        events: Vec<Event>,
    }

    /// `[user, notice, assistant]` with `parts` leading parts on the assistant.
    fn single_turn(parts: usize) -> Turn {
        let session = SessionId::new();
        let (user, notice, assistant) = (MessageId::new(), MessageId::new(), MessageId::new());
        let started = |message, role| Event::MessageStarted {
            session,
            message,
            role,
            agent: None,
            model: None,
        };
        let mut events = vec![
            Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: "/tmp".to_string(),
                project: None,
                kind: crate::SessionKind::Project,
            },
            started(user, Role::User),
            started(notice, Role::System),
            started(assistant, Role::Assistant),
        ];
        let parts: Vec<PartId> = (0..parts).map(|_| PartId::new()).collect();
        events.extend(parts.iter().map(|part| Event::TextStart {
            session,
            message: assistant,
            part: *part,
        }));
        Turn {
            session,
            user,
            notice,
            assistant,
            parts,
            events,
        }
    }

    impl Turn {
        fn step_fold(&mut self, through: usize) -> MessageId {
            let marker = MessageId::new();
            self.events.push(Event::MessageStarted {
                session: self.session,
                message: marker,
                role: Role::System,
                agent: None,
                model: None,
            });
            self.events.push(Event::ContextCompacted {
                session: self.session,
                message: marker,
                strategy: CompactionStrategy::LocalSummarizer,
                from_message: self.assistant,
                to_message: self.assistant,
                folded_count: 1,
                input_tokens_est: 100,
                threshold: 75,
                step_cut: Some(crate::StepCut {
                    through_part: self.parts[through],
                    kept_head: self.user,
                }),
            });
            marker
        }

        fn fold(&self) -> Projection {
            let envelopes: Vec<_> = self
                .events
                .iter()
                .cloned()
                .enumerate()
                .map(|(seq, event)| env(seq as u64 + 1, event))
                .collect();
            Projection::from_events(&envelopes)
        }
    }

    #[test]
    fn step_cut_keeps_the_turn_head_and_folds_leading_parts() {
        let mut turn = single_turn(4);
        let marker = turn.step_fold(1);
        let projection = turn.fold();
        let ids: Vec<MessageId> = projection.session.messages.iter().map(|m| m.id).collect();
        assert_eq!(ids, vec![marker, turn.user, turn.notice, turn.assistant]);
        let assistant = &projection.session.messages[3];
        assert_eq!(
            assistant.folded_parts, 2,
            "parts through the cut are folded"
        );
        assert_eq!(assistant.parts.len(), 4, "the projection keeps every part");
    }

    #[test]
    fn second_step_cut_moves_the_new_marker_before_the_head() {
        let mut turn = single_turn(6);
        let first = turn.step_fold(1);
        let second = turn.step_fold(3);
        let projection = turn.fold();
        let ids: Vec<MessageId> = projection.session.messages.iter().map(|m| m.id).collect();
        assert_eq!(
            ids,
            vec![first, second, turn.user, turn.notice, turn.assistant]
        );
        assert_eq!(projection.session.messages[4].folded_parts, 4);
    }

    #[test]
    fn deleting_a_folded_part_keeps_the_kept_parts_kept() {
        let mut turn = single_turn(4);
        turn.step_fold(1);
        turn.events.push(Event::PartDeleted {
            session: turn.session,
            message: turn.assistant,
            part: turn.parts[0],
        });
        let projection = turn.fold();
        let assistant = &projection.session.messages[3];
        assert_eq!(assistant.folded_parts, 1);
        assert_eq!(assistant.parts[1].id(), turn.parts[2], "first kept part");
    }
}

mod usage_fold_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::ids::EventSeq;
    use crate::message::UsagePurpose;
    use crate::usage::UNATTRIBUTED_MODEL;

    fn envs(events: Vec<Event>) -> Vec<Envelope> {
        events
            .into_iter()
            .enumerate()
            .map(|(index, event)| Envelope {
                seq: EventSeq(index as u64 + 1),
                ts_millis: 0,
                event,
            })
            .collect()
    }

    fn created(session: SessionId) -> Event {
        Event::SessionCreated {
            session,
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("session-model"),
            workdir: "/tmp".to_string(),
            project: None,
            kind: crate::SessionKind::Project,
        }
    }

    fn started(session: SessionId, message: MessageId) -> Event {
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
            agent: None,
            model: None,
        }
    }

    fn usage(input: u64, output: u64, reasoning: u64, unknown: bool) -> TokenUsage {
        TokenUsage {
            input,
            output,
            reasoning,
            cache_read: 1,
            cache_write: 2,
            reasoning_unknown: unknown,
        }
    }

    fn round(
        session: SessionId,
        message: MessageId,
        step: u32,
        model: &str,
        tokens: TokenUsage,
    ) -> Event {
        Event::UsageRecorded {
            session,
            message: Some(message),
            step: Some(step),
            model: ModelRef::new(model),
            purpose: UsagePurpose::Turn,
            tokens,
        }
    }

    fn finished(session: SessionId, message: MessageId, tokens: Option<TokenUsage>) -> Event {
        Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens,
            cause: None,
        }
    }

    #[test]
    fn a_turn_error_folds_onto_its_message_and_other_errors_do_not() {
        let session = SessionId::new();
        let message = MessageId::new();
        let other = MessageId::new();
        let log = envs(vec![
            created(session),
            started(session, message),
            started(session, other),
            Event::Error {
                session: Some(session),
                code: "global".to_string(),
                message: "not tied to a message".to_string(),
                failed_message: None,
            },
            Event::Error {
                session: Some(session),
                code: "provider_error".to_string(),
                message: "http status 400: bad request".to_string(),
                failed_message: Some(message),
            },
            Event::MessageFinished {
                session,
                message,
                role: Role::Assistant,
                finish: FinishReason::Error,
                tokens: None,
                cause: Some(FinishCause::ProviderError),
            },
        ]);
        let projection = Projection::from_events(&log);
        let failed = &projection.session.messages[0];
        assert_eq!(
            failed.error,
            Some(MessageError {
                code: "provider_error".to_string(),
                message: "http status 400: bad request".to_string(),
            })
        );
        assert_eq!(projection.session.messages[1].error, None);
        // Old logs: the field is absent on the wire and in snapshots.
        let json = serde_json::to_value(&projection.session.messages[1]).unwrap();
        assert!(json.get("error").is_none(), "{json}");
        let legacy: Event = serde_json::from_value(serde_json::json!({
            "type": "error",
            "session": session,
            "code": "x",
            "message": "y",
        }))
        .unwrap();
        assert!(matches!(
            legacy,
            Event::Error {
                failed_message: None,
                ..
            }
        ));
    }

    #[test]
    fn rounds_fold_by_model_and_purpose_without_double_counting_the_message_sum() {
        let session = SessionId::new();
        let message = MessageId::new();
        let first = usage(100, 30, 10, false);
        let second = usage(50, 20, 0, true);
        let mut sum = first;
        sum.add(second);
        let log = envs(vec![
            created(session),
            started(session, message),
            round(session, message, 0, "model-a", first),
            round(session, message, 1, "model-b", second),
            finished(session, message, Some(sum)),
            Event::UsageRecorded {
                session,
                message: None,
                step: None,
                model: ModelRef::new("model-a"),
                purpose: UsagePurpose::Title,
                tokens: usage(5, 3, 0, false),
            },
        ]);
        let projection = Projection::from_events(&log);
        let usage = &projection.session.usage;
        let a = usage.by_model[&ModelRef::new("model-a")];
        assert_eq!((a.input, a.output, a.reasoning, a.rounds), (105, 33, 10, 2));
        assert_eq!((a.cache_read, a.cache_write), (2, 4));
        let b = usage.by_model[&ModelRef::new("model-b")];
        assert_eq!(
            (b.output, b.reasoning_unknown_output, b.rounds),
            (20, 20, 1)
        );
        assert!(
            !usage
                .by_model
                .contains_key(&ModelRef::new(UNATTRIBUTED_MODEL)),
            "a message with round records must not fall back to its sum"
        );
        assert_eq!(usage.by_purpose[&UsagePurpose::Turn].rounds, 2);
        assert_eq!(usage.by_purpose[&UsagePurpose::Title].output, 3);
        assert_eq!(usage.total().output, 53);

        let message_usage = projection.session.messages[0].usage.clone().unwrap();
        assert_eq!(message_usage.model, ModelRef::new("model-b"));
        assert_eq!(message_usage.rounds, 2);
        assert_eq!(message_usage.tokens, sum);

        // Deterministic: replaying the same log (and re-delivering it) folds
        // to the same totals.
        let mut replayed = Projection::from_events(&log);
        for env in &log {
            replayed.apply(env);
        }
        assert_eq!(replayed.session.usage, projection.session.usage);
    }

    #[test]
    fn legacy_message_sums_fold_as_unattributed_with_unknown_thinking() {
        let session = SessionId::new();
        let message = MessageId::new();
        let empty = MessageId::new();
        let legacy_json = serde_json::json!({
            "type": "message_finished",
            "session": session,
            "message": message,
            "role": "assistant",
            "finish": "stop",
            "tokens": {"input": 40, "output": 12, "reasoning": 5, "cache_read": 8, "cache_write": 0}
        });
        let legacy: Event = serde_json::from_value(legacy_json).unwrap();
        let log = envs(vec![
            created(session),
            started(session, message),
            legacy,
            started(session, empty),
            finished(session, empty, None),
        ]);
        let usage = Projection::from_events(&log).session.usage;
        let totals = usage.by_model[&ModelRef::new(UNATTRIBUTED_MODEL)];
        assert_eq!(totals.legacy_messages, 1);
        assert_eq!(totals.rounds, 0);
        assert_eq!(
            (totals.input, totals.cache_read, totals.output),
            (40, 8, 12)
        );
        assert_eq!(usage.by_model.len(), 1);
    }

    #[test]
    fn forked_copies_do_not_count_the_source_usage() {
        let session = SessionId::new();
        let copied = MessageId::new();
        let own = MessageId::new();
        let log = envs(vec![
            created(session),
            Event::SessionForked {
                session,
                source: SessionId::new(),
                before_message: None,
            },
            started(session, copied),
            finished(session, copied, Some(usage(10, 4, 0, false))),
            started(session, own),
            round(session, own, 0, "model-a", usage(3, 2, 0, false)),
            finished(session, own, Some(usage(3, 2, 0, false))),
        ]);
        let projection = Projection::from_events(&log);
        assert!(projection.session.forked_from.is_some());
        let usage = projection.session.usage;
        assert_eq!(usage.by_model.len(), 1);
        assert_eq!(usage.total().output, 2);
    }

    #[test]
    fn usage_is_absent_from_serialized_projection_until_recorded() {
        let session = SessionId::new();
        let projection = Projection::from_events(&envs(vec![created(session)]));
        let json = serde_json::to_string(&projection).unwrap();
        assert!(!json.contains("usage"), "{json}");
        assert!(!json.contains("forked_from"), "{json}");
    }
}

mod orchestration_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::event::{ArchiveReason, ReportOutcome};
    use crate::ids::EventSeq;
    use crate::mail::ChannelKind;

    fn env(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event,
        }
    }

    fn register_lead(root: SessionId, child: SessionId) -> Event {
        Event::AgentRegistered {
            session: root,
            agent_session: child,
            handle: "lead-1".to_string(),
            parent: Some(scope::ROOT_HANDLE.to_string()),
            agent_type: AgentName::new("lead"),
            mode: SubagentMode::Resident,
        }
    }

    /// Archive is the sole exit (ADR-0015): the live roster row disappears, group
    /// membership ends, but the DM channel — the revival address — persists, and
    /// the archived map keeps user-visible history.
    #[test]
    fn agent_archived_removes_roster_and_group_membership_but_keeps_dm() {
        let root = SessionId::new();
        let lead = SessionId::new();
        let dm = "DM-aB12Cd34".to_string();
        let projection = Projection::from_events(&[
            env(1, register_lead(root, lead)),
            env(
                2,
                Event::ChannelCreated {
                    session: root,
                    channel: "main#build".to_string(),
                    kind: ChannelKind::Group,
                    unit: Some("main".into()),
                    members: vec!["main/lead-1".to_string()],
                },
            ),
            env(
                3,
                Event::ChannelCreated {
                    session: root,
                    channel: dm.clone(),
                    kind: ChannelKind::Dm,
                    unit: None,
                    members: vec![scope::ROOT_HANDLE.to_string(), "main/lead-1".to_string()],
                },
            ),
            env(
                4,
                Event::AgentArchived {
                    session: root,
                    handle: "main/lead-1".to_string(),
                    child: lead,
                    reason: ArchiveReason::Reported,
                },
            ),
        ]);

        assert!(
            !projection.team.roster.contains_key("main/lead-1"),
            "archived agent leaves the live roster"
        );
        let build = projection.team.channels.get("main#build").unwrap();
        assert!(
            !build.members.contains("main/lead-1"),
            "group membership ends at archive"
        );
        let dm_channel = projection.team.channels.get(&dm).unwrap();
        assert!(
            dm_channel.members.contains("main/lead-1"),
            "the DM channel persists as the revival address"
        );
        let archived = projection.team.archived.get("main/lead-1").unwrap();
        assert_eq!(archived.session, lead);
        assert_eq!(archived.reason, ArchiveReason::Reported);
        assert_eq!(archived.agent_type, AgentName::new("lead"));
    }

    /// Revive is register-upsert + restart marker: the roster row returns and the
    /// archive history entry clears in the same replay.
    #[test]
    fn agent_restarted_clears_archive_history_once_reregistered() {
        let root = SessionId::new();
        let lead = SessionId::new();
        let projection = Projection::from_events(&[
            env(1, register_lead(root, lead)),
            env(
                2,
                Event::AgentArchived {
                    session: root,
                    handle: "main/lead-1".to_string(),
                    child: lead,
                    reason: ArchiveReason::Reported,
                },
            ),
            env(3, register_lead(root, lead)),
            env(
                4,
                Event::AgentRestarted {
                    session: root,
                    handle: "main/lead-1".to_string(),
                    child: lead,
                    epoch: ActorEpoch::INITIAL,
                },
            ),
        ]);

        let entry = projection.team.roster.get("main/lead-1").unwrap();
        assert_eq!(entry.session, lead, "revived agent is live again");
        assert!(
            !projection.team.archived.contains_key("main/lead-1"),
            "restart clears the archive history row"
        );
    }

    /// `ChannelCreated` is the minting fact: kind and initial members fold.
    #[test]
    fn channel_created_folds_kind_and_members() {
        let root = SessionId::new();
        let projection = Projection::from_events(&[
            env(
                1,
                Event::ChannelCreated {
                    session: root,
                    channel: "announce-aB12Cd34".to_string(),
                    kind: ChannelKind::Group,
                    unit: None,
                    members: vec![scope::ROOT_HANDLE.to_string()],
                },
            ),
            env(
                2,
                Event::ChannelCreated {
                    session: root,
                    channel: "main#build".to_string(),
                    kind: ChannelKind::Group,
                    unit: Some("main".into()),
                    members: vec!["main".to_string()],
                },
            ),
        ]);
        let announce = projection.team.channels.get("announce-aB12Cd34").unwrap();
        assert_eq!(announce.kind, ChannelKind::Group);
        assert!(announce.members.contains("main"));
        let named = projection.team.channels.get("main#build").unwrap();
        assert_eq!(named.kind, ChannelKind::Group);
    }

    /// A report is a terminal member status on the parent log (ADR-0015).
    #[test]
    fn subagent_reported_marks_the_member_terminal() {
        let parent = SessionId::new();
        let child = SessionId::new();
        let member = MemberId::new();
        let projection = Projection::from_events(&[
            env(
                1,
                Event::MemberSpawned {
                    session: parent,
                    member,
                    child: Some(child),
                    subagent_type: AgentName::new("explore"),
                    description: "scan".to_string(),
                    depth: 1,
                    directive: "find it".to_string(),
                    tool_call: None,
                },
            ),
            env(
                2,
                Event::SubagentReported {
                    session: parent,
                    member,
                    child,
                    handle: "main/explore-1".to_string(),
                    outcome: ReportOutcome::Done,
                    report: "found it".to_string(),
                },
            ),
        ]);
        let row = &projection.session.members[0];
        assert_eq!(row.status, MemberRunStatus::Done);
        assert_eq!(row.summary, "found it");
    }

    /// Steer consumption advances the durable cursor without touching
    /// liveness — the report gate must not re-block on mail the agent saw
    /// inside a tool result (fix for the unread-mail report loop).
    #[test]
    fn terminal_activity_does_not_consume_unread_mail() {
        let root = SessionId::new();
        let actor = SessionId::new();
        for note in ["resident stopped", "archived by parent"] {
            let projection = Projection::from_events(&[
                env(
                    1,
                    Event::AgentRegistered {
                        session: root,
                        agent_session: actor,
                        handle: "worker".into(),
                        parent: Some("main".into()),
                        agent_type: AgentName::new("worker"),
                        mode: SubagentMode::Resident,
                    },
                ),
                env(
                    2,
                    Event::MailSent {
                        session: root,
                        from: scope::HARNESS_HANDLE.into(),
                        to: MailEndpoint::Handle("main/worker".into()),
                        kind: MailKind::Message,
                        body: "unread".into(),
                    },
                ),
                env(
                    3,
                    Event::AgentActivityChanged {
                        session: root,
                        handle: "main/worker".into(),
                        status: RosterStatus::Failed,
                        current_task: Some(note.into()),
                    },
                ),
            ]);
            assert_eq!(
                projection.team.roster["main/worker"].resident_cursor, 0,
                "{note}"
            );
            assert_eq!(projection.team.inboxes["main/worker"].len(), 1);
        }
    }

    #[test]
    fn mail_consumed_advances_the_durable_inbox_cursor() {
        let root = SessionId::new();
        let lead = SessionId::new();
        let projection = Projection::from_events(&[
            env(
                1,
                Event::AgentRegistered {
                    session: root,
                    agent_session: lead,
                    handle: "lead-1".to_string(),
                    parent: Some(scope::ROOT_HANDLE.to_string()),
                    agent_type: AgentName::new("lead"),
                    mode: SubagentMode::Resident,
                },
            ),
            env(
                2,
                Event::MailSent {
                    session: root,
                    from: scope::HARNESS_HANDLE.to_string(),
                    to: MailEndpoint::Handle("main/lead-1".to_string()),
                    kind: MailKind::Message,
                    body: "urgent".to_string(),
                },
            ),
            env(
                3,
                Event::MailConsumed {
                    session: root,
                    handle: "main/lead-1".to_string(),
                    through: 1,
                },
            ),
        ]);
        let entry = projection.team.roster.get("main/lead-1").unwrap();
        assert_eq!(
            entry.resident_cursor, 1,
            "steer consumption advances the cursor"
        );
    }

    /// Harness heartbeats (ADR-0002 liveness) fold onto the roster row and
    /// never regress: the fold keeps the max, so a stale (lower) heartbeat —
    /// replayed or reordered — cannot make a progressing agent look stalled,
    /// and an unknown handle invents no roster row.
    #[test]
    fn agent_heartbeat_records_max_activity_on_the_roster_row() {
        let root = SessionId::new();
        let lead = SessionId::new();
        let heartbeat = |heartbeat_ms: u64| Event::AgentHeartbeat {
            session: root,
            handle: "main/lead-1".to_string(),
            heartbeat_ms,
        };
        let projection = Projection::from_events(&[
            env(1, register_lead(root, lead)),
            env(2, heartbeat(2_000)),
            env(3, heartbeat(1_000)),
        ]);
        let entry = projection.team.roster.get("main/lead-1").unwrap();
        assert_eq!(
            entry.heartbeat_ms, 2_000,
            "a stale heartbeat must never overwrite a newer one"
        );

        // A heartbeat for an unknown handle is dropped, not materialized.
        let projection = Projection::from_events(&[env(
            1,
            Event::AgentHeartbeat {
                session: root,
                handle: "main/ghost".to_string(),
                heartbeat_ms: 5,
            },
        )]);
        assert!(
            !projection.team.roster.contains_key("main/ghost"),
            "heartbeats observe existing roster rows; they never create one"
        );
    }

    /// The handoff is the only carried state (ADR-0015): it projects onto the
    /// child session for revival context and search.
    #[test]
    fn handoff_committed_projects_onto_the_child_session() {
        let child = SessionId::new();
        let projection = Projection::from_events(&[
            env(
                1,
                Event::SessionCreated {
                    session: child,
                    parent: None,
                    agent: AgentName::new("explore"),
                    model: ModelRef::new("fake"),
                    workdir: "/w".to_string(),
                    project: None,
                    kind: crate::SessionKind::Project,
                },
            ),
            env(
                2,
                Event::HandoffCommitted {
                    session: child,
                    handle: "main/explore-1".to_string(),
                    generation: 2,
                    doc: "1. Goal - find it".to_string(),
                    degraded: true,
                },
            ),
        ]);
        let handoff = projection.session.handoff.as_ref().unwrap();
        assert_eq!(handoff.generation, 2);
        assert!(handoff.degraded);
        assert_eq!(handoff.doc, "1. Goal - find it");
    }
}
