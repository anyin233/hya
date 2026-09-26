//! ADR-0026: a Project's saved permission rows go away with the Project;
//! global rows and other Projects' rows stay.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use hya_store::{SavedPermission, SessionStore};

fn row(id: &str, project_id: &str) -> SavedPermission {
    SavedPermission {
        id: id.to_string(),
        project_id: project_id.to_string(),
        action: "externaldirectory".to_string(),
        resource: "/outside/a/*".to_string(),
        time_created_ms: None,
    }
}

#[tokio::test]
async fn deleting_a_project_deletes_its_saved_permissions() {
    let store = SessionStore::connect_memory().await.unwrap();
    let doomed = store
        .create_project("doomed", &["/w/doomed".to_string()])
        .await
        .unwrap();
    let kept = store
        .create_project("kept", &["/w/kept".to_string()])
        .await
        .unwrap();
    for entry in [
        row("psv_doomed", &doomed.id.to_string()),
        row("psv_kept", &kept.id.to_string()),
        row("psv_global", "global"),
    ] {
        store.save_permission(&entry).await.unwrap();
    }

    assert!(store.delete_project(doomed.id).await.unwrap());

    let ids: Vec<String> = store
        .list_saved_permissions(None)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(ids, vec!["psv_global".to_string(), "psv_kept".to_string()]);
}
