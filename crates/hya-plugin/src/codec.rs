//! Newline-delimited JSON framing over async stdio (mirrors `hya_mcp`).

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::error::PluginError;
use crate::protocol::Frame;

pub const MAX_LINE_BYTES: usize = 1024 * 1024;

pub struct FrameReader<R> {
    inner: BufReader<R>,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
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
        self.buf.clear();
        let read = self
            .inner
            .read_until(b'\n', &mut self.buf)
            .await
            .map_err(|e| PluginError::Io(e.to_string()))?;
        if read == 0 {
            return Ok(None);
        }
        if self.buf.len() > MAX_LINE_BYTES {
            return Err(PluginError::OversizedLine(MAX_LINE_BYTES));
        }
        let line = std::str::from_utf8(self.buf.trim_ascii_end())
            .map_err(|e| PluginError::Json(e.to_string()))?;
        if line.is_empty() {
            return Ok(None);
        }
        Frame::parse(line).map(Some).map_err(PluginError::Json)
    }
}

pub struct FrameWriter<W> {
    inner: W,
}

impl<W: AsyncWrite + Unpin> FrameWriter<W> {
    pub fn new(writer: W) -> Self {
        Self { inner: writer }
    }

    /// # Errors
    /// `Json` if `frame` cannot serialize, `Io` on a write/flush failure.
    pub async fn write<T: Serialize>(&mut self, frame: &T) -> Result<(), PluginError> {
        let line = serde_json::to_vec(frame).map_err(|e| PluginError::Json(e.to_string()))?;
        self.inner
            .write_all(&line)
            .await
            .map_err(|e| PluginError::Io(e.to_string()))?;
        self.inner
            .write_all(b"\n")
            .await
            .map_err(|e| PluginError::Io(e.to_string()))?;
        self.inner
            .flush()
            .await
            .map_err(|e| PluginError::Io(e.to_string()))?;
        Ok(())
    }
}
