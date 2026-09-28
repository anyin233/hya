use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::UpdaterError;
use crate::layout::layout;
use crate::metadata::AcceptedFloor;

/// Durable activation journal states for crash-consistent updates.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationPhase {
    /// Activation started; selector/floor not yet known to be committed.
    Prepare,
    /// Selector and accepted floor both point at a complete generation.
    Committed,
    /// Interrupted prepare discarded; previous complete generation retained.
    Aborted,
}

/// One journal record written under the independent updater root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivationJournalRecord {
    /// Journal phase.
    pub phase: ActivationPhase,
    /// Candidate release sequence.
    pub sequence: u64,
    /// Previously selected release sequence.
    pub previous_sequence: u64,
    /// Owner capability token that prepared the record.
    #[serde(default)]
    pub owner_token: String,
    /// Active generation expected by the owner.
    #[serde(default)]
    pub generation: u64,
}

/// Active generation selector and accepted floor under `root/`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivationSelector {
    /// Currently selected release sequence.
    pub current_sequence: u64,
    /// Highest accepted release sequence.
    pub accepted_floor: u64,
    /// Monotonic active-generation fence.
    pub generation: u64,
}

fn journal_path(root: &Path) -> PathBuf {
    layout(root).journal
}

fn selector_path(root: &Path) -> PathBuf {
    layout(root).selector
}

fn floor_path(root: &Path) -> PathBuf {
    layout(root).accepted_floor
}

pub(crate) fn journal_prepare_owned(
    root: &Path,
    sequence: u64,
    previous_sequence: u64,
    owner_token: &str,
    generation: u64,
) -> Result<(), UpdaterError> {
    write_journal(
        root,
        &ActivationJournalRecord {
            phase: ActivationPhase::Prepare,
            sequence,
            previous_sequence,
            owner_token: owner_token.to_string(),
            generation,
        },
    )
}

pub(crate) fn commit_activation_owned(
    root: &Path,
    sequence: u64,
    owner_token: &str,
    generation: u64,
) -> Result<ActivationSelector, UpdaterError> {
    let last = read_last_journal_record(root)?.ok_or(UpdaterError::StaleOwnerGeneration)?;
    if last.phase != ActivationPhase::Prepare
        || last.sequence != sequence
        || last.owner_token != owner_token
        || last.generation != generation
    {
        return Err(UpdaterError::StaleOwnerGeneration);
    }
    commit_activation_unlocked(root, sequence, generation)
}

fn commit_activation_unlocked(
    root: &Path,
    sequence: u64,
    previous_generation: u64,
) -> Result<ActivationSelector, UpdaterError> {
    let previous_selector = read_selector(root)?;
    if previous_selector.generation != previous_generation {
        return Err(UpdaterError::StaleOwnerGeneration);
    }
    let generation = previous_generation
        .checked_add(1)
        .ok_or(UpdaterError::StaleOwnerGeneration)?;
    let previous = previous_selector.current_sequence;
    let previous_floor = previous_selector.accepted_floor;
    if sequence <= previous_floor {
        return Err(UpdaterError::NonIncreasingSequence {
            sequence,
            floor: previous_floor,
        });
    }
    let path = selector_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| UpdaterError::InvalidMetadata(format!("create selector parent: {e}")))?;
    }
    let tmp = root.join("current.tmp");
    {
        let mut file = fs::File::create(&tmp)
            .map_err(|e| UpdaterError::InvalidMetadata(format!("create selector temp: {e}")))?;
        file.write_all(format!("{sequence}\n").as_bytes())
            .map_err(|e| UpdaterError::InvalidMetadata(format!("write selector temp: {e}")))?;
        file.sync_all()
            .map_err(|e| UpdaterError::InvalidMetadata(format!("fsync selector temp: {e}")))?;
    }
    fs::rename(&tmp, &path)
        .map_err(|e| UpdaterError::InvalidMetadata(format!("atomic selector rename: {e}")))?;
    write_floor(root, &AcceptedFloor { sequence })?;
    write_generation(root, generation)?;
    let owner = read_last_journal_record(root)?;
    write_journal(
        root,
        &ActivationJournalRecord {
            phase: ActivationPhase::Committed,
            sequence,
            previous_sequence: previous,
            owner_token: owner
                .as_ref()
                .map(|r| r.owner_token.clone())
                .unwrap_or_default(),
            generation,
        },
    )?;
    Ok(ActivationSelector {
        current_sequence: sequence,
        accepted_floor: sequence,
        generation,
    })
}

