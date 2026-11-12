//! Reserve direct writers before SQLite opens, or route to the owner server.

use crate::db_lock::{self, Discovery};

/// How a writing command reaches its database.
pub(crate) enum Writer {
    /// An opened store already holding ownership (or an in-memory store).
    Direct(hya_store::SessionStore),
    /// A live server owns the database: go through it.
    Server(Discovery),
}

/// Find out how `hya <command>` writes `db`. Exits 75 when the database is
/// held by a process that serves no HTTP yet.
pub(crate) async fn claim(db: &str, command: &str) -> anyhow::Result<Writer> {
    match hya_app::open_store(db).await {
        Ok(store) => Ok(Writer::Direct(store)),
        Err(error)
            if error
                .downcast_ref::<hya_store::StoreError>()
                .is_some_and(|error| matches!(error, hya_store::StoreError::RuntimeOwnerBusy)) =>
        {
            let busy = db_lock::holder(db)?;
            if let Some(server) = busy.as_ref().and_then(|busy| busy.discovery.clone()) {
                return Ok(Writer::Server(server));
            }
            eprintln!(
                "{}",
                starting_message(command, db, busy.and_then(|busy| busy.holder_pid))
            );
            std::process::exit(db_lock::EXIT_DB_IN_USE);
        }
        Err(error) => Err(error),
    }
}

/// The exit-75 line for a database held by a process that has not
/// published a server yet (shared with `hya sessions archive`).
pub(crate) fn starting_message(command: &str, db: &str, holder_pid: Option<u32>) -> String {
    format!(
        "hya {command}: database {db} is in use by pid {} and it does not serve HTTP yet; try again or stop it",
        holder_pid.map_or_else(|| "unknown".to_string(), |pid| pid.to_string())
    )
}

/// The exit-75 line for a command that cannot go through the server that
/// owns the database.
pub(crate) fn unroutable_message(
    command: &str,
    db: &str,
    server: &Discovery,
    reason: &str,
) -> String {
    format!(
        "hya {command}: database {db} is in use by hya server pid {} at {}, and {reason}; stop it (`hya serve stop --db {db}`) or pass another --db",
        server.pid, server.url
    )
}

/// Print [`unroutable_message`] and exit 75.
pub(crate) fn exit_unroutable(command: &str, db: &str, server: &Discovery, reason: &str) -> ! {
    eprintln!("{}", unroutable_message(command, db, server, reason));
    std::process::exit(db_lock::EXIT_DB_IN_USE);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_stores_are_direct_and_unlocked() {
        for db in ["", ":memory:", "file:x.db?mode=memory"] {
            assert!(matches!(
                claim(db, "exec").await.unwrap(),
                Writer::Direct(_)
            ));
        }
    }

    #[test]
    fn messages_name_the_holder_and_the_way_out() {
        let starting = starting_message("exec", "/d/s.db", Some(42));
        assert_eq!(
            starting,
            "hya exec: database /d/s.db is in use by pid 42 and it does not serve HTTP yet; try again or stop it"
        );
        let server = Discovery {
            url: "http://127.0.0.1:5".into(),
            pid: 7,
            version: "0".into(),
            started_at: 0,
            relay: None,
            allow_hosts: Vec::new(),
        };
        let message = unroutable_message("exec", "/d/s.db", &server, "--pure cannot apply to it");
        assert!(message.contains("pid 7 at http://127.0.0.1:5"), "{message}");
        assert!(message.contains("hya serve stop --db /d/s.db"), "{message}");
    }
}
