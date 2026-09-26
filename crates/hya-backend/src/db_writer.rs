//! Every writer respects the database lock (ADR-0022, ADR-0023; docs/cli.md
//! "Database lock and the backend daemon").
//!
//! A command that writes a file database first asks who owns it:
//!
//! - nobody: the command takes `<db>.lock` itself and holds it until it
//!   exits, so a `hya serve` (a daemon) started meanwhile exits 75 instead of
//!   becoming a second writer;
//! - a server that has published `<db>.server.json`: the command goes through
//!   that server over `/v1` ([`crate::routed`]), or exits 75 when it cannot
//!   be expressed there;
//! - a holder that has not published yet (a server still starting, or
//!   another command-line writer): exit 75.
//!
//! In-memory stores and SQLite URIs are not locked.

use crate::db_lock::{self, Claim, DbLock, Discovery};

/// How a writing command reaches its database.
#[derive(Debug)]
pub(crate) enum Writer {
    /// Open the store in this process. `Some` holds the database lock for as
    /// long as it lives; `None` is an in-memory store (nothing to lock).
    Direct(Option<DbLock>),
    /// A live server owns the database: go through it.
    Server(Discovery),
}

/// Find out how `hya <command>` writes `db`. Exits 75 when the database is
/// held by a process that serves no HTTP yet.
pub(crate) fn claim(db: &str, command: &str) -> anyhow::Result<Writer> {
    match db_lock::try_claim(db)? {
        Claim::Unlocked => Ok(Writer::Direct(None)),
        Claim::Owned(lock) => Ok(Writer::Direct(Some(lock))),
        Claim::Busy(busy) => match busy.discovery {
            Some(server) => Ok(Writer::Server(server)),
            None => {
                eprintln!("{}", starting_message(command, db, busy.holder_pid));
                std::process::exit(db_lock::EXIT_DB_IN_USE);
            }
        },
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

    #[test]
    fn in_memory_stores_are_direct_and_unlocked() {
        for db in ["", ":memory:", "file:x.db?mode=memory"] {
            assert!(matches!(claim(db, "exec").unwrap(), Writer::Direct(None)));
        }
    }

    #[test]
    fn a_free_file_database_is_locked_for_the_writer() {
        let dir = std::env::temp_dir().join(format!(
            "hya-writer-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("s.db").to_string_lossy().into_owned();
        let Writer::Direct(Some(mut lock)) = claim(&db, "exec").unwrap() else {
            panic!("expected the lock");
        };
        assert!(db_lock::holder(&db).unwrap().is_some(), "held while alive");
        // A holder that publishes a server is routed to.
        let published = lock.publish("http://127.0.0.1:9").unwrap();
        let other = std::thread::spawn({
            let db = db.clone();
            move || match claim(&db, "exec").unwrap() {
                Writer::Server(found) => found,
                other => panic!("expected the server, got {other:?}"),
            }
        })
        .join()
        .unwrap();
        assert_eq!(other, published);
        drop(lock);
        assert!(db_lock::holder(&db).unwrap().is_none(), "released on drop");
        let _ = std::fs::remove_dir_all(&dir);
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
