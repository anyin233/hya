#![allow(clippy::unwrap_used)]

use super::*;
use hya_proto::{
    AgentName, Envelope, Event, EventSeq, FinishReason, MessageId, ModelRef, PartId, Role,
};
use hya_store::SessionStore;

async fn append_created(store: &SessionStore, session: SessionId, model: &str, workdir: &str) {
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new(model),
                workdir: workdir.to_string(),
            },
        )
        .await
        .unwrap();
}

async fn append_title(store: &SessionStore, session: SessionId, title: &str) {
    store
        .append_event(
            session,
            &Event::SessionTitled {
                session,
                title: title.to_string(),
            },
        )
        .await
        .unwrap();
}

async fn append_assistant_text(store: &SessionStore, session: SessionId, text: &str) {
    let message = MessageId::new();
    let part = PartId::new();
    for event in [
        Event::MessageStarted {
            session,
            message,
            role: Role::Assistant,
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
        Event::TextEnd {
            session,
            message,
            part,
        },
        Event::MessageFinished {
            session,
            message,
            role: Role::Assistant,
            finish: FinishReason::Stop,
            tokens: None,
        },
    ] {
        store.append_event(session, &event).await.unwrap();
    }
}

fn temp_history() -> HistoryStore {
    HistoryStore::new(std::env::temp_dir().join(format!(
        "hya-session-summaries-test-{}-{}",
        std::process::id(),
        SessionId::new()
    )))
}

fn envelope(seq: u64, event: Event) -> Envelope {
    Envelope {
        seq: EventSeq(seq),
        ts_millis: 0,
        event,
    }
}

fn append_history_created(history: &HistoryStore, session: SessionId, model: &str, workdir: &str) {
    history
        .create_session(session, model, "build", workdir)
        .unwrap();
    history
        .append_envelope(
            session,
            &envelope(
                1,
                Event::SessionCreated {
                    session,
                    parent: None,
                    agent: AgentName::new("build"),
                    model: ModelRef::new(model),
                    workdir: workdir.to_string(),
                },
            ),
        )
        .unwrap();
}

fn append_history_title(history: &HistoryStore, session: SessionId, title: &str) {
    history
        .append_envelope(
            session,
            &envelope(
                2,
                Event::SessionTitled {
                    session,
                    title: title.to_string(),
                },
            ),
        )
        .unwrap();
}

#[tokio::test]
async fn session_summaries_use_db_title_as_primary_label() {
    let store = SessionStore::connect_memory().await.unwrap();
    let history = temp_history();
    let session = SessionId::new();
    append_created(&store, session, "fake", "/db-workdir").await;
    append_title(&store, session, "DB title").await;

    let summaries = session_summaries_from_store(&history, &store).await;

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].id, session.to_string());
    assert_eq!(summaries[0].title, "DB title");
    assert_eq!(summaries[0].detail, "fake · /db-workdir");
}

#[tokio::test]
async fn session_summaries_import_json_history_once_before_listing() {
    let store = SessionStore::connect_memory().await.unwrap();
    let history = temp_history();
    let session = SessionId::new();
    append_history_created(&history, session, "fake", "/json-workdir");
    append_history_title(&history, session, "JSON title");

    let first = session_summaries_from_store(&history, &store).await;
    let event_count = store.replay(session).await.unwrap().len();
    let second = session_summaries_from_store(&history, &store).await;

    assert_eq!(first.len(), 1);
    assert_eq!(first[0].title, "JSON title");
    assert_eq!(second[0].title, "JSON title");
    assert_eq!(event_count, 2);
    assert_eq!(store.replay(session).await.unwrap().len(), event_count);
}

#[tokio::test]
async fn session_summaries_include_json_only_session_when_db_has_other_sessions() {
    let store = SessionStore::connect_memory().await.unwrap();
    let history = temp_history();
    let db_session = SessionId::new();
    let json_session = SessionId::new();
    append_created(&store, db_session, "fake", "/db-workdir").await;
    append_title(&store, db_session, "DB title").await;
    append_history_created(&history, json_session, "fake", "/json-workdir");
    append_history_title(&history, json_session, "JSON title");

    let summaries = session_summaries_from_store(&history, &store).await;
    let titles = summaries
        .iter()
        .map(|summary| summary.title.as_str())
        .collect::<Vec<_>>();

    assert_eq!(summaries.len(), 2);
    assert!(titles.contains(&"DB title"));
    assert!(titles.contains(&"JSON title"));
}

#[tokio::test]
async fn session_summaries_prefer_db_title_when_json_has_same_id() {
    let store = SessionStore::connect_memory().await.unwrap();
    let history = temp_history();
    let session = SessionId::new();
    append_created(&store, session, "fake", "/db-workdir").await;
    append_title(&store, session, "DB title").await;
    append_history_created(&history, session, "fake", "/json-workdir");
    append_history_title(&history, session, "JSON stale title");

    let summaries = session_summaries_from_store(&history, &store).await;

    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].title, "DB title");
    assert_eq!(store.replay(session).await.unwrap().len(), 2);
}

#[tokio::test]
async fn session_summaries_do_not_use_assistant_text_as_fallback_title() {
    let store = SessionStore::connect_memory().await.unwrap();
    let history = temp_history();
    let session = SessionId::new();
    append_created(&store, session, "fake", "/db-workdir").await;
    append_assistant_text(&store, session, "assistant stream text").await;

    let summaries = session_summaries_from_store(&history, &store).await;

    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].title.starts_with("Untitled Session_"));
    assert_ne!(summaries[0].title, "assistant stream text");
}

#[test]
fn session_summaries_parse_hysec_and_legacy_resume_ids() {
    let hysec = SessionId::new();
    assert_eq!(resume_session_id(&hysec.to_string()), Some(hysec));

    let uuid = uuid::Uuid::now_v7();
    assert_eq!(
        resume_session_id(&uuid.to_string()),
        Some(SessionId::from_uuid(uuid))
    );

    assert_eq!(resume_session_id("not-a-session"), None);
}
