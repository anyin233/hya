//! Opaque pagination cursors for the v1 list operations.
//!
//! A cursor is the base64url encoding of an offset into a stable ordering.
//! Clients treat it as opaque; the encoding exists only so cursors survive
//! JSON round trips without ambiguity.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Encode a list offset into an opaque cursor string.
#[must_use]
pub fn encode_offset(offset: u64) -> String {
    URL_SAFE_NO_PAD.encode(offset.to_le_bytes())
}

/// Decode an opaque cursor into a list offset.
///
/// An empty cursor decodes to offset `0`. A malformed cursor is an
/// [`InvalidCursor`](InvalidCursor) error, not a panic.
pub fn decode_offset(cursor: &str) -> Result<u64, InvalidCursor> {
    if cursor.is_empty() {
        return Ok(0);
    }
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| InvalidCursor {
        cursor: cursor.into(),
    })?;
    if bytes.len() > 8 {
        return Err(InvalidCursor {
            cursor: cursor.into(),
        });
    }
    let mut le = [0u8; 8];
    le[..bytes.len()].copy_from_slice(&bytes);
    Ok(u64::from_le_bytes(le))
}

/// A cursor string that is not a valid v1 cursor.
#[derive(Debug)]
pub struct InvalidCursor {
    cursor: String,
}

impl std::fmt::Display for InvalidCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid pagination cursor: {}", self.cursor)
    }
}

impl std::error::Error for InvalidCursor {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_cursor_is_offset_zero() {
        assert_eq!(decode_offset("").ok(), Some(0));
    }

    #[test]
    fn offsets_round_trip() {
        for offset in [0u64, 1, 42, u32::MAX as u64, u64::MAX] {
            assert_eq!(decode_offset(&encode_offset(offset)).ok(), Some(offset));
        }
    }

    #[test]
    fn malformed_cursors_are_rejected() {
        assert!(decode_offset("!!!").is_err());
        assert!(decode_offset("aaaaaaaaaaaaaaa").is_err());
    }
}
