//! Daemon discovery metadata. The store owns the sole database lock.
//! Discovery trusts PID/URL metadata only while the kernel lock is held.

use std::path::{Path, PathBuf};
use std::time::Duration;

use hya_server::ShutdownReason;
pub(crate) use hya_store::DatabasePaths as DbPaths;
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
    /// The relay the server is joined to (`hya serve --relay`, `hya serve
    /// relay connect`), so `hya serve restart` can rejoin it. Never the
    /// link or any key; absent while not joined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) relay: Option<DiscoveredRelay>,
    /// `--allow-host` names the server accepts besides loopback, so `hya
    /// serve restart` keeps them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) allow_hosts: Vec<String>,
}

/// The relay settings recorded in the discovery file (public values only).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DiscoveredRelay {
    /// The relay's public URL as given (`https://…`, `http://…`).
    pub(crate) proxy_url: String,
    /// `auto`, `grpc`, or `ws`.
    #[serde(default = "auto_transport")]
    pub(crate) transport: String,
    /// Extra trusted CA certificates (absolute path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ca: Option<PathBuf>,
    /// A throwaway identity (no identity file).
    #[serde(default)]
    pub(crate) ephemeral: bool,
    /// `--relay-heartbeat` in seconds, when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) heartbeat_secs: Option<u64>,
}

fn auto_transport() -> String {
    "auto".to_owned()
}

/// The lock and discovery paths of `db`, or `None` for stores that are not
/// locked (in-memory).
pub(crate) fn paths(db: &str) -> Option<DbPaths> {
    // Best-effort paths for optional log/relay display. Ownership callers use
    // the fallible store resolver directly so filesystem errors surface.
    hya_store::database_paths(db).ok().flatten()
}

/// Discovery lifecycle held inside the store runtime-owner claim.
#[derive(Debug)]
pub(crate) struct DbLock {
    paths: DbPaths,
    published: bool,
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

/// Create discovery state only after the store's runtime-owner claim succeeds.
pub(crate) fn owned_metadata(db: &str) -> std::io::Result<Option<DbLock>> {
    let Some(paths) = hya_store::database_paths(db)? else {
        return Ok(None);
    };
    Ok(Some(DbLock {
        paths,
        published: false,
    }))
}

/// Probe the kernel lock before trusting any PID or discovery metadata.
pub(crate) fn holder(db: &str) -> std::io::Result<Option<Busy>> {
    use std::fs::TryLockError;
    use std::io::Read as _;
    let Some(paths) = hya_store::database_paths(db)? else {
        return Ok(None);
    };
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match options.open(&paths.lock) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(with_path(error, &paths.lock)),
    };
    match file.try_lock_shared() {
        Ok(()) => return Ok(None),
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(error)) => return Err(with_path(error, &paths.lock)),
    }
    let mut text = String::new();
    file.read_to_string(&mut text)
        .map_err(|error| with_path(error, &paths.lock))?;
    let pid = text.trim().parse().ok().filter(|pid| *pid > 0);
    let discovery = read_discovery(&paths.discovery).filter(|found| Some(found.pid) == pid);
    // The owner may have released the lock while we read its metadata.
    match file.try_lock_shared() {
        Ok(()) => return Ok(None),
        Err(TryLockError::WouldBlock) => {}
        Err(TryLockError::Error(error)) => return Err(with_path(error, &paths.lock)),
    }
    Ok(Some(Busy {
        db: db.to_owned(),
        holder_pid: pid,
        discovery,
        paths,
    }))
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

/// Why the lock holder was signalled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StopNotice {
    pub(crate) reason: ShutdownReason,
}

/// Write the stop/restart reason atomically before signalling the daemon.
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

    /// Where [`request_stop`] writes (for a shutdown future that cannot
    /// borrow the lock).
    pub(crate) fn stop_request_path(&self) -> PathBuf {
        self.paths.stop.clone()
    }

    /// [`Self::publish_with`] without `--allow-host` names (tests).
    #[cfg(test)]
    pub(crate) fn publish(&mut self, url: &str) -> std::io::Result<Discovery> {
        self.publish_with(url, &[])
    }

    /// Atomically write the discovery file for a server listening on `url`
    /// that accepts the `--allow-host` names `allow_hosts`.
    pub(crate) fn publish_with(
        &mut self,
        url: &str,
        allow_hosts: &[String],
    ) -> std::io::Result<Discovery> {
        let discovery = Discovery {
            url: url.to_string(),
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            started_at: {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |since| {
                        u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
                    })
            },
            relay: None,
            allow_hosts: allow_hosts.to_vec(),
        };
        write_discovery(&self.paths.discovery, &discovery)?;
        self.published = true;
        Ok(discovery)
    }

    /// The discovery file (for [`set_discovery_relay`]).
    pub(crate) fn discovery_path(&self) -> PathBuf {
        self.paths.discovery.clone()
    }
}

/// Rewrite the discovery file at `path` with `relay` (the lock holder only).
/// Does nothing when there is no discovery file yet.
pub(crate) fn set_discovery_relay(
    path: &Path,
    relay: Option<DiscoveredRelay>,
) -> std::io::Result<()> {
    let Some(mut discovery) = read_discovery(path) else {
        return Ok(());
    };
    discovery.relay = relay;
    write_discovery(path, &discovery)
}

