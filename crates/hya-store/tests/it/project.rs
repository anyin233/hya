//! Integration tests for `hya-store`: Projects (ADR-0024) — CRUD, root
//! validation, cwd resolution, session membership, and activity queries.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_proto::{
    AgentName, Event, FileChange, FileState, MessageId, ModelRef, ProjectId, SessionId, SessionKind,
};
use hya_store::{SessionStore, StoreError};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

fn temp_db() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir()
        .join(format!(
            "hya-project-{nanos}-{}-{id}.db",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

fn remove_db(path: &str) {
    for suffix in ["", "-wal", "-shm", ".runtime-owner.lock"] {
        let _ = std::fs::remove_file(format!("{path}{suffix}"));
    }
}

fn roots(paths: &[&str]) -> Vec<String> {
    paths.iter().map(|path| (*path).to_string()).collect()
}

async fn tick() {
    tokio::time::sleep(Duration::from_millis(3)).await;
}

async fn create_session(
    store: &SessionStore,
    parent: Option<SessionId>,
    project: Option<ProjectId>,
    kind: SessionKind,
) -> SessionId {
    let session = SessionId::new();
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent,
                agent: AgentName::new("build"),
                model: ModelRef::new("fake"),
                workdir: "/repo".to_string(),
                project,
                kind,
            },
        )
        .await
        .unwrap();
    session
}

async fn raw_pool(path: &str) -> sqlx::SqlitePool {
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))
        .unwrap()
        .foreign_keys(true);
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap()
}

#[tokio::test]
async fn create_get_normalize_and_reject_invalid_projects() {
    let store = SessionStore::connect_memory().await.unwrap();

    // Invalid creations fail with typed errors and persist nothing.
    assert!(matches!(
        store.create_project("p", &[]).await,
        Err(StoreError::ProjectRootsEmpty)
    ));
    assert!(matches!(
        store.create_project("p", &roots(&["relative/dir"])).await,
        Err(StoreError::ProjectRootNotAbsolute { path }) if path == "relative/dir"
    ));
    assert!(matches!(
        store.create_project("p", &roots(&[""])).await,
        Err(StoreError::ProjectRootNotAbsolute { .. })
    ));
    assert!(matches!(
        store.create_project("p", &roots(&["/repo/../etc"])).await,
        Err(StoreError::ProjectRootInvalid { path, .. }) if path == "/repo/../etc"
    ));
    assert!(matches!(
        store.create_project("  ", &roots(&["/repo"])).await,
        Err(StoreError::ProjectNameEmpty)
    ));
    assert!(store.list_projects().await.unwrap().is_empty());

    // Valid creation preserves root order and metadata.
    let project = store
        .create_project("hya", &roots(&["/repo/hya", "/repo/docs", "/data"]))
        .await
        .unwrap();
    assert_eq!(project.name, "hya");
    assert_eq!(project.roots, roots(&["/repo/hya", "/repo/docs", "/data"]));
    assert!(!project.archived);
    assert!(project.created_at_ms > 0);
    assert_eq!(project.created_at_ms, project.updated_at_ms);
    assert!(project.id.to_string().starts_with("prj_"));
    assert_eq!(store.get_project(project.id).await.unwrap(), Some(project));
    assert_eq!(store.get_project(ProjectId::new()).await.unwrap(), None);

    // Roots are normalized and deduplicated while preserving first-seen order.
    let normalized = store
        .create_project(
            "p",
            &roots(&["/repo/hya/", "/repo/./docs", "/repo/hya", "//repo//docs/"]),
        )
        .await
        .unwrap();
    assert_eq!(normalized.roots, roots(&["/repo/hya", "/repo/docs"]));
}

