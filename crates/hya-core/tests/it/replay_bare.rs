//! Offline verification of request views recorded by `hya exec --bare`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use hya_core::kernel::{self, View, request::AttachmentData};
use hya_proto::{Event, Projection};
use hya_store::SessionStore;

#[tokio::test]
#[ignore = "requires HYA_BARE_REPLAY_DB pointing to a recorded database"]
async fn replay_bare_request_views() {
    let db =
        std::env::var("HYA_BARE_REPLAY_DB").expect("set HYA_BARE_REPLAY_DB to the SQLite path");
    let store = SessionStore::connect_read_only(&db).await.unwrap();
    let tools = kernel::tools::builtin();
    let mut equal = 0;
    let mut unequal = 0;
    for info in store.list_sessions().await.unwrap() {
        let log = store.replay(info.session).await.unwrap();
        let Some(Event::SessionCreated {
            agent,
            model,
            workdir,
            ..
        }) = log.first().map(|envelope| &envelope.event)
        else {
            continue;
        };
        if agent.as_str() != kernel::AGENT
            || log
                .iter()
                .any(|entry| matches!(entry.event, Event::TurnBindingRecorded { .. }))
        {
            continue;
        }
        let system = kernel::prompt::system_prompt(Path::new(workdir));
        for envelope in &log {
            let Event::StepStarted {
                view: Some(recorded),
                ..
            } = &envelope.event
            else {
                continue;
            };
            let end = log.partition_point(|entry| entry.seq <= recorded.cut);
            let projection = Projection::from_events(&log[..end]);
            let view = View {
                projection: &projection,
            };
            let request = kernel::request::request(
                &view,
                &system,
                tools.iter().map(|tool| tool.schema()).collect(),
                model,
                agent,
                &AttachmentData::new(),
                None,
            );
            let digest = kernel::request::request_sha256(&request).unwrap();
            if recorded.cut < envelope.seq
                && recorded.cut == view.cut()
                && digest == recorded.request_sha256
            {
                equal += 1;
            } else {
                unequal += 1;
                eprintln!(
                    "session {} step event {} cut {}: cannot reproduce recorded view",
                    info.session, envelope.seq.0, recorded.cut.0
                );
            }
        }
    }
    println!("equal={equal} unequal={unequal}");
    assert_eq!(unequal, 0);
}
