//! Minimal-diff, comment-preserving edits of `config.yaml`.
//!
//! hya's config writers compute the new document as a [`Value`] (the same
//! value the old full re-render wrote). This module turns the difference
//! between the file's current value and that target into line edits of the
//! original text, so comments, blank lines, key order, quoting, and
//! indentation outside the changed entries survive.
//!
//! The editor understands the block-style YAML subset hya's own config uses:
//! block mappings and sequences (indented or compact `key:\n- item`), single
//! line plain/quoted scalars, and single-line flow collections or block
//! scalars as opaque values. A changed value it cannot edit inside is
//! re-rendered in block style *locally* (only that entry or list item). Files
//! it cannot map safely — anchors, aliases, tags, merge keys, multi-document
//! streams, multi-line flow or quoted scalars, tab indentation, mixed line
//! endings — return [`Unsupported`], and the caller falls back to a full
//! re-render. The caller must re-parse the result and compare it with the
//! target before writing.

use serde_norway::{Mapping, Value};

/// Reason the minimal editor declined an edit; the caller falls back to a
/// full re-render of the document.
#[derive(Debug)]
pub(crate) struct Unsupported(pub(crate) String);

type Res<T> = Result<T, Unsupported>;

fn unsupported<T>(reason: impl Into<String>) -> Res<T> {
    Err(Unsupported(reason.into()))
}

/// Rewrite `raw` (whose parsed value is `old`) so it parses to `new`,
/// touching only the lines of entries whose values differ.
pub(crate) fn minimal_edit(raw: &str, old: &Value, new: &Value) -> Res<String> {
    let text = Text::split(raw)?;
    let mut parser = Parser {
        lines: &text.lines,
        step: None,
        seq_offset: None,
    };
    let root = parser.parse_document()?;
    let (Value::Mapping(old), Value::Mapping(new)) = (old, new) else {
        return unsupported("config root is not a mapping");
    };
    let mut editor = Editor {
        lines: &text.lines,
        step: parser.step.unwrap_or(2),
        seq_offset: parser.seq_offset.unwrap_or(2),
        splices: Vec::new(),
    };
    editor.diff_map(&root, old, new)?;
    let splices = editor.splices;
    Ok(text.join(apply(&text.lines, splices)?))
}

/// The file split into lines, remembering BOM and line-ending style.
struct Text {
    bom: bool,
    lines: Vec<String>,
    eol: &'static str,
    trailing_eol: bool,
}

impl Text {
    fn split(raw: &str) -> Res<Self> {
        let (bom, body) = match raw.strip_prefix('\u{feff}') {
            Some(body) => (true, body),
            None => (false, raw),
        };
        let eol = if body.contains("\r\n") { "\r\n" } else { "\n" };
        let trailing_eol = body.ends_with(eol);
        let body = body.strip_suffix(eol).unwrap_or(body);
        let lines: Vec<String> = body.split(eol).map(str::to_string).collect();
        if lines.iter().any(|line| line.contains(['\r', '\n'])) {
            return unsupported("mixed line endings");
        }
        Ok(Self {
            bom,
            lines,
            eol,
            trailing_eol,
        })
    }

    fn join(&self, lines: Vec<String>) -> String {
        let mut out = String::new();
        if self.bom {
            out.push('\u{feff}');
        }
        out.push_str(&lines.join(self.eol));
        if self.trailing_eol && !lines.is_empty() {
            out.push_str(self.eol);
        }
        out
    }
}

/// Replace lines `start..end` with `lines` (an insertion when empty range).
struct Splice {
    start: usize,
    end: usize,
    lines: Vec<String>,
}

fn apply(original: &[String], mut splices: Vec<Splice>) -> Res<Vec<String>> {
    // Bottom-up so earlier indices stay valid; at one start, replace the
    // range before inserting in front of it.
    splices.sort_by(|a, b| {
        b.start
            .cmp(&a.start)
            .then_with(|| (a.start == a.end).cmp(&(b.start == b.end)))
    });
    let mut out = original.to_vec();
    let mut floor = usize::MAX;
    for splice in splices {
        if splice.end > floor || splice.start > splice.end || splice.end > original.len() {
            return unsupported("overlapping edits");
        }
        floor = splice.start;
        out.splice(splice.start..splice.end, splice.lines);
    }
    Ok(out)
}

// ---------------------------------------------------------------- parsing

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quote {
    Plain,
    Single,
    Double,
}