#[tokio::test]
async fn update_project_bumps_timestamp_combines_fields_and_is_atomic() {
    let store = SessionStore::connect_memory().await.unwrap();
    let project = store
        .create_project("old", &roots(&["/a", "/b"]))
        .await
        .unwrap();

    tick().await;
    let renamed = store
        .update_project(project.id, Some("new"), None)
        .await
        .unwrap();
    assert_eq!(renamed.name, "new");
    assert_eq!(renamed.roots, project.roots);
    assert!(renamed.updated_at_ms > project.updated_at_ms);
    assert_eq!(renamed.created_at_ms, project.created_at_ms);

    tick().await;
    let rerooted = store
        .update_project(project.id, None, Some(&roots(&["/c", "/a"])))
        .await
        .unwrap();
    assert_eq!(rerooted.roots, roots(&["/c", "/a"]));
    assert_eq!(rerooted.name, "new");
    assert!(rerooted.updated_at_ms > renamed.updated_at_ms);

    tick().await;
    let both = store
        .update_project(project.id, Some("renamed"), Some(&roots(&["/b", "/c/"])))
        .await
        .unwrap();
    assert_eq!(both.name, "renamed");
    assert_eq!(both.roots, roots(&["/b", "/c"]));
    assert!(both.updated_at_ms > rerooted.updated_at_ms);

    // No-op update returns the unchanged Project.
    let same = store.update_project(project.id, None, None).await.unwrap();
    assert_eq!(same, both);

    // Invalid updates (including one invalid half) fail cleanly without partial mutation.
    assert!(matches!(
        store.update_project(project.id, None, Some(&[])).await,
        Err(StoreError::ProjectRootsEmpty)
    ));
    assert!(matches!(
        store.update_project(project.id, Some(""), None).await,
        Err(StoreError::ProjectNameEmpty)
    ));
    assert!(matches!(
        store
            .update_project(project.id, Some("other"), Some(&roots(&["relative"])))
            .await,
        Err(StoreError::ProjectRootNotAbsolute { .. })
    ));
    assert!(matches!(
        store
            .update_project(project.id, Some("  "), Some(&roots(&["/d"])))
            .await,
        Err(StoreError::ProjectNameEmpty)
    ));
    let unchanged = store.get_project(project.id).await.unwrap().unwrap();
    assert_eq!(unchanged.name, "renamed");
    assert_eq!(unchanged.roots, roots(&["/b", "/c"]));

    let missing = ProjectId::new();
    assert!(matches!(
        store.update_project(missing, Some("x"), None).await,
        Err(StoreError::ProjectNotFound { project }) if project == missing
    ));
    assert!(matches!(
        store
            .update_project(missing, None, Some(&roots(&["/x"])))
            .await,
        Err(StoreError::ProjectNotFound { .. })
    ));
}

#[tokio::test]
async fn list_projects_counts_root_sessions_and_filters_session_listings() {
    let store = SessionStore::connect_memory().await.unwrap();
    let first = store
        .create_project("first", &roots(&["/a"]))
        .await
        .unwrap();
    tick().await;
    let second = store
        .create_project("second", &roots(&["/b"]))
        .await
        .unwrap();
    assert_eq!(store.project_session_count(first.id).await.unwrap(), 0);

    let root = create_session(&store, None, Some(first.id), SessionKind::Project).await;
    let child = create_session(&store, Some(root), Some(first.id), SessionKind::Project).await;
    let other_root = create_session(&store, None, Some(first.id), SessionKind::Project).await;
    let in_second = create_session(&store, None, Some(second.id), SessionKind::Project).await;
    let temp = create_session(&store, None, None, SessionKind::Temporary).await;

    let listed = store.list_projects().await.unwrap();
    let counts: Vec<(ProjectId, u64)> = listed
        .iter()
        .map(|entry| (entry.project.id, entry.session_count))
        .collect();
    // Most recently updated first; subagent sessions are not counted.
    assert_eq!(counts, vec![(second.id, 1), (first.id, 2)]);
    assert_eq!(listed[1].project.roots, roots(&["/a"]));
    assert_eq!(store.project_session_count(first.id).await.unwrap(), 2);

    let ids = |infos: Vec<hya_store::SessionInfo>| {
        let mut ids: Vec<SessionId> = infos.into_iter().map(|info| info.session).collect();
        ids.sort();
        ids
    };
    let mut expect_first = vec![root, child, other_root];
    expect_first.sort();
    assert_eq!(
        ids(store.list_sessions_in(Some(first.id)).await.unwrap()),
        expect_first
    );
    assert_eq!(
        ids(store.list_sessions_in(Some(second.id)).await.unwrap()),
        vec![in_second]
    );
    let mut all = vec![root, child, other_root, in_second, temp];
    all.sort();
    assert_eq!(ids(store.list_sessions_in(None).await.unwrap()), all);
    assert_eq!(
        store.list_sessions_in(None).await.unwrap(),
        store.list_sessions().await.unwrap()
    );

    // A deleted session no longer counts.
    assert!(store.delete_session(root).await.unwrap());
    assert_eq!(store.project_session_count(first.id).await.unwrap(), 1);
    assert_eq!(store.list_projects().await.unwrap()[1].session_count, 1);
}