/// Recover interrupted activation to exactly one complete verified generation.
///
/// Rules:
/// - no journal / last phase Committed or Aborted → return current selector
/// - last phase Prepare and selector still on previous → abort prepare, keep old
/// - last phase Prepare and selector already on candidate → finish floor+commit
///
/// Never leaves a mixed selector/floor and never decrements the accepted floor.
pub fn recover_activation(root: &Path) -> Result<ActivationSelector, UpdaterError> {
    let _lease = crate::lease::acquire(root)?;
    recover_activation_locked(root)
}

pub(crate) fn recover_activation_locked(root: &Path) -> Result<ActivationSelector, UpdaterError> {
    let selector = read_selector(root)?;
    let Some(last) = read_last_journal_record(root)? else {
        return Ok(selector);
    };
    match last.phase {
        ActivationPhase::Committed | ActivationPhase::Aborted => Ok(selector),
        ActivationPhase::Prepare => {
            if selector.current_sequence == last.sequence {
                // Selector switched; ensure floor matches and journal commits.
                if selector.accepted_floor < last.sequence {
                    write_floor(
                        root,
                        &AcceptedFloor {
                            sequence: last.sequence,
                        },
                    )?;
                }
                let generation = last
                    .generation
                    .checked_add(1)
                    .ok_or(UpdaterError::StaleOwnerGeneration)?;
                write_generation(root, generation)?;
                write_journal(
                    root,
                    &ActivationJournalRecord {
                        phase: ActivationPhase::Committed,
                        sequence: last.sequence,
                        previous_sequence: last.previous_sequence,
                        owner_token: last.owner_token.clone(),
                        generation,
                    },
                )?;
                Ok(ActivationSelector {
                    current_sequence: last.sequence,
                    accepted_floor: last.sequence.max(selector.accepted_floor),
                    generation,
                })
            } else {
                // Crash before selector rename: keep previous complete generation.
                write_journal(
                    root,
                    &ActivationJournalRecord {
                        phase: ActivationPhase::Aborted,
                        sequence: last.sequence,
                        previous_sequence: last.previous_sequence,
                        owner_token: last.owner_token.clone(),
                        generation: last.generation,
                    },
                )?;
                Ok(selector)
            }
        }
    }
}

/// Read the current selector and accepted floor (defaults to zero).
pub fn read_selector(root: &Path) -> Result<ActivationSelector, UpdaterError> {
    let current = match fs::read_to_string(selector_path(root)) {
        Ok(text) => text
            .trim()
            .parse::<u64>()
            .map_err(|error| UpdaterError::InvalidMetadata(format!("selector parse: {error}")))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(UpdaterError::InvalidMetadata(format!(
                "read selector: {error}"
            )));
        }
    };
    let floor = read_floor(root)?.sequence;
    let generation = match fs::read_to_string(&layout(root).generation) {
        Ok(text) => text
            .trim()
            .parse::<u64>()
            .map_err(|e| UpdaterError::InvalidMetadata(format!("generation parse: {e}")))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(UpdaterError::InvalidMetadata(format!(
                "read generation: {error}"
            )));
        }
    };
    Ok(ActivationSelector {
        current_sequence: current,
        accepted_floor: floor,
        generation,
    })
}

fn write_generation(root: &Path, generation: u64) -> Result<(), UpdaterError> {
    let path = layout(root).generation;
    let tmp = root.join("generation.tmp");
    let mut file = fs::File::create(&tmp)
        .map_err(|e| UpdaterError::InvalidMetadata(format!("create generation temp: {e}")))?;
    file.write_all(format!("{generation}\n").as_bytes())
        .map_err(|e| UpdaterError::InvalidMetadata(format!("write generation temp: {e}")))?;
    file.sync_all()
        .map_err(|e| UpdaterError::InvalidMetadata(format!("fsync generation temp: {e}")))?;
    fs::rename(tmp, path)
        .map_err(|e| UpdaterError::InvalidMetadata(format!("atomic generation rename: {e}")))
}

