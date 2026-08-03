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

pub(crate) async fn read_bounded_line<R>(
    reader: &mut BufReader<R>,
    buf: &mut Vec<u8>,
) -> Result<usize, PluginError>
where
    R: AsyncRead + Unpin,
{
    buf.clear();
    loop {
        let (take, has_newline) = {
            let available = reader
                .fill_buf()
                .await
                .map_err(|error| PluginError::Io(error.to_string()))?;
            if available.is_empty() {
                return Ok(buf.len());
            }
            let newline = available.iter().position(|byte| *byte == b'\n');
            let take = newline.map_or(available.len(), |index| index + 1);
            let next_len = buf
                .len()
                .checked_add(take)
                .ok_or(PluginError::OversizedLine(MAX_LINE_BYTES))?;
            if next_len > MAX_LINE_BYTES {
                return Err(PluginError::OversizedLine(MAX_LINE_BYTES));
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
