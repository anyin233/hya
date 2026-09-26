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

use crate::db_lock::{self, Claim};

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
    /// Every session except archived roots (the default).
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
    let store = hya_app::open_store(db)
        .await?
        .with_projection_snapshot_interval(u64::MAX);
    let sessions = store.list_sessions().await.context("list sessions")?;
    let mut shown = 0usize;
    for row in sessions {
        let projection = store
            .read_projection_shared(row.session)
            .await
            .with_context(|| format!("read session {}", row.session))?;
        let archived_at = projection.session.archived_at_millis();
        let keep = match listing {
            Listing::Active => archived_at.is_none(),
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
    match db_lock::try_claim(db)? {
        Claim::Busy(busy) => {
            let Some(server) = busy.discovery else {
                eprintln!(
                    "{}",
                    crate::db_writer::starting_message("sessions", db, busy.holder_pid)
                );
                std::process::exit(db_lock::EXIT_DB_IN_USE);
            };
            update_through_server(&server.url, session, archived).await?;
        }
        // Held (or not lockable) for the whole write; dropped on return.
        claim @ (Claim::Owned(_) | Claim::Unlocked) => {
            let store = hya_app::open_store(db).await?;
            let projection = store.read_projection(session).await?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |elapsed| {
                    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
                });
            match session_archive_event(&projection, archived, now) {
                Ok(Some(event)) => {
                    store
                        .append_event(session, &event)
                        .await
                        .context("append archive event")?;
                }
                Ok(None) => {}
                Err(error) => anyhow::bail!("{error}: {session}"),
            }
            drop(claim);
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
    let response = reqwest::Client::new()
        .patch(format!(
            "{}/v1/sessions/{session}",
            url.trim_end_matches('/')
        ))
        .json(&serde_json::json!({ "archived": archived }))
        .send()
        .await
        .with_context(|| format!("reach the hya server at {url}"))?;
    if response.status().is_success() {
        return Ok(());
    }
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or_default();
    let message = body["error"]["message"]
        .as_str()
        .map_or_else(|| status.to_string(), str::to_owned);
    anyhow::bail!("{message}: {session}")
}
