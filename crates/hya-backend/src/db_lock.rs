//! One writer per database (ADR-0022; docs/cli.md "`hya serve`").
//!
//! A server (`hya serve`, or bare `hya`'s in-process server) on a file
//! database holds an exclusive advisory lock (`flock`) on `<db>.lock` for its
//! whole lifetime; the OS releases it when the process exits, crash
//! included. The lock file holds the owner's pid (informational). Once the
//! listener is bound the owner publishes `<db>.server.json`
//! (`{"url","pid","version","startedAt"}`, written to a temporary file and
//! renamed) so other processes can attach to it instead of opening the
//! database themselves; a clean shutdown removes it before the lock is
//! released.
//!
//! The discovery file is trusted only while its lock is held: whoever takes
//! the lock deletes any discovery file it finds (a crashed owner's), and
//! readers that cannot test the lock (the Bun TUI) also check that the pid is
//! alive and that `GET <url>/v1/health` answers.
//!
//! `<db>` is the database path with its directory canonicalized, so different
//! spellings of one file share one lock. In-memory stores (`""`, `:memory:`)
//! and SQLite URIs (`file:…`) are not locked.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use hya_server::ShutdownReason;
use serde::{Deserialize, Serialize};

/// `hya serve`'s exit status when another process holds the database
/// (sysexits `EX_TEMPFAIL`).
pub(crate) const EXIT_DB_IN_USE: i32 = 75;

/// The discovery file a running server publishes next to its database.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Discovery {
    /// Base URL a local client connects to (`http://127.0.0.1:<port>`; an
    /// unspecified bind address is published as loopback).
    pub(crate) url: String,
    pub(crate) pid: u32,
    /// `hya` version of the server.
    pub(crate) version: String,
    /// Unix time in milliseconds when the server started listening.
    pub(crate) started_at: u64,
}

/// `<db>.lock`, `<db>.server.json`, the daemon log `<db>.server.log`, and
/// the stop request `<db>.server.stop` of one database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DbPaths {
    pub(crate) lock: PathBuf,
    pub(crate) discovery: PathBuf,
    /// Where a detached `hya serve` daemon of this database writes its
    /// output (ADR-0023).
    pub(crate) log: PathBuf,
    /// Why `hya serve stop|restart` asked the holder to stop
    /// ([`request_stop`]); read by the server when its SIGTERM arrives.
    pub(crate) stop: PathBuf,
}

/// The lock and discovery paths of `db`, or `None` for stores that are not
/// locked (in-memory, SQLite URIs).
pub(crate) fn paths(db: &str) -> Option<DbPaths> {
    if db.is_empty() || db == ":memory:" || db.starts_with("file:") || db.starts_with("sqlite:") {
        return None;
    }
    let path = Path::new(db);
    let name = path.file_name()?;
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let dir = parent.canonicalize().unwrap_or(parent);
    let with = |suffix: &str| {
        let mut file = name.to_os_string();
        file.push(suffix);
        dir.join(file)
    };
    Some(DbPaths {
        lock: with(".lock"),
        discovery: with(".server.json"),
        log: with(".server.log"),
        stop: with(".server.stop"),
    })
}

/// The held lock of a database; publishes and (on drop) removes its
/// discovery file. Dropping it releases the lock.
#[derive(Debug)]
pub(crate) struct DbLock {
    paths: DbPaths,
    published: bool,
    // Held for the flock; closed (unlocked) last, after `Drop` ran.
    _file: File,
}

/// Another process holds the database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Busy {
    pub(crate) db: String,
    pub(crate) paths: DbPaths,
    /// The pid recorded in the lock file, if readable.
    pub(crate) holder_pid: Option<u32>,
    /// The published discovery file, if the holder is already listening.
    pub(crate) discovery: Option<Discovery>,
}

