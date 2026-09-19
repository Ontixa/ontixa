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
//! {"op":"stats"}                                   → result {queries, oracle, last_evaluated}
//! {"op":"close",   "path":"x.ixa"}                  → drops the path mapping
//! {"op":"shutdown"}                                → exits 0
//! ```
//!
//! Malformed lines get a `daemon` envelope with an `io`-kind error —
//! the loop never dies on bad input.

use ontixa_cli::envelope::Envelope;
use ontixa_cli::explain::explain_result;
use ontixa_db::{Artifacts, Db};
use ontixa_source::{FileId, SourceFile};
use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

/// Per-session state: the query engine plus path → file-id bindings.
#[derive(Default)]
struct Session {
    db: Db,
    files: HashMap<String, usize>,
}

impl Session {
    /// The file index for `path`, opening/reading it if needed.
    fn open(&mut self, path: &str) -> Result<usize, String> {
        if let Some(&f) = self.files.get(path) {
            return Ok(f);
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
        Ok(self.bind(path, text))
    }

    /// Registers `path` with `text`, reusing the slot when known.
    fn bind(&mut self, path: &str, text: String) -> usize {
        match self.files.get(path) {
            Some(&f) => {
                self.db.set_source(f, text);
                f
            }
            None => {
                let f = self.db.add_source(text);
                self.files.insert(path.to_string(), f);
                f
            }
        }
    }

    /// A `SourceFile` view of a bound path — for diagnostic spans.
    fn source_file(&self, path: &str, f: usize) -> SourceFile {
        SourceFile::new(
            FileId::new(f as u32),
            path.to_string(),
            Some(PathBuf::from(path)),
            self.db.source(f).to_string(),
        )
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
        "check" => match s.open(path) {
            Err(msg) => Envelope::new("check").error("io", msg, 2).into_parts().0,
            Ok(f) => {
                let sf = s.source_file(path, f);
                let (a, ev) = compile(s, f);
                Envelope::new("check")
                    .diagnostics(&a.diags, &sf)
                    .result(json!({"evaluated": ev}))
                    .into_parts()
                    .0
            }
        },
        "explain" => match s.open(path) {
            Err(msg) => Envelope::new("explain").error("io", msg, 2).into_parts().0,
            Ok(f) => {
                let sf = s.source_file(path, f);
                let (mut a, ev) = compile(s, f);
                a.diags.sort();
                let (mut result, extra) = explain_result(&a, req["symbol"].as_str(), path);
                if let Json::Object(m) = &mut result {
                    m.insert("evaluated".into(), ev);
                }
                let mut e = Envelope::new("explain")
                    .diagnostics(&a.diags, &sf)
                    .result(result);
                if let Some(d) = &extra {
                    e = e.extra_diagnostic(d, &sf);
                }
                e.into_parts().0
            }
        },
        "stats" => {
            Envelope::new("daemon")
                .result(stats_result(&s.db))
                .into_parts()
                .0
        }
        "close" => {
            let had = s.files.remove(path).is_some();
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
            Envelope::new("daemon")
                .error("internal", "panic in request handler", 3)
                .into_parts()
                .0
        });
        let _ = writeln!(out, "{resp}");
        let _ = out.flush();
    }
}
