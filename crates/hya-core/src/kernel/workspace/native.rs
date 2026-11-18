//! Linux orders a private marker on the same inotify instance as the root.
//! Callback work is bounded by distinct dirty names, even with idle readers.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

use hya_proto::SessionId;
use hya_proto::event::FileStamp;
use hya_store::DatabasePaths;
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

const MAX_DIRTY: usize = 8192;
const MAX_READERS: usize = 256;

pub(super) fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Default)]
struct Journal {
    rev: u64,
    floor: u64,
    broken: bool,
    topology: BTreeSet<PathBuf>,
    paths: BTreeMap<PathBuf, u64>,
    readers: HashMap<SessionId, Reader>,
}

#[derive(Default)]
struct Reader {
    polled: BTreeSet<String>,
    cut: Option<u64>,
    own: HashMap<String, (u64, FileStamp)>,
}

pub(super) struct Monitor {
    watcher: RecommendedWatcher,
    directories: BTreeSet<PathBuf>,
    marker: fs::File,
    receipts: mpsc::Receiver<()>,
    root_id: (u64, u64),
    journal: Arc<Mutex<Journal>>,
}

#[derive(Clone)]
pub(super) struct Cut {
    journal: Arc<Mutex<Journal>>,
    session: SessionId,
    rev: u64,
}

impl Cut {
    pub(super) fn acknowledge(self) {
        let mut j = lock(&self.journal);
        let reader = j.readers.entry(self.session).or_default();
        reader.cut = Some(reader.cut.unwrap_or_default().max(self.rev));
        reader.own.retain(|_, (rev, _)| *rev > self.rev);
    }

    pub(super) fn set_polled(&self, paths: &BTreeSet<String>) {
        lock(&self.journal)
            .readers
            .entry(self.session)
            .or_default()
            .polled
            .clone_from(paths);
    }

    pub(super) fn observe_tool(&self, files: &[FileStamp]) {
        let mut j = lock(&self.journal);
        let reader = j.readers.entry(self.session).or_default();
        for file in files {
            reader
                .own
                .insert(file.path.clone(), (self.rev, file.clone()));
        }
    }
}

pub(super) struct Snapshot {
    pub polled: BTreeSet<String>,
    pub cut: Cut,
    // None requires re-enumerating the scope, including surviving equal-stamp files.
    pub paths: Option<BTreeMap<PathBuf, u64>>,
    pub own: HashMap<String, (u64, FileStamp)>,
}

pub(super) struct Filter {
    root: PathBuf,
    excluded: Vec<PathBuf>,
}

impl Filter {
    pub fn new(root: PathBuf, db: Option<&DatabasePaths>) -> Self {
        let mut excluded = Vec::new();
        if let Some(db) = db {
            excluded.extend(
                [
                    &db.database,
                    &db.lock,
                    &db.discovery,
                    &db.log,
                    &db.stop,
                    &db.relay_identity,
                ]
                .map(Clone::clone),
            );
            for suffix in ["-wal", "-shm", "-journal"] {
                let mut name = db.database.as_os_str().to_owned();
                name.push(suffix);
                excluded.push(PathBuf::from(name));
            }
        }
        Self { root, excluded }
    }

    // Tool calls may explicitly observe files through symlinks or outside the
    // watched tree. Poll only these exceptions between native checkpoints.
    pub fn requires_stat(&self, path: &Path) -> bool {
        !self.contains(path) || path.canonicalize().is_ok_and(|p| p != path)
    }

    pub fn contains(&self, path: &Path) -> bool {
        path.strip_prefix(&self.root).is_ok_and(|rel| {
            !rel.components().any(|c| c.as_os_str() == ".git")
                && !rel.starts_with(".hya/tool-output")
                && !rel.starts_with(".hya/local")
        }) && !self.excluded.iter().any(|p| p == path)
    }
}

