//! `hya sessions` (docs/cli.md "`hya sessions`"): list a database's sessions
//! and archive or unarchive root sessions.
//!
//! Listing reads the database directly. `archive`/`unarchive` append the
//! event directly while holding the database lock, or, when a server holds
//! the database (ADR-0022), go through that server's `UpdateSession` so its
//! clients see the change live.

use anyhow::Context as _;
use clap::Subcommand;
use hya_proto::{SessionId, session_archive_event};

use crate::db_writer::{self, Writer};

/// `hya sessions archive|unarchive <id>`.
#[derive(Subcommand, Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionsAction {
    /// Archive a root session: hide it from default session lists (a
    /// running turn still finishes).
    Archive {
        /// Session id (`hysec_...`, `ses_...`, or legacy raw UUID).
        id: String,
    },
    /// Unarchive a session so default lists show it again.
    Unarchive {
        /// Session id (`hysec_...`, `ses_...`, or legacy raw UUID).
        id: String,
    },
}

/// Which sessions `hya sessions` lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Listing {
    /// Non-archived root sessions (the default).
    Active,
    /// Every session (`--all`).
    All,
    /// Only archived roots (`--archived`).
    Archived,
}

/// Run `hya sessions` on the resolved database path `db`.
pub(crate) async fn run(
    db: String,
    action: Option<SessionsAction>,
    listing: Listing,
) -> anyhow::Result<()> {
    match action {
        None => list(&db, listing).await,
        Some(SessionsAction::Archive { id }) => set_archived(&db, &id, true).await,
        Some(SessionsAction::Unarchive { id }) => set_archived(&db, &id, false).await,
    }
}

async fn list(db: &str, listing: Listing) -> anyhow::Result<()> {
    print!("{}", render_list(db, listing).await?);
    Ok(())
}

async fn render_list(db: &str, listing: Listing) -> anyhow::Result<String> {
    use std::fmt::Write as _;
    // A read never persists projection snapshots: another process (the
    // database's server) may own it.
    let store = hya_store::SessionStore::connect_read_only(db)
        .await
        .with_context(|| format!("open existing database read-only at {db}"))?;
    let mut sessions = store.list_sessions().await.context("list sessions")?;
    sessions.sort_by(|left, right| {
        right
            .started_millis
            .cmp(&left.started_millis)
            .then_with(|| left.session.cmp(&right.session))
    });
    let mut out = String::new();
    let mut shown = 0usize;
    for row in sessions {
        let projection = store
            .read_projection_shared(row.session)
            .await
            .with_context(|| format!("read session {}", row.session))?;
        let archived_at = projection.session.archived_at_millis();
        let keep = match listing {
            Listing::Active => projection.session.parent.is_none() && archived_at.is_none(),
            Listing::All => true,
            Listing::Archived => archived_at.is_some(),
        };
        if !keep {
            continue;
        }
        shown += 1;
        let archived = archived_at.map_or_else(String::new, |ms| format!("  archived_ms={ms}"));
        let _ = writeln!(
            out,
            "{}  events={}  started_ms={}{archived}",
            row.session, row.events, row.started_millis
        );
    }
    if shown == 0 {
        let what = match listing {
            Listing::Archived => "no archived sessions",
            Listing::Active | Listing::All => "no sessions",
        };
        let _ = writeln!(out, "{what} found in {db}");
    }
    Ok(out)
}

