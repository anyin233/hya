//! Shared bounded NDJSON framing for MCP and native plugins.
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
/// Failure while reading or writing an NDJSON frame.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// An underlying read, write or flush failed.
    #[error("io: {0}")]
    Io(String),
    /// JSON serialization failed.
    #[error("json: {0}")]
    Json(String),
    /// Input exceeded the shared byte limit.
    #[error("line exceeded 1048576 bytes")]
    OversizedLine,
}
/// Maximum line length, including the newline (1 MiB).
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Read one line, rejecting excess bytes before growing the frame buffer.
///
/// # Errors
/// Returns an I/O error or `FrameError::OversizedLine`.
pub async fn read_bounded_line<R>(
    reader: &mut BufReader<R>,
    buf: &mut Vec<u8>,
) -> Result<usize, FrameError>
where
    R: AsyncRead + Unpin,
{
    buf.clear();
    loop {
        let (take, has_newline) = {
            let available = reader
                .fill_buf()
                .await
                .map_err(|error| FrameError::Io(error.to_string()))?;
            if available.is_empty() {
                return Ok(buf.len());
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |index| index + 1);
            let next_len = buf
                .len()
                .checked_add(take)
                .ok_or(FrameError::OversizedLine)?;
            if next_len > MAX_LINE_BYTES {
                return Err(FrameError::OversizedLine);
            }
            buf.extend_from_slice(&available[..take]);
            (take, newline.is_some())
        };
        reader.consume(take);
        if has_newline {
            return Ok(buf.len());
        }
    }
}

/// Serialize, write and flush one JSON frame followed by a newline.
///
/// # Errors
/// Returns a serialization or I/O error.
pub async fn write_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    frame: &T,
) -> Result<(), FrameError> {
    let line = serde_json::to_vec(frame).map_err(|e| FrameError::Json(e.to_string()))?;
    writer
        .write_all(&line)
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    writer
        .write_all(b"\n")
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|e| FrameError::Io(e.to_string()))?;
    Ok(())
}
