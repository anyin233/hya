#![allow(clippy::expect_used)]

use super::*;
use yaca_proto::{Envelope, EventSeq, FinishReason, MessageId, PartId, Projection, Role};

fn env(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn text_message_events(
    session: SessionId,
    message: MessageId,
    part: PartId,
    text: &str,
) -> [Event; 4] {
    [
        Event::MessageStarted {
            session,
            message,
            role: Role::User,
        },
        Event::TextStart {
            session,
            message,
            part,
        },
        Event::TextDelta {
            session,
            message,
            part,
            delta: text.to_string(),
        },
        Event::MessageFinished {
            session,
            message,
            finish: FinishReason::Stop,
        },
    ]
}

#[test]
fn prefix_events_stop_at_selected_message_and_rewrite_session() {
    let source = SessionId::new();
    let target = SessionId::new();
    let first = MessageId::new();
    let second = MessageId::new();
    let first_part = PartId::new();
    let second_part = PartId::new();
    let mut replay = vec![env(
        1,
        Event::SessionCreated {
            session: source,
            parent: None,
            agent: "build".into(),
            model: "fake".into(),
            workdir: "/tmp".into(),
        },
    )];
    replay.extend(
        text_message_events(source, first, first_part, "one")
            .into_iter()
            .enumerate()
            .map(|(idx, event)| env(u64::try_from(idx).unwrap_or(0) + 2, event)),
    );
    replay.extend(
        text_message_events(source, second, second_part, "two")
            .into_iter()
            .enumerate()
            .map(|(idx, event)| env(u64::try_from(idx).unwrap_or(0) + 6, event)),
    );
    let projection = Projection::from_events(&replay);

    let copied = prefix_events_for_selection(&projection, &replay, 0).expect("prefix");
    assert_eq!(copied.len(), 4);
    assert!(
        copied
            .iter()
            .all(|event| !matches!(event, Event::SessionCreated { .. }))
    );
    let rewritten = copied
        .iter()
        .map(|event| rewrite_session(event, target))
        .collect::<Vec<_>>();
    let fork_events = [env(
        1,
        Event::SessionCreated {
            session: target,
            parent: Some(source),
            agent: "build".into(),
            model: "fake".into(),
            workdir: "/tmp".into(),
        },
    )]
    .into_iter()
    .chain(
        rewritten
            .into_iter()
            .enumerate()
            .map(|(idx, event)| env(u64::try_from(idx).unwrap_or(0) + 2, event)),
    )
    .collect::<Vec<_>>();
    let forked = Projection::from_events(&fork_events);
    assert_eq!(forked.session.id, Some(target));
    assert_eq!(forked.session.parent, Some(source));
    assert_eq!(forked.session.messages.len(), 1);
}