impl Busy {
    /// The `hya serve` error for a database that is already in use.
    pub(crate) fn serve_message(&self) -> String {
        match &self.discovery {
            Some(found) => format!(
                "hya serve: database {} is already in use by hya server pid {} at {} (hya {}); connect to it (`hya-tui --server {}`, or run bare `hya`, which attaches), stop it, or pass another --db",
                self.db, found.pid, found.url, found.version, found.url
            ),
            None => format!(
                "hya serve: database {} is already in use by pid {} (lock {}); it is still starting or does not serve HTTP; stop it or pass another --db",
                self.db,
                self.holder_pid
                    .map_or_else(|| "unknown".to_string(), |pid| pid.to_string()),
                self.paths.lock.display()
            ),
        }
    }
}

/// The result of trying to take a database's lock.
#[derive(Debug)]
pub(crate) enum Claim {
    /// The store is not locked (in-memory, SQLite URI).
    Unlocked,
    /// This process now owns the database.
    Owned(DbLock),
    /// Another process owns it.
    Busy(Busy),
}

/// Try (without waiting) to take `db`'s lock. On success any discovery file
/// left by a crashed owner is removed and the lock file records this pid.
pub(crate) fn try_claim(db: &str) -> std::io::Result<Claim> {
    let Some(paths) = paths(db) else {
        return Ok(Claim::Unlocked);
    };
    // A missing database directory is an error here (naming the lock path),
    // as it would be for the store itself.
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&paths.lock)
        .map_err(|error| with_path(error, &paths.lock))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut text = String::new();
            let holder_pid = file
                .read_to_string(&mut text)
                .ok()
                .and_then(|_| text.trim().parse().ok());
            let discovery = read_discovery(&paths.discovery);
            return Ok(Claim::Busy(Busy {
                db: db.to_string(),
                paths,
                holder_pid,
                discovery,
            }));
        }
        Err(std::fs::TryLockError::Error(error)) => return Err(with_path(error, &paths.lock)),
    }
    // We hold the lock: any discovery file is a crashed owner's, and any
    // stop request was addressed to an earlier owner.
    for stale in [&paths.discovery, &paths.stop] {
        match std::fs::remove_file(stale) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(with_path(error, stale)),
        }
    }
    file.set_len(0)?;
    file.rewind()?;
    writeln!(file, "{}", std::process::id())?;
    Ok(Claim::Owned(DbLock {
        paths,
        published: false,
        _file: file,
    }))
}

/// Who holds `db`'s lock, found without keeping it: `None` when the lock is
/// free or the store is not locked. (Testing a `flock` means taking it for an
/// instant; a `hya serve` that tries to claim it in that instant exits 75,
/// which every caller of this already retries.)
pub(crate) fn holder(db: &str) -> std::io::Result<Option<Busy>> {
    let Some(paths) = paths(db) else {
        return Ok(None);
    };
    let mut file = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&paths.lock)
    {
        Ok(file) => file,
        // No database directory: nothing can hold it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(with_path(error, &paths.lock)),
    };
    match file.try_lock() {
        // Free: closing the file releases the instant claim.
        Ok(()) => Ok(None),
        Err(std::fs::TryLockError::WouldBlock) => {
            let mut text = String::new();
            let holder_pid = file
                .read_to_string(&mut text)
                .ok()
                .and_then(|_| text.trim().parse().ok());
            Ok(Some(Busy {
                db: db.to_string(),
                discovery: read_discovery(&paths.discovery),
                paths,
                holder_pid,
            }))
        }
        Err(std::fs::TryLockError::Error(error)) => Err(with_path(error, &paths.lock)),
    }
}