/// A parsed value with the source span the editor may rewrite.
enum Node {
    /// One-line scalar at `line[start..end]`.
    Scalar {
        line: usize,
        start: usize,
        end: usize,
        quote: Quote,
    },
    Map(MapNode),
    Seq(SeqNode),
    /// Empty value, flow collection, or block scalar: replaced whole.
    Other,
}

struct MapNode {
    entries: Vec<Entry>,
    col: usize,
}

struct Entry {
    key: String,
    /// Key text as written (quotes included).
    key_raw: String,
    line: usize,
    col: usize,
    value: Node,
    /// Last content line of the entry.
    last: usize,
    /// Whitespace gap plus `# comment` ending the key line.
    comment: Option<String>,
}

struct SeqNode {
    items: Vec<Item>,
    col: usize,
}

struct Item {
    line: usize,
    col: usize,
    value: Node,
    last: usize,
    comment: Option<String>,
}

enum LineKind {
    Blank,
    Comment,
    Content(usize),
}

fn classify(line: &str) -> Res<LineKind> {
    let trimmed = line.trim_start_matches([' ', '\t']);
    if trimmed.is_empty() {
        return Ok(LineKind::Blank);
    }
    if trimmed.starts_with('#') {
        return Ok(LineKind::Comment);
    }
    let indent = line.len() - line.trim_start_matches(' ').len();
    if line.as_bytes().get(indent) == Some(&b'\t') {
        return unsupported("tab indentation");
    }
    Ok(LineKind::Content(indent))
}

fn is_ws(byte: u8) -> bool {
    byte == b' ' || byte == b'\t'
}

fn skip_ws(line: &str, mut pos: usize) -> usize {
    let bytes = line.as_bytes();
    while pos < bytes.len() && is_ws(bytes[pos]) {
        pos += 1;
    }
    pos
}

/// `line[col]` is a sequence dash (`-` then whitespace or end of line).
fn is_dash(line: &str, col: usize) -> bool {
    let bytes = line.as_bytes();
    bytes.get(col) == Some(&b'-') && bytes.get(col + 1).is_none_or(|b| is_ws(*b))
}

/// End (exclusive) of a quoted scalar starting at `start`, on this line.
fn quoted_end(line: &str, start: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let quote = bytes[start];
    let mut pos = start + 1;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' if quote == b'"' => pos += 2,
            b'\'' if quote == b'\'' && bytes.get(pos + 1) == Some(&b'\'') => pos += 2,
            byte if byte == quote => return Some(pos + 1),
            _ => pos += 1,
        }
    }
    None
}

/// End (exclusive) of a flow collection starting at `start`, on this line.
fn flow_end(line: &str, start: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut pos = start;
    while pos < bytes.len() {
        match bytes[pos] {
            b'"' | b'\'' => pos = quoted_end(line, pos)?,
            b'[' | b'{' => {
                depth += 1;
                pos += 1;
            }
            b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                pos += 1;
                if depth == 0 {
                    return Some(pos);
                }
            }
            b'#' if pos > start && is_ws(bytes[pos - 1]) => return None,
            _ => pos += 1,
        }
    }
    None
}

/// Parse a mapping key at `line[col..]`: `(key, key_raw, index after ':')`.
fn parse_key(line: &str, col: usize) -> Res<Option<(String, String, usize)>> {
    let bytes = line.as_bytes();
    let Some(&first) = bytes.get(col) else {
        return Ok(None);
    };
    let (key, raw_end) = match first {
        b'"' => {
            let Some(end) = quoted_end(line, col) else {
                return Ok(None);
            };
            let inner = &line[col + 1..end - 1];
            if inner.contains('\\') {
                return unsupported("escaped quoted key");
            }
            (inner.to_string(), end)
        }
        b'\'' => {
            let Some(end) = quoted_end(line, col) else {
                return Ok(None);
            };
            (line[col + 1..end - 1].replace("''", "'"), end)
        }
        b'-' | b'?' | b':' | b',' | b'[' | b']' | b'{' | b'}' | b'#' | b'&' | b'*' | b'!'
        | b'|' | b'>' | b'%' | b'@' | b'`' => return Ok(None),
        _ => {
            let mut pos = col;
            loop {
                if pos >= bytes.len() {
                    return Ok(None);
                }
                if bytes[pos] == b'#' && is_ws(bytes[pos - 1]) {
                    return Ok(None);
                }
                if bytes[pos] == b':' && bytes.get(pos + 1).is_none_or(|b| is_ws(*b)) {
                    break;
                }
                pos += 1;
            }
            let key = line[col..pos].trim_end_matches([' ', '\t']);
            (key.to_string(), col + key.len())
        }
    };
    let colon = skip_ws(line, raw_end);
    if bytes.get(colon) != Some(&b':') || bytes.get(colon + 1).is_some_and(|b| !is_ws(*b)) {
        return Ok(None);
    }
    if key == "<<" {
        return unsupported("merge keys");
    }
    Ok(Some((key, line[col..raw_end].to_string(), colon + 1)))
}

