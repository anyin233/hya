//! Per-session content-addressed file blobs (revert snapshots): stored once
//! per hash, read back byte-exact, sized per session, and removed with the
//! session.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_proto::{AgentName, Event, ModelRef, SessionId};
use hya_store::SessionStore;

async fn session(store: &SessionStore) -> SessionId {
    let session = SessionId::new();
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: "/tmp".to_string(),
            },
        )
        .await
        .unwrap();
    session
}

#[tokio::test]
async fn blobs_are_stored_once_per_session_and_read_back_exactly() {
    let store = SessionStore::connect_memory().await.unwrap();
    let a = session(&store).await;
    let b = session(&store).await;
    let bytes = b"\x00binary\xffcontent".to_vec();

    store.put_file_blob(a, "h1", &bytes).await.unwrap();
    // Idempotent: the same hash again is a no-op.
    store.put_file_blob(a, "h1", &bytes).await.unwrap();

    assert_eq!(store.file_blob(a, "h1").await.unwrap(), Some(bytes.clone()));
    // Blobs are scoped to their session.
    assert_eq!(store.file_blob(b, "h1").await.unwrap(), None);
    assert_eq!(store.file_blob_bytes(a).await.unwrap(), bytes.len() as u64);
    assert_eq!(store.file_blob_bytes(b).await.unwrap(), 0);
}

#[tokio::test]
async fn deleting_a_session_removes_its_blobs() {
    let store = SessionStore::connect_memory().await.unwrap();
    let a = session(&store).await;
    store.put_file_blob(a, "h1", b"one").await.unwrap();
    store.put_file_blob(a, "h2", b"two").await.unwrap();
    assert_eq!(store.file_blob_bytes(a).await.unwrap(), 6);

    assert!(store.delete_session(a).await.unwrap());

    assert_eq!(store.file_blob(a, "h1").await.unwrap(), None);
    assert_eq!(store.file_blob_bytes(a).await.unwrap(), 0);
}
