use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use yaca_proto::{Envelope, Event, SessionId, now_millis};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    pub summary: String,
    pub model: String,
    pub agent: String,
    pub workdir: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub message_count: u64,
    pub last_user_message: String,
}

pub struct HistoryStore {
    root: PathBuf,
}

impl HistoryStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn from_env() -> Self {
        if let Ok(dir) = std::env::var("YACA_HISTORY_DIR") {
            return Self::new(PathBuf::from(dir));
        }
        if let Ok(home) = std::env::var("HOME") {
            return Self::new(PathBuf::from(home).join(".yaca/history"));
        }
        Self::new(std::env::temp_dir().join("yaca/history"))
    }

    pub fn create_session(
        &self,
        session: SessionId,
        model: &str,
        agent: &str,
        workdir: &str,
    ) -> anyhow::Result<SessionMeta> {
        let now = now_millis();
        let meta = SessionMeta {
            id: session.to_string(),
            title: "Untitled session".to_string(),
            summary: String::new(),
            model: model.to_string(),
            agent: agent.to_string(),
            workdir: workdir.to_string(),
            created_at: now,
            updated_at: now,
            message_count: 0,
            last_user_message: String::new(),
        };
        let dir = self.session_dir(session);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        self.write_meta(&meta)?;
        self.rebuild_index()?;
        Ok(meta)
    }

    pub fn append_envelope(&self, session: SessionId, envelope: &Envelope) -> anyhow::Result<()> {
        let dir = self.session_dir(session);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("events.jsonl"))
            .with_context(|| format!("open {}", dir.join("events.jsonl").display()))?;
        let line = serde_json::to_string(envelope).context("serialize envelope")?;
        writeln!(file, "{line}").context("append envelope")?;
        self.update_meta_from_event(session, &envelope.event)?;
        Ok(())
    }

    pub fn load_events(&self, session: SessionId) -> anyhow::Result<Vec<Envelope>> {
        let path = self.session_dir(session).join("events.jsonl");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        let mut out = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(envelope) = serde_json::from_str::<Envelope>(line) {
                out.push(envelope);
            }
        }
        Ok(out)
    }

    pub async fn hydrate_store(
        &self,
        store: &yaca_store::SessionStore,
        session: SessionId,
    ) -> anyhow::Result<()> {
        for envelope in self.load_events(session)? {
            store
                .append_event(session, &envelope.event)
                .await
                .context("append hydrated event")?;
        }
        Ok(())
    }

    pub fn list_sessions(&self) -> anyhow::Result<Vec<SessionMeta>> {
        let sessions_dir = self.root.join("sessions");
        let mut metas = Vec::new();
        let entries = match fs::read_dir(&sessions_dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("read {}", sessions_dir.display())),
        };
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path().join("meta.json");
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if let Ok(meta) = serde_json::from_str::<SessionMeta>(&text) {
                metas.push(meta);
            }
        }
        metas.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(metas)
    }

    pub fn rebuild_index(&self) -> anyhow::Result<()> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("create {}", self.root.display()))?;
        let metas = self.list_sessions()?;
        let path = self.root.join("index.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(&metas).context("serialize history index")?,
        )
        .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    fn session_dir(&self, session: SessionId) -> PathBuf {
        self.root.join("sessions").join(session.to_string())
    }

    fn write_meta(&self, meta: &SessionMeta) -> anyhow::Result<()> {
        let dir = self.root.join("sessions").join(&meta.id);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let path = dir.join("meta.json");
        fs::write(
            &path,
            serde_json::to_string_pretty(meta).context("serialize session meta")?,
        )
        .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    fn read_meta(&self, session: SessionId) -> anyhow::Result<Option<SessionMeta>> {
        let path = self.session_dir(session).join("meta.json");
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Ok(Some(
            serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))?,
        ))
    }

    fn update_meta_from_event(&self, session: SessionId, event: &Event) -> anyhow::Result<()> {
        let Some(mut meta) = self.read_meta(session)? else {
            return Ok(());
        };
        meta.updated_at = now_millis();
        match event {
            Event::SessionTitled { title, .. } => meta.title = title.clone(),
            Event::MessageStarted { .. } => {
                meta.message_count = meta.message_count.saturating_add(1)
            }
            Event::TextDelta { delta, .. } if meta.last_user_message.is_empty() => {
                meta.last_user_message = delta.chars().take(120).collect();
            }
            _ => {}
        }
        self.write_meta(&meta)?;
        self.rebuild_index()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    use yaca_proto::{Envelope, Event, EventSeq, MessageId, PartId, Role, SessionId};

    static TEMP_ROOT_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_root() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let seq = TEMP_ROOT_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "yaca-history-test-{nanos}-{}-{seq}",
            std::process::id()
        ))
    }

    fn env(seq: u64, event: Event) -> Envelope {
        Envelope {
            seq: EventSeq(seq),
            ts_millis: 0,
            event,
        }
    }

    #[test]
    fn creates_one_directory_per_session_and_appends_jsonl() {
        let root = temp_root();
        let store = HistoryStore::new(root.clone());
        let session = SessionId::new();

        store
            .create_session(session, "fake", "build", "/tmp")
            .expect("create session bundle");
        store
            .append_envelope(
                session,
                &env(
                    1,
                    Event::SessionTitled {
                        session,
                        title: "First".to_string(),
                    },
                ),
            )
            .expect("append event");

        assert!(
            root.join("sessions")
                .join(session.to_string())
                .join("meta.json")
                .exists()
        );
        let jsonl = std::fs::read_to_string(
            root.join("sessions")
                .join(session.to_string())
                .join("events.jsonl"),
        )
        .expect("events jsonl");
        assert_eq!(jsonl.lines().count(), 1);
    }

    #[test]
    fn lists_sessions_from_meta_when_index_is_missing() {
        let root = temp_root();
        let store = HistoryStore::new(root.clone());
        let session = SessionId::new();
        store
            .create_session(session, "fake", "build", "/tmp")
            .expect("create session bundle");
        let _ = std::fs::remove_file(root.join("index.json"));

        let sessions = store.list_sessions().expect("list sessions");

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.to_string());
    }

    #[test]
    fn malformed_session_does_not_hide_other_sessions() {
        let root = temp_root();
        let store = HistoryStore::new(root.clone());
        let good = SessionId::new();
        store
            .create_session(good, "fake", "build", "/tmp")
            .expect("create good bundle");
        let bad_dir = root.join("sessions").join("bad");
        std::fs::create_dir_all(&bad_dir).expect("bad dir");
        std::fs::write(bad_dir.join("meta.json"), "{not-json").expect("bad meta");

        let sessions = store.list_sessions().expect("list sessions");

        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, good.to_string());
    }

    #[test]
    fn loads_events_for_one_session() {
        let root = temp_root();
        let store = HistoryStore::new(root);
        let session = SessionId::new();
        store
            .create_session(session, "fake", "build", "/tmp")
            .expect("create session bundle");
        store
            .append_envelope(
                session,
                &env(
                    1,
                    Event::SessionTitled {
                        session,
                        title: "Loaded".to_string(),
                    },
                ),
            )
            .expect("append event");

        let events = store.load_events(session).expect("load events");

        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].event, Event::SessionTitled { .. }));
    }

    #[tokio::test]
    async fn hydrates_session_store_from_jsonl_events() {
        let root = temp_root();
        let history = HistoryStore::new(root);
        let session = SessionId::new();
        let message = MessageId::new();
        let part = PartId::new();
        history
            .create_session(session, "fake", "build", "/tmp")
            .expect("create session bundle");
        for (seq, event) in [
            (
                1,
                Event::MessageStarted {
                    session,
                    message,
                    role: Role::User,
                },
            ),
            (
                2,
                Event::TextStart {
                    session,
                    message,
                    part,
                },
            ),
            (
                3,
                Event::TextDelta {
                    session,
                    message,
                    part,
                    delta: "restored prompt".to_string(),
                },
            ),
        ] {
            history
                .append_envelope(session, &env(seq, event))
                .expect("append event");
        }
        let store = yaca_store::SessionStore::connect_memory()
            .await
            .expect("memory store");

        history
            .hydrate_store(&store, session)
            .await
            .expect("hydrate store");
        let projection = store.read_projection(session).await.expect("projection");

        assert_eq!(projection.session.messages.len(), 1);
        assert!(format!("{:?}", projection).contains("restored prompt"));
    }
}
