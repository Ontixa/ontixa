//! Half-open byte-offset source spans.

use std::fmt;

/// A half-open byte range `[start, end)` into a source file.
///
/// Offsets are **byte** offsets into UTF-8 text, not character indices.
/// `end` is exclusive. A span where `start == end` is an empty insertion
/// point (used for "expected X here" diagnostics).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct Span {
    /// Byte offset of the first byte in the range.
    pub start: u32,
    /// Byte offset one past the last byte in the range.
    pub end: u32,
}

impl Span {
    /// Creates a span. Callers must uphold `start <= end`; in debug builds
    /// this invariant is checked.
    pub const fn new(start: u32, end: u32) -> Self {
        debug_assert!(start <= end, "span start must not exceed end");
        Self { start, end }
    }

    /// An empty span at `offset`.
    pub const fn empty(offset: u32) -> Self {
        Self::new(offset, offset)
    }

    /// Length in bytes.
    pub const fn len(self) -> u32 {
        self.end - self.start
    }

    /// Whether the span covers zero bytes.
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// Whether `offset` lies inside `[start, end)`.
    pub const fn contains(self, offset: u32) -> bool {
        offset >= self.start && offset < self.end
    }

    /// This span relative to `base`: `start - base .. end - base`.
    /// Item-local coordinates — per-definition query results carry
    /// relative spans so an edit that shifts an item's absolute
    /// offset leaves its memoized values unchanged. `base` is the
    /// item's absolute start; `self` must lie at or after it.
    pub const fn rel(self, base: u32) -> Self {
        debug_assert!(self.start >= base, "span lies before its item base");
        Self::new(self.start - base, self.end - base)
    }

    /// Inverse of [`Span::rel`]: shifts an item-relative span right
    /// by `base` to file-absolute coordinates.
    pub const fn abs(self, base: u32) -> Self {
        Self::new(self.start + base, self.end + base)
    }

    /// The smallest span covering both inputs.
    pub const fn covering(self, other: Span) -> Span {
        Span::new(
            if self.start < other.start {
                self.start
            } else {
                other.start
            },
            if self.end > other.end {
                self.end
            } else {
                other.end
            },
        )
    }
}

impl fmt::Debug for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}..{}", self.start, self.end)
    }
}

impl From<Span> for rowan::TextRange {
    fn from(span: Span) -> Self {
        rowan::TextRange::new(span.start.into(), span.end.into())
    }
}

impl From<rowan::TextRange> for Span {
    fn from(range: rowan::TextRange) -> Self {
        Span::new(u32::from(range.start()), u32::from(range.end()))
    }
}