#[tokio::test]
async fn delete_project_enforces_live_session_guard_and_sqlite_root_cascade() {
    let path = temp_db();
    let store = SessionStore::connect(&path).await.unwrap();
    let p_archived = store
        .create_project("p-archived", &roots(&["/repo", "/docs"]))
        .await
        .unwrap();
    let p_deleted = store
        .create_project("p-deleted", &roots(&["/other"]))
        .await
        .unwrap();
    let s1 = create_session(&store, None, Some(p_archived.id), SessionKind::Project).await;
    let s2 = create_session(&store, None, Some(p_deleted.id), SessionKind::Project).await;

    assert!(matches!(
        store.delete_project(p_archived.id).await,
        Err(StoreError::ProjectInUse { project: id, sessions: 1 }) if id == p_archived.id
    ));
    assert!(store.get_project(p_archived.id).await.unwrap().is_some());

    // Archived or deleted sessions do not block project deletion.
    store
        .append_event(
            s1,
            &Event::SessionArchived {
                session: s1,
                archived: serde_json::Number::from(1_700_000_000_000_i64),
            },
        )
        .await
        .unwrap();
    assert!(store.delete_project(p_archived.id).await.unwrap());
    assert_eq!(store.get_project(p_archived.id).await.unwrap(), None);
    assert_eq!(store.resolve_project_by_path("/repo").await.unwrap(), None);
    assert!(!store.delete_project(p_archived.id).await.unwrap());

    store.delete_session(s2).await.unwrap();
    assert!(store.delete_project(p_deleted.id).await.unwrap());
    assert!(store.list_projects().await.unwrap().is_empty());
    drop(store);

    let pool = raw_pool(&path).await;
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM project_root")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0, "ON DELETE CASCADE removes the roots");
    pool.close().await;
    remove_db(&path);
}

