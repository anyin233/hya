//! Real-binary round-trip for `hya sessions archive|unarchive` on a direct DB.
//! Filtering/ordering formatting lives in `src/sessions_cmd.rs` unit tests, and
//! live-server routed archive/unarchive is covered in `db_writers.rs`.

use hya_proto::{AgentName, Event, ModelRef, SessionId};
use hya_store::SessionStore;

use crate::common::{Scratch, TestResult, assert_success};

async fn seed(
    db: &std::path::Path,
    parent: Option<SessionId>,
) -> Result<SessionId, Box<dyn std::error::Error>> {
    let store = SessionStore::connect(&db.to_string_lossy()).await?;
    let session = SessionId::new();
    store
        .append_event(
            session,
            &Event::SessionCreated {
                session,
                parent,
                agent: AgentName::new("hya-main"),
                model: ModelRef::new("fake"),
                workdir: "/tmp".into(),
                project: None,
                kind: hya_proto::SessionKind::Project,
            },
        )
        .await?;
    Ok(session)
}

#[tokio::test]
async fn sessions_archive_and_unarchive_round_trip_and_reject_child_or_unknown() -> TestResult {
    let scratch = Scratch::new("hya-sessions-archive")?;
    std::fs::create_dir_all(scratch.0.join("state"))?;
    let db = scratch.db();
    let kept = seed(&db, None).await?.to_string();
    let parked = seed(&db, None).await?;
    let child = seed(&db, Some(parked)).await?.to_string();
    let parked = parked.to_string();

    let run = |args: &[&str]| {
        scratch
            .hya()
            .arg("sessions")
            .args(args)
            .arg("--db")
            .arg(&db)
            .output()
    };

    let out = run(&["archive", &parked])?;
    assert_success("archive", &out);
    assert!(String::from_utf8(out.stdout)?.contains(&format!("archived {parked}")));

    let active = String::from_utf8(run(&[])?.stdout)?;
    assert!(
        active.contains(&kept) && !active.contains(&parked),
        "{active}"
    );
    assert!(String::from_utf8(run(&["--archived"])?.stdout)?.contains(&parked));
    let all = String::from_utf8(run(&["--all"])?.stdout)?;
    assert!(all.contains(&parked) && all.contains(&kept) && all.contains(&child));

    assert!(!run(&["archive", &child])?.status.success());
    assert!(
        !run(&["archive", &SessionId::new().to_string()])?
            .status
            .success()
    );

    let out = run(&["unarchive", &parked])?;
    assert_success("unarchive", &out);
    assert!(String::from_utf8(run(&[])?.stdout)?.contains(&parked));
    Ok(())
}
