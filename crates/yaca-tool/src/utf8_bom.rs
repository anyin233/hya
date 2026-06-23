use std::path::Path;

use crate::tool::ToolError;

const UTF8_BOM: char = '\u{feff}';
const UTF8_BOM_BYTES: &[u8; 3] = b"\xEF\xBB\xBF";

pub(crate) fn split(text: &str) -> (bool, &str) {
    if text.starts_with(UTF8_BOM) {
        return (true, &text[UTF8_BOM.len_utf8()..]);
    }
    (false, text)
}

pub(crate) fn encode(text: &str, bom: bool) -> Vec<u8> {
    let extra = if bom { UTF8_BOM_BYTES.len() } else { 0 };
    let mut out = Vec::with_capacity(text.len() + extra);
    if bom {
        out.extend_from_slice(UTF8_BOM_BYTES);
    }
    out.extend_from_slice(text.as_bytes());
    out
}

pub(crate) async fn file_has_bom(path: &Path) -> Result<bool, ToolError> {
    Ok(tokio::fs::read(path).await?.starts_with(UTF8_BOM_BYTES))
}

pub(crate) async fn read_text(path: &Path) -> Result<(bool, String), ToolError> {
    let bytes = tokio::fs::read(path).await?;
    let source_has_bom = bytes.starts_with(UTF8_BOM_BYTES);
    let bytes = bytes.strip_prefix(UTF8_BOM_BYTES).unwrap_or(&bytes);
    let text = std::str::from_utf8(bytes)
        .map_err(|err| ToolError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, err)))?;
    Ok((source_has_bom, text.to_string()))
}

pub(crate) async fn sync_file(path: &Path, bom: bool) -> Result<(), ToolError> {
    let bytes = tokio::fs::read(path).await?;
    let has_bom = bytes.starts_with(UTF8_BOM_BYTES);
    if has_bom == bom {
        return Ok(());
    }
    let bytes = bytes.strip_prefix(UTF8_BOM_BYTES).unwrap_or(&bytes);
    let out = if bom {
        let mut out = Vec::with_capacity(bytes.len() + UTF8_BOM_BYTES.len());
        out.extend_from_slice(UTF8_BOM_BYTES);
        out.extend_from_slice(bytes);
        out
    } else {
        bytes.to_vec()
    };
    tokio::fs::write(path, out).await?;
    Ok(())
}
