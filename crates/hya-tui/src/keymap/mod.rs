pub mod action;
pub mod binding;
pub mod defaults;
pub mod dispatch;
pub mod modes;

pub use action::{canonical_command, command_catalog, command_mapping, CommandCatalogEntry};
pub use binding::{
    default_leader_key, parse_binding_spec, parse_binding_spec_with_leader, ParseKeyBindingError,
};
pub use defaults::{
    default_binding_specs, default_bindings, default_dispatcher, DefaultBinding,
    DEFAULT_LEADER_TIMEOUT_MS,
};
pub use dispatch::{DispatchOutcome, KeyBinding, KeymapDispatcher};
pub use modes::KeymapMode;

#[cfg(test)]
mod tests;
