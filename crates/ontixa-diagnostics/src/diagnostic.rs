//! The [`Diagnostic`] record.

use crate::code::Code;
use ontixa_source::{DefId, FileId, Span};
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
#[derive(Debug, Clone, PartialEq)]
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
    /// The definition this diagnostic's spans are relative to:
    /// `Some(d)` means `primary`/`labels`/span-shaped `details` are
    /// in `d`'s item-local coordinates; `None` means file-absolute.
    /// Internal pipeline metadata — `Diagnostics(file)` rebases
    /// tagged diagnostics and clears the tag before they surface.
    pub origin: Option<DefId>,
    /// The source file this diagnostic's spans index into. `None`
    /// means "the operation's root file" — set during collection or
    /// by [`Diagnostics::set_file`] in multi-file passes.
    pub file: Option<FileId>,
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
            origin: None,
            file: None,
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

    /// A copy with every span shifted right by `base` — rebasing
    /// item-relative coordinates back to file-absolute. The result's
    /// `origin` is cleared: an absolute diagnostic has no home item.
    pub fn rebased(&self, base: u32) -> Self {
        let mut d = self.clone();
        d.primary = d.primary.map(|s| s.abs(base));
        for l in &mut d.labels {
            l.span = l.span.abs(base);
        }
        for v in d.details.values_mut() {
            if let JsonValue::Object(o) = v {
                let shifted = match (o.get("start"), o.get("end")) {
                    (Some(s), Some(e)) => match (s.as_u64(), e.as_u64()) {
                        (Some(s), Some(e)) => Some((s + u64::from(base), e + u64::from(base))),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some((s, e)) = shifted {
                    o.insert("start".into(), JsonValue::from(s));
                    o.insert("end".into(), JsonValue::from(e));
                }
            }
        }
        d.origin = None;
        d
    }
}

/// An ordered collection of diagnostics with convenience queries.
#[derive(Debug, Default, Clone)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
    /// Stamped onto pushed diagnostics that don't already carry a
    /// `file` — set by passes that emit for a specific source file.
    current_file: Option<FileId>,
}

impl Diagnostics {
    /// Creates an empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one diagnostic. If [`Self::set_file`] established a
    /// current file and the diagnostic doesn't already carry one, it
    /// is stamped with it.
    pub fn push(&mut self, mut d: Diagnostic) {
        if d.file.is_none() {
            d.file = self.current_file;
        }
        self.items.push(d);
    }

    /// Sets the file stamped onto subsequently pushed diagnostics.
    /// Workspace-level passes call this once per file so each
    /// diagnostic knows which source text its spans index into.
    /// `None` restores unstamped pushing.
    pub fn set_file(&mut self, file: Option<FileId>) {
        self.current_file = file;
    }

    /// Tags `file` on diagnostics pushed at or after `mark` that
    /// don't already carry one — the post-hoc form of `set_file`,
    /// for collectors stamping a whole stage's output.
    pub fn tag_file_from(&mut self, mark: usize, file: FileId) {
        for d in &mut self.items[mark..] {
            d.file.get_or_insert(file);
        }
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

    /// Sorts deterministically by file, then primary span, then code.
    /// Call before rendering when diagnostics were gathered by
    /// multiple passes.
    pub fn sort(&mut self) {
        self.items.sort_by(|a, b| {
            let fa = a.file.map(|f| f.index()).unwrap_or(0);
            let fb = b.file.map(|f| f.index()).unwrap_or(0);
            let ka = a
                .primary
                .map(|s| (s.start, s.end))
                .unwrap_or((u32::MAX, u32::MAX));
            let kb = b
                .primary
                .map(|s| (s.start, s.end))
                .unwrap_or((u32::MAX, u32::MAX));
            fa.cmp(&fb)
                .then_with(|| ka.cmp(&kb))
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

    /// Tags `origin = def` on diagnostics pushed at or after `mark`
    /// that don't already carry one. Per-definition passes call this
    /// so a collector can rebase item-relative spans to absolute.
    pub fn tag_origin_from(&mut self, mark: usize, def: DefId) {
        for d in &mut self.items[mark..] {
            d.origin.get_or_insert(def);
        }
    }

    /// Rebases every `origin`-tagged diagnostic to file-absolute via
    /// `base_of` (def → its item's absolute start), clearing tags.
    /// Used by whole-module convenience paths; the database's
    /// `Diagnostics(file)` query does the same during collection.
    pub fn rebase_tagged(&mut self, base_of: impl Fn(DefId) -> u32) {
        for d in &mut self.items {
            if let Some(def) = d.origin {
                *d = d.rebased(base_of(def));
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `rebased` shifts primary, labels, and `{"start","end"}`
    /// detail objects by the item base — and clears `origin`, so a
    /// rebased diagnostic can never be rebased twice.
    #[test]
    fn rebased_shifts_spans_and_details() {
        let mut d = Diagnostic::error(Code::UseAfterMove, "moved")
            .primary(Span::new(10, 12))
            .label(Span::new(4, 5), "moved here")
            .detail("moved_at", serde_json::json!({"start": 4, "end": 5}));
        d.origin = Some(DefId::new(0));
        let r = d.rebased(100);
        assert_eq!(r.primary, Some(Span::new(110, 112)));
        assert_eq!(r.labels[0].span, Span::new(104, 105));
        assert_eq!(r.details["moved_at"]["start"], 104);
        assert_eq!(r.details["moved_at"]["end"], 105);
        assert!(r.origin.is_none());
    }

    /// `rebase_tagged` touches only `origin`-tagged diagnostics and
    /// resolves each tag through the supplied base lookup.
    #[test]
    fn rebase_tagged_shifts_only_tagged() {
        let mut ds = Diagnostics::new();
        ds.push(Diagnostic::error(Code::Parse, "abs").primary(Span::new(1, 2)));
        let mut tagged = Diagnostic::error(Code::UnknownType, "rel").primary(Span::new(3, 4));
        tagged.origin = Some(DefId::new(1));
        ds.push(tagged);
        ds.rebase_tagged(|d| {
            assert_eq!(d, DefId::new(1));
            50
        });
        let v: Vec<_> = ds.iter().collect();
        assert_eq!(v[0].primary, Some(Span::new(1, 2)));
        assert_eq!(v[1].primary, Some(Span::new(53, 54)));
        assert!(v.iter().all(|d| d.origin.is_none()));
    }
}