#[tokio::test]
async fn resolve_project_matches_component_boundaries_specificity_recency_and_archive() {
    let path = temp_db();
    let store = SessionStore::connect(&path).await.unwrap();
    let ab = store.create_project("ab", &roots(&["/a/b"])).await.unwrap();

    for inside in ["/a/b", "/a/b/", "/a/b/c", "/a/b/c/d.rs", "/a/b/./c"] {
        assert_eq!(
            store
                .resolve_project_by_path(inside)
                .await
                .unwrap()
                .map(|p| p.id),
            Some(ab.id),
            "{inside}"
        );
    }
    for outside in ["/a/bc", "/a/bc/d", "/a", "/", "/x/a/b"] {
        assert_eq!(
            store.resolve_project_by_path(outside).await.unwrap(),
            None,
            "{outside}"
        );
    }
    assert!(matches!(
        store.resolve_project_by_path("a/b").await,
        Err(StoreError::ProjectRootNotAbsolute { .. })
    ));

    let outer = store
        .create_project("outer", &roots(&["/other", "/work"]))
        .await
        .unwrap();
    tick().await;
    let inner = store
        .create_project("inner", &roots(&["/work/repo"]))
        .await
        .unwrap();
    tick().await;
    let late_outer = store
        .create_project("late-outer", &roots(&["/work"]))
        .await
        .unwrap();

    let resolved = |p: &'static str| {
        let store = store.clone();
        async move {
            store
                .resolve_project_by_path(p)
                .await
                .unwrap()
                .map(|proj| proj.id)
        }
    };
    assert_eq!(resolved("/work/repo/src").await, Some(inner.id));
    assert_eq!(resolved("/work/repo").await, Some(inner.id));
    assert_eq!(resolved("/work/notes").await, Some(late_outer.id));
    tick().await;
    store
        .update_project(outer.id, Some("outer2"), None)
        .await
        .unwrap();
    assert_eq!(resolved("/work/notes").await, Some(outer.id));
    assert_eq!(resolved("/other/x").await, Some(outer.id));

    // Archived projects are skipped by resolve and list_projects, but readable via get_project.
    let pool = raw_pool(&path).await;
    sqlx::query("UPDATE project SET archived = 1 WHERE id = ?")
        .bind(ab.id.to_string())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(store.resolve_project_by_path("/a/b/x").await.unwrap(), None);
    assert!(
        !store
            .list_projects()
            .await
            .unwrap()
            .iter()
            .any(|p| p.project.id == ab.id)
    );
    assert!(store.get_project(ab.id).await.unwrap().unwrap().archived);
    pool.close().await;
    drop(store);
    remove_db(&path);
}

