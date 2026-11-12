//! Newline-delimited JSON framing over async stdio shared with `hya_mcp`.

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncWrite, BufReader};

use crate::error::PluginError;
use crate::protocol::Frame;

/// Maximum accepted length of a single NDJSON line (1 MiB).
pub use hya_mcp::codec::MAX_LINE_BYTES;

/// Async reader that yields one classified [`Frame`] per newline-delimited line.
pub struct FrameReader<R> {
    inner: BufReader<R>,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// Wrap an async byte reader for NDJSON frame parsing.
    pub fn new(reader: R) -> Self {
        Self {
            inner: BufReader::new(reader),
            buf: Vec::new(),
        }
    }

    /// # Errors
    /// `Io` on a read failure, `OversizedLine` past [`MAX_LINE_BYTES`], or
    /// `Json` on an undecodable / unclassifiable line. `Ok(None)` at clean EOF.
    pub async fn next(&mut self) -> Result<Option<Frame>, PluginError> {
        let read = read_bounded_line(&mut self.inner, &mut self.buf).await?;
        if read == 0 {
            return Ok(None);
        }
        let line = std::str::from_utf8(self.buf.trim_ascii_end())
            .map_err(|e| PluginError::Json(e.to_string()))?;
        if line.is_empty() {
            return Ok(None);
        }
        Frame::parse(line).map(Some).map_err(PluginError::Json)
    }
}

pub(crate) async fn read_bounded_line<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    buf: &mut Vec<u8>,
) -> Result<usize, PluginError> {
    hya_mcp::codec::read_bounded_line(reader, buf)
        .await
        .map_err(framing_error)
}

pub(crate) fn framing_error(error: hya_mcp::codec::FrameError) -> PluginError {
    match error {
        hya_mcp::codec::FrameError::Io(message) => PluginError::Io(message),
        hya_mcp::codec::FrameError::Json(message) => PluginError::Json(message),
        hya_mcp::codec::FrameError::OversizedLine => PluginError::OversizedLine(MAX_LINE_BYTES),
    }
}

/// Async writer that serializes one JSON object per line (NDJSON).
pub struct FrameWriter<W> {
    inner: W,
}

impl<W: AsyncWrite + Unpin> FrameWriter<W> {
    /// Wrap an async byte writer for NDJSON frame emission.
    pub fn new(writer: W) -> Self {
        Self { inner: writer }
    }

    /// # Errors
    /// `Json` if `frame` cannot serialize, `Io` on a write/flush failure.
    pub async fn write<T: Serialize>(&mut self, frame: &T) -> Result<(), PluginError> {
        hya_mcp::codec::write_frame(&mut self.inner, frame)
            .await
            .map_err(framing_error)
    }
}
