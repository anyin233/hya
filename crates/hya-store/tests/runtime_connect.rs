//! Runtime reservation, shared path identity, and read-only connection contracts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::time::Duration;

use hya_proto::{AgentName, Event, ModelRef, SessionId, SessionKind};
use hya_store::{OwnerRunId, SessionStore, database_paths};
use sqlx::Connection as _;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("hya-store-connect-{}", SessionId::new()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }

    fn db(&self) -> String {
        self.0.join("sessions.db").to_str().unwrap().to_owned()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
#[test]
fn symlink_identity_is_stable_when_the_target_and_its_parent_are_created() {
    let scratch = Scratch::new();
    let target = scratch.0.join("new/sessions.db");
    let alias = scratch.0.join("alias.db");
    std::os::unix::fs::symlink("new/sessions.db", &alias).unwrap();
    let before = database_paths(alias.to_str().unwrap()).unwrap().unwrap();
    assert_eq!(before.database, target);
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "").unwrap();
    assert_eq!(
        database_paths(alias.to_str().unwrap()).unwrap().unwrap(),
        before
    );
    assert_eq!(
        database_paths(target.to_str().unwrap()).unwrap().unwrap(),
        before
    );
}

#[tokio::test]
async fn a_transient_discovery_probe_does_not_reject_a_starter() {
    let scratch = Scratch::new();
    let paths = database_paths(&scratch.db()).unwrap().unwrap();
    let probe = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&paths.lock)
        .unwrap();
    probe.try_lock_shared().unwrap();
    let (probing, started) = std::sync::mpsc::channel();
    let starter = std::thread::spawn(move || {
        probing.send(()).unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(SessionStore::connect_runtime(
                paths.database.to_str().unwrap(),
            ))
    });
    started.recv().unwrap();
    // Model a short discovery lock already in flight when the starter arrives.
    std::thread::sleep(Duration::from_millis(30));
    drop(probe);
    let store = starter.join().unwrap().unwrap();
    store.claim_runtime_owner(OwnerRunId::new()).unwrap();
}

#[tokio::test]
async fn read_only_connect_neither_migrates_nor_writes_projection_snapshots() {
    let scratch = Scratch::new();
    assert!(
        SessionStore::connect_read_only(&scratch.db())
            .await
            .is_err()
    );
    assert!(!std::path::Path::new(&scratch.db()).exists());
    let store = SessionStore::connect_runtime(&scratch.db()).await.unwrap();
    let session = SessionId::new();
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent: None,
                agent: AgentName::new("hya-main"),
                model: ModelRef::new("fake"),
                workdir: scratch.0.to_string_lossy().into_owned(),
                project: None,
                kind: SessionKind::Project,
            },
        )
        .await
        .unwrap();
    let mut sqlite = sqlx::SqliteConnection::connect(&format!("sqlite://{}", scratch.db()))
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut sqlite)
        .await
        .unwrap();
    // An ordinary connect would attempt BEGIN IMMEDIATE and wait behind this writer.
    let reader = tokio::time::timeout(
        Duration::from_secs(1),
        SessionStore::connect_read_only(&scratch.db()),
    )
    .await
    .unwrap()
    .unwrap()
    .with_projection_snapshot_interval(1);
    assert_eq!(reader.read_projection(session).await.unwrap().last_seq, 1);
    assert_eq!(reader.replay(session).await.unwrap().len(), 1);
    sqlx::query("ROLLBACK").execute(&mut sqlite).await.unwrap();
    let snapshots: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM projection_snapshot")
        .fetch_one(&mut sqlite)
        .await
        .unwrap();
    assert_eq!(snapshots, 0);
    assert!(
        reader
            .append_event(
                session,
                &Event::SessionArchived {
                    session,
                    archived: 1.into()
                }
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn runtime_spellings_cannot_take_over_a_live_file_owner() {
    let relative_dir = PathBuf::from(format!("hya-owner-spellings-{}", SessionId::new()));
    std::fs::create_dir(&relative_dir).unwrap();
    let scratch = Scratch(relative_dir.canonicalize().unwrap());
    let relative = relative_dir.join("sessions.db");
    let first = SessionStore::connect_runtime(&scratch.db()).await.unwrap();
    first.claim_runtime_owner(OwnerRunId::new()).unwrap();
    let mut spellings = vec![
        scratch.db(),
        relative.to_str().unwrap().to_owned(),
        format!("sqlite:{}", scratch.db()),
        format!("sqlite://{}", scratch.db()),
        format!("file:{}", scratch.db()),
        format!("file://{}", scratch.db()),
        format!("sqlite:{}", relative.display()),
        format!("file:{}", relative.display()),
    ];
    #[cfg(unix)]
    {
        let alias = scratch.0.join("alias.db");
        std::os::unix::fs::symlink("sessions.db", &alias).unwrap();
        spellings.extend([
            alias.to_str().unwrap().to_owned(),
            format!("sqlite:{}", alias.display()),
            format!("file:{}", alias.display()),
        ]);
    }
    for spelling in spellings {
        match SessionStore::connect_runtime(&spelling).await {
            Ok(second) => assert!(
                second.claim_runtime_owner(OwnerRunId::new()).is_err(),
                "two runtime owners for {} via {spelling}",
                scratch.db()
            ),
            Err(hya_store::StoreError::RuntimeOwnerBusy) => {}
            Err(hya_store::StoreError::RuntimeOwnerLock { source, .. }) => {
                assert_eq!(source.kind(), std::io::ErrorKind::InvalidInput);
            }
            Err(error) => panic!("unexpected error for {spelling}: {error}"),
        }
    }
}

#[tokio::test]
async fn persistent_uri_is_rejected_before_sqlite_initialization() {
    let scratch = Scratch::new();
    for uri in [
        format!("sqlite:{}", scratch.db()),
        format!("file:{}", scratch.db()),
        format!("file:{}%3Fmode=rw?mode=memory", scratch.db()),
        format!("file:{}%3Fmode=memory&mode=rw", scratch.db()),
    ] {
        let error = match SessionStore::connect_runtime(&uri).await {
            Ok(_) => panic!("persistent URI accepted: {uri}"),
            Err(error) => error,
        };
        assert!(matches!(error,
            hya_store::StoreError::RuntimeOwnerLock { source, .. }
                if source.kind() == std::io::ErrorKind::InvalidInput
                    && source.to_string().contains("plain filesystem path")));
        assert!(!PathBuf::from(scratch.db()).exists());
        assert!(
            !database_paths(&scratch.db())
                .unwrap()
                .unwrap()
                .lock
                .exists()
        );
    }
}

#[tokio::test]
async fn explicit_memory_uri_spellings_remain_unlocked() {
    for uri in [
        "sqlite::memory:",
        "sqlite://:memory:",
        "file::memory:",
        "sqlite:%3Amemory%3A",
        "file:%253Amemory%253A",
        "file:named?mode=memory&cache=shared",
        "sqlite:named?m%6fde=mem%6fry&cache=shared",
        "file:named%3Fmode=memory",
    ] {
        assert!(database_paths(uri).unwrap().is_none(), "{uri}");
        let store = SessionStore::connect_runtime(uri).await.unwrap();
        store.claim_runtime_owner(OwnerRunId::new()).unwrap();
    }
}
