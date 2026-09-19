//! Immutable source file with a byte-offset line index.

use crate::ids::FileId;
use crate::span::Span;
use std::fmt;
use std::path::PathBuf;

/// A 1-based line/column position suitable for human-facing diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct LineCol {
    /// 1-based line number.
    pub line: u32,
    /// 1-based column number, counted in **characters** (not bytes) so it
    /// matches what editors display.
    pub column: u32,
}

/// An immutable UTF-8 source file.
///
/// `line_starts` holds the byte offset of the first byte of each line.
/// Line 1 starts at offset 0. `\r\n` sequences are treated as a single
/// line break, with the break attributed to the `\n`.
pub struct SourceFile {
    id: FileId,
    /// Display name used in diagnostics (usually the path).
    name: String,
    /// Filesystem path when the file came from disk.
    path: Option<PathBuf>,
    text: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    /// Creates a source file, building the line index.
    pub fn new(id: FileId, name: impl Into<String>, path: Option<PathBuf>, text: String) -> Self {
        let mut line_starts = vec![0];
        for (idx, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                line_starts.push(idx as u32 + 1);
            }
        }
        Self {
            id,
            name: name.into(),
            path,
            text,
            line_starts,
        }
    }

    /// File identity within the compilation session.
    pub fn id(&self) -> FileId {
        self.id
    }

    /// Display name for diagnostics.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Filesystem path, when known.
    pub fn path(&self) -> Option<&PathBuf> {
        self.path.as_ref()
    }

    /// The full UTF-8 text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Total length in bytes.
    pub fn len(&self) -> u32 {
        self.text.len() as u32
    }

    /// Whether the file is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The text covered by `span`, or `None` when the span is out of
    /// bounds. Callers dealing with parser-produced spans can rely on
    /// bounds already being checked; this is defensive for diagnostics.
    pub fn slice(&self, span: Span) -> Option<&str> {
        self.text.get(span.start as usize..span.end as usize)
    }

    /// 1-based line/column for a byte offset. Offsets past the end clamp
    /// to the last line.
    pub fn line_col(&self, offset: u32) -> LineCol {
        let line_idx = match self.line_starts.binary_search(&offset) {
            Ok(idx) => idx,
            Err(idx) => idx.saturating_sub(1),
        };
        let line_start = self.line_starts[line_idx] as usize;
        let line_end = self
            .line_starts
            .get(line_idx + 1)
            .map(|v| *v as usize)
            .unwrap_or(self.text.len());
        // Column = number of characters (not bytes) from the line start to
        // `offset`, clamped to the line end. `get` guards against offsets
        // that do not fall on a UTF-8 char boundary.
        let clamped = (offset as usize).clamp(line_start, line_end);
        let column = self
            .text
            .get(line_start..clamped)
            .map_or(0, |s| s.chars().count())
            + 1;
        LineCol {
            line: line_idx as u32 + 1,
            column: column as u32,
        }
    }

    /// 1-based line/column for the start of `span`.
    pub fn start_line_col(&self, span: Span) -> LineCol {
        self.line_col(span.start)
    }

    /// Byte range of a 1-based line, excluding the trailing newline, or
    /// `None` if `line` is out of range.
    pub fn line_span(&self, line: u32) -> Option<Span> {
        if line == 0 || line as usize > self.line_starts.len() {
            return None;
        }
        let start = self.line_starts[line as usize - 1];
        let mut end = self
            .line_starts
            .get(line as usize)
            .copied()
            .unwrap_or(self.len());
        // Strip the newline itself, and a preceding `\r`.
        if end > start && self.text.as_bytes().get(end as usize - 1) == Some(&b'\n') {
            end -= 1;
            if end > start && self.text.as_bytes().get(end as usize - 1) == Some(&b'\r') {
                end -= 1;
            }
        }
        Some(Span::new(start, end))
    }

    /// The text of a 1-based line without its line terminator.
    pub fn line_text(&self, line: u32) -> Option<&str> {
        self.line_span(line).and_then(|span| self.slice(span))
    }

    /// Number of lines in the file.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }
}

impl fmt::Debug for SourceFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceFile")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("len", &self.text.len())
            .field("lines", &self.line_starts.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> SourceFile {
        SourceFile::new(FileId::new(0), "test.ixa", None, text.to_string())
    }

    #[test]
    fn line_index_counts_newlines() {
        let f = file("a\nbb\nccc");
        assert_eq!(f.line_count(), 3);
        assert_eq!(f.line_col(0).line, 1);
        assert_eq!(f.line_col(2).line, 2);
        assert_eq!(f.line_col(5).line, 3);
        assert_eq!(f.line_col(7).line, 3);
    }

    #[test]
    fn columns_are_characters_not_bytes() {
        let f = file("let ünï = 1;");
        // 'ü' occupies two bytes; a byte offset after it should still be
        // a small column number.
        let offset_of_1 = f.text().find('1').unwrap() as u32;
        let lc = f.line_col(offset_of_1);
        assert_eq!(lc.line, 1);
        assert_eq!(lc.column, 11);
    }

    #[test]
    fn crlf_counts_as_one_break() {
        let f = file("a\r\nb");
        assert_eq!(f.line_count(), 2);
        assert_eq!(f.line_col(4).line, 2);
        assert_eq!(f.line_text(1), Some("a"));
        assert_eq!(f.line_text(2), Some("b"));
    }

    #[test]
    fn line_text_excludes_terminator() {
        let f = file("one\ntwo\n");
        assert_eq!(f.line_text(1), Some("one"));
        assert_eq!(f.line_text(2), Some("two"));
    }
}
