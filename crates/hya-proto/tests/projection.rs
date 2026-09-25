//! Projection replay: durable-cursor advancement and serde round-tripping.

#![allow(clippy::expect_used)]

use hya_proto::{
    AgentName, FinishReason, MessageId, ModelRef, PartId, PartProjection, Projection, Role,
    SessionId,
};
use hya_proto::{Envelope, Event, EventSeq};

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

#[test]
fn live_zero_seq_events_apply_without_advancing_durable_cursor() {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let mut projection = Projection::default();

    projection.apply(&env(
        1,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
            agent: None,
            model: None,
        },
    ));
    projection.apply(&env(
        0,
        Event::TextStart {
            session,
            message,
            part,
        },
    ));
    projection.apply(&env(
        0,
        Event::TextDelta {
            session,
            message,
            part,
            delta: "hello".to_string(),
        },
    ));

    let text = projection
        .session
        .messages
        .first()
        .expect("assistant message")
        .parts
        .first()
        .and_then(|part| match part {
            PartProjection::Text { text, .. } => Some(text.as_str()),
            PartProjection::Reasoning { .. } | PartProjection::Tool { .. } => None,
        });

    assert_eq!(text, Some("hello"));
    assert_eq!(projection.last_seq, 1);

    projection.apply(&env(
        2,
        Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        },
    ));
    assert_eq!(projection.last_seq, 2);
}

#[test]
fn reasoning_provider_data_survives_serde_and_projection_replay() {
    let session = SessionId::new();
    let message = MessageId::new();
    let part = PartId::new();
    let provider_data = serde_json::json!({
        "type": "reasoning",
        "id": "rs_123",
        "encrypted_content": "opaque",
    });
    let legacy_start: Event = serde_json::from_value(serde_json::json!({
        "type": "reasoning_start",
        "session": session,
        "message": message,
        "part": part,
    }))
    .expect("legacy reasoning start event");
    assert!(matches!(
        legacy_start,
        Event::ReasoningStart { reason: None, .. }
    ));
    let legacy_end: Event = serde_json::from_value(serde_json::json!({
        "type": "reasoning_end",
        "session": session,
        "message": message,
        "part": part,
    }))
    .expect("legacy reasoning event");
    assert!(matches!(
        legacy_end,
        Event::ReasoningEnd {
            provider_data: None,
            ..
        }
    ));
    let legacy_projection: PartProjection = serde_json::from_value(serde_json::json!({
        "kind": "reasoning",
        "id": part,
        "text": "legacy reasoning",
    }))
    .expect("legacy reasoning projection");
    assert!(matches!(
        legacy_projection,
        PartProjection::Reasoning {
            reason: None,
            provider_data: None,
            ..
        }
    ));

    let log = vec![
        env(
            1,
            Event::MessageStarted {
                session,
                message,
                role: Role::Assistant,
                agent: None,
                model: None,
            },
        ),
        env(
            2,
            Event::ReasoningStart {
                session,
                message,
                part,
                reason: Some("low".to_string()),
            },
        ),
        env(
            3,
            Event::ReasoningDelta {
                session,
                message,
                part,
                delta: "visible summary".to_string(),
            },
        ),
        env(
            4,
            Event::ReasoningEnd {
                session,
                message,
                part,
                provider_data: Some(provider_data.clone()),
            },
        ),
    ];
    let bytes = serde_json::to_vec(&log).expect("serialize reasoning log");
    let decoded: Vec<Envelope> = serde_json::from_slice(&bytes).expect("deserialize reasoning log");
    let projection = Projection::from_events(&decoded);
    let stored = projection.session.messages[0].parts[0].clone();

    assert_eq!(
        stored,
        PartProjection::Reasoning {
            id: part,
            text: "visible summary".to_string(),
            reason: Some("low".to_string()),
            provider_data: Some(provider_data),
        }
    );
}

