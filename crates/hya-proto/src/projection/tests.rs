use std::sync::Arc;

use super::{MessageError, Projection, ProjectionReducer, ProjectionReducerRegistry};
use crate::event::{Envelope, Event};
use crate::ids::{EventSeq, MessageId, PartId, SessionId};
use crate::message::{FinishCause, FinishReason, Role, TokenUsage};
use crate::model::{AgentName, ModelRef};

mod extension_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    struct CounterReducer {
        version: u32,
    }

    impl ProjectionReducer for CounterReducer {
        fn bundle_id(&self) -> &str {
            "hya/counter"
        }

        fn version(&self) -> u32 {
            self.version
        }

        fn fold_extension(
            &self,
            projection: &mut Projection,
            _session: SessionId,
            kind: &str,
            payload: &serde_json::Value,
        ) {
            let entry = projection
                .extensions
                .entry(self.bundle_id().to_owned())
                .or_insert_with(|| serde_json::json!({ "count": 0, "notes": [] }));
            if let Some(obj) = entry.as_object_mut() {
                if kind == "increment" {
                    let delta = payload.get("delta").and_then(|v| v.as_u64()).unwrap_or(1);
                    let next = obj.get("count").and_then(|v| v.as_u64()).unwrap_or(0) + delta;
                    obj.insert("count".to_owned(), serde_json::json!(next));
                } else if kind == "note"
                    && let Some(text) = payload.get("text").and_then(|v| v.as_str())
                    && let Some(notes) = obj.get_mut("notes").and_then(|v| v.as_array_mut())
                {
                    notes.push(serde_json::json!(text));
                }
            }
        }
    }

    fn env(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event,
        }
    }

    #[test]
    fn known_bundle_folds_via_reducer_while_unknown_bundle_preserves_raw_entries() {
        let session = SessionId::new();
        let log = vec![
            env(
                1,
                Event::SessionCreated {
                    session,
                    parent: None,
                    agent: AgentName::new("build"),
                    model: ModelRef::new("fake"),
                    workdir: "/tmp".to_owned(),
                    project: None,
                    kind: crate::SessionKind::Project,
                },
            ),
            env(
                2,
                Event::Extension {
                    session,
                    bundle: "hya/counter".to_owned(),
                    kind: "increment".to_owned(),
                    payload: serde_json::json!({ "delta": 3 }),
                },
            ),
            env(
                3,
                Event::Extension {
                    session,
                    bundle: "vendor/unknown".to_owned(),
                    kind: "alpha".to_owned(),
                    payload: serde_json::json!({ "k": 1 }),
                },
            ),
            env(
                4,
                Event::Extension {
                    session,
                    bundle: "hya/counter".to_owned(),
                    kind: "note".to_owned(),
                    payload: serde_json::json!({ "text": "checkpoint" }),
                },
            ),
            env(
                5,
                Event::Extension {
                    session,
                    bundle: "vendor/unknown".to_owned(),
                    kind: "beta".to_owned(),
                    payload: serde_json::json!({ "k": 2 }),
                },
            ),
        ];

        let registry =
            ProjectionReducerRegistry::new().with_reducer(Arc::new(CounterReducer { version: 1 }));
        let folded = Projection::from_events_with(&log, &registry);
        assert_eq!(folded.last_seq, 5);
        assert_eq!(
            folded.extensions.get("hya/counter"),
            Some(&serde_json::json!({
                "count": 3,
                "notes": ["checkpoint"]
            }))
        );
        assert_eq!(
            folded.extensions.get("vendor/unknown"),
            Some(&serde_json::json!([
                { "kind": "alpha", "payload": { "k": 1 } },
                { "kind": "beta", "payload": { "k": 2 } }
            ]))
        );

        // Idempotent by seq: replaying an older seq is a no-op.
        let mut incremental = folded.clone();
        incremental.apply_with(&log[1], &registry);
        assert_eq!(incremental, folded);

        // Without any registered reducers, both bundles are preserved raw.
        let raw = Projection::from_events(&log);
        assert_eq!(
            raw.extensions.get("hya/counter"),
            Some(&serde_json::json!([
                { "kind": "increment", "payload": { "delta": 3 } },
                { "kind": "note", "payload": { "text": "checkpoint" } }
            ]))
        );
        assert_eq!(
            raw.extensions.get("vendor/unknown"),
            folded.extensions.get("vendor/unknown")
        );
    }

    #[test]
    fn snapshot_validates_reducer_signature() {
        let session = SessionId::new();
        let log = vec![env(
            1,
            Event::Extension {
                session,
                bundle: "hya/counter".to_owned(),
                kind: "increment".to_owned(),
                payload: serde_json::json!({ "delta": 2 }),
            },
        )];
        let v1 =
            ProjectionReducerRegistry::new().with_reducer(Arc::new(CounterReducer { version: 1 }));
        let v2 =
            ProjectionReducerRegistry::new().with_reducer(Arc::new(CounterReducer { version: 2 }));
        let p = Projection::from_events_with(&log, &v1);
        let bytes = p.encode_snapshot_with(&v1.signature()).unwrap();
        assert_eq!(
            Projection::decode_snapshot_with(&bytes, &v1.signature()).unwrap(),
            p
        );
        assert!(Projection::decode_snapshot_with(&bytes, &v2.signature()).is_err());
        assert!(Projection::decode_snapshot(&bytes).is_err());
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