/// Read the accepted anti-rollback floor from `root/accepted_floor`.
///
/// The floor is the highest release sequence the host has already accepted.
/// Verification and activation require every new sequence to strictly exceed
/// this value (`NonIncreasingSequence` otherwise). A **too-low** floor (for
/// example after tampering or a bad recovery write) re-admits previously
/// accepted or superseded sequences and weakens rollback protection. A
/// **too-high** floor blocks legitimate newer releases until a signed recovery
/// path advances past it. Missing file defaults to sequence `0`.
pub fn read_floor(root: &Path) -> Result<AcceptedFloor, UpdaterError> {
    match fs::read_to_string(floor_path(root)) {
        Ok(text) => {
            let sequence = text.trim().parse::<u64>().map_err(|error| {
                UpdaterError::InvalidMetadata(format!("accepted floor parse: {error}"))
            })?;
            Ok(AcceptedFloor { sequence })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(AcceptedFloor { sequence: 0 })
        }
        Err(error) => Err(UpdaterError::InvalidMetadata(format!(
            "read accepted floor: {error}"
        ))),
    }
}

fn read_last_journal_record(root: &Path) -> Result<Option<ActivationJournalRecord>, UpdaterError> {
    let path = journal_path(root);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(UpdaterError::InvalidMetadata(format!(
                "read journal: {error}"
            )));
        }
    };
    let mut last = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        last = Some(serde_json::from_str(line).map_err(|error| {
            UpdaterError::InvalidMetadata(format!("parse journal record: {error}"))
        })?);
    }
    Ok(last)
}

fn write_floor(root: &Path, floor: &AcceptedFloor) -> Result<(), UpdaterError> {
    let path = floor_path(root);
    let tmp = root.join("accepted_floor.tmp");
    {
        let mut file = fs::File::create(&tmp).map_err(|error| {
            UpdaterError::InvalidMetadata(format!("create floor temp: {error}"))
        })?;
        file.write_all(format!("{}\n", floor.sequence).as_bytes())
            .map_err(|error| UpdaterError::InvalidMetadata(format!("write floor temp: {error}")))?;
        file.sync_all()
            .map_err(|error| UpdaterError::InvalidMetadata(format!("fsync floor temp: {error}")))?;
    }
    fs::rename(&tmp, &path)
        .map_err(|error| UpdaterError::InvalidMetadata(format!("atomic floor rename: {error}")))
}

fn write_journal(root: &Path, record: &ActivationJournalRecord) -> Result<(), UpdaterError> {
    if let Some(parent) = journal_path(root).parent() {
        fs::create_dir_all(parent).map_err(|error| {
            UpdaterError::InvalidMetadata(format!("create journal parent: {error}"))
        })?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_path(root))
        .map_err(|error| UpdaterError::InvalidMetadata(format!("open journal: {error}")))?;
    let line = serde_json::to_string(record).map_err(|error| {
        UpdaterError::InvalidMetadata(format!("serialize journal record: {error}"))
    })?;
    writeln!(file, "{line}")
        .map_err(|error| UpdaterError::InvalidMetadata(format!("write journal: {error}")))?;
    file.sync_all()
        .map_err(|error| UpdaterError::InvalidMetadata(format!("fsync journal: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_owner_generation_cannot_commit_after_newer_prepare() {
        let root = std::env::temp_dir().join(format!("hya-updater-fence-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        assert!(journal_prepare_owned(&root, 1, 0, "owner-a", 1).is_ok());
        assert!(journal_prepare_owned(&root, 2, 0, "owner-b", 2).is_ok());
        assert_eq!(
            commit_activation_owned(&root, 1, "owner-a", 1),
            Err(UpdaterError::StaleOwnerGeneration)
        );
        let _ = fs::remove_dir_all(root);
    }
}
