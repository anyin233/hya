//! Opt-in lifecycle benchmark instrumentation; absent from product builds.
use std::ffi::{CStr, c_void};
use std::sync::{Arc, Mutex};

use libsqlite3_sys as ffi;

/// Counts since process start. Only one benchmark may measure at a time.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    /// SQLite statements started, including transaction control.
    pub statements: u64,
    /// Committed transactions containing event appends.
    pub transactions: u64,
    /// Event rows appended in committed transactions.
    pub events: u64,
    /// UTF-8 bytes of committed event JSON payloads.
    pub bytes: u64,
}
static TOTAL: Mutex<Counts> = Mutex::new(Counts {
    statements: 0,
    transactions: 0,
    events: 0,
    bytes: 0,
});

/// Snapshot without querying SQLite (observer reads do not affect counts).
pub fn snapshot() -> Counts {
    *TOTAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct Pending {
    events: u64,
    bytes: u64,
}

// SQLite invokes this on its connection worker, serially. The commit-hook closure
// owns the Arc until connection close; the trace pointer borrows that allocation.
unsafe extern "C" fn trace(
    kind: u32,
    context: *mut c_void,
    statement: *mut c_void,
    _: *mut c_void,
) -> i32 {
    unsafe {
        let pending = &*context.cast::<Mutex<Pending>>();
        let statement = statement.cast::<ffi::sqlite3_stmt>();
        let sql = CStr::from_ptr(ffi::sqlite3_sql(statement)).to_bytes();
        if kind == ffi::SQLITE_TRACE_STMT as u32 {
            TOTAL
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .statements += 1;
        } else if sql.starts_with(b"INSERT INTO event_log ") {
            let expanded = ffi::sqlite3_expanded_sql(statement);
            let text = CStr::from_ptr(expanded).to_string_lossy();
            // The append primitive binds (session_id, payload, ts), in that order.
            let values = text.split_once("VALUES (").map_or("", |(_, v)| v);
            let mut literals = values.split('\'');
            let _ = literals.next();
            let _ = literals.next();
            let _ = literals.next();
            let mut bytes = 0;
            while let Some(fragment) = literals.next() {
                bytes += fragment.len();
                match literals.next() {
                    Some("") => bytes += 1, // SQL escapes a literal quote as ''.
                    _ => break,
                }
            }
            let mut p = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            p.events += 1;
            p.bytes += bytes as u64;
            ffi::sqlite3_free(expanded.cast());
        } else if sql == b"COMMIT" || sql == b"COMMIT;" {
            let mut p = pending
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if p.events > 0 && ffi::sqlite3_get_autocommit(ffi::sqlite3_db_handle(statement)) != 0 {
                let mut total = TOTAL
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                total.transactions += 1;
                total.events += p.events;
                total.bytes += p.bytes;
                *p = Pending::default();
            }
        }
    }
    0
}

pub(crate) async fn install(connection: &mut sqlx::SqliteConnection) -> Result<(), sqlx::Error> {
    let mut handle = connection.lock_handle().await?;
    let pending = Arc::new(Mutex::new(Pending::default()));
    let pointer = Arc::as_ptr(&pending).cast_mut().cast();
    let rollback = pending.clone();
    handle.set_rollback_hook(move || {
        *rollback
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Pending::default();
    });
    handle.set_commit_hook(move || {
        let _keep_alive = &pending;
        true
    });
    unsafe {
        ffi::sqlite3_trace_v2(
            handle.as_raw_handle().as_ptr(),
            (ffi::SQLITE_TRACE_STMT | ffi::SQLITE_TRACE_PROFILE) as u32,
            Some(trace),
            pointer,
        );
    }
    Ok(())
}

/// Open every pool connection before a measured phase; excludes lazy setup noise.
pub async fn warm(store: &crate::SessionStore) -> Result<(), sqlx::Error> {
    let mut connections = Vec::new();
    for _ in 0..8 {
        connections.push(store.pool.acquire().await?);
    }
    drop(connections);
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn counts_committed_payloads_and_discards_rollbacks()
    -> Result<(), Box<dyn std::error::Error>> {
        let path =
            std::env::temp_dir().join(format!("hya-counter-{}.db", hya_proto::SessionId::new()));
        let store = crate::SessionStore::connect(path.to_str().ok_or("path")?).await?;
        super::warm(&store).await?;
        let before_sql = super::snapshot();
        sqlx::raw_sql("SELECT 1; SELECT 2;")
            .execute(&store.pool)
            .await?;
        assert_eq!(super::snapshot().statements - before_sql.statements, 2);
        let session = hya_proto::SessionId::new();
        let event = hya_proto::Event::SessionCreated {
            session,
            parent: None,
            agent: "bench".into(),
            model: "fake".into(),
            workdir: "é's workspace".into(),
            project: None,
            kind: hya_proto::SessionKind::Project,
        };
        let before = super::snapshot();
        store
            .append_events(session, vec![event.clone(), event.clone()])
            .await?;
        let after = super::snapshot();
        assert_eq!(after.transactions - before.transactions, 1);
        assert_eq!(after.events - before.events, 2);
        assert_eq!(
            after.bytes - before.bytes,
            2 * serde_json::to_vec(&event)?.len() as u64
        );
        let mut tx = store.begin_transaction().await?;
        crate::append_event_in_transaction(&mut tx, session, event).await?;
        tx.rollback().await?;
        let rolled_back = super::snapshot();
        assert_eq!(rolled_back.events, after.events);
        assert_eq!(rolled_back.bytes, after.bytes);
        drop(store);
        std::fs::remove_file(path)?;
        Ok(())
    }
}