fn with_path(error: std::io::Error, path: &Path) -> std::io::Error {
    std::io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

/// `<db>.server.stop`: why `hya serve stop|restart` stopped the holder.
#[derive(Debug, Serialize, Deserialize)]
struct StopRequest {
    /// The lock holder it is addressed to.
    pid: u32,
    /// `stop` or `restart` (`ServerStopping.reason`).
    reason: String,
}

/// Tell the holder `pid` of `paths` why it is about to get SIGTERM (ADR-0023:
/// clients of a stopped server stay disconnected, clients of a restarted
/// one wait for the next). Written atomically before the signal; the server
/// reads it when the signal arrives ([`DbLock::take_stop_request`]). Without
/// it (or when it names another pid) the server reports a plain `signal`.
pub(crate) fn request_stop(
    paths: &DbPaths,
    pid: u32,
    reason: ShutdownReason,
) -> std::io::Result<()> {
    let body = serde_json::to_vec(&StopRequest {
        pid,
        reason: reason.as_str().to_owned(),
    })
    .map_err(std::io::Error::other)?;
    let mut temp = paths.stop.clone().into_os_string();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    std::fs::write(&temp, body).map_err(|error| with_path(error, &temp))?;
    if let Err(error) = std::fs::rename(&temp, &paths.stop) {
        let _ = std::fs::remove_file(&temp);
        return Err(with_path(error, &paths.stop));
    }
    Ok(())
}

/// Parse a discovery file; `None` when missing or malformed.
pub(crate) fn read_discovery(path: &Path) -> Option<Discovery> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The URL a local client should use for a listener on `addr`: an
/// unspecified address (`0.0.0.0`, `::`) becomes loopback.
pub(crate) fn connect_url(addr: std::net::SocketAddr) -> String {
    let ip = match addr.ip() {
        std::net::IpAddr::V4(ip) if ip.is_unspecified() => {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        }
        std::net::IpAddr::V6(ip) if ip.is_unspecified() => {
            std::net::IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)
        }
        ip => ip,
    };
    format!("http://{}", std::net::SocketAddr::new(ip, addr.port()))
}

impl DbLock {
    #[cfg(test)]
    pub(crate) fn paths(&self) -> &DbPaths {
        &self.paths
    }

    /// The stop request addressed to this process, removed once read;
    /// `None` when there is none (a plain signal) or it names another pid.
    #[cfg(test)]
    pub(crate) fn take_stop_request(&self) -> Option<ShutdownReason> {
        take_stop_request(&self.paths.stop, std::process::id())
    }

    /// Where [`request_stop`] writes (for a shutdown future that cannot
    /// borrow the lock).
    pub(crate) fn stop_request_path(&self) -> PathBuf {
        self.paths.stop.clone()
    }

    /// Atomically write the discovery file for a server listening on `url`.
    pub(crate) fn publish(&mut self, url: &str) -> std::io::Result<Discovery> {
        let discovery = Discovery {
            url: url.to_string(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| {
                    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                }),
        };
        let mut temp = self.paths.discovery.clone().into_os_string();
        temp.push(format!(".{}.tmp", std::process::id()));
        let temp = PathBuf::from(temp);
        let body = serde_json::to_vec(&discovery).map_err(std::io::Error::other)?;
        std::fs::write(&temp, body).map_err(|error| with_path(error, &temp))?;
        if let Err(error) = std::fs::rename(&temp, &self.paths.discovery) {
            let _ = std::fs::remove_file(&temp);
            return Err(with_path(error, &self.paths.discovery));
        }
        self.published = true;
        Ok(discovery)
    }
}

/// [`DbLock::take_stop_request`] for a process `pid` holding the lock whose
/// stop request file is `path`.
pub(crate) fn take_stop_request(path: &Path, pid: u32) -> Option<ShutdownReason> {
    let text = std::fs::read_to_string(path).ok()?;
    let request: StopRequest = serde_json::from_str(&text).ok()?;
    if request.pid != pid {
        return None;
    }
    // Still under the lock: nobody else takes it.
    let _ = std::fs::remove_file(path);
    ShutdownReason::parse(&request.reason)
}

impl Drop for DbLock {
    fn drop(&mut self) {
        // Still under the lock, so the file can only be ours.
        if self.published {
            let _ = std::fs::remove_file(&self.paths.discovery);
        }
    }
}

