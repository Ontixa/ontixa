//! `ontixa fmt` — canonical formatting over the lossless CST.
//!
//! The formatter itself lives in [`ontixa_syntax::format_file`]; this
//! module holds the shared command machinery: per-file outcomes, the
//! `result` payload shape, and the staged-write path for `--write`.
//!
//! Contract (mirroring the other commands and `rename`'s
//! preview/apply split):
//!
//! * `ontixa fmt <files...>` prints each file's canonical form to
//!   stdout — the preview. Nothing is written.
//! * `--check` writes nothing and exits non-zero when any file would
//!   change (`result.would_change` in JSON mode).
//! * `--write` persists every changed file through the same staged,
//!   journaled transaction rename uses — disk bytes must still equal
//!   what was read, or nothing is written.
//! * a file with parse diagnostics is never reformatted: its
//!   diagnostics surface and the file is skipped (exit 1).

use serde_json::{Value as Json, json};

/// One input file's format outcome.
pub struct FileResult {
    /// Display path as given on the command line.
    pub file: String,
    /// `Some(bool)` when the file parsed — whether the canonical form
    /// differs from the input. `None` on parse failure.
    pub changed: Option<bool>,
    /// The canonical text — `None` on parse failure.
    pub formatted: Option<String>,
    /// Whether `--write` actually persisted this file.
    pub written: bool,
}

/// The `result` payload for `fmt` envelopes.
pub fn result_json(files: &[FileResult]) -> Json {
    json!({
        "would_change": files.iter().any(|f| f.changed == Some(true)),
        "files": files
            .iter()
            .map(|f| json!({
                "file": f.file,
                "changed": f.changed,
                "formatted": f.formatted,
                "written": f.written,
            }))
            .collect::<Vec<_>>(),
    })
}