#[test]
fn session_agent_model_overrides_replay_replace_and_clear() {
    let session = SessionId::new();
    let agent = AgentName::new("general");
    let other = AgentName::new("plan");
    let events = vec![
        env(
            1,
            Event::SessionCreated {
                session,
                parent: None,
                agent: agent.clone(),
                model: ModelRef::new("provider/base"),
                workdir: "/tmp".to_string(),
            },
        ),
        env(
            2,
            Event::SessionAgentModelOverrideSet {
                session,
                agent: agent.clone(),
                model: Some(ModelRef::new("provider/first")),
            },
        ),
        env(
            3,
            Event::SessionAgentModelOverrideSet {
                session,
                agent: other.clone(),
                model: Some(ModelRef::new("provider/other")),
            },
        ),
        env(
            4,
            Event::SessionAgentModelOverrideSet {
                session,
                agent: agent.clone(),
                model: Some(ModelRef::new("provider/replaced")),
            },
        ),
        env(
            5,
            Event::SessionAgentModelOverrideSet {
                session,
                agent,
                model: None,
            },
        ),
    ];
    let bytes = serde_json::to_vec(&events).expect("serialize override log");
    let replayed: Vec<Envelope> = serde_json::from_slice(&bytes).expect("replay override log");
    assert_eq!(replayed, events);
    let projection = Projection::from_events(&replayed);
    assert_eq!(
        projection.session.agent_model_overrides.get("general"),
        None
    );
    assert_eq!(
        projection.session.agent_model_overrides.get("plan"),
        Some(&ModelRef::new("provider/other"))
    );
    assert_eq!(projection.last_seq, 5);
}

#[test]
fn session_permission_mode_replays_last_write_wins() {
    let session = SessionId::new();
    let events = vec![
        env(
            1,
            Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("provider/base"),
                workdir: "/tmp".to_string(),
            },
        ),
        env(
            2,
            Event::SessionPermissionModeSet {
                session,
                mode: "yolo".to_string(),
            },
        ),
        env(
            3,
            Event::SessionPermissionModeSet {
                session,
                mode: "acme/approver/careful".to_string(),
            },
        ),
    ];
    let bytes = serde_json::to_vec(&events).expect("serialize mode log");
    let text = String::from_utf8(bytes.clone()).expect("utf8");
    assert!(text.contains(r#""type":"session_permission_mode_set""#));
    let replayed: Vec<Envelope> = serde_json::from_slice(&bytes).expect("replay mode log");
    assert_eq!(replayed, events);
    let projection = Projection::from_events(&replayed);
    assert_eq!(
        projection.session.permission_mode.as_deref(),
        Some("acme/approver/careful")
    );
    assert_eq!(events[1].event.session(), Some(session));
}

#[test]
fn session_permission_mode_is_omitted_from_the_wire_until_set() {
    let session = SessionId::new();
    let projection = Projection::from_events(&[env(
        1,
        Event::SessionCreated {
            session,
            parent: None,
            agent: AgentName::new("build"),
            model: ModelRef::new("provider/base"),
            workdir: "/tmp".to_string(),
        },
    )]);
    assert_eq!(projection.session.permission_mode, None);
    let encoded = serde_json::to_string(&projection).expect("encode projection");
    assert!(!encoded.contains("permission_mode"), "{encoded}");
    // Projections written before the field existed still decode.
    let decoded: Projection = serde_json::from_str(&encoded).expect("decode projection");
    assert_eq!(decoded, projection);
}

/// An older binary folds the new variant as `Unknown` (a no-op), so a log
/// written by a newer binary still replays.
#[test]
fn permission_mode_event_is_opaque_to_the_unknown_fallback() {
    let json = r#"{"type":"session_permission_mode_set_v2","session":"ses_00000000000000000000000000000001","mode":"yolo"}"#;
    let event: Event = serde_json::from_str(json).expect("unknown future variant decodes");
    assert_eq!(event, Event::Unknown);
}

fn env_at(seq: u64, ts_millis: i64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis,
        event,
    }
}

/// Each assistant message keeps the agent and model its own turn ran with,
/// not the session's current binding: a later `/model` switch leaves older
/// messages attributed to the model that produced them.
#[test]
fn assistant_messages_keep_their_own_agent_and_model_across_switches() {
    let session = SessionId::new();
    let first = MessageId::new();
    let second = MessageId::new();
    let projection = Projection::from_events(&[
        env(
            1,
            Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake/alpha"),
                workdir: "/tmp".to_string(),
            },
        ),
        env(
            2,
            Event::MessageStarted {
                session,
                message: first,
                role: Role::Assistant,
                agent: Some(AgentName::new("build")),
                model: Some(ModelRef::new("fake/alpha")),
            },
        ),
        env(
            3,
            Event::ModelSwitched {
                session,
                message: None,
                model: ModelRef::new("fake/beta"),
            },
        ),
        env(
            4,
            Event::AgentSwitched {
                session,
                message: None,
                agent: AgentName::new("plan"),
            },
        ),
        env(
            5,
            Event::MessageStarted {
                session,
                message: second,
                role: Role::Assistant,
                agent: Some(AgentName::new("plan")),
                model: Some(ModelRef::new("fake/beta")),
            },
        ),
    ]);
    let messages = &projection.session.messages;
    assert_eq!(messages[0].agent, Some(AgentName::new("build")));
    assert_eq!(messages[0].model, Some(ModelRef::new("fake/alpha")));
    assert_eq!(messages[1].agent, Some(AgentName::new("plan")));
    assert_eq!(messages[1].model, Some(ModelRef::new("fake/beta")));
}

