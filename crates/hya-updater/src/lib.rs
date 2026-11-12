//! `hya update`: reinstall the running prefix from the published releases.
//!
//! This crate must not depend on `hya-core`, `hya-plugin`, `hya-mcp`,
//! `hya-bundle`, `hya-app`, or session storage: `hya` dispatches `update`
//! before it composes any runtime.
//!
//! [`release_install`] pipes the installer embedded from
//! `scripts/hya-install.sh` to `sh`, so `hya update` runs exactly what
//! `https://hya.ed-aisys.com/install.sh` installs: each side's release is
//! verified against its `SHA256SUMS` and a failed side is rolled back.

pub mod cli;
pub mod release_install;
