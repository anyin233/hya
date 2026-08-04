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
    Prepare,
    Committed,
    /// Interrupted prepare discarded; previous complete generation retained.
    Aborted,
}

/// One journal record written under the independent updater root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivationJournalRecord {
    pub phase: ActivationPhase,
    pub sequence: u64,
    pub previous_sequence: u64,
}

/// Active generation selector and accepted floor under `root/`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActivationSelector {
    pub current_sequence: u64,
    pub accepted_floor: u64,
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

/// Write a prepare journal entry before switching the selector.
pub fn journal_prepare(
    root: &Path,
    sequence: u64,
    previous_sequence: u64,
) -> Result<(), UpdaterError> {
    write_journal(
        root,
        &ActivationJournalRecord {
            phase: ActivationPhase::Prepare,
            sequence,
            previous_sequence,
        },
    )
}

/// Atomically switch the current selector and advance the accepted floor.
///
/// The floor never decreases. After commit, old bits activate only through a
/// newly signed higher-sequence recovery release (verified by metadata policy).
pub fn commit_activation(root: &Path, sequence: u64) -> Result<ActivationSelector, UpdaterError> {
    let previous_selector = read_selector(root)?;
    let previous = previous_selector.current_sequence;
    let previous_floor = previous_selector.accepted_floor;
    if sequence <= previous_floor {
        return Err(UpdaterError::NonIncreasingSequence {
            sequence,
            floor: previous_floor,
        });
    }
    // Selector is a small file rewritten then fsynced.
    let path = selector_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            UpdaterError::InvalidMetadata(format!("create selector parent: {error}"))
        })?;
    }
    let body = format!("{sequence}\n");
    let tmp = root.join("current.tmp");
    {
        let mut file = fs::File::create(&tmp).map_err(|error| {
            UpdaterError::InvalidMetadata(format!("create selector temp: {error}"))
        })?;
        file.write_all(body.as_bytes()).map_err(|error| {
            UpdaterError::InvalidMetadata(format!("write selector temp: {error}"))
        })?;
        file.sync_all().map_err(|error| {
            UpdaterError::InvalidMetadata(format!("fsync selector temp: {error}"))
        })?;
    }
    fs::rename(&tmp, &path).map_err(|error| {
        UpdaterError::InvalidMetadata(format!("atomic selector rename: {error}"))
    })?;

    let floor = AcceptedFloor { sequence };
    write_floor(root, &floor)?;
    write_journal(
        root,
        &ActivationJournalRecord {
            phase: ActivationPhase::Committed,
            sequence,
            previous_sequence: previous,
        },
    )?;
    Ok(ActivationSelector {
        current_sequence: sequence,
        accepted_floor: sequence,
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
                write_journal(
                    root,
                    &ActivationJournalRecord {
                        phase: ActivationPhase::Committed,
                        sequence: last.sequence,
                        previous_sequence: last.previous_sequence,
                    },
                )?;
                Ok(ActivationSelector {
                    current_sequence: last.sequence,
                    accepted_floor: last.sequence.max(selector.accepted_floor),
                })
            } else {
                // Crash before selector rename: keep previous complete generation.
                write_journal(
                    root,
                    &ActivationJournalRecord {
                        phase: ActivationPhase::Aborted,
                        sequence: last.sequence,
                        previous_sequence: last.previous_sequence,
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
    Ok(ActivationSelector {
        current_sequence: current,
        accepted_floor: floor,
    })
}

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
