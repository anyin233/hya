//! Per-runtime native watches for the session's durable workspace observations.
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use hya_proto::{Projection, SessionId, event::FileStamp};
use hya_store::SessionStore;

use super::{Emit, file_snapshot};
use crate::error::CoreError;

#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
use {
    native::{Cut, Filter, Monitor, Snapshot, lock},
    std::{
        collections::VecDeque,
        fs, io,
        path::PathBuf,
        sync::Mutex,
        time::{Duration, Instant},
    },
};

/// Shared by the sessions of an engine. Dropping the last handle releases its watches.
#[derive(Default, Clone)]
pub struct WorkspaceWatches {
    #[cfg(target_os = "linux")]
    roots: Arc<Mutex<VecDeque<Arc<Root>>>>,
}

#[cfg(target_os = "linux")]
struct Root {
    path: PathBuf,
    filter: Arc<Filter>,
    monitor: Mutex<WatchState>,
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct WatchState {
    active: Option<Monitor>,
    retry_after: Option<Instant>,
}

#[derive(Default)]
pub(super) struct Batch {
    pub files: Vec<FileStamp>,
    #[cfg(target_os = "linux")]
    cut: Option<Cut>,
}

impl Batch {
    /// Whether a native watch produced this batch.
    #[cfg(target_os = "linux")]
    fn native(&self) -> bool {
        self.cut.is_some()
    }
    #[cfg(not(target_os = "linux"))]
    fn native(&self) -> bool {
        false
    }
    pub fn committed(self) {
        #[cfg(target_os = "linux")]
        if let Some(cut) = self.cut {
            cut.acknowledge();
        }
    }
    pub fn tool_observed(&self, _files: &[FileStamp]) {
        #[cfg(target_os = "linux")]
        if let Some(cut) = &self.cut {
            cut.observe_tool(_files);
        }
    }
}

fn known(
    files: &BTreeMap<String, FileStamp>,
    pending: BTreeMap<String, FileStamp>,
) -> Cow<'_, BTreeMap<String, FileStamp>> {
    if pending.is_empty() {
        return Cow::Borrowed(files);
    }
    let mut known = files.clone();
    known.extend(pending);
    Cow::Owned(known)
}

impl WorkspaceWatches {
    /// Reconcile `session`'s workspace record with its root's native watch
    /// outside a round, as a round does, appending any observation through
    /// `emit`. `pending` holds stamps the running tool call has observed but
    /// not yet appended, so they are not reported as external changes.
    ///
    /// Returns the session's file revision afterwards, or `None` when the root
    /// has no native watch: the record then cannot see new files.
    ///
    /// # Errors
    /// Store or emit failures.
    pub async fn reconcile(
        &self,
        store: &SessionStore,
        emit: &dyn Emit,
        session: SessionId,
        pending: BTreeMap<String, FileStamp>,
    ) -> Result<Option<u64>, CoreError> {
        let projection = store.read_projection_shared(session).await?;
        let mut batch = self
            .changes(store, session, projection, pending, true)
            .await;
        let native = batch.native();
        if let Some(event) =
            file_snapshot::observation(session, true, std::mem::take(&mut batch.files))
        {
            emit.emit(session, vec![event]).await?;
        }
        let revision = store
            .read_projection_shared(session)
            .await?
            .session
            .file_revision;
        batch.committed();
        Ok(native.then_some(revision))
    }