/// Write `discovery` to `path` atomically (temporary file, then rename).
fn write_discovery(path: &Path, discovery: &Discovery) -> std::io::Result<()> {
    let mut temp = path.to_path_buf().into_os_string();
    temp.push(format!(".{}.tmp", std::process::id()));
    let temp = PathBuf::from(temp);
    let body = serde_json::to_vec(discovery).map_err(std::io::Error::other)?;
    std::fs::write(&temp, body).map_err(|error| with_path(error, &temp))?;
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(with_path(error, path));
    }
    Ok(())
}

/// [`DbLock::take_stop_request`] for a process `pid` holding the lock whose
/// stop request file is `path`.
pub(crate) fn take_stop_request(path: &Path, pid: u32) -> Option<StopNotice> {
    let text = std::fs::read_to_string(path).ok()?;
    let request: StopRequest = serde_json::from_str(&text).ok()?;
    if request.pid != pid {
        return None;
    }
    // Still under the lock: nobody else takes it.
    let _ = std::fs::remove_file(path);
    let reason = ShutdownReason::parse(&request.reason)?;
    Some(StopNotice { reason })
}

impl Drop for DbLock {
    fn drop(&mut self) {
        // Removed before the store releases runtime ownership.
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

    #[test]
    fn in_memory_stores_are_not_locked() {
        for db in ["", ":memory:"] {
            assert!(paths(db).is_none(), "{db}");
            assert!(owned_metadata(db).unwrap().is_none(), "{db}");
        }
    }

    #[test]
    fn paths_sit_next_to_the_database_with_a_canonical_directory() {
        let scratch = Scratch::new("paths");
        let nested = scratch.0.join("a");
        std::fs::create_dir_all(&nested).unwrap();
        let spelled = format!("{}/../sessions.db", nested.display());
        let found = paths(&spelled).unwrap();
        assert_eq!(found.lock, scratch.0.join("sessions.db.runtime-owner.lock"));
        assert_eq!(found.discovery, scratch.0.join("sessions.db.server.json"));
        assert_eq!(found.log, scratch.0.join("sessions.db.server.log"));
        assert_eq!(found.stop, scratch.0.join("sessions.db.server.stop"));
        assert_eq!(
            found.relay_identity,
            scratch.0.join("sessions.db.relay-identity.json")
        );
    }

    #[test]
    fn discovery_never_creates_or_claims_a_lock() {
        let scratch = Scratch::new("read-only");
        let db = scratch.db();
        assert!(holder(&db).unwrap().is_none());
        assert!(!paths(&db).unwrap().lock.exists());
    }

    #[test]
    fn concurrent_shared_probes_do_not_look_like_a_runtime_owner() {
        let scratch = Scratch::new("shared-probes");
        let db = scratch.db();
        let paths = paths(&db).unwrap();
        let probe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&paths.lock)
            .unwrap();
        std::fs::write(&paths.lock, format!("{}\n", std::process::id())).unwrap();
        probe.try_lock_shared().unwrap();
        assert!(holder(&db).unwrap().is_none());
    }

    #[test]
    fn the_discovery_file_has_the_documented_shape() {
        let scratch = Scratch::new("shape");
        let mut lock = owned_metadata(&scratch.db()).unwrap().unwrap();
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
    fn the_relay_is_recorded_in_the_discovery_file_and_cleared_again() {
        let scratch = Scratch::new("relay");
        let mut lock = owned_metadata(&scratch.db()).unwrap().unwrap();
        let path = lock.discovery_path();
        // Before publishing there is nothing to update.
        set_discovery_relay(&path, Some(DiscoveredRelay::default())).unwrap();
        assert!(!path.exists());
        let published = lock.publish("http://127.0.0.1:9").unwrap();
        let relay = DiscoveredRelay {
            proxy_url: "https://relay.example.com/hya".into(),
            transport: "ws".into(),
            ca: None,
            ephemeral: false,
            heartbeat_secs: None,
        };
        set_discovery_relay(&path, Some(relay.clone())).unwrap();
        let found = read_discovery(&path).unwrap();
        assert_eq!(found.relay.as_ref(), Some(&relay));
        assert_eq!(found.url, published.url);
        let text = std::fs::read_to_string(&path).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value["relay"],
            serde_json::json!({"proxyUrl": "https://relay.example.com/hya", "transport": "ws", "ephemeral": false})
        );
        set_discovery_relay(&path, None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("relay"), "{text}");
    }

    #[tokio::test]
    async fn claiming_removes_a_stale_discovery_file() {
        let scratch = Scratch::new("stale");
        let db = scratch.db();
        let stale = paths(&db).unwrap().discovery;
        std::fs::write(
            &stale,
            r#"{"url":"http://127.0.0.1:1","pid":1,"version":"0","startedAt":1}"#,
        )
        .unwrap();
        let _store = hya_store::SessionStore::connect_runtime(&db).await.unwrap();
        assert!(!stale.exists());
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
