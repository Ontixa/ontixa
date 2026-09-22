//! `ontixad` — the persistent compile daemon over `Db`.
//!
//! One JSON request per line on stdin, one schema-1 envelope per line
//! on stdout (compact — daemons don't pretty-print). The session `Db`
//! persists across requests: `set` edits re-run only dirty queries,
//! and `stats` reports exactly which queries evaluated — the
//! observable unit of incremental work.
//!
//! Protocol:
//!
//! ```json
//! {"op":"open",    "path":"x.ixa"}                  → result {"file": N}
//! {"op":"set",     "path":"x.ixa", "text":"..."}    → updates source (opens if new)
//! {"op":"check",   "path":"x.ixa"}                  → check envelope; result.evaluated lists re-run queries
//! {"op":"explain", "path":"x.ixa", "symbol":"s"}    → explain envelope (symbol optional)
//! {"op":"fmt",     "path":"x.ixa"}                  → fmt envelope: canonical text, no mutation
//! {"op":"stats"}                                   → result {queries, oracle, last_evaluated}
//! {"op":"close",   "path":"x.ixa"}                  → drops the path mapping
//! {"op":"shutdown"}                                → exits 0
//! ```
//!
//! Malformed lines get a `daemon` envelope with an `io`-kind error —
//! the loop never dies on bad input.

use ontixa_cli::envelope::Envelope;
use ontixa_cli::explain::explain_result;
use ontixa_cli::workspace::{module_name, path_key, workspace_files};
use ontixa_db::{Artifacts, Db};
use ontixa_source::{FileId, SourceFile};
use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

/// Per-session state: the query engine plus path → file-id bindings.
/// `files` keys are canonicalized (`path_key`) so `./x.ixa` and
/// `x.ixa` share a slot; `display` is indexed by file id for
/// diagnostic rendering. `poisoned` marks a session whose handler
/// panicked — the `Db` may hold a half-applied mutation, so it is
/// never used again: reads error until `open`/`set` resets it.
#[derive(Default)]
struct Session {
    db: Db,
    files: HashMap<String, usize>,
    display: Vec<String>,
    poisoned: bool,
}

impl Session {
    /// Drops all state after a panic — a fresh `Db` incarnation, so
    /// no plan or memo from the poisoned session can leak through.
    fn reset(&mut self) {
        *self = Session::default();
    }