impl Monitor {
    pub fn new(root: &Path, filter: Arc<Filter>) -> io::Result<Self> {
        let meta = fs::metadata(root)?;
        if !meta.is_dir() {
            return Err(io::ErrorKind::NotADirectory.into());
        }
        // Watch an anonymous inode. A one-byte positional write orders the
        // query behind earlier inotify events and leaves no files on a crash.
        let marker = tempfile::tempfile()?;
        let marker_path = PathBuf::from(format!("/proc/self/fd/{}", marker.as_raw_fd()));
        let watched_marker = marker_path.clone();
        let (tx, receipts) = mpsc::sync_channel(1);
        let journal = Arc::new(Mutex::new(Journal::default()));
        let state = journal.clone();
        let watched = root.to_owned();
        let event_filter = filter.clone();
        let mut watcher = RecommendedWatcher::new(
            move |ev: notify::Result<Event>| {
                let mut j = lock(&state);
                let Ok(event) = ev else {
                    j.broken = true;
                    return;
                };
                if event.need_rescan() {
                    j.broken = true;
                }
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                for p in event.paths {
                    if p == watched_marker
                        && matches!(
                            event.kind,
                            EventKind::Modify(notify::event::ModifyKind::Data(_))
                        )
                    {
                        let _ = tx.try_send(());
                    } else if p == watched {
                        if matches!(
                            event.kind,
                            EventKind::Remove(_)
                                | EventKind::Modify(notify::event::ModifyKind::Name(_))
                        ) {
                            j.broken = true;
                        }
                    } else if event_filter.contains(&p) {
                        if matches!(
                            event.kind,
                            EventKind::Create(notify::event::CreateKind::Folder)
                                | EventKind::Remove(notify::event::RemoveKind::Folder)
                                | EventKind::Modify(notify::event::ModifyKind::Name(_))
                        ) {
                            if j.topology.len() < MAX_DIRTY {
                                j.topology.insert(p.clone());
                            } else {
                                j.broken = true;
                            }
                        }
                        j.rev += 1;
                        let rev = j.rev;
                        if j.paths.len() >= MAX_DIRTY && !j.paths.contains_key(&p) {
                            j.paths.clear();
                            j.floor = rev;
                        }
                        j.paths.insert(p, rev);
                    }
                }
            },
            Config::default().with_follow_symlinks(false),
        )
        .map_err(io::Error::other)?;
        watcher
            .watch(&marker_path, RecursiveMode::NonRecursive)
            .map_err(io::Error::other)?;
        let mut monitor = Self {
            watcher,
            directories: BTreeSet::new(),
            marker,
            receipts,
            root_id: (meta.dev(), meta.ino()),
            journal,
        };
        monitor.attach(root, &filter)?;
        Ok(monitor)
    }

    // Explicit registration lets discovery and watching prune the same subtrees
    // and bound the time spent setting up a new directory tree.
    fn attach(&mut self, path: &Path, filter: &Filter) -> io::Result<()> {
        let mut dirs = vec![path.to_owned()];
        let deadline = Instant::now() + Duration::from_secs(30);
        while let Some(dir) = dirs.pop() {
            if !filter.contains(&dir) || self.directories.contains(&dir) {
                continue;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "workspace watch setup exceeded 30s",
                ));
            }
            match fs::symlink_metadata(&dir) {
                Ok(m) if m.is_dir() && dir.canonicalize()? == dir => (),
                Ok(_) => continue,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e),
            };
            match self.watcher.watch(&dir, RecursiveMode::NonRecursive) {
                Ok(()) => {}
                Err(e) if matches!(e.kind, notify::ErrorKind::PathNotFound) => continue,
                Err(e) => return Err(io::Error::other(e)),
            }
            self.directories.insert(dir.clone());
            let entries = match fs::read_dir(dir) {
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
                let entry = entry?;
                if entry.file_type()?.is_dir() && filter.contains(&entry.path()) {
                    dirs.push(entry.path());
                }
            }
        }
        Ok(())
    }

    fn update_directories(&mut self, paths: BTreeSet<PathBuf>, filter: &Filter) -> io::Result<()> {
        // Remove stale descriptor mappings before watching the new names of
        // the same inodes. notify may already have removed the old descriptors.
        for path in &paths {
            // A directory moved away and back can have the same inode after
            // notify already detached its watch on MOVED_FROM.
            if self.directories.contains(path) {
                let stale: Vec<_> = self
                    .directories
                    .range(path.clone()..)
                    .take_while(|p| p.starts_with(path))
                    .cloned()
                    .collect();
                for old in stale {
                    if let Err(e) = self.watcher.unwatch(&old)
                        && !matches!(
                            e.kind,
                            notify::ErrorKind::WatchNotFound | notify::ErrorKind::PathNotFound
                        )
                    {
                        return Err(io::Error::other(e));
                    }
                    self.directories.remove(&old);
                }
            }
        }
        for path in paths {
            self.attach(&path, filter)?;
        }
        Ok(())
    }

    pub fn snapshot(
        &mut self,
        root: &Path,
        filter: &Arc<Filter>,
        session: SessionId,
    ) -> io::Result<Snapshot> {
        let meta = fs::metadata(root)?;
        if self.root_id != (meta.dev(), meta.ino()) {
            return Err(io::Error::other("workspace root replaced"));
        }
        self.marker.write_all_at(&[1], 0)?;
        self.receipts
            .recv_timeout(Duration::from_secs(5))
            .map_err(io::Error::other)?;
        let mut j = lock(&self.journal);
        let topology = std::mem::take(&mut j.topology);
        if j.broken {
            return Err(io::Error::other(
                "workspace notification stream interrupted",
            ));
        }
        if j.readers.len() >= MAX_READERS && !j.readers.contains_key(&session) {
            j.readers.clear();
        }
        let reader = j.readers.entry(session).or_default();
        let polled = reader.polled.clone();
        let since = reader.cut;
        let own = reader.own.clone();
        let paths = since.filter(|v| *v >= j.floor).map(|v| {
            if v == j.rev {
                BTreeMap::new()
            } else {
                j.paths
                    .iter()
                    .filter(|(_, rev)| **rev > v)
                    .map(|(p, r)| (p.clone(), *r))
                    .collect()
            }
        });
        let snapshot = Snapshot {
            polled,
            cut: Cut {
                journal: self.journal.clone(),
                session,
                rev: j.rev,
            },
            paths,
            own,
        };
        drop(j);
        self.update_directories(topology, filter)?;
        Ok(snapshot)
    }
}
