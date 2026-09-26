//! Integration tests for `hya-store`: Projects (ADR-0024) — CRUD, root
//! validation, cwd resolution, session membership, and the migration that
//! adds `session.project_id` / `session.kind` to an existing database.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hya_proto::{AgentName, Event, ModelRef, ProjectId, SessionId, SessionKind};
use hya_store::{SessionStore, StoreError};
use sqlx::Row;
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
    // `updated_at` has millisecond resolution; keep orderings unambiguous.
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

#[tokio::test]
async fn create_and_get_project_keeps_root_order() {
    let store = SessionStore::connect_memory().await.unwrap();
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

    let fetched = store.get_project(project.id).await.unwrap();
    assert_eq!(fetched, Some(project));
    assert_eq!(store.get_project(ProjectId::new()).await.unwrap(), None);
}

#[tokio::test]
async fn roots_are_normalized_and_deduplicated() {
    let store = SessionStore::connect_memory().await.unwrap();
    let project = store
        .create_project(
            "p",
            &roots(&["/repo/hya/", "/repo/./docs", "/repo/hya", "//repo//docs/"]),
        )
        .await
        .unwrap();
    assert_eq!(project.roots, roots(&["/repo/hya", "/repo/docs"]));
}

#[tokio::test]
async fn invalid_projects_are_rejected_with_typed_errors() {
    let store = SessionStore::connect_memory().await.unwrap();
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
}

#[tokio::test]
async fn rename_and_replace_roots_bump_updated_at() {
    let store = SessionStore::connect_memory().await.unwrap();
    let project = store
        .create_project("old", &roots(&["/a", "/b"]))
        .await
        .unwrap();
    tick().await;
    let renamed = store.rename_project(project.id, "new").await.unwrap();
    assert_eq!(renamed.name, "new");
    assert_eq!(renamed.roots, project.roots);
    assert!(renamed.updated_at_ms > project.updated_at_ms);
    assert_eq!(renamed.created_at_ms, project.created_at_ms);

    tick().await;
    let rerooted = store
        .replace_project_roots(project.id, &roots(&["/c", "/a"]))
        .await
        .unwrap();
    assert_eq!(rerooted.roots, roots(&["/c", "/a"]));
    assert_eq!(rerooted.name, "new");
    assert!(rerooted.updated_at_ms > renamed.updated_at_ms);
    assert_eq!(store.get_project(project.id).await.unwrap(), Some(rerooted));

    assert!(matches!(
        store.replace_project_roots(project.id, &[]).await,
        Err(StoreError::ProjectRootsEmpty)
    ));
    assert!(matches!(
        store.rename_project(project.id, "").await,
        Err(StoreError::ProjectNameEmpty)
    ));
    let missing = ProjectId::new();
    assert!(matches!(
        store.rename_project(missing, "x").await,
        Err(StoreError::ProjectNotFound { project }) if project == missing
    ));
    assert!(matches!(
        store.replace_project_roots(missing, &roots(&["/x"])).await,
        Err(StoreError::ProjectNotFound { .. })
    ));
}

#[tokio::test]
async fn list_projects_counts_root_sessions() {
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
    let root = create_session(&store, None, Some(first.id), SessionKind::Project).await;
    create_session(&store, Some(root), Some(first.id), SessionKind::Project).await;
    create_session(&store, None, Some(first.id), SessionKind::Project).await;
    create_session(&store, None, None, SessionKind::Temporary).await;

    let listed = store.list_projects().await.unwrap();
    let counts: Vec<(ProjectId, u64)> = listed
        .iter()
        .map(|entry| (entry.project.id, entry.session_count))
        .collect();
    // Most recently updated first; subagent sessions are not counted.
    assert_eq!(counts, vec![(second.id, 0), (first.id, 2)]);
    assert_eq!(listed[1].project.roots, roots(&["/a"]));

    // A deleted session no longer counts.
    store.delete_session(root).await.unwrap();
    let listed = store.list_projects().await.unwrap();
    assert_eq!(listed[1].session_count, 1);
}

