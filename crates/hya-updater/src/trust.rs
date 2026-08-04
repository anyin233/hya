//! Trust-root loading for the independent updater root.

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::UpdaterError;
use crate::metadata::TrustRoot;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TrustRootsFile {
    roots: Vec<TrustRootRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct TrustRootRecord {
    key_id: String,
    /// Lower-hex 32-byte ed25519 verifying key.
    verifying_key_hex: String,
}

/// Load trust roots from `root/trust_roots.json` (or an explicit path).
pub fn load_trust_roots(path: &Path) -> Result<Vec<TrustRoot>, UpdaterError> {
    let text = fs::read_to_string(path).map_err(|error| {
        UpdaterError::InvalidMetadata(format!(
            "read trust roots {}: {error}",
            path.display()
        ))
    })?;
    let parsed: TrustRootsFile = serde_json::from_str(&text).map_err(|error| {
        UpdaterError::InvalidMetadata(format!("parse trust roots: {error}"))
    })?;
    if parsed.roots.is_empty() {
        return Err(UpdaterError::InvalidMetadata(
            "trust roots file must list at least one root".to_string(),
        ));
    }
    let mut roots = Vec::with_capacity(parsed.roots.len());
    for record in parsed.roots {
        if record.key_id.is_empty() {
            return Err(UpdaterError::InvalidMetadata(
                "trust root key_id must be non-empty".to_string(),
            ));
        }
        let verifying_key = decode_hex_key(&record.verifying_key_hex)?;
        roots.push(TrustRoot {
            key_id: record.key_id,
            verifying_key,
        });
    }
    Ok(roots)
}

/// Write trust roots in the canonical on-disk format (operator/bootstrap only).
pub fn write_trust_roots(path: &Path, roots: &[TrustRoot]) -> Result<(), UpdaterError> {
    let records = roots
        .iter()
        .map(|root| TrustRootRecord {
            key_id: root.key_id.clone(),
            verifying_key_hex: root
                .verifying_key
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        })
        .collect::<Vec<_>>();
    let body = TrustRootsFile { roots: records };
    let json = serde_json::to_string_pretty(&body).map_err(|error| {
        UpdaterError::InvalidMetadata(format!("serialize trust roots: {error}"))
    })?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            UpdaterError::InvalidMetadata(format!("create trust roots parent: {error}"))
        })?;
    }
    fs::write(path, format!("{json}\n")).map_err(|error| {
        UpdaterError::InvalidMetadata(format!("write trust roots: {error}"))
    })
}

fn decode_hex_key(hex: &str) -> Result<[u8; 32], UpdaterError> {
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(UpdaterError::InvalidTrustRootKey);
    }
    if hex.bytes().any(|b| b.is_ascii_uppercase()) {
        return Err(UpdaterError::InvalidMetadata(
            "verifying_key_hex must be lower-hex".to_string(),
        ));
    }
    let mut out = [0u8; 32];
    for (i, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Ok(out)
}

fn hex_nibble(b: u8) -> Result<u8, UpdaterError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        _ => Err(UpdaterError::InvalidTrustRootKey),
    }
}
