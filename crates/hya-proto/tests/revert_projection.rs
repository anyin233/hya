//! Revert, unrevert, and commit fold deterministically; file changes fold onto
//! the message that made them; fork records its cut point.

#![allow(clippy::expect_used)]

use hya_proto::{
    Envelope, Event, EventSeq, FileChange, FileRestore, FileState, MessageId, PartId, Projection,
    Role, SessionId, ToolCallId,
};

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

/// `user → assistant` pairs, each user message carrying `text`.
fn turn(session: SessionId, text: &str, seq: &mut u64) -> (MessageId, MessageId, Vec<Envelope>) {
    let user = MessageId::new();
    let assistant = MessageId::new();
    let part = PartId::new();
    let mut out = Vec::new();
    let mut push = |event| {
        *seq += 1;
        out.push(env(*seq, event));
    };
    push(Event::MessageStarted {
        session,
        message: user,
        role: Role::User,
        agent: None,
        model: None,
    });
    push(Event::TextStart {
        session,
        message: user,
        part,
    });
    push(Event::TextDelta {
        session,
        message: user,
        part,
        delta: text.to_string(),
    });
    push(Event::MessageStarted {
        session,
        message: assistant,
        role: Role::Assistant,
        agent: None,
        model: None,
    });
    (user, assistant, out)
}

fn ids(projection: &Projection) -> Vec<MessageId> {
    projection.session.messages.iter().map(|m| m.id).collect()
}

fn stored(hash: &str) -> FileState {
    FileState::Stored {
        hash: hash.to_string(),
        size: 3,
    }
}

#[test]
fn files_changed_folds_onto_the_message_that_changed_them() {
    let session = SessionId::new();
    let mut seq = 0;
    let (_, assistant, mut log) = turn(session, "one", &mut seq);
    let call = ToolCallId::new();
    log.push(env(
        seq + 1,
        Event::FilesChanged {
            session,
            message: assistant,
            call: Some(call),
            files: vec![FileChange {
                path: "/w/a.txt".to_string(),
                before: FileState::Absent,
            }],
        },
    ));
    let projection = Projection::from_events(&log);
    let message = projection.session.messages.last().expect("assistant");
    assert_eq!(message.file_changes.len(), 1);
    assert_eq!(message.file_changes[0].call, Some(call));
    assert_eq!(message.file_changes[0].path, "/w/a.txt");
    assert_eq!(message.file_changes[0].before, FileState::Absent);
}

#[test]
fn revert_hides_the_target_and_later_messages_and_unrevert_restores_them() {
    let session = SessionId::new();
    let mut seq = 0;
    let (u1, a1, mut log) = turn(session, "one", &mut seq);
    let (u2, a2, more) = turn(session, "two", &mut seq);
    log.extend(more);
    let (u3, a3, more) = turn(session, "three", &mut seq);
    log.extend(more);
    let files = vec![FileRestore {
        path: "/w/a.txt".to_string(),
        restored: FileState::Absent,
        saved: stored("h1"),
        error: None,
    }];
    seq += 1;
    log.push(env(
        seq,
        Event::SessionReverted {
            session,
            message: u2,
            files: files.clone(),
        },
    ));
    let reverted = Projection::from_events(&log);
    assert_eq!(ids(&reverted), vec![u1, a1]);
    let revert = reverted.session.revert.as_ref().expect("revert pending");
    assert_eq!(revert.message, u2);
    assert_eq!(
        revert.hidden.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![u2, a2, u3, a3]
    );
    assert_eq!(revert.files, files);

    seq += 1;
    log.push(env(
        seq,
        Event::SessionUnreverted {
            session,
            files: Vec::new(),
        },
    ));
    let restored = Projection::from_events(&log);
    assert_eq!(ids(&restored), vec![u1, a1, u2, a2, u3, a3]);
    assert!(restored.session.revert.is_none());
}