async fn set_archived(db: &str, id: &str, archived: bool) -> anyhow::Result<()> {
    let session: SessionId = id
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid session id: {id}"))?;
    let verb = if archived { "archived" } else { "unarchived" };
    match db_writer::claim(db, "sessions").await? {
        Writer::Server(server) => {
            update_through_server(&server.url, session, archived).await?;
        }
        Writer::Direct(store) => {
            let mut signals = crate::stop_signals::StopSignals::install()?;
            let work = async {
                store.claim_runtime_owner(hya_store::OwnerRunId::new())?;
                let projection = store.read_projection_shared(session).await?;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |elapsed| {
                        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
                    });
                let event = session_archive_event(&projection, archived, now);
                drop(projection);
                match event {
                    Ok(Some(event)) => {
                        store
                            .append_event(session, &event)
                            .await
                            .context("append archive event")?;
                    }
                    Ok(None) => {}
                    Err(error) => anyhow::bail!("{error}: {session}"),
                }
                Ok::<(), anyhow::Error>(())
            };
            let (outcome, stop) = tokio::select! {
                outcome = work => (outcome, None),
                stop = signals.recv() => (Ok(()), Some(stop)),
            };
            signals.drain_store_on_exit(&store, stop.is_some()).await;
            crate::stop_signals::exit_if_stopped(stop);
            outcome?;
        }
    }
    println!("{verb} {session}");
    Ok(())
}

/// `PATCH <url>/v1/sessions/<id> {"archived": ...}` on the server that
/// holds the database.
async fn update_through_server(
    url: &str,
    session: SessionId,
    archived: bool,
) -> anyhow::Result<()> {
    hya_sdk_v1::V1Sdk::for_local_daemon(url.trim_end_matches('/'), "")?
        .update_session(hya_api::v1::UpdateSessionRequest {
            session: session.to_string(),
            archived: Some(archived),
            ..Default::default()
        })
        .await
        .map_err(|error| match error {
            hya_sdk_v1::SdkError::Api { message, .. } => anyhow::anyhow!("{message}: {session}"),
            error => anyhow::Error::new(error).context(format!("reach the hya server at {url}")),
        })?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use hya_proto::{AgentName, Event, ModelRef};
    use hya_store::SessionStore;

    use super::*;

    fn listed_ids(rendered: &str) -> Vec<String> {
        rendered
            .lines()
            .filter(|line| line.starts_with("hysec_"))
            .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
            .collect()
    }

    async fn seed(store: &SessionStore, parent: Option<SessionId>) -> SessionId {
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
            .await
            .unwrap();
        session
    }

    #[tokio::test]
    async fn render_list_filters_archived_children_and_orders_by_creation_then_id() {
        let dir = std::env::temp_dir().join(format!("hya-sessions-unit-{}", SessionId::new()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("sessions.db");
        let db_str = db.to_str().unwrap();

        let store = SessionStore::connect(db_str).await.unwrap();
        let empty = render_list(db_str, Listing::Active).await.unwrap();
        assert_eq!(empty, format!("no sessions found in {db_str}\n"));
        assert_eq!(
            render_list(db_str, Listing::Archived).await.unwrap(),
            format!("no archived sessions found in {db_str}\n")
        );

        let kept = seed(&store, None).await;
        let parked = seed(&store, None).await;
        let child = seed(&store, Some(parked)).await;
        let parked_proj = store.read_projection_shared(parked).await.unwrap();
        let archive_event = session_archive_event(&parked_proj, true, 1_700_000_000_000)
            .unwrap()
            .unwrap();
        drop(parked_proj);
        store.append_event(parked, &archive_event).await.unwrap();
        // Updating `kept` keeps its creation timestamp ordering intact.
        store
            .append_event(
                kept,
                &Event::SessionTitled {
                    session: kept,
                    title: "updated".into(),
                },
            )
            .await
            .unwrap();
        drop(store);

        assert_eq!(
            listed_ids(&render_list(db_str, Listing::Active).await.unwrap()),
            vec![kept.to_string()]
        );
        let archived = render_list(db_str, Listing::Archived).await.unwrap();
        assert_eq!(listed_ids(&archived), vec![parked.to_string()]);
        assert!(archived.contains("archived_ms=1700000000000"), "{archived}");
        let all = listed_ids(&render_list(db_str, Listing::All).await.unwrap());
        assert!(
            all.contains(&kept.to_string())
                && all.contains(&parked.to_string())
                && all.contains(&child.to_string()),
            "{all:?}"
        );

        let missing = dir.join("does-not-exist.db");
        assert!(
            render_list(missing.to_str().unwrap(), Listing::Active)
                .await
                .is_err()
        );
        assert!(!missing.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
