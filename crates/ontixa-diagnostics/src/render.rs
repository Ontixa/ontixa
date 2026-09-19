//! Human and JSON rendering of diagnostics.
//!
//! Human output follows the rust-style annotated-source convention:
//!
//! ```text
//! error[E_USE_AFTER_MOVE]: value `user` was used after ownership moved
//!   --> examples/use-after-move.ixa:13:12
//!    |
//! 12 |     let moved = identity(user);
//!    |                          ---- moved here
//! 13 |     return user.id;
//!    |            ^^^^ used here
//!    |
//!    = note: ownership escaped through call to `identity`
//! ```
//!
//! JSON output is schema version 1 — see `docs/diagnostics.md`.

use crate::diagnostic::{Diagnostic, Severity};
use ontixa_source::{SourceFile, Span};
use serde_json::{Map as JsonMap, Value as JsonValue};
use std::fmt::Write as _;

/// JSON schema version of the diagnostics document.
pub const DIAGNOSTICS_SCHEMA_VERSION: u32 = 1;

/// Renders one diagnostic as annotated source text.
pub fn render(d: &Diagnostic, file: &SourceFile) -> String {
    let mut out = String::new();
    let sev = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    let _ = writeln!(out, "{sev}[{}]: {}", d.code.as_str(), d.message);

    let primary = d.primary.unwrap_or_else(|| Span::empty(0));
    let lc = file.start_line_col(primary);
    let _ = writeln!(out, "  --> {}:{}:{}", file.name(), lc.line, lc.column);

    // Collect every span we annotate: primary + labels, in source order.
    let mut annotated: Vec<(Span, &str, bool)> = Vec::new();
    if let Some(p) = d.primary {
        annotated.push((p, "", true));
    }
    for l in &d.labels {
        annotated.push((l.span, l.message.as_str(), false));
    }
    annotated.sort_by_key(|(s, _, primary)| (s.start, !(*primary)));

    let line_no_width = annotated
        .iter()
        .map(|(s, _, _)| file.start_line_col(*s).line)
        .max()
        .unwrap_or(1)
        .to_string()
        .len();
    let gutter = " ".repeat(line_no_width);

    let _ = writeln!(out, "{gutter} |");

    let mut last_line: Option<u32> = None;
    for (span, message, is_primary) in &annotated {
        let start = file.start_line_col(*span);
        let end = file.line_col(span.end.saturating_sub(1).max(span.start));
        if last_line.is_some_and(|l| start.line > l + 1) {
            let _ = writeln!(out, "{gutter} ...");
        }
        if last_line == Some(start.line) {
            continue; // one annotation per line keeps output compact
        }
        last_line = Some(start.line);

        let text = file.line_text(start.line).unwrap_or("");
        let _ = writeln!(
            out,
            "{:>width$} | {}",
            start.line,
            text,
            width = line_no_width
        );

        // Caret line: primary uses `^^^`, secondary `---`.
        let mark = if *is_primary { '^' } else { '-' };
        let col_char_count = text
            .chars()
            .take(start.column.saturating_sub(1) as usize)
            .map(|c| if c == '\t' { '\t' } else { ' ' })
            .collect::<String>();
        let span_len = if end.line == start.line {
            end.column.saturating_sub(start.column) as usize
        } else {
            text.chars().count().saturating_sub(start.column as usize) + 1
        }
        .max(1);
        let _ = writeln!(
            out,
            "{gutter} | {col_char_count}{marks} {message}",
            marks = mark.to_string().repeat(span_len),
        );
    }

    if !annotated.is_empty() {
        let _ = writeln!(out, "{gutter} |");
    }
    for note in &d.notes {
        let _ = writeln!(out, "{gutter} = note: {note}");
    }
    for help in &d.help {
        let _ = writeln!(out, "{gutter} = help: {help}");
    }
    out
}

/// Renders all diagnostics joined by blank lines.
pub fn render_all(diags: &[Diagnostic], file: &SourceFile) -> String {
    diags
        .iter()
        .map(|d| render(d, file))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Serializes one diagnostic to the versioned JSON shape.
pub fn diagnostic_json(d: &Diagnostic, file: &SourceFile) -> JsonValue {
    let mut obj = JsonMap::new();
    obj.insert("code".into(), d.code.as_str().into());
    obj.insert(
        "severity".into(),
        match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
        .into(),
    );
    obj.insert("message".into(), d.message.clone().into());
    obj.insert(
        "primary".into(),
        match d.primary {
            Some(span) => serde_json::json!({
                "file": file.name(),
                "start": span.start,
                "end": span.end,
            }),
            None => JsonValue::Null,
        },
    );
    obj.insert(
        "labels".into(),
        d.labels
            .iter()
            .map(|l| {
                serde_json::json!({
                    "start": l.span.start,
                    "end": l.span.end,
                    "message": l.message,
                })
            })
            .collect(),
    );
    obj.insert("notes".into(), d.notes.clone().into());
    obj.insert("help".into(), d.help.clone().into());
    if let Some(subject) = &d.subject {
        obj.insert("subject".into(), subject.clone().into());
    }
    if !d.details.is_empty() {
        obj.insert("details".into(), JsonValue::Object(d.details.clone()));
    }
    JsonValue::Object(obj)
}

/// Serializes a full diagnostic list to the versioned document:
///
/// ```json
/// { "version": 1, "diagnostics": [ ... ] }
/// ```
pub fn to_json(diags: &[Diagnostic], file: &SourceFile) -> JsonValue {
    serde_json::json!({
        "version": DIAGNOSTICS_SCHEMA_VERSION,
        "diagnostics": diags.iter().map(|d| diagnostic_json(d, file)).collect::<Vec<_>>(),
    })
}