/// Inline value after `:` or `- `: `None` when the rest of the line is empty
/// (maybe a comment).
struct Inline {
    node: Option<Node>,
    last: usize,
    comment: Option<String>,
}

struct Parser<'a> {
    lines: &'a [String],
    step: Option<usize>,
    seq_offset: Option<usize>,
}

impl Parser<'_> {
    /// Next content line at or after `from`: `(index, indent)`.
    fn next_content(&self, from: usize) -> Res<Option<(usize, usize)>> {
        for index in from..self.lines.len() {
            if let LineKind::Content(indent) = classify(&self.lines[index])? {
                return Ok(Some((index, indent)));
            }
        }
        Ok(None)
    }

    fn parse_document(&mut self) -> Res<MapNode> {
        if self.lines.iter().any(|line| {
            let t = line.trim_start();
            t.starts_with('%') || t.starts_with("...")
        }) {
            return unsupported("YAML directives or document end markers");
        }
        let Some((mut first, _)) = self.next_content(0)? else {
            return unsupported("empty document");
        };
        if self.lines[first].trim_end() == "---" {
            match self.next_content(first + 1)? {
                Some((next, _)) => first = next,
                None => return unsupported("empty document"),
            }
        }
        if !matches!(classify(&self.lines[first])?, LineKind::Content(0)) {
            return unsupported("indented root mapping");
        }
        let (map, last) = self.parse_map(first, 0)?;
        if self.next_content(last + 1)?.is_some() {
            return unsupported("content after the root mapping");
        }
        Ok(map)
    }

    fn parse_map(&mut self, first: usize, col: usize) -> Res<(MapNode, usize)> {
        let mut entries = Vec::new();
        let mut line = first;
        loop {
            let text = &self.lines[line];
            let Some((key, key_raw, after)) = parse_key(text, col)? else {
                return unsupported(format!("unrecognized mapping line {}", line + 1));
            };
            let (value, last, comment) = self.parse_entry_value(line, after, col)?;
            entries.push(Entry {
                key,
                key_raw,
                line,
                col,
                value,
                last,
                comment,
            });
            match self.next_content(last + 1)? {
                Some((next, indent)) if indent == col => {
                    if is_dash(&self.lines[next], col) {
                        return unsupported(format!("unexpected list item on line {}", next + 1));
                    }
                    line = next;
                }
                Some((next, indent)) if indent > col => {
                    return unsupported(format!("unexpected indentation on line {}", next + 1));
                }
                _ => return Ok((MapNode { entries, col }, last)),
            }
        }
    }

    fn parse_entry_value(
        &mut self,
        line: usize,
        after: usize,
        col: usize,
    ) -> Res<(Node, usize, Option<String>)> {
        let inline = self.parse_inline(line, after, col)?;
        if let Some(node) = inline.node {
            return Ok((node, inline.last, inline.comment));
        }
        let (node, last) = match self.next_content(line + 1)? {
            Some((next, indent)) if indent > col => {
                if is_dash(&self.lines[next], indent) {
                    self.seq_offset.get_or_insert(indent - col);
                    let (seq, last) = self.parse_seq(next, indent)?;
                    (Node::Seq(seq), last)
                } else {
                    self.step.get_or_insert(indent - col);
                    let (map, last) = self.parse_map(next, indent)?;
                    (Node::Map(map), last)
                }
            }
            Some((next, indent)) if indent == col && is_dash(&self.lines[next], col) => {
                self.seq_offset.get_or_insert(0);
                let (seq, last) = self.parse_seq(next, col)?;
                (Node::Seq(seq), last)
            }
            _ => (Node::Other, line),
        };
        Ok((node, last, inline.comment))
    }

    fn parse_seq(&mut self, first: usize, col: usize) -> Res<(SeqNode, usize)> {
        let mut items = Vec::new();
        let mut line = first;
        loop {
            let (value, last, comment) = self.parse_item_value(line, col)?;
            items.push(Item {
                line,
                col,
                value,
                last,
                comment,
            });
            match self.next_content(last + 1)? {
                Some((next, indent)) if indent == col && is_dash(&self.lines[next], col) => {
                    line = next;
                }
                Some((next, indent)) if indent > col => {
                    return unsupported(format!("unexpected indentation on line {}", next + 1));
                }
                _ => return Ok((SeqNode { items, col }, last)),
            }
        }
    }

    fn parse_item_value(&mut self, line: usize, col: usize) -> Res<(Node, usize, Option<String>)> {
        let text = &self.lines[line];
        let pos = skip_ws(text, col + 1);
        if pos < text.len() && text.as_bytes()[pos] != b'#' && parse_key(text, pos)?.is_some() {
            let (map, last) = self.parse_map(line, pos)?;
            let comment = map.entries.first().and_then(|entry| entry.comment.clone());
            return Ok((Node::Map(map), last, comment));
        }
        if is_dash(text, pos) {
            return unsupported("nested inline list");
        }
        let inline = self.parse_inline(line, col + 1, col)?;
        if let Some(node) = inline.node {
            return Ok((node, inline.last, inline.comment));
        }
        let (node, last) = match self.next_content(line + 1)? {
            Some((next, indent)) if indent > col => {
                if is_dash(&self.lines[next], indent) {
                    let (seq, last) = self.parse_seq(next, indent)?;
                    (Node::Seq(seq), last)
                } else {
                    let (map, last) = self.parse_map(next, indent)?;
                    (Node::Map(map), last)
                }
            }
            _ => (Node::Other, line),
        };
        Ok((node, last, inline.comment))
    }

    /// Parse the value starting at `line[from..]`; `parent` is the column of
    /// the owning key or dash (continuation lines are indented past it).
    fn parse_inline(&self, line: usize, from: usize, parent: usize) -> Res<Inline> {
        let text = &self.lines[line];
        let bytes = text.as_bytes();
        let pos = skip_ws(text, from);
        let comment_from = |end: usize| -> Res<Option<String>> {
            let rest = &text[end..];
            let body = rest.trim_start_matches([' ', '\t']);
            if body.is_empty() {
                Ok(None)
            } else if body.starts_with('#') && (rest.len() != body.len() || end == from) {
                Ok(Some(rest.to_string()))
            } else {
                unsupported(format!(
                    "unexpected text after a value on line {}",
                    line + 1
                ))
            }
        };
        if pos >= bytes.len() || bytes[pos] == b'#' {
            return Ok(Inline {
                node: None,
                last: line,
                comment: comment_from(from)?,
            });
        }
        let (node, end) = match bytes[pos] {
            b'&' | b'*' | b'!' => return unsupported("anchors, aliases, or tags"),
            b'?' | b'%' | b'@' | b'`' => {
                return unsupported(format!("unsupported value on line {}", line + 1));
            }
            b'-' if is_dash(text, pos) => return unsupported("nested inline list"),
            b'|' | b'>' => return self.block_scalar(line, parent),
            b'[' | b'{' => {
                let Some(end) = flow_end(text, pos) else {
                    return unsupported(format!("multi-line flow value on line {}", line + 1));
                };
                (Node::Other, end)
            }
            b'"' | b'\'' => {
                let Some(end) = quoted_end(text, pos) else {
                    return unsupported(format!("multi-line quoted value on line {}", line + 1));
                };
                let quote = if bytes[pos] == b'"' {
                    Quote::Double
                } else {
                    Quote::Single
                };
                (
                    Node::Scalar {
                        line,
                        start: pos,
                        end,
                        quote,
                    },
                    end,
                )
            }
            _ => {
                let mut end = bytes.len();
                for index in pos + 1..bytes.len() {
                    if bytes[index] == b'#' && is_ws(bytes[index - 1]) {
                        end = index;
                        break;
                    }
                }
                let end = pos + text[pos..end].trim_end_matches([' ', '\t']).len();
                (
                    Node::Scalar {
                        line,
                        start: pos,
                        end,
                        quote: Quote::Plain,
                    },
                    end,
                )
            }
        };
        let comment = comment_from(end)?;
        if let Some((next, indent)) = self.next_content(line + 1)?
            && indent > parent
        {
            return unsupported(format!("multi-line value continues on line {}", next + 1));
        }
        Ok(Inline {
            node: Some(node),
            last: line,
            comment,
        })
    }

    /// `|` / `>` block scalar: every following blank line or line indented
    /// past `parent` belongs to it.
    fn block_scalar(&self, line: usize, parent: usize) -> Res<Inline> {
        let mut last = line;
        for index in line + 1..self.lines.len() {
            let text = &self.lines[index];
            if text.trim().is_empty() {
                continue;
            }
            let indent = text.len() - text.trim_start_matches(' ').len();
            if indent <= parent {
                break;
            }
            last = index;
        }
        Ok(Inline {
            node: Some(Node::Other),
            last,
            comment: None,
        })
    }
}

