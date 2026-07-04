mod table;

use crate::contracts::{BindingId, KeyChord};

use super::action::canonical_command;
use super::binding::{default_leader_key, parse_binding_spec_with_leader, ParseKeyBindingError};
use super::dispatch::{KeyBinding, KeymapDispatcher};
use super::modes::KeymapMode;

pub const DEFAULT_LEADER_TIMEOUT_MS: u64 = 2_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefaultBinding {
    pub config_key: &'static str,
    pub command: BindingId,
    pub description: &'static str,
    pub enabled: bool,
    pub chords: Vec<KeyChord>,
    pub mode: Option<KeymapMode>,
    pub priority: i16,
}

#[must_use]
pub fn default_binding_specs() -> &'static [(&'static str, &'static str, &'static str)] {
    table::DEFAULT_SPECS
}

pub fn default_bindings() -> Result<Vec<DefaultBinding>, ParseKeyBindingError> {
    let leader = default_leader_key()?;
    table::DEFAULT_SPECS
        .iter()
        .map(|(config_key, value, description)| {
            default_binding(config_key, value, description, &leader)
        })
        .collect()
}

pub fn default_dispatcher() -> Result<KeymapDispatcher, ParseKeyBindingError> {
    let leader = default_leader_key()?;
    let key_bindings = default_bindings()?
        .into_iter()
        .filter(|binding| binding.config_key != "leader")
        .flat_map(|binding| {
            let command = binding.command;
            let mode = binding.mode;
            binding.chords.into_iter().map(move |chord| KeyBinding {
                command: command.clone(),
                chord,
                mode: mode.clone(),
                priority: binding.priority,
            })
        });
    Ok(KeymapDispatcher::new(
        key_bindings.collect(),
        leader,
        DEFAULT_LEADER_TIMEOUT_MS,
    ))
}

fn default_binding(
    config_key: &'static str,
    value: &'static str,
    description: &'static str,
    leader: &KeyChord,
) -> Result<DefaultBinding, ParseKeyBindingError> {
    let command = canonical_command(config_key);
    let enabled = value != "none";
    let chords = if enabled {
        parse_binding_spec_with_leader(value, leader)?
    } else {
        Vec::new()
    };
    Ok(DefaultBinding {
        config_key,
        mode: default_mode(&command),
        priority: default_priority(&command),
        command,
        description,
        enabled,
        chords,
    })
}

fn default_mode(command: &BindingId) -> Option<KeymapMode> {
    let command = command.0.as_str();
    if command.starts_with("prompt.autocomplete.") {
        return Some(KeymapMode::Autocomplete);
    }
    if command.starts_with("dialog.") || command.starts_with("plugins.") {
        return Some(KeymapMode::Modal);
    }
    Some(KeymapMode::Base)
}

fn default_priority(command: &BindingId) -> i16 {
    match command.0.as_str() {
        "session.background" | "session.first" | "session.last" => 1,
        _ => 0,
    }
}