    /// `pending` stamps take precedence over the projection's known files.
    pub(super) async fn changes(
        &self,
        store: &SessionStore,
        session: SessionId,
        projection: Arc<Projection>,
        pending: BTreeMap<String, FileStamp>,
        inventory: bool,
    ) -> Batch {
        #[cfg(target_os = "linux")]
        if let Some(dir) = &projection.session.workdir {
            let path = PathBuf::from(dir);
            let watches = self.clone();
            let db = store.paths().cloned();
            let view = projection.clone();
            let overlay = pending.clone();
            let result = tokio::task::spawn_blocking(move || {
                let path = path.canonicalize()?;
                let root = watches.root(path, db.as_ref());
                let Some(snapshot) = root.snapshot(session)? else {
                    return Ok(None);
                };
                if !inventory && snapshot.paths.is_none() {
                    // A standalone shell turn cannot attribute an entire cold baseline to its command.
                    return Ok::<_, io::Error>(None);
                }
                root.collect(snapshot, &known(&view.session.files, overlay))
                    .map(Some)
            })
            .await;
            match result {
                Ok(Ok(Some(batch))) => return batch,
                Ok(Ok(None)) => {}
                Ok(Err(error)) => {
                    tracing::warn!(workdir=%dir,%error,"workspace watch unavailable; checking known files")
                }
                Err(error) => tracing::warn!(%error,"workspace watch task failed"),
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (store, session, inventory);
        Batch {
            files: file_snapshot::external_changes(&known(&projection.session.files, pending))
                .await,
            #[cfg(target_os = "linux")]
            cut: None,
        }
    }

    /// Fence notifications before taking the tool's own metadata samples. A later
    /// write at the same size/mtime then has a strictly newer watch revision.
    pub(super) async fn tool_cut(&self, dir: &Path, session: SessionId) -> Batch {
        #[cfg(target_os = "linux")]
        {
            let watches = self.clone();
            let dir = dir.to_owned();
            let cut = tokio::task::spawn_blocking(move || {
                let path = dir.canonicalize().ok()?;
                let root = lock(&watches.roots)
                    .iter()
                    .find(|r| r.path == path)?
                    .clone();
                root.snapshot(session).ok().flatten().map(|s| s.cut)
            })
            .await
            .unwrap_or_default();
            Batch {
                cut,
                files: Vec::new(),
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (dir, session);
            Batch::default()
        }
    }

    #[cfg(target_os = "linux")]
    fn root(&self, path: PathBuf, db: Option<&hya_store::DatabasePaths>) -> Arc<Root> {
        // Bound inotify instances in a long-lived daemon. Evicted readers
        // reconcile with their durable W on their next visit.
        const MAX_ROOTS: usize = 16;
        let mut roots = lock(&self.roots);
        let root = roots
            .iter()
            .position(|r| r.path == path)
            .and_then(|i| roots.remove(i))
            .unwrap_or_else(|| {
                Arc::new(Root {
                    filter: Arc::new(Filter::new(path.clone(), db)),
                    path,
                    monitor: Mutex::default(),
                })
            });
        roots.push_front(root.clone());
        // A concurrent reader holds its root through reconciliation. Retain
        // in-use roots so another session cannot start a second watcher there.
        while roots.len() > MAX_ROOTS {
            let Some(index) = roots.iter().rposition(|r| Arc::strong_count(r) == 1) else {
                break;
            };
            roots.remove(index);
        }
        root
    }
}

#[cfg(target_os = "linux")]
impl Root {
    fn snapshot(&self, session: SessionId) -> io::Result<Option<Snapshot>> {
        let mut slot = lock(&self.monitor);
        if slot.retry_after.is_some_and(|t| t > Instant::now()) {
            return Ok(None);
        }
        if let Some(monitor) = slot.active.as_mut() {
            match monitor.snapshot(&self.path, &self.filter, session) {
                Ok(snapshot) => return Ok(Some(snapshot)),
                Err(error) => {
                    tracing::warn!(path=%self.path.display(),%error,"restarting workspace watch")
                }
            }
            slot.active = None;
        }
        slot.retry_after = Some(Instant::now() + Duration::from_secs(30));
        let mut monitor = Monitor::new(&self.path, self.filter.clone())?;
        let snapshot = monitor.snapshot(&self.path, &self.filter, session)?;
        slot.active = Some(monitor);
        slot.retry_after = None;
        Ok(Some(snapshot))
    }

    fn collect(
        &self,
        snapshot: Snapshot,
        known: &BTreeMap<String, FileStamp>,
    ) -> io::Result<Batch> {
        let mut changes = BTreeMap::new();
        let deadline = Instant::now() + Duration::from_secs(30);
        let full = snapshot.paths.is_none();
        let mut polled = snapshot.polled;
        // Only paths newly observed by a tool may have become polling
        // exceptions. An idle round performs no tree walk or per-file stat.
        let candidates: Box<dyn Iterator<Item = &String>> = if full {
            Box::new(known.keys())
        } else {
            Box::new(snapshot.own.keys())
        };
        let extra: std::collections::BTreeSet<_> = candidates
            .filter(|key| !polled.contains(*key) && self.filter.requires_stat(Path::new(key)))
            .cloned()
            .collect();
        polled.extend(extra);
        let paths = snapshot
            .paths
            .unwrap_or_else(|| BTreeMap::from([(self.path.clone(), 0)]));
        for (path, rev) in paths {
            // A former directory may have been renamed, removed or replaced.
            // Its descendants need tombstones even if only the directory was named.
            if let Some(name) = path.to_str() {
                let prefix = format!("{}/", name.trim_end_matches('/'));
                for (key, stamp) in known
                    .range(prefix.clone()..)
                    .take_while(|(k, _)| k.starts_with(&prefix))
                {
                    if stamp.size.is_some()
                        && !polled.contains(key)
                        && self.filter.contains(Path::new(key))
                    {
                        changes
                            .entry(key.clone())
                            .and_modify(|r: &mut u64| *r = (*r).max(rev))
                            .or_insert(rev);
                    }
                }
                if known.get(name).is_some_and(|s| s.size.is_some()) {
                    changes
                        .entry(name.to_owned())
                        .and_modify(|r| *r = (*r).max(rev))
                        .or_insert(rev);
                }
            }
            let mut todo = vec![path];
            while let Some(p) = todo.pop() {
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "workspace inventory exceeded 30s",
                    ));
                }
                if !self.filter.contains(&p) {
                    continue;
                }
                // Do not traverse a symlink that replaced a notified ancestor.
                if p != self.path
                    && p.parent()
                        .and_then(|dir| dir.canonicalize().ok())
                        .as_deref()
                        != p.parent()
                {
                    continue;
                }
                let meta = match fs::symlink_metadata(&p) {
                    Ok(m) => m,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                if meta.is_dir() {
                    let entries = match fs::read_dir(&p) {
                        Ok(entries) => entries,
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                            ) =>
                        {
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    for entry in entries {
                        todo.push(entry?.path());
                    }
                } else if meta.is_file()
                    && let Some(name) = p.to_str()
                {
                    changes
                        .entry(name.to_owned())
                        .and_modify(|r| *r = (*r).max(rev))
                        .or_insert(rev);
                }
            }
        }

        let mut files = Vec::new();
        for (name, rev) in changes {
            let path = Path::new(&name);
            let before = known.get(&name);
            if polled.contains(&name) {
                if !path.canonicalize().is_ok_and(|p| p == path) {
                    continue;
                }
                // An explicitly read symlink may now be a regular file.
                polled.remove(&name);
            }
            let meta = match path.parent().map(fs::canonicalize).transpose() {
                Ok(parent) if parent.as_deref() == path.parent() => fs::symlink_metadata(path),
                Ok(_) => Err(io::ErrorKind::NotFound.into()),
                Err(e) => Err(e),
            };
            if let Some(after) = file_snapshot::stamp_metadata(path, meta)
                && (after.size.is_some() || before.is_some_and(|s| s.size.is_some()))
                && (full
                    || !snapshot.own.get(&name).is_some_and(|(cut, own)| {
                        rev <= *cut && own == &after && before == Some(own)
                    }))
            {
                files.push(after);
            }
        }
        snapshot.cut.set_polled(&polled);
        for name in polled {
            if let Some(before) = known.get(&name)
                && let Some(after) = file_snapshot::stamp_sync(Path::new(&name))
                && after != *before
            {
                files.push(after);
            }
        }
        Ok(Batch {
            files,
            cut: Some(snapshot.cut),
        })
    }
}