    /// The file index for `path`, opening/reading it if needed.
    fn open(&mut self, path: &str) -> Result<usize, String> {
        let key = path_key(Path::new(path));
        if let Some(&f) = self.files.get(&key) {
            return Ok(f);
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        Ok(self.bind(path, text))
    }

    /// Registers `path` with `text`, reusing the slot when known.
    /// The file provides a module named by its stem.
    fn bind(&mut self, path: &str, text: String) -> usize {
        let key = path_key(Path::new(path));
        match self.files.get(&key) {
            Some(&f) => {
                self.db.set_source(f, text);
                f
            }
            None => self.bind_named(path, &key, text),
        }
    }

    /// Binds a fresh file slot providing `path`'s stem module.
    fn bind_named(&mut self, display: &str, key: &str, text: String) -> usize {
        let f = self
            .db
            .add_source_named(module_name(Path::new(display)), text);
        self.files.insert(key.to_string(), f);
        debug_assert_eq!(self.display.len(), f);
        self.display.push(display.to_string());
        f
    }

    /// Ensures `path`'s workspace is bound: the file itself plus
    /// every sibling `.ixa` not already bound. Client `set`s always
    /// win — a bound sibling is never re-read from disk.
    fn ensure_workspace(&mut self, path: &str) -> Result<usize, String> {
        let f = self.open(path)?;
        for (p, text) in workspace_files(Path::new(path)) {
            let key = path_key(&p);
            if self.files.contains_key(&key) {
                continue;
            }
            if let Some(text) = text {
                self.bind_named(&p.display().to_string(), &key, text);
            }
        }
        Ok(f)
    }

    /// `SourceFile` views of every bound file, indexed by file id —
    /// diagnostics render against their `file` tag.
    fn source_files(&self) -> Vec<SourceFile> {
        (0..self.display.len())
            .map(|f| {
                SourceFile::new(
                    FileId::new(f as u32),
                    self.display[f].clone(),
                    Some(PathBuf::from(&self.display[f])),
                    self.db.source(f).to_string(),
                )
            })
            .collect()
    }
}

/// The query keys evaluated by the last demand — the evidence that
/// only dirty work re-ran. Serialized as `hir(0:1)`-style strings.
fn evaluated(db: &Db) -> Json {
    json!(
        db.last_evaluated()
            .iter()
            .map(|k| format!("{k:?}"))
            .collect::<Vec<_>>()
    )
}

/// Session-level counters for `stats`.
fn stats_result(db: &Db) -> Json {
    let o = db.oracle();
    json!({
        "queries": db.stats(),
        "oracle": {
            "facts_collected": o.last_collected,
            "facts_reused": o.last_reused,
            "rounds": o.last_rounds,
        },
        "last_evaluated": db
            .last_evaluated()
            .iter()
            .map(|k| format!("{k:?}"))
            .collect::<Vec<_>>(),
    })
}

/// Compiles a bound file and returns `(artifacts, evaluated-keys)`.
fn compile(s: &mut Session, f: usize) -> (Artifacts, Json) {
    let a = s.db.compile_owned(f);
    let e = evaluated(&s.db);
    (a, e)
}

fn handle(s: &mut Session, req: &Json) -> Json {
    let op = req["op"].as_str().unwrap_or("");
    let path = req["path"].as_str().unwrap_or("");
    if s.poisoned {
        // A handler panic may have left the Db mid-mutation — never
        // serve reads from it. `open`/`set` rebuilds the session
        // from scratch; everything else tells the client to reopen.
        if matches!(op, "open" | "set") {
            s.reset();
        } else {
            return Envelope::new("daemon")
                .error(
                    "internal",
                    "session was reset after an internal failure; reopen files",
                    3,
                )
                .into_parts()
                .0;
        }
    }
    match op {
        "open" | "set" => {
            // `set` carries text; `open` reads from disk.
            let r = match req["text"].as_str() {
                Some(text) => Ok(s.bind(path, text.to_string())),
                None => s.open(path),
            };
            match r {
                Ok(f) => {
                    Envelope::new("daemon")
                        .result(json!({"file": f}))
                        .into_parts()
                        .0
                }
                Err(msg) => Envelope::new("daemon").error("io", msg, 2).into_parts().0,
            }
        }
        "check" => match s.ensure_workspace(path) {
            Err(msg) => Envelope::new("check").error("io", msg, 2).into_parts().0,
            Ok(f) => {
                // Check-only demand: diagnostics without graph/MIR.
                let report = s.db.check(f);
                let ev = evaluated(&s.db);
                let sfs = s.source_files();
                Envelope::new("check")
                    .diagnostics(&report.diags, &sfs)
                    .result(json!({"evaluated": ev}))
                    .into_parts()
                    .0
            }
        },
        "explain" => match s.ensure_workspace(path) {
            Err(msg) => Envelope::new("explain").error("io", msg, 2).into_parts().0,
            Ok(f) => {
                let (mut a, ev) = compile(s, f);
                a.diags.sort();
                let (mut result, extra) = explain_result(&a, req["symbol"].as_str(), path);
                if let Json::Object(m) = &mut result {
                    m.insert("evaluated".into(), ev);
                }
                let sfs = s.source_files();
                let mut e = Envelope::new("explain")
                    .diagnostics(&a.diags, &sfs)
                    .result(result);
                if let Some(d) = &extra {
                    e = e.extra_diagnostic(d, &sfs);
                }
                e.into_parts().0
            }
        },
        // `rename` is a transaction: preview returns the planned
        // edits plus the revision they were computed against; apply
        // requires that revision back as a stale guard. The daemon
        // updates its own sources only — persisting is the client's
        // job (the response carries the new texts). `at` selects a
        // local binding by byte offset instead of `symbol`.
        "rename" => match s.ensure_workspace(path) {
            Err(msg) => Envelope::new("rename").error("io", msg, 2).into_parts().0,
            Ok(f) => {
                let symbol = req["symbol"].as_str().unwrap_or("");
                let to = req["to"].as_str().unwrap_or("");
                let apply = req["apply"].as_bool().unwrap_or(false);
                if apply && req["revision"].as_u64() != Some(s.db.revision()) {
                    let d = ontixa_diagnostics::Diagnostic::error(
                        ontixa_diagnostics::Code::StaleRevision,
                        format!(
                            "workspace changed since the rename was planned \
                             (revision {} → {}); re-plan and retry",
                            req["revision"].as_u64().unwrap_or(0),
                            s.db.revision(),
                        ),
                    );
                    Envelope::new("rename")
                        .extra_diagnostic(&d, &s.source_files())
                        .into_parts()
                        .0
                } else {
                    let planned = match req["at"].as_u64() {
                        Some(off) => s.db.plan_rename_at(f, f, off as u32, to),
                        None => s.db.plan_rename(f, symbol, to),
                    };
                    match planned {
                        Err(e) => ontixa_cli::rename::rejection_json(&e, &s.source_files()),
                        Ok(plan) if apply => match s.db.apply_rename(&plan) {
                            Err(e) => ontixa_cli::rename::rejection_json(&e, &s.source_files()),
                            Ok(rep) => {
                                let sfs = s.source_files();
                                let mut result =
                                    ontixa_cli::rename::applied_json(&plan, &rep, &sfs);
                                result["new_sources"] = json!(
                                    plan.new_sources()
                                        .iter()
                                        .map(|(f, text)| json!({
                                            "file": sfs[*f].name(),
                                            "path": sfs[*f].path().map(|p| p.display().to_string()),
                                            "text": text,
                                        }))
                                        .collect::<Vec<_>>()
                                );
                                Envelope::new("rename")
                                    .diagnostics(&rep.diags, &sfs)
                                    .result(result)
                                    .into_parts()
                                    .0
                            }
                        },
                        Ok(plan) => {
                            let sfs = s.source_files();
                            Envelope::new("rename")
                                .result(ontixa_cli::rename::plan_json(&plan, &sfs, false))
                                .into_parts()
                                .0
                        }
                    }
                }
            }
        },
        // `fmt` formats the bound source without mutating it — the
        // canonical text comes back in `result.formatted`; a client
        // that wants it installed issues a `set` with that text
        // (same split as `rename`, which returns `new_sources`).
        // Formatting is syntactic and per-file, so `open` alone —
        // no workspace load — is enough.
        "fmt" => match s.open(path) {
            Err(msg) => Envelope::new("fmt").error("io", msg, 2).into_parts().0,
            Ok(f) => {
                let text = s.db.source(f).to_string();
                match ontixa_syntax::format_file(&text) {
                    Ok(formatted) => {
                        Envelope::new("fmt")
                            .result(json!({
                                "file": path,
                                "changed": formatted != text,
                                "formatted": formatted,
                            }))
                            .into_parts()
                            .0
                    }
                    Err(mut diags) => {
                        diags.tag_file_from(0, FileId::new(f as u32));
                        Envelope::new("fmt")
                            .diagnostics(&diags, &s.source_files())
                            .into_parts()
                            .0
                    }
                }
            }
        },
        "stats" => {
            Envelope::new("daemon")
                .result(stats_result(&s.db))
                .into_parts()
                .0
        }
        "close" => {
            let had = s.files.remove(&path_key(Path::new(path))).is_some();
            Envelope::new("daemon")
                .result(json!({"closed": had}))
                .into_parts()
                .0
        }
        other => {
            Envelope::new("daemon")
                .error("io", format!("unknown op `{other}`"), 2)
                .into_parts()
                .0
        }
    }
}

fn main() {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut s = Session::default();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Json = match serde_json::from_str(line) {
            Ok(j) => j,
            Err(e) => {
                let (doc, _) = Envelope::new("daemon")
                    .error("io", format!("malformed request: {e}"), 2)
                    .into_parts();
                let _ = writeln!(out, "{doc}");
                let _ = out.flush();
                continue;
            }
        };
        if req["op"].as_str() == Some("shutdown") {
            return;
        }
        let resp = catch_unwind(AssertUnwindSafe(|| handle(&mut s, &req))).unwrap_or_else(|_| {
            // The Db may hold a half-applied mutation — poison the
            // session so nothing reads it until a reset.
            s.poisoned = true;
            Envelope::new("daemon")
                .error(
                    "internal",
                    "panic in request handler; session requires reset",
                    3,
                )
                .into_parts()
                .0
        });
        let _ = writeln!(out, "{resp}");
        let _ = out.flush();
    }
}
