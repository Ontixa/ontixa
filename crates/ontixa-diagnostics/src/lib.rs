//! Structured compiler diagnostics.
//!
//! Diagnostics are a compiler API, not formatted strings. Every diagnostic
//! carries a stable [`Code`], a [`Severity`], a human-oriented `message`,
//! source spans, secondary labels, notes, help text, and an optional
//! machine-readable `details` payload for semantic consumers.
//!
//! Two renderings exist:
//!
//! - [`render`] — rust-style annotated source output for humans.
//! - [`to_json`] — deterministic, versioned JSON for machines
//!   (`docs/diagnostics.md`, schema version 1).

mod code;
mod diagnostic;
mod render;

pub use code::Code;
pub use diagnostic::{Diagnostic, Diagnostics, Label, Severity};
pub use render::{
    diagnostic_json, diagnostic_json_in, render, render_all, render_all_in, render_in, to_json,
    to_json_in,
};
