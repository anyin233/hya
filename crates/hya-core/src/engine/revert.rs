//! Session revert / unrevert (`SessionReverted`, `SessionUnreverted`).
//!
//! A revert hides a user message and every later message, and restores each
//! file the hidden turns changed to its state before the earliest of those
//! changes (from the `FilesChanged` snapshots folded onto the hidden
//! messages). The files' current content is kept first, so an unrevert can
//! write it back. The next message on the session commits the revert: the
//! reducer drops the hidden messages for good (see `hya_proto::Projection`).
//!
//! Both operations hold the session's turn lease while they run, so no turn
//! can start (or be running) on the session meanwhile.

use std::collections::BTreeSet;
use std::path::Path;

use hya_proto::{Event, FileRestore, FileState, MessageId, Role, SessionId};

use super::SessionEngine;
use super::file_snapshot::BlobBudget;
use crate::error::CoreError;

/// Which user message a revert hides from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevertTarget {
    /// The last visible user message (`/undo`).
    LastUserMessage,
    /// This visible user message.
    Message(MessageId),
}

/// Result of a revert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevertOutcome {
    /// The reverted (first hidden) user message.
    pub message: MessageId,
    /// Files the revert wrote (or could not restore).
    pub files: Vec<FileRestore>,
}

/// Why a revert or unrevert was refused.
#[derive(Debug, thiserror::Error)]
pub enum RevertError {
    /// The session has no log.
    #[error("session not found")]
    SessionNotFound,
    /// A turn is running (or starting) on the session.
    #[error("session busy")]
    Busy,
    /// The visible transcript has no user message.
    #[error("nothing to revert: the transcript has no user message")]
    NothingToRevert,
    /// The target message is not in the session.
    #[error("message not found: {0}")]
    MessageNotFound(MessageId),
    /// The target is not a user message.
    #[error("message {0} is not a user message")]
    NotUserMessage(MessageId),
    /// The target is already hidden by the pending revert.
    #[error("message {0} is already reverted")]
    AlreadyReverted(MessageId),
    /// Unrevert without a pending revert (none, or already committed).
    #[error("no revert to undo")]
    NoRevertPending,
    /// Store or runtime failure.
    #[error(transparent)]
    Core(#[from] CoreError),
}

impl From<hya_store::StoreError> for RevertError {
    fn from(error: hya_store::StoreError) -> Self {
        Self::Core(CoreError::Store(error))
    }
}

impl SessionEngine {
    /// Revert `session` to just before the `target` user message.
    ///
    /// # Errors
    /// [`RevertError::Busy`] while a turn is active; the target errors; store
    /// failures. A file that cannot be written is reported in its
    /// [`FileRestore::error`], not as an error.
    pub async fn revert_session(
        &self,
        session: SessionId,
        target: RevertTarget,
    ) -> Result<RevertOutcome, RevertError> {
        let _lease = self.revert_lease(session)?;
        let projection = self.store.read_projection(session).await?;
        if projection.session.id.is_none() {
            return Err(RevertError::SessionNotFound);
        }
        let messages = &projection.session.messages;
        let index = match target {
            RevertTarget::LastUserMessage => messages
                .iter()
                .rposition(|message| message.role == Role::User)
                .ok_or(RevertError::NothingToRevert)?,
            RevertTarget::Message(id) => match messages.iter().position(|m| m.id == id) {
                Some(index) if messages[index].role == Role::User => index,
                Some(_) => return Err(RevertError::NotUserMessage(id)),
                None => {
                    let hidden = projection
                        .session
                        .revert
                        .as_ref()
                        .is_some_and(|revert| revert.hidden.iter().any(|m| m.id == id));
                    return Err(if hidden {
                        RevertError::AlreadyReverted(id)
                    } else {
                        RevertError::MessageNotFound(id)
                    });
                }
            },
        };
        let message = messages[index].id;
        // Earliest recorded prior state per path over everything that will
        // be hidden: the newly hidden messages, then the already hidden ones.
        let pending = projection.session.revert.iter().flat_map(|r| &r.hidden);
        let mut seen = BTreeSet::new();
        let mut targets = Vec::new();
        for record in messages[index..]
            .iter()
            .chain(pending)
            .flat_map(|m| &m.file_changes)
        {
            if seen.insert(record.path.clone()) {
                targets.push((record.path.clone(), record.before.clone()));
            }
        }
        let mut budget = self.blob_budget(session).await?;
        let mut files = Vec::with_capacity(targets.len());
        for (path, restored) in targets {
            files.push(
                self.restore_file(session, path, restored, &mut budget)
                    .await?,
            );
        }
        self.emit(
            session,
            Event::SessionReverted {
                session,
                message,
                files: files.clone(),
            },
        )
        .await?;
        Ok(RevertOutcome { message, files })
    }

