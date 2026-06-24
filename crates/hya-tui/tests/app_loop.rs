use hya_sdk::{EventPayload, GlobalEvent};
use hya_tui::app::{run, AppEvent};
use hya_tui::state::AppState;
use serde_json::json;
use tokio::sync::mpsc;

#[tokio::test]
async fn run_batches_queued_events_and_drops_heartbeat() {
    let state = AppState::default();
    let (tx, rx) = mpsc::unbounded_channel();

    tx.send(AppEvent::Sse(event(
        "message.updated",
        json!({ "info": { "id": "msg_1", "sessionID": "ses_test", "role": "user", "time": { "created": 1 } } }),
    )))
    .expect("queue message.updated event");
    tx.send(AppEvent::Sse(event("server.heartbeat", json!({}))))
        .expect("queue heartbeat event");
    tx.send(AppEvent::Sse(event(
        "message.part.updated",
        json!({ "part": { "id": "prt_1", "messageID": "msg_1", "sessionID": "ses_test", "type": "text", "text": "hi" } }),
    )))
    .expect("queue message.part.updated event");
    tx.send(AppEvent::Internal("coalesce-probe".into()))
        .expect("queue internal event");
    tx.send(AppEvent::Quit).expect("queue quit event");

    let stats = run(rx, state).await;

    assert_eq!(stats.events_applied, 3);
    assert_eq!(stats.batches, 1);
}

fn event(kind: &str, properties: serde_json::Value) -> GlobalEvent {
    GlobalEvent {
        directory: None,
        project: None,
        workspace: None,
        payload: EventPayload {
            id: None,
            kind: kind.to_string(),
            properties,
        },
    }
}