#[test]
fn the_next_message_commits_a_pending_revert() {
    let session = SessionId::new();
    let mut seq = 0;
    let (u1, a1, mut log) = turn(session, "one", &mut seq);
    let (u2, _, more) = turn(session, "two", &mut seq);
    log.extend(more);
    seq += 1;
    log.push(env(
        seq,
        Event::SessionReverted {
            session,
            message: u2,
            files: Vec::new(),
        },
    ));
    let (u4, a4, more) = turn(session, "four", &mut seq);
    log.extend(more);
    let committed = Projection::from_events(&log);
    assert_eq!(ids(&committed), vec![u1, a1, u4, a4]);
    assert!(committed.session.revert.is_none());

    // An unrevert after the commit has nothing to restore.
    seq += 1;
    log.push(env(
        seq,
        Event::SessionUnreverted {
            session,
            files: Vec::new(),
        },
    ));
    assert_eq!(ids(&Projection::from_events(&log)), vec![u1, a1, u4, a4]);
}

#[test]
fn reverting_further_back_extends_the_hidden_range_and_keeps_the_first_saved_state() {
    let session = SessionId::new();
    let mut seq = 0;
    let (u1, a1, mut log) = turn(session, "one", &mut seq);
    let (u2, a2, more) = turn(session, "two", &mut seq);
    log.extend(more);
    seq += 1;
    log.push(env(
        seq,
        Event::SessionReverted {
            session,
            message: u2,
            files: vec![FileRestore {
                path: "/w/a.txt".to_string(),
                restored: stored("before-two"),
                saved: stored("head"),
                error: None,
            }],
        },
    ));
    seq += 1;
    log.push(env(
        seq,
        Event::SessionReverted {
            session,
            message: u1,
            files: vec![
                FileRestore {
                    path: "/w/a.txt".to_string(),
                    restored: FileState::Absent,
                    saved: stored("before-two"),
                    error: None,
                },
                FileRestore {
                    path: "/w/b.txt".to_string(),
                    restored: FileState::Absent,
                    saved: stored("b"),
                    error: None,
                },
            ],
        },
    ));
    let projection = Projection::from_events(&log);
    assert!(projection.session.messages.is_empty());
    let revert = projection.session.revert.expect("revert pending");
    assert_eq!(revert.message, u1);
    assert_eq!(
        revert.hidden.iter().map(|m| m.id).collect::<Vec<_>>(),
        vec![u1, a1, u2, a2]
    );
    // Unrevert must return to the state before the FIRST revert.
    let a = revert
        .files
        .iter()
        .find(|file| file.path == "/w/a.txt")
        .expect("a.txt");
    assert_eq!(a.saved, stored("head"));
    assert_eq!(a.restored, FileState::Absent);
    assert!(revert.files.iter().any(|file| file.path == "/w/b.txt"));
}

#[test]
fn a_log_without_revert_events_folds_as_before() {
    let session = SessionId::new();
    let mut seq = 0;
    let (u1, a1, log) = turn(session, "one", &mut seq);
    let projection = Projection::from_events(&log);
    assert_eq!(ids(&projection), vec![u1, a1]);
    assert!(projection.session.revert.is_none());
    let wire = serde_json::to_value(&projection).expect("encode");
    assert!(wire["session"].get("revert").is_none());
    assert!(wire["session"]["messages"][1].get("file_changes").is_none());
    assert!(wire["session"].get("forked_before").is_none());
}

#[test]
fn session_forked_records_the_cut_point() {
    let session = SessionId::new();
    let source = SessionId::new();
    let cut = MessageId::new();
    let projection = Projection::from_events(&[env(
        1,
        Event::SessionForked {
            session,
            source,
            before_message: Some(cut),
        },
    )]);
    assert_eq!(projection.session.forked_from, Some(source));
    assert_eq!(projection.session.forked_before, Some(cut));
}

#[test]
fn revert_events_round_trip_and_decode_as_unknown_elsewhere() {
    let session = SessionId::new();
    let event = Event::SessionReverted {
        session,
        message: MessageId::new(),
        files: vec![FileRestore {
            path: "/w/a.txt".to_string(),
            restored: FileState::Omitted {
                size: 9_000_000,
                reason: "too_large".to_string(),
            },
            saved: FileState::Absent,
            error: Some("denied".to_string()),
        }],
    };
    let json = serde_json::to_string(&event).expect("encode");
    let back: Event = serde_json::from_str(&json).expect("decode");
    assert_eq!(back, event);
    assert_eq!(event.session(), Some(session));
    let value: serde_json::Value = serde_json::from_str(&json).expect("json");
    assert_eq!(value["type"], "session_reverted");
    assert_eq!(value["files"][0]["restored"]["kind"], "omitted");
}
