//! Subagent handle naming (re-exported from `hya-multi-agent`).
//!
//! Pure naming helpers live in [`hya_proto::handle_naming`]; [`HandleNamer`]
//! and [`team_leaves`] live in the `hya/multi-agent` native bundle and are
//! re-exported here for the resident supervisor.

pub use hya_multi_agent_core::handle_naming::{
    HANDLE_SEED_ENV, HandleNamer, HandleReservation, HandleRng, PAIR_NAME_ATTEMPTS,
    SINGLE_NAME_ATTEMPTS, SplitMix64, mint_leaf, operator_names, team_leaves,
};