#[tokio::test]
async fn delete_cascades_roots_and_is_refused_while_live_sessions_reference_it() {
    let store = SessionStore::connect_memory().await.unwrap();
    let project = store
        .create_project("p", &roots(&["/repo", "/docs"]))
        .await
        .unwrap();
    let session = create_session(&store, None, Some(project.id), SessionKind::Project).await;

    assert!(matches!(
        store.delete_project(project.id).await,
        Err(StoreError::ProjectInUse { project: id, sessions: 1 }) if id == project.id
    ));
    assert!(store.get_project(project.id).await.unwrap().is_some());

    // Archived sessions do not block the delete.
    store
        .append_event(
            session,
            &Event::SessionArchived {
                session,
                archived: serde_json::Number::from(1_700_000_000_000_i64),
            },
        )
        .await
        .unwrap();
    assert!(store.delete_project(project.id).await.unwrap());
    assert_eq!(store.get_project(project.id).await.unwrap(), None);
    assert!(store.list_projects().await.unwrap().is_empty());
    // Roots went with it: nothing resolves any more.
    assert_eq!(store.resolve_project_by_path("/repo").await.unwrap(), None);
    // Deleting again reports that nothing was removed.
    assert!(!store.delete_project(project.id).await.unwrap());
}

#[tokio::test]
async fn deleted_sessions_do_not_block_project_delete() {
    let store = SessionStore::connect_memory().await.unwrap();
    let project = store.create_project("p", &roots(&["/repo"])).await.unwrap();
    let session = create_session(&store, None, Some(project.id), SessionKind::Project).await;
    store.delete_session(session).await.unwrap();
    assert!(store.delete_project(project.id).await.unwrap());
}

