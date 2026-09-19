//! The [`Diagnostic`] record.

use crate::code::Code;
use ontixa_source::Span;
use serde_json::{Map as JsonMap, Value as JsonValue};

/// How bad the finding is. Errors reject the program; warnings allow
/// compilation to continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Compilation must not produce a runnable artifact.
    Error,
    /// Something suspicious that is not (yet) an error.
    Warning,
}

/// A secondary source annotation attached to a diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    /// The source range being annotated.
    pub span: Span,
    /// Short explanation of what this range means.
    pub message: String,
}

/// A structured compiler diagnostic.
///
/// `details` is the escape hatch for machine consumers: stable keys with
/// JSON values (typically span objects `{start, end}` or short strings).
/// Only add keys whose meaning is documented in `docs/diagnostics.md`.
#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// Stable code, e.g. `Code::UseAfterMove`.
    pub code: Code,
    /// Error or warning.
    pub severity: Severity,
    /// One-sentence description of what went wrong.
    pub message: String,
    /// Primary source range. `None` only for diagnostics with no
    /// meaningful location (e.g. missing `main`).
    pub primary: Option<Span>,
    /// Secondary labeled ranges, e.g. "moved here".
    pub labels: Vec<Label>,
    /// Extra context lines ("why"), rendered as `= note:` entries.
    pub notes: Vec<String>,
    /// Actionable suggestions, rendered as `= help:` entries.
    pub help: Vec<String>,
    /// Name of the semantic subject (symbol) when one exists, so machine
    /// consumers can join diagnostics with the semantic graph.
    pub subject: Option<String>,
    /// Structured extras (stable keys), e.g. `moved_at`, `reason`.
    pub details: JsonMap<String, JsonValue>,
}

impl Diagnostic {
    /// Starts building an error diagnostic.
    pub fn error(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            primary: None,
            labels: Vec::new(),
            notes: Vec::new(),
            help: Vec::new(),
            subject: None,
            details: JsonMap::new(),
        }
    }

    /// Starts building a warning diagnostic.
    pub fn warning(code: Code, message: impl Into<String>) -> Self {
        let mut d = Self::error(code, message);
        d.severity = Severity::Warning;
        d
    }

    /// Sets the primary span.
    pub fn primary(mut self, span: Span) -> Self {
        self.primary = Some(span);
        self
    }

    /// Adds a secondary labeled span.
    pub fn label(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label {
            span,
            message: message.into(),
        });
        self
    }

    /// Adds a note line.
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Adds a help line.
    pub fn help(mut self, help: impl Into<String>) -> Self {
        self.help.push(help.into());
        self
    }

    /// Sets the semantic subject symbol name.
    pub fn subject(mut self, name: impl Into<String>) -> Self {
        self.subject = Some(name.into());
        self
    }

    /// Adds a structured detail key/value.
    pub fn detail(mut self, key: &str, value: JsonValue) -> Self {
        self.details.insert(key.to_string(), value);
        self
    }

    /// Adds a structured span detail (`{"start": .., "end": ..}`).
    pub fn detail_span(self, key: &str, span: Span) -> Self {
        self.detail(
            key,
            serde_json::json!({"start": span.start, "end": span.end}),
        )
    }
}

/// An ordered collection of diagnostics with convenience queries.
#[derive(Debug, Default)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
}

impl Diagnostics {
    /// Creates an empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one diagnostic.
    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    /// Whether any diagnostic has error severity.
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    /// Number of error-severity diagnostics.
    pub fn error_count(&self) -> usize {
        self.items
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count()
    }

    /// Number of diagnostics total.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the collection is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Sorts deterministically by primary span, then code. Call before
    /// rendering when diagnostics were gathered by multiple passes.
    pub fn sort(&mut self) {
        self.items.sort_by(|a, b| {
            let ka = a
                .primary
                .map(|s| (s.start, s.end))
                .unwrap_or((u32::MAX, u32::MAX));
            let kb = b
                .primary
                .map(|s| (s.start, s.end))
                .unwrap_or((u32::MAX, u32::MAX));
            ka.cmp(&kb)
                .then_with(|| a.code.as_str().cmp(b.code.as_str()))
        });
    }

    /// All diagnostics in stored order.
    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.items.iter()
    }

    /// Consumes into the underlying vector.
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.items
    }

    /// Appends all diagnostics from `other`.
    pub fn extend(&mut self, other: Diagnostics) {
        self.items.extend(other.items);
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type Item = &'a Diagnostic;
    type IntoIter = std::slice::Iter<'a, Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}