// ---------------------------------------------------------------- editing

struct Editor<'a> {
    lines: &'a [String],
    step: usize,
    seq_offset: usize,
    splices: Vec<Splice>,
}

fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
    )
}

fn render_scalar(value: &Value) -> Res<String> {
    if !is_scalar(value) {
        return unsupported("not a scalar");
    }
    let rendered = serde_norway::to_string(value)
        .map_err(|error| Unsupported(format!("render scalar: {error}")))?;
    let rendered = rendered.strip_suffix('\n').unwrap_or(&rendered);
    if rendered.contains('\n') {
        return unsupported("multi-line scalar");
    }
    Ok(rendered.to_string())
}

/// Render a replacement scalar, keeping the quote style of the old one.
fn render_scalar_styled(value: &Value, quote: Quote) -> Res<String> {
    match (value, quote) {
        (Value::String(text), Quote::Double) => serde_json::to_string(text)
            .map_err(|error| Unsupported(format!("render scalar: {error}"))),
        (Value::String(text), Quote::Single) if !text.chars().any(char::is_control) => {
            Ok(format!("'{}'", text.replace('\'', "''")))
        }
        _ => render_scalar(value),
    }
}

fn render_key(key: &Value) -> Res<String> {
    match key {
        Value::String(_) => render_scalar(key),
        _ => unsupported("non-string mapping key"),
    }
}

fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

impl Editor<'_> {
    fn render_entry(&self, key_raw: &str, value: &Value, col: usize) -> Res<Vec<String>> {
        let pad = " ".repeat(col);
        Ok(match value {
            Value::Mapping(map) if map.is_empty() => vec![format!("{pad}{key_raw}: {{}}")],
            Value::Sequence(seq) if seq.is_empty() => vec![format!("{pad}{key_raw}: []")],
            Value::Mapping(map) => {
                let mut lines = vec![format!("{pad}{key_raw}:")];
                for (key, value) in map {
                    lines.extend(self.render_entry(&render_key(key)?, value, col + self.step)?);
                }
                lines
            }
            Value::Sequence(seq) => {
                let mut lines = vec![format!("{pad}{key_raw}:")];
                for item in seq {
                    lines.extend(self.render_item(item, col + self.seq_offset)?);
                }
                lines
            }
            Value::Tagged(_) => return unsupported("tagged value"),
            scalar => vec![format!("{pad}{key_raw}: {}", render_scalar(scalar)?)],
        })
    }

    fn render_item(&self, value: &Value, col: usize) -> Res<Vec<String>> {
        let pad = " ".repeat(col);
        Ok(match value {
            Value::Mapping(map) if map.is_empty() => vec![format!("{pad}- {{}}")],
            Value::Sequence(seq) if seq.is_empty() => vec![format!("{pad}- []")],
            Value::Mapping(map) => {
                let mut lines = Vec::new();
                for (key, value) in map {
                    lines.extend(self.render_entry(&render_key(key)?, value, col + 2)?);
                }
                if let Some(first) = lines.first_mut() {
                    *first = format!("{pad}- {}", &first[col + 2..]);
                }
                lines
            }
            Value::Sequence(_) => return unsupported("nested list"),
            Value::Tagged(_) => return unsupported("tagged value"),
            scalar => vec![format!("{pad}- {}", render_scalar(scalar)?)],
        })
    }

    /// Replace lines `first..=last` with `rendered`, keeping the first
    /// line's prefix before `col` (indentation or a `- ` dash) and its
    /// trailing comment.
    fn replace_lines(
        &mut self,
        first: usize,
        last: usize,
        col: usize,
        mut rendered: Vec<String>,
        comment: Option<&String>,
    ) {
        if let Some(head) = rendered.first_mut() {
            let mut line = format!("{}{}", &self.lines[first][..col], &head[col..]);
            if let Some(comment) = comment {
                line.push_str(comment);
            }
            *head = line;
        }
        self.splices.push(Splice {
            start: first,
            end: last + 1,
            lines: rendered,
        });
    }

    /// Run `edit`; on failure drop its partial splices and run `fallback`.
    fn attempt(
        &mut self,
        edit: impl FnOnce(&mut Self) -> Res<()>,
        fallback: impl FnOnce(&mut Self) -> Res<()>,
    ) -> Res<()> {
        let mark = self.splices.len();
        if edit(self).is_ok() {
            return Ok(());
        }
        self.splices.truncate(mark);
        fallback(self)
    }

    fn diff_node(&mut self, node: &Node, old: &Value, new: &Value) -> Res<()> {
        match (node, old, new) {
            (Node::Map(map), Value::Mapping(old), Value::Mapping(new)) => {
                self.diff_map(map, old, new)
            }
            (Node::Seq(seq), Value::Sequence(old), Value::Sequence(new)) => {
                self.diff_seq(seq, old, new)
            }
            (
                Node::Scalar {
                    line,
                    start,
                    end,
                    quote,
                },
                old,
                new,
            ) if is_scalar(old) && is_scalar(new) => {
                let text = &self.lines[*line];
                let value = render_scalar_styled(new, *quote)?;
                self.splices.push(Splice {
                    start: *line,
                    end: line + 1,
                    lines: vec![format!("{}{value}{}", &text[..*start], &text[*end..])],
                });
                Ok(())
            }
            _ => unsupported("value changes shape"),
        }
    }

    fn diff_map(&mut self, node: &MapNode, old: &Mapping, new: &Mapping) -> Res<()> {
        if node.entries.len() != old.len()
            || node
                .entries
                .iter()
                .any(|entry| !old.contains_key(Value::String(entry.key.clone())))
        {
            return unsupported("mapping keys do not match the parsed document");
        }
        let mut kept = 0;
        for entry in &node.entries {
            let key = Value::String(entry.key.clone());
            let Some(new_value) = new.get(&key) else {
                if indent_of(&self.lines[entry.line]) != entry.col {
                    return unsupported("removed key shares a line with a list dash");
                }
                self.splices.push(Splice {
                    start: entry.line,
                    end: entry.last + 1,
                    lines: Vec::new(),
                });
                continue;
            };
            kept += 1;
            let Some(old_value) = old.get(&key) else {
                return unsupported("mapping keys do not match the parsed document");
            };
            if old_value == new_value {
                continue;
            }
            self.attempt(
                |editor| editor.diff_node(&entry.value, old_value, new_value),
                |editor| {
                    let rendered = editor.render_entry(&entry.key_raw, new_value, entry.col)?;
                    editor.replace_lines(
                        entry.line,
                        entry.last,
                        entry.col,
                        rendered,
                        entry.comment.as_ref(),
                    );
                    Ok(())
                },
            )?;
        }
        if kept == 0 {
            return unsupported("mapping would become empty");
        }
        let mut added = Vec::new();
        for (key, value) in new {
            if !old.contains_key(key) {
                added.extend(self.render_entry(&render_key(key)?, value, node.col)?);
            }
        }
        if !added.is_empty() {
            let at = node.entries.last().map_or(0, |entry| entry.last + 1);
            self.splices.push(Splice {
                start: at,
                end: at,
                lines: added,
            });
        }
        Ok(())
    }

    fn diff_seq(&mut self, node: &SeqNode, old: &[Value], new: &[Value]) -> Res<()> {
        if node.items.len() != old.len() {
            return unsupported("list length does not match the parsed document");
        }
        if new.len() == old.len() {
            for ((item, old_value), new_value) in node.items.iter().zip(old).zip(new) {
                if old_value == new_value {
                    continue;
                }
                self.attempt(
                    |editor| editor.diff_node(&item.value, old_value, new_value),
                    |editor| {
                        let rendered = editor.render_item(new_value, item.col)?;
                        editor.replace_lines(
                            item.line,
                            item.last,
                            item.col,
                            rendered,
                            item.comment.as_ref(),
                        );
                        Ok(())
                    },
                )?;
            }
            return Ok(());
        }
        if new.is_empty() {
            return unsupported("list would become empty");
        }
        if new.len() < old.len() {
            let mut next = 0;
            let mut removed = Vec::new();
            for (index, value) in old.iter().enumerate() {
                if new.get(next) == Some(value) {
                    next += 1;
                } else {
                    removed.push(index);
                }
            }
            if next != new.len() {
                return unsupported("list change is not a removal");
            }
            for index in removed {
                let item = &node.items[index];
                if indent_of(&self.lines[item.line]) != item.col {
                    return unsupported("removed item shares a line");
                }
                self.splices.push(Splice {
                    start: item.line,
                    end: item.last + 1,
                    lines: Vec::new(),
                });
            }
            return Ok(());
        }
        if new[..old.len()] != *old {
            return unsupported("list change is not an append");
        }
        let mut added = Vec::new();
        for value in &new[old.len()..] {
            added.extend(self.render_item(value, node.col)?);
        }
        let at = node.items.last().map_or(0, |item| item.last + 1);
        self.splices.push(Splice {
            start: at,
            end: at,
            lines: added,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn parse(yaml: &str) -> Value {
        serde_norway::from_str(yaml).unwrap()
    }

    /// Set `providers.gw.kind` to `anthropic` through the minimal editor.
    fn set_kind(yaml: &str) -> Res<String> {
        let old = parse(yaml);
        let mut new = old.clone();
        new["providers"]["gw"]["kind"] = Value::String("anthropic".into());
        let out = minimal_edit(yaml, &old, &new)?;
        assert_eq!(parse(&out), new, "{out}");
        Ok(out)
    }

    #[test]
    fn bom_and_leading_document_marker_are_kept() {
        let yaml = "\u{feff}# c\nproviders:\n  gw:\n    kind: openai # k\n";
        assert_eq!(
            set_kind(yaml).unwrap(),
            "\u{feff}# c\nproviders:\n  gw:\n    kind: anthropic # k\n"
        );
        let yaml = "---\n# c\nproviders:\n  gw:\n    kind: openai # k\n";
        assert_eq!(
            set_kind(yaml).unwrap(),
            "---\n# c\nproviders:\n  gw:\n    kind: anthropic # k\n"
        );
    }

    #[test]
    fn missing_trailing_newline_is_kept() {
        let yaml = "providers:\n  gw:\n    kind: openai";
        assert_eq!(
            set_kind(yaml).unwrap(),
            "providers:\n  gw:\n    kind: anthropic"
        );
    }

    #[test]
    fn unsafe_structures_are_declined() {
        for yaml in [
            // anchors / aliases
            "a: &x 1\nb: *x\nproviders:\n  gw:\n    kind: openai\n",
            // merge keys
            "base: {kind: openai}\nproviders:\n  gw:\n    <<: {kind: openai}\n    base_url: x\n",
            // multi-line quoted scalar
            "note: \"one\n  two\"\nproviders:\n  gw:\n    kind: openai\n",
            // multi-line plain scalar
            "note: one\n  two\nproviders:\n  gw:\n    kind: openai\n",
            // multi-line flow collection
            "list: [a,\n  b]\nproviders:\n  gw:\n    kind: openai\n",
            // directives
            "%YAML 1.2\n---\nproviders:\n  gw:\n    kind: openai\n",
            // tags
            "note: !!str 1\nproviders:\n  gw:\n    kind: openai\n",
        ] {
            assert!(set_kind(yaml).is_err(), "{yaml}");
        }
        // Mixed line endings.
        let yaml = "providers:\r\n  gw:\n    kind: openai\r\n";
        let old = parse(yaml);
        assert!(minimal_edit(yaml, &old, &old).is_err());
    }

    #[test]
    fn block_scalars_and_flow_values_elsewhere_are_opaque() {
        let yaml = "prompt: >\n  folded\n  # text\nlist: [a, {b: c}]  # flow\nproviders:\n  gw:\n    kind: openai\n";
        assert_eq!(
            set_kind(yaml).unwrap(),
            "prompt: >\n  folded\n  # text\nlist: [a, {b: c}]  # flow\nproviders:\n  gw:\n    kind: anthropic\n"
        );
    }

    #[test]
    fn quoted_keys_and_single_quoted_values_keep_their_style() {
        let yaml = "\"providers\":\n  'gw':\n    kind: 'openai'   # q\n";
        assert_eq!(
            set_kind(yaml).unwrap(),
            "\"providers\":\n  'gw':\n    kind: 'anthropic'   # q\n"
        );
    }
}