/// Whether `GET <url>/v1/health` answers `200` with `"ok":true` within
/// `timeout` (a plain HTTP/1.1 request; `url` is `http://host:port`).
pub(crate) async fn probe(url: &str, timeout: Duration) -> bool {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let Some(authority) = url
        .strip_prefix("http://")
        .map(|rest| rest.split('/').next().unwrap_or(rest))
        .filter(|authority| !authority.is_empty())
    else {
        return false;
    };
    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect(authority).await.ok()?;
        let request = format!(
            "GET /v1/health HTTP/1.1\r\nHost: {authority}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.ok()?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.ok()?;
        Some(String::from_utf8_lossy(&response).into_owned())
    };
    match tokio::time::timeout(timeout, exchange).await {
        Ok(Some(response)) => {
            let status_ok = response
                .lines()
                .next()
                .is_some_and(|line| line.split_whitespace().nth(1) == Some("200"));
            let body = response.split_once("\r\n\r\n").map_or("", |(_, body)| body);
            status_ok
                && serde_json::from_str::<serde_json::Value>(body.trim()).map_or_else(
                    |_| body.contains("\"ok\":true"),
                    |value| value["ok"] == true,
                )
        }
        _ => false,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "hya-dblock-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.canonicalize().unwrap())
        }

        fn db(&self) -> String {
            self.0.join("sessions.db").to_string_lossy().into_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn owned(claim: Claim) -> DbLock {
        match claim {
            Claim::Owned(lock) => lock,
            other => panic!("expected the lock, got {other:?}"),
        }
    }

    fn busy(claim: Claim) -> Busy {
        match claim {
            Claim::Busy(busy) => busy,
            other => panic!("expected busy, got {other:?}"),
        }
    }

    #[test]
    fn in_memory_and_uri_stores_are_not_locked() {
        for db in ["", ":memory:", "file:x.db?mode=memory", "sqlite::memory:"] {
            assert!(paths(db).is_none(), "{db}");
            assert!(matches!(try_claim(db).unwrap(), Claim::Unlocked), "{db}");
        }
    }

    #[test]
    fn paths_sit_next_to_the_database_with_a_canonical_directory() {
        let scratch = Scratch::new("paths");
        let nested = scratch.0.join("a");
        std::fs::create_dir_all(&nested).unwrap();
        let spelled = format!("{}/../sessions.db", nested.display());
        let found = paths(&spelled).unwrap();
        assert_eq!(found.lock, scratch.0.join("sessions.db.lock"));
        assert_eq!(found.discovery, scratch.0.join("sessions.db.server.json"));
        assert_eq!(found.log, scratch.0.join("sessions.db.server.log"));
        assert_eq!(found.stop, scratch.0.join("sessions.db.server.stop"));
    }

    #[test]
    fn holder_reports_the_lock_owner_without_keeping_the_lock() {
        let scratch = Scratch::new("holder");
        let db = scratch.db();
        assert!(holder(&db).unwrap().is_none(), "a free lock has no holder");
        // Asking did not keep the lock: it can be claimed right after.
        let mut lock = owned(try_claim(&db).unwrap());
        let held = holder(&db).unwrap().expect("held");
        assert_eq!(held.holder_pid, Some(std::process::id()));
        assert_eq!(held.discovery, None);
        let published = lock.publish("http://127.0.0.1:4").unwrap();
        assert_eq!(holder(&db).unwrap().unwrap().discovery, Some(published));
        drop(lock);
        assert!(holder(&db).unwrap().is_none());
        assert!(holder(":memory:").unwrap().is_none());
        let missing = scratch.0.join("no-such-dir/s.db");
        assert!(holder(&missing.to_string_lossy()).unwrap().is_none());
    }

    #[test]
    fn a_second_claim_is_busy_and_names_the_holder_until_the_first_is_dropped() {
        let scratch = Scratch::new("busy");
        let db = scratch.db();
        let mut first = owned(try_claim(&db).unwrap());
        let pending = busy(try_claim(&db).unwrap());
        assert_eq!(pending.holder_pid, Some(std::process::id()));
        assert_eq!(pending.discovery, None);
        assert!(
            pending.serve_message().contains("still starting"),
            "{}",
            pending.serve_message()
        );

        let published = first.publish("http://127.0.0.1:4321").unwrap();
        let listening = busy(try_claim(&db).unwrap());
        assert_eq!(listening.discovery.as_ref(), Some(&published));
        let message = listening.serve_message();
        assert!(message.contains("already in use"), "{message}");
        assert!(message.contains("http://127.0.0.1:4321"), "{message}");
        assert!(
            message.contains(&format!("pid {}", std::process::id())),
            "{message}"
        );

        let discovery = first.paths().discovery.clone();
        drop(first);
        assert!(!discovery.exists(), "drop removes the discovery file");
        let _again = owned(try_claim(&db).unwrap());
    }

    #[test]
    fn the_discovery_file_has_the_documented_shape() {
        let scratch = Scratch::new("shape");
        let mut lock = owned(try_claim(&scratch.db()).unwrap());
        lock.publish("http://127.0.0.1:9").unwrap();
        let text = std::fs::read_to_string(&lock.paths().discovery).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let object = value.as_object().unwrap();
        let mut keys: Vec<_> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["pid", "startedAt", "url", "version"]);
        assert_eq!(value["url"], "http://127.0.0.1:9");
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        // No temporary file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&scratch.0)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn claiming_removes_a_stale_discovery_file() {
        let scratch = Scratch::new("stale");
        let db = scratch.db();
        let stale = paths(&db).unwrap().discovery;
        std::fs::write(
            &stale,
            r#"{"url":"http://127.0.0.1:1","pid":1,"version":"0","startedAt":1}"#,
        )
        .unwrap();
        let _lock = owned(try_claim(&db).unwrap());
        assert!(!stale.exists());
    }

    #[test]
    fn claiming_removes_a_stale_stop_request() {
        let scratch = Scratch::new("stale-stop");
        let db = scratch.db();
        let paths = paths(&db).unwrap();
        request_stop(&paths, std::process::id(), ShutdownReason::Stop).unwrap();
        let lock = owned(try_claim(&db).unwrap());
        assert!(!paths.stop.exists());
        assert_eq!(lock.take_stop_request(), None);
    }

    #[test]
    fn a_stop_request_names_its_pid_and_is_taken_once() {
        let scratch = Scratch::new("stop-request");
        let db = scratch.db();
        let lock = owned(try_claim(&db).unwrap());
        let paths = lock.paths().clone();
        // Addressed to another process (a stale file, pid reuse): ignored.
        request_stop(&paths, std::process::id() + 1, ShutdownReason::Stop).unwrap();
        assert_eq!(lock.take_stop_request(), None);
        request_stop(&paths, std::process::id(), ShutdownReason::Restart).unwrap();
        let text = std::fs::read_to_string(&paths.stop).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["reason"], serde_json::json!("restart"));
        assert_eq!(value["pid"], serde_json::json!(std::process::id()));
        assert_eq!(lock.take_stop_request(), Some(ShutdownReason::Restart));
        assert!(!paths.stop.exists(), "taken requests are removed");
        assert_eq!(lock.take_stop_request(), None);
        // Garbage is ignored.
        std::fs::write(&paths.stop, "not json").unwrap();
        assert_eq!(lock.take_stop_request(), None);
    }

    #[test]
    fn unspecified_bind_addresses_publish_loopback() {
        assert_eq!(
            connect_url("0.0.0.0:80".parse().unwrap()),
            "http://127.0.0.1:80"
        );
        assert_eq!(connect_url("[::]:80".parse().unwrap()), "http://[::1]:80");
        assert_eq!(
            connect_url("127.0.0.1:5".parse().unwrap()),
            "http://127.0.0.1:5"
        );
    }

    #[tokio::test]
    async fn probe_accepts_only_a_healthy_hya_server() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
        async fn serve_once(body: &'static str, status: &'static str) -> String {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0u8; 1024];
                let _ = socket.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
            url
        }
        let healthy = serve_once(r#"{"ok":true,"version":"1"}"#, "200 OK").await;
        assert!(probe(&healthy, Duration::from_secs(2)).await);
        let failing = serve_once(r#"{"ok":false}"#, "500 Internal Server Error").await;
        assert!(!probe(&failing, Duration::from_secs(2)).await);
        // Nothing listening.
        let closed = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            format!("http://{}", listener.local_addr().unwrap())
        };
        assert!(!probe(&closed, Duration::from_secs(2)).await);
        assert!(!probe("not a url", Duration::from_secs(1)).await);
    }
}