#[tokio::test]
async fn ensure_project_for_path_creates_reuses_nests_and_validates() {
    let store = SessionStore::connect_memory().await.unwrap();

    // Validation errors.
    assert!(matches!(
        store.ensure_project_for_path("relative").await,
        Err(StoreError::ProjectRootNotAbsolute { .. })
    ));
    assert!(matches!(
        store.ensure_project_for_path("/a/../b").await,
        Err(StoreError::ProjectRootInvalid { .. })
    ));

    // Root slash names itself "/".
    let (slash, was_created) = store.ensure_project_for_path("/").await.unwrap();
    assert!(was_created);
    assert_eq!(slash.name, "/");
    assert_eq!(slash.roots, roots(&["/"]));
    assert!(store.delete_project(slash.id).await.unwrap());

    // Creates once by last component, then reuses with unchanged timestamp.
    let (created, was_created) = store.ensure_project_for_path("/work/my-app").await.unwrap();
    assert!(was_created);
    assert_eq!(created.name, "my-app");
    assert_eq!(created.roots, roots(&["/work/my-app"]));
    let (reused, was_created) = store.ensure_project_for_path("/work/my-app").await.unwrap();
    assert!(!was_created);
    assert_eq!(reused.id, created.id);
    assert_eq!(reused.updated_at_ms, created.updated_at_ms);

    // Nested paths reuse containing Project, with longest-root-wins.
    let outer = store
        .create_project("outer", &roots(&["/nest"]))
        .await
        .unwrap();
    let (resolved, was_created) = store
        .ensure_project_for_path("/nest/deep/dir")
        .await
        .unwrap();
    assert!(!was_created);
    assert_eq!(resolved.id, outer.id);
    assert_eq!(resolved.roots, roots(&["/nest"]));

    tick().await;
    let inner = store
        .create_project("inner", &roots(&["/nest/deep"]))
        .await
        .unwrap();
    let (resolved, was_created) = store
        .ensure_project_for_path("/nest/deep/dir")
        .await
        .unwrap();
    assert!(!was_created);
    assert_eq!(resolved.id, inner.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ensure_project_for_path_concurrent_calls_produce_one_project() {
    let db = temp_db();
    let store = std::sync::Arc::new(SessionStore::connect(&db).await.unwrap());
    let path = "/work/concurrent-app";

    let mut handles = Vec::new();
    for _ in 0..8 {
        let store = store.clone();
        handles.push(tokio::spawn(async move {
            store.ensure_project_for_path(path).await.unwrap()
        }));
    }

    let mut ids = std::collections::HashSet::new();
    let mut created_count = 0_u32;
    for handle in handles {
        let (project, was_created) = handle.await.unwrap();
        ids.insert(project.id);
        if was_created {
            created_count += 1;
        }
    }
    assert_eq!(ids.len(), 1);
    assert_eq!(created_count, 1);
    assert_eq!(
        store
            .list_projects()
            .await
            .unwrap()
            .iter()
            .filter(|s| s.project.roots == roots(&[path]))
            .count(),
        1
    );
    drop(store);
    remove_db(&db);
}

fn files_changed(session: SessionId, path: &str, before: FileState) -> Event {
    Event::FilesChanged {
        session,
        message: MessageId::new(),
        call: None,
        files: vec![FileChange {
            path: path.to_string(),
            before,
            observed: false,
        }],
    }
}

fn stored() -> FileState {
    FileState::Stored {
        hash: "h".into(),
        size: 1,
    }
}

#[tokio::test]
async fn project_activity_is_scoped_deduplicated_and_honours_since_limit_and_exclude() {
    let store = SessionStore::connect_memory().await.unwrap();
    let first = store
        .create_project("first", &roots(&["/first"]))
        .await
        .unwrap();
    let other = store
        .create_project("other", &roots(&["/other"]))
        .await
        .unwrap();
    let a = create_session(&store, None, Some(first.id), SessionKind::Project).await;
    let b = create_session(&store, None, Some(first.id), SessionKind::Project).await;
    let outsider = create_session(&store, None, Some(other.id), SessionKind::Project).await;

    let since = hya_proto::now_millis();
    for (session, event) in [
        (a, files_changed(a, "/first/new", FileState::Absent)),
        (a, files_changed(a, "/first/shared", FileState::Absent)),
        (
            outsider,
            files_changed(outsider, "/other/x", FileState::Absent),
        ),
    ] {
        store.append_event(session, &event).await.unwrap();
    }
    tick().await;
    let cutoff = hya_proto::now_millis();
    tick().await;
    store
        .append_event(b, &files_changed(b, "/first/shared", stored()))
        .await
        .unwrap();

    // Scoping, newest-first order, and last-writer ownership.
    let rows = store
        .project_activity_files(first.id, since, 200, None)
        .await
        .unwrap();
    let summary: Vec<_> = rows
        .iter()
        .map(|row| (row.path.as_str(), row.session, row.created))
        .collect();
    assert_eq!(
        summary,
        vec![("/first/shared", b, false), ("/first/new", a, true)]
    );

    // Excluding the last writer falls back to the previous writer.
    let without_b = store
        .project_activity_files(first.id, since, 200, Some(b))
        .await
        .unwrap();
    assert!(without_b.iter().all(|row| row.session == a && row.created));
    assert_eq!(without_b.len(), 2);

    // Since cutoff, limit cap, and session listing since/exclude.
    let recent = store
        .project_activity_files(first.id, cutoff, 200, None)
        .await
        .unwrap();
    assert_eq!(
        recent.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
        ["/first/shared"]
    );
    let sessions = store
        .list_sessions_in_since(first.id, cutoff, 200, None)
        .await
        .unwrap();
    assert_eq!(sessions.iter().map(|s| s.session).collect::<Vec<_>>(), [b]);
    let limited = store
        .project_activity_files(first.id, 0, 1, None)
        .await
        .unwrap();
    assert_eq!(limited.len(), 1);
    let not_b = store
        .list_sessions_in_since(first.id, 0, 200, Some(b))
        .await
        .unwrap();
    assert_eq!(not_b.iter().map(|s| s.session).collect::<Vec<_>>(), [a]);
}