    /// Undo the pending revert of `session`: restore the hidden messages and
    /// write back the file contents the revert replaced.
    ///
    /// # Errors
    /// [`RevertError::Busy`] while a turn is active,
    /// [`RevertError::NoRevertPending`] when there is nothing to undo, store
    /// failures.
    pub async fn unrevert_session(
        &self,
        session: SessionId,
    ) -> Result<Vec<FileRestore>, RevertError> {
        let _lease = self.revert_lease(session)?;
        let projection = self.store.read_projection(session).await?;
        if projection.session.id.is_none() {
            return Err(RevertError::SessionNotFound);
        }
        let revert = projection
            .session
            .revert
            .ok_or(RevertError::NoRevertPending)?;
        let mut files = Vec::with_capacity(revert.files.len());
        for file in revert.files {
            let error = if file.saved.restorable() {
                self.write_state(session, Path::new(&file.path), &file.saved)
                    .await
                    .err()
            } else {
                None
            };
            files.push(FileRestore {
                path: file.path,
                // The unrevert writes back what the revert found on disk…
                restored: file.saved,
                // …over what the revert had written.
                saved: file.restored,
                error,
            });
        }
        self.emit(
            session,
            Event::SessionUnreverted {
                session,
                files: files.clone(),
            },
        )
        .await?;
        Ok(files)
    }

    fn revert_lease(&self, session: SessionId) -> Result<super::TurnLease, RevertError> {
        self.turn_gate
            .try_acquire(session)
            .map_err(|error| match error {
                // A draining gate refuses every claim too.
                CoreError::TurnAlreadyActive { .. } | CoreError::Cancelled => RevertError::Busy,
                other => RevertError::Core(other),
            })
    }

    /// Keep `path`'s current content, then write `restored` over it.
    async fn restore_file(
        &self,
        session: SessionId,
        path: String,
        restored: FileState,
        budget: &mut BlobBudget,
    ) -> Result<FileRestore, CoreError> {
        let target = Path::new(&path);
        let saved = self.snapshot_path(session, target, budget).await?;
        let error = if restored.restorable() && restored != saved {
            self.write_state(session, target, &restored).await.err()
        } else {
            None
        };
        Ok(FileRestore {
            path,
            restored,
            saved,
            error,
        })
    }

    /// Make the file at `path` match `state`; the error text on failure.
    async fn write_state(
        &self,
        session: SessionId,
        path: &Path,
        state: &FileState,
    ) -> Result<(), String> {
        match state {
            FileState::Absent => match tokio::fs::metadata(path).await {
                Ok(metadata) if metadata.is_file() => tokio::fs::remove_file(path)
                    .await
                    .map_err(|error| error.to_string()),
                Ok(_) => Err("not a regular file".to_string()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error.to_string()),
            },
            FileState::Stored { hash, .. } => {
                let content = self
                    .store
                    .file_blob(session, hash)
                    .await
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("snapshot content {hash} is missing"))?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent)
                        .await
                        .map_err(|error| error.to_string())?;
                }
                tokio::fs::write(path, content)
                    .await
                    .map_err(|error| error.to_string())
            }
            FileState::Omitted { .. } => Ok(()),
        }
    }
}