#[tokio::test]
async fn project_root_cascade_is_enforced_by_sqlite() {
    let path = temp_db();
    let store = SessionStore::connect(&path).await.unwrap();
    let project = store
        .create_project("p", &roots(&["/repo", "/docs"]))
        .await
        .unwrap();
    assert!(store.delete_project(project.id).await.unwrap());
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
async fn resolve_matches_whole_path_components_only() {
    let store = SessionStore::connect_memory().await.unwrap();
    let project = store.create_project("ab", &roots(&["/a/b"])).await.unwrap();

    for inside in ["/a/b", "/a/b/", "/a/b/c", "/a/b/c/d.rs", "/a/b/./c"] {
        assert_eq!(
            store
                .resolve_project_by_path(inside)
                .await
                .unwrap()
                .map(|p| p.id),
            Some(project.id),
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
}

#[tokio::test]
async fn resolve_prefers_the_longest_matching_root_then_most_recent() {
    let store = SessionStore::connect_memory().await.unwrap();
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
    // Created last, but its matching root is shorter than `inner`'s.
    let late_outer = store
        .create_project("late-outer", &roots(&["/work"]))
        .await
        .unwrap();

    let resolved = |path: &'static str| {
        let store = store.clone();
        async move {
            store
                .resolve_project_by_path(path)
                .await
                .unwrap()
                .map(|p| p.id)
        }
    };
    assert_eq!(resolved("/work/repo/src").await, Some(inner.id));
    assert_eq!(resolved("/work/repo").await, Some(inner.id));
    // Same root length: the most recently updated Project wins.
    assert_eq!(resolved("/work/notes").await, Some(late_outer.id));
    tick().await;
    store.rename_project(outer.id, "outer2").await.unwrap();
    assert_eq!(resolved("/work/notes").await, Some(outer.id));
    // A non-primary root matches too.
    assert_eq!(resolved("/other/x").await, Some(outer.id));
}

#[tokio::test]
async fn resolve_skips_archived_projects() {
    let path = temp_db();
    let store = SessionStore::connect(&path).await.unwrap();
    let project = store.create_project("p", &roots(&["/repo"])).await.unwrap();
    let pool = raw_pool(&path).await;
    sqlx::query("UPDATE project SET archived = 1 WHERE id = ?")
        .bind(project.id.to_string())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        store.resolve_project_by_path("/repo/x").await.unwrap(),
        None
    );
    assert!(store.list_projects().await.unwrap().is_empty());
    assert!(
        store
            .get_project(project.id)
            .await
            .unwrap()
            .unwrap()
            .archived
    );
    pool.close().await;
    drop(store);
    remove_db(&path);
}

#[tokio::test]
async fn session_listing_filters_by_project() {
    let store = SessionStore::connect_memory().await.unwrap();
    let a = store.create_project("a", &roots(&["/a"])).await.unwrap();
    let b = store.create_project("b", &roots(&["/b"])).await.unwrap();
    let in_a = create_session(&store, None, Some(a.id), SessionKind::Project).await;
    let child_a = create_session(&store, Some(in_a), Some(a.id), SessionKind::Project).await;
    let in_b = create_session(&store, None, Some(b.id), SessionKind::Project).await;
    let temp = create_session(&store, None, None, SessionKind::Temporary).await;

    let ids = |infos: Vec<hya_store::SessionInfo>| {
        let mut ids: Vec<SessionId> = infos.into_iter().map(|info| info.session).collect();
        ids.sort();
        ids
    };
    let mut expect_a = vec![in_a, child_a];
    expect_a.sort();
    assert_eq!(
        ids(store.list_sessions_in(Some(a.id)).await.unwrap()),
        expect_a
    );
    assert_eq!(
        ids(store.list_sessions_in(Some(b.id)).await.unwrap()),
        vec![in_b]
    );
    let mut all = vec![in_a, child_a, in_b, temp];
    all.sort();
    assert_eq!(ids(store.list_sessions_in(None).await.unwrap()), all);
    assert_eq!(
        store.list_sessions_in(None).await.unwrap(),
        store.list_sessions().await.unwrap()
    );
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

/// A database written before the Project migration: its sessions keep
/// working, read as `kind = project` with no Project, and new sessions record
/// theirs.
#[tokio::test]
async fn project_migration_upgrades_a_database_with_sessions() {
    let path = temp_db();
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{path}"))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let mut migrator = sqlx::migrate!("./migrations");
    let before: Vec<_> = migrator
        .migrations
        .iter()
        .filter(|migration| migration.version < 14)
        .cloned()
        .collect();
    migrator.migrations = before.into();
    migrator.run(&pool).await.unwrap();

    let old = SessionId::new();
    let payload = serde_json::json!({
        "type": "session_created",
        "session": old.to_string(),
        "parent": null,
        "agent": "build",
        "model": "fake",
        "workdir": "/old",
    })
    .to_string();
    sqlx::query("INSERT INTO event_log (session_id, payload, ts) VALUES (?, ?, 1)")
        .bind(old.storage_key())
        .bind(payload)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO session (id, parent_id, agent, model, workdir, title, permission, \
         created_at, updated_at) VALUES (?, NULL, 'build', 'fake', '/old', NULL, '{}', 1, 1)",
    )
    .bind(old.storage_key())
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let store = SessionStore::connect(&path).await.unwrap();
    let projection = store.read_projection(old).await.unwrap();
    assert_eq!(projection.session.project, None);
    assert_eq!(projection.session.kind, SessionKind::Project);
    assert_eq!(store.list_sessions().await.unwrap().len(), 1);

    let project = store.create_project("p", &roots(&["/new"])).await.unwrap();
    let new = create_session(&store, None, Some(project.id), SessionKind::Project).await;
    let temp = create_session(&store, None, None, SessionKind::Temporary).await;
    assert_eq!(
        store
            .list_sessions_in(Some(project.id))
            .await
            .unwrap()
            .into_iter()
            .map(|info| info.session)
            .collect::<Vec<_>>(),
        vec![new]
    );
    drop(store);

    let pool = raw_pool(&path).await;
    let row = |session: SessionId| {
        let pool = pool.clone();
        async move {
            let row = sqlx::query("SELECT project_id, kind FROM session WHERE id = ?")
                .bind(session.storage_key())
                .fetch_one(&pool)
                .await
                .unwrap();
            (
                row.try_get::<Option<String>, _>("project_id").unwrap(),
                row.try_get::<String, _>("kind").unwrap(),
            )
        }
    };
    assert_eq!(row(old).await, (None, "project".to_string()));
    assert_eq!(
        row(new).await,
        (Some(project.id.to_string()), "project".to_string())
    );
    assert_eq!(row(temp).await, (None, "temporary".to_string()));
    pool.close().await;
    remove_db(&path);
}
