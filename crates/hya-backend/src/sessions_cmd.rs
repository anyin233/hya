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
        println!(
            "{}  events={}  started_ms={}{archived}",
            row.session, row.events, row.started_millis
        );
    }
    if shown == 0 {
        let what = match listing {
            Listing::Archived => "no archived sessions",
            Listing::Active | Listing::All => "no sessions",
        };
        println!("{what} found in {db}");
    }
    Ok(())
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
