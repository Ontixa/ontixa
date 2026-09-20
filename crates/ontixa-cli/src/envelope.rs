//! The single-document machine-output contract (schema 1).
//!
//! Every `--json` invocation writes **exactly one** envelope document
//! to stdout — never a diagnostics document followed by a payload
//! document, never bare logs, never ANSI. The shape is stable:
//!
//! ```json
//! {
//!   "schema": 1,
//!   "command": "check",
//!   "success": true,
//!   "diagnostics": [],
//!   "result": null,
//!   "timings": [],
//!   "error": null
//! }
//! ```
//!
//! - `diagnostics` — structured diagnostics (same per-item shape as
//!   `ontixa_diagnostics::to_json`).
//! - `result` — the command payload (`tokens`, `ast`, `mir`, `graph`,
//!   `symbol`, run `value`, ...), `null` when the command has none.
//! - `timings` — per-stage nanoseconds; empty unless `--timings`.
//!   Kept out by default so output is deterministic.
//! - `error` — `{kind, message}` for non-diagnostic failures:
//!   `io` (unreadable file), `runtime` (trap / missing entry),
//!   `internal` (ICE).

use ontixa_db::StageTiming;
use ontixa_diagnostics::{Diagnostic, Diagnostics, diagnostic_json_in};
use ontixa_source::SourceFile;
use serde_json::{Map as JsonMap, Value as Json};
use std::process::ExitCode;

/// Builds the one-document JSON response for a command.
pub struct Envelope {
    command: &'static str,
    diags: Vec<Json>,
    has_errors: bool,
    result: Json,
    timings: Json,
    error: Option<(u8, Json)>,
}

impl Envelope {
    /// A fresh envelope for `command` (clap subcommand name).
    pub fn new(command: &'static str) -> Self {
        Self {
            command,
            diags: Vec::new(),
            has_errors: false,
            result: Json::Null,
            timings: Json::Array(Vec::new()),
            error: None,
        }
    }

    /// Fills `diagnostics` from the compiled artifacts. `files` holds
    /// the workspace's source files — each diagnostic renders against
    /// the file its `file` tag names.
    pub fn diagnostics(mut self, diags: &Diagnostics, files: &[SourceFile]) -> Self {
        self.has_errors = diags.has_errors();
        self.diags = diags.iter().map(|d| diagnostic_json_in(d, files)).collect();
        self
    }

    /// Attaches a single extra diagnostic (e.g. an ambiguity report
    /// produced by `explain` rather than the compiler passes).
    pub fn extra_diagnostic(mut self, d: &Diagnostic, files: &[SourceFile]) -> Self {
        if d.severity == ontixa_diagnostics::Severity::Error {
            self.has_errors = true;
        }
        self.diags.push(diagnostic_json_in(d, files));
        self
    }

    /// Sets the command payload.
    pub fn result(mut self, result: Json) -> Self {
        self.result = result;
        self
    }

    /// Includes per-stage timings (opt-in — nondeterministic data).
    pub fn timings(mut self, timings: &[StageTiming]) -> Self {
        self.timings = serde_json::to_value(timings).unwrap_or(Json::Array(Vec::new()));
        self
    }

    /// Marks the command failed for a non-diagnostic reason. `code`
    /// is the process exit code (2 = io/runtime, 3 = internal).
    pub fn error(mut self, kind: &str, message: impl Into<String>, code: u8) -> Self {
        self.error = Some((
            code,
            Json::Object(JsonMap::from_iter([
                ("kind".into(), kind.into()),
                ("message".into(), message.into().into()),
            ])),
        ));
        self
    }

    /// The envelope document plus its exit code — the pure form, for
    /// callers that serialize it themselves (the daemon).
    pub fn into_parts(self) -> (Json, u8) {
        let success = !self.has_errors && self.error.is_none();
        let mut obj = JsonMap::new();
        obj.insert("schema".into(), 1.into());
        obj.insert("command".into(), self.command.into());
        obj.insert("success".into(), success.into());
        obj.insert("diagnostics".into(), Json::Array(self.diags));
        obj.insert("result".into(), self.result);
        obj.insert("timings".into(), self.timings);
        obj.insert(
            "error".into(),
            self.error
                .as_ref()
                .map(|(_, e)| e.clone())
                .unwrap_or(Json::Null),
        );
        let code = match self.error {
            Some((code, _)) => code,
            None if self.has_errors => 1,
            None => 0,
        };
        (Json::Object(obj), code)
    }

    /// Prints the envelope to stdout and returns the process exit code.
    pub fn emit(self) -> ExitCode {
        let (doc, code) = self.into_parts();
        println!("{}", serde_json::to_string_pretty(&doc).unwrap());
        ExitCode::from(code)
    }
}

/// Failure paths from `compile`: io errors and ICEs.
pub enum CompileFailure {
    /// The source file could not be read (exit 2).
    Io(String),
    /// The pipeline panicked; `diagnostic` is the `I_INTERNAL` record
    /// (exit 3). Boxed to keep the `Err` variant small.
    Ice(SourceFile, Box<Diagnostic>),
}

/// Reports a compile failure in the active output mode.
pub fn emit_failure(f: CompileFailure, command: &'static str, json: bool) -> ExitCode {
    match (f, json) {
        (CompileFailure::Io(msg), true) => Envelope::new(command).error("io", msg, 2).emit(),
        (CompileFailure::Io(msg), false) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
        (CompileFailure::Ice(sf, d), true) => Envelope::new(command)
            .extra_diagnostic(&d, std::slice::from_ref(&sf))
            .error("internal", d.message.clone(), 3)
            .emit(),
        (CompileFailure::Ice(sf, d), false) => {
            eprint!("{}", ontixa_diagnostics::render(&d, &sf));
            ExitCode::from(3)
        }
    }
}

/// Renders diagnostics for human mode; returns the exit code.
/// `files` holds the workspace's source files — each diagnostic
/// renders against the file its `file` tag names.
pub fn emit_human_diags(diags: &Diagnostics, files: &[SourceFile]) -> ExitCode {
    if !diags.is_empty() {
        let diags: Vec<Diagnostic> = diags.iter().cloned().collect();
        eprint!("{}", ontixa_diagnostics::render_all_in(&diags, files));
    }
    if diags.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Prints `--timings` output for human mode.
pub fn print_timings(timings: &[StageTiming]) {
    eprintln!("timings:");
    for t in timings {
        eprintln!("  {:>9}  {:>7} µs", t.stage, t.nanos / 1000);
    }
}
