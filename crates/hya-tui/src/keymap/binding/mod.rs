mod atom;
mod event;
mod parser;
mod util;

use crate::contracts::KeyChord;

pub(crate) use event::key_events_match;
pub(crate) use parser::parse_binding_spec_inner;

pub const LEADER_TOKEN: &str = "leader";
pub const LEADER_DEFAULT: &str = "ctrl+x";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseKeyBindingError {
    #[error("key binding spec is empty")]
    EmptySpec,
    #[error("key binding alternative {index} is empty")]
    EmptyAlternative { index: usize },
    #[error("unknown key `{key}`")]
    UnknownKey { key: String },
    #[error("`<leader>` cannot be used while parsing the leader key")]
    LeaderUnavailable,
    #[error("unterminated key token starting at `{token}`")]
    UnterminatedToken { token: String },
}

pub fn default_leader_key() -> Result<KeyChord, ParseKeyBindingError> {
    let mut chords = parse_binding_spec_inner(LEADER_DEFAULT, None)?;
    if chords.len() == 1 {
        return Ok(chords.remove(0));
    }
    Err(ParseKeyBindingError::EmptySpec)
}

pub fn parse_binding_spec(input: &str) -> Result<Vec<KeyChord>, ParseKeyBindingError> {
    parse_binding_spec_with_leader(input, &default_leader_key()?)
}

pub fn parse_binding_spec_with_leader(
    input: &str,
    leader: &KeyChord,
) -> Result<Vec<KeyChord>, ParseKeyBindingError> {
    parse_binding_spec_inner(input, Some(leader))
}
