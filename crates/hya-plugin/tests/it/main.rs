//! Integration tests for `hya-plugin`, built as one binary; each module covers one area.

mod codec_lines;
mod configured_id_mismatch;
mod connect_all_observed_in;
mod hook_wire_golden;
mod kind_wire;
mod manifest_config;
mod permission_identity;
mod protocol_roundtrip;
mod respawn_declaration_drift;
mod transport;
