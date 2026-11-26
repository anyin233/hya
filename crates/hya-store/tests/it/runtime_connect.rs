//! Runtime reservation, shared path identity, and read-only connection contracts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::time::Duration;

use hya_proto::{AgentName, Event, ModelRef, SessionId, SessionKind};
use hya_store::{OwnerRunId, SessionStore, database_paths, validate_database_arg};
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
#[tokio::test]
async fn symlink_identity_and_canonical_directory_lock_contention() {
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

    let directory = scratch.0.join("database");
    std::fs::create_dir(&directory).unwrap();
    let dir_alias = scratch.0.join("alias");
    std::os::unix::fs::symlink(&directory, &dir_alias).unwrap();
    let db = directory.join("sessions.db");
    let spelling = dir_alias.join("sessions.db");
    let paths = database_paths(spelling.to_str().unwrap()).unwrap().unwrap();
    assert_eq!(
        paths.database.parent().unwrap(),
        directory.canonicalize().unwrap()
    );
    let first = SessionStore::connect_runtime(db.to_str().unwrap())
        .await
        .unwrap();
    first.claim_runtime_owner(OwnerRunId::new()).unwrap();
    assert!(matches!(
        SessionStore::connect_runtime(spelling.to_str().unwrap()).await,
        Err(hya_store::StoreError::RuntimeOwnerBusy)
    ));
}

#[tokio::test]
async fn transient_discovery_probe_allows_starter_while_live_owner_blocks_spellings() {
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
    std::thread::sleep(Duration::from_millis(20));
    drop(probe);
    let store = starter.join().unwrap().unwrap();
    store.claim_runtime_owner(OwnerRunId::new()).unwrap();
    drop(store);

    let relative_dir = PathBuf::from(format!("hya-owner-spellings-{}", SessionId::new()));
    std::fs::create_dir(&relative_dir).unwrap();
    let rel_scratch = Scratch(relative_dir.canonicalize().unwrap());
    let relative = relative_dir.join("sessions.db");
    let first = SessionStore::connect_runtime(&rel_scratch.db())
        .await
        .unwrap();
    first.claim_runtime_owner(OwnerRunId::new()).unwrap();
    let mut spellings = vec![rel_scratch.db(), relative.to_str().unwrap().to_owned()];
    #[cfg(unix)]
    {
        let alias = rel_scratch.0.join("alias.db");
        std::os::unix::fs::symlink("sessions.db", &alias).unwrap();
        spellings.push(alias.to_str().unwrap().to_owned());
    }
    for spelling in spellings {
        assert!(
            matches!(
                SessionStore::connect_runtime(&spelling).await,
                Err(hya_store::StoreError::RuntimeOwnerBusy)
            ),
            "{spelling}"
        );
    }
}

#[tokio::test]
async fn file_store_write_waits_for_sqlite_lock() {
    let scratch = Scratch::new();
    let store = SessionStore::connect(&scratch.db()).await.unwrap();
    let mut sqlite = sqlx::SqliteConnection::connect(&format!("sqlite://{}", scratch.db()))
        .await
        .unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut sqlite)
        .await
        .unwrap();

    let session = SessionId::new();
    let write = store.append_events(
        session,
        vec![Event::SessionCreated {
            session,
            parent: None,
            agent: AgentName::new("hya-main"),
            model: ModelRef::new("fake"),
            workdir: scratch.0.to_string_lossy().into_owned(),
            project: None,
            kind: SessionKind::Project,
        }],
    );
    tokio::pin!(write);
    tokio::select! {
        biased;
        result = &mut write => panic!("store write finished while the lock was held: {result:?}"),
        () = tokio::time::sleep(Duration::from_millis(60)) => {}
    }
    sqlx::query("COMMIT").execute(&mut sqlite).await.unwrap();

    let appended = write.await.unwrap();
    assert_eq!(store.health(), hya_store::StoreHealth::Healthy);
    let replayed = store.replay(session).await.unwrap();
    assert_eq!(replayed.len(), 1);
    assert_eq!(replayed[0].seq, appended[0].seq);
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
                    archived: 1.into(),
                },
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn database_uri_rejection_lexical_validation_and_literal_punctuation() {
    assert!(validate_database_arg("").is_ok());
    assert!(validate_database_arg(":memory:").is_ok());
    assert!(validate_database_arg("/nonexistent/unlikely/path/hya.db").is_ok());
    for bad in ["file:hya.db", "sqlite:hya.db"] {
        assert_eq!(
            validate_database_arg(bad).unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    let scratch = Scratch::new();
    for uri in [
        format!("sqlite:{}", scratch.db()),
        format!("file:{}", scratch.db()),
        format!("file:{}%3Fmode=memory&mode%2500=rwc", scratch.db()),
        "sqlite::memory:".into(),
        "file::memory:".into(),
        "file:named?mode=memory&cache=shared".into(),
    ] {
        for result in [
            SessionStore::connect(&uri).await,
            SessionStore::connect_runtime(&uri).await,
            SessionStore::connect_read_only(&uri).await,
        ] {
            let error = match result {
                Ok(_) => panic!("database URI accepted: {uri}"),
                Err(error) => error,
            };
            assert!(
                matches!(error,
                hya_store::StoreError::RuntimeOwnerLock { source, .. }
                    if source.kind() == std::io::ErrorKind::InvalidInput
                        && source.to_string().contains("plain filesystem path")),
                "{uri}"
            );
        }
        assert_eq!(std::fs::read_dir(&scratch.0).unwrap().count(), 0);
    }

    let path = scratch.0.join("literal?%23#sessions.db");
    let path = path.to_str().unwrap();
    let store = SessionStore::connect_runtime(path).await.unwrap();
    store.claim_runtime_owner(OwnerRunId::new()).unwrap();
    let session = SessionId::new();
    store
        .append_event(
            session,
            &Event::SessionArchived {
                session,
                archived: 1.into(),
            },
        )
        .await
        .unwrap();
    assert!(std::path::Path::new(path).exists());
    let reader = SessionStore::connect_read_only(path).await.unwrap();
    assert_eq!(reader.replay(session).await.unwrap().len(), 1);
}