/// The model that actually served a message (after fallback or routing, per
/// `UsageRecorded`) wins over the model the turn requested.
#[test]
fn served_model_prefers_the_usage_record_over_the_requested_model() {
    let session = SessionId::new();
    let message = MessageId::new();
    let started = env(
        1,
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
            agent: Some(AgentName::new("build")),
            model: Some(ModelRef::new("fake/alpha")),
        },
    );
    let requested_only = Projection::from_events(std::slice::from_ref(&started));
    assert_eq!(
        requested_only.session.messages[0].served_model(),
        Some(&ModelRef::new("fake/alpha"))
    );

    let served = Projection::from_events(&[
        started,
        env(
            2,
            Event::UsageRecorded {
                session,
                message: Some(message),
                step: Some(0),
                model: ModelRef::new("fake/fallback"),
                purpose: hya_proto::UsagePurpose::Turn,
                tokens: hya_proto::TokenUsage {
                    input: 1,
                    output: 1,
                    ..hya_proto::TokenUsage::default()
                },
            },
        ),
    ]);
    let message = &served.session.messages[0];
    assert_eq!(message.model, Some(ModelRef::new("fake/alpha")));
    assert_eq!(
        message.served_model(),
        Some(&ModelRef::new("fake/fallback"))
    );
}

/// Logs written before `MessageStarted` carried attribution still replay;
/// their messages simply have none.
#[test]
fn legacy_message_started_without_attribution_still_decodes() {
    let json = r#"{"type":"message_started","session":"ses_00000000000000000000000000000001","message":"00000000-0000-0000-0000-000000000002","role":"assistant"}"#;
    let event: Event = serde_json::from_str(json).expect("legacy message_started decodes");
    let Event::MessageStarted { agent, model, .. } = &event else {
        panic!("expected MessageStarted, got {event:?}");
    };
    assert_eq!((agent, model), (&None, &None));
    // Unset attribution stays off the wire.
    let encoded = serde_json::to_string(&event).expect("encode");
    assert!(
        !encoded.contains("agent") && !encoded.contains("model"),
        "{encoded}"
    );
}

/// A message's creation time is its `MessageStarted` envelope time; its
/// update time follows the newest event that changed it (live deltas included).
#[test]
fn message_times_fold_from_envelope_timestamps() {
    let session = SessionId::new();
    let message = MessageId::new();
    let other = MessageId::new();
    let part = PartId::new();
    let mut projection = Projection::from_events(&[
        env_at(
            1,
            1_000,
            Event::MessageStarted {
                session,
                message,
                role: Role::Assistant,
                agent: None,
                model: None,
            },
        ),
        env_at(
            2,
            1_500,
            Event::TextStart {
                session,
                message,
                part,
            },
        ),
    ]);
    assert_eq!(projection.session.messages[0].time_created, Some(1_000));
    assert_eq!(projection.session.messages[0].time_updated, Some(1_500));

    projection.apply(&env_at(
        0,
        1_700,
        Event::TextDelta {
            session,
            message,
            part,
            delta: "hi".to_string(),
        },
    ));
    assert_eq!(projection.session.messages[0].time_updated, Some(1_700));

    // Events about another message, or session-level events, leave it alone.
    projection.apply(&env_at(
        3,
        2_000,
        Event::MessageStarted {
            session,
            message: other,
            role: Role::User,
            agent: None,
            model: None,
        },
    ));
    projection.apply(&env_at(
        4,
        2_100,
        Event::SessionTitled {
            session,
            title: "t".to_string(),
        },
    ));
    assert_eq!(projection.session.messages[0].time_updated, Some(1_700));

    projection.apply(&env_at(
        5,
        3_000,
        Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
            cause: None,
        },
    ));
    let first = &projection.session.messages[0];
    assert_eq!(
        (first.time_created, first.time_updated),
        (Some(1_000), Some(3_000))
    );
    let second = &projection.session.messages[1];
    assert_eq!(
        (second.time_created, second.time_updated),
        (Some(2_000), Some(2_000))
    );
}
