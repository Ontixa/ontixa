//! Request-owned source snapshots for the read-only daemon comparison.
//!
//! No filesystem loader, session, revision, or editing transaction participates.
//! Each side compiles in its own temporary database; all input is in the request.

use super::{Comparison, Compiled, valid_module};
use crate::envelope::{CompileFailure, Envelope};
use ontixa_db::Db;
use ontixa_diagnostics::{Code, Diagnostic};
use ontixa_source::{FileId, SourceFile};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::BTreeSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

#[derive(Deserialize)]
enum Operation {
    #[serde(rename = "diff")]
    Diff,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    #[serde(rename = "op")]
    _op: Operation,
    semantic: bool,
    before: Sources,
    after: Sources,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sources {
    entry: String,
    sources: Vec<Source>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Source {
    module: String,
    text: String,
}

impl Sources {
    fn validate(&self, side: &str) -> Result<(), String> {
        if !valid_module(&self.entry) {
            return Err(format!(
                "{side}.entry must be an ASCII identifier module name"
            ));
        }
        let mut names = BTreeSet::new();
        for source in &self.sources {
            if !valid_module(&source.module) {
                return Err(format!("{side} source module must be an ASCII identifier"));
            }
            if !names.insert(source.module.as_str()) {
                return Err(format!(
                    "{side} contains duplicate module `{}`",
                    source.module
                ));
            }
        }
        if !names.contains(self.entry.as_str()) {
            return Err(format!(
                "{side} sources must contain entry module `{}`",
                self.entry
            ));
        }
        Ok(())
    }

    /// Called only after validation: the entry exists exactly once. Register it
    /// first, then sort all other modules so file IDs do not depend on input order.
    fn compile(mut self, side: &str) -> Compiled {
        self.sources.sort_by(|a, b| {
            (a.module != self.entry, &a.module).cmp(&(b.module != self.entry, &b.module))
        });
        let root = SourceFile::new(
            FileId::new(0),
            format!("{side}/{}.ixa", self.entry),
            None,
            self.sources[0].text.clone(),
        );
        catch_unwind(AssertUnwindSafe(|| {
            let mut db = Db::new();
            let mut files = Vec::with_capacity(self.sources.len());
            for source in self.sources {
                let file = db.add_source_named(&source.module, source.text.clone());
                files.push(SourceFile::new(
                    FileId::new(file as u32),
                    format!("{side}/{}.ixa", source.module),
                    None,
                    source.text,
                ));
            }
            (files, db.compile_owned(0))
        }))
        .map_err(|_| {
            CompileFailure::Ice(
                root,
                Box::new(Diagnostic::error(
                    Code::Internal,
                    "internal compiler error while compiling inline snapshot",
                )),
            )
        })
    }
}

fn invalid(message: impl Into<String>) -> Json {
    Envelope::new("diff")
        .error("invalid_comparison_request", message, 2)
        .into_parts()
        .0
}

/// One pure NDJSON response. In particular, this function cannot access the
/// daemon's session: successful and failing comparisons leave it untouched.
pub fn response(request: &Json) -> Json {
    isolated(|| compare_request(request))
}

/// A failure anywhere in this isolated query must not escape into the daemon's
/// session-poisoning boundary. Compiler panics get richer per-side failures.
fn isolated(work: impl FnOnce() -> Json) -> Json {
    catch_unwind(AssertUnwindSafe(work)).unwrap_or_else(|_| {
        Envelope::new("diff")
            .error("internal", "internal inline comparison failure", 3)
            .into_parts()
            .0
    })
}

fn compare_request(request: &Json) -> Json {
    // Serde structs also accept positional sequences. The protocol deliberately
    // requires named objects, so check those boundaries before deserializing.
    if !request.is_object() {
        return invalid("inline comparison must be an object");
    }
    for side in ["before", "after"] {
        let Some(sources) = request.get(side).and_then(Json::as_object) else {
            return invalid(format!("{side} must be an inline workspace object"));
        };
        let Some(sources) = sources.get("sources").and_then(Json::as_array) else {
            return invalid(format!("{side}.sources must be an array of source objects"));
        };
        if sources.iter().any(|source| !source.is_object()) {
            return invalid(format!("{side}.sources entries must be source objects"));
        }
    }
    let request: Request = match serde_json::from_value(request.clone()) {
        Ok(request) => request,
        Err(error) => return invalid(format!("invalid inline comparison: {error}")),
    };
    if !request.semantic {
        return invalid("inline comparison requires semantic: true");
    }
    for (side, sources) in [("before", &request.before), ("after", &request.after)] {
        if let Err(message) = sources.validate(side) {
            return invalid(message);
        }
    }
    let before_path = PathBuf::from(format!("before/{}.ixa", request.before.entry));
    let after_path = PathBuf::from(format!("after/{}.ixa", request.after.entry));
    let mut comparison = Comparison::new(
        &before_path,
        &after_path,
        request.before.compile("before"),
        request.after.compile("after"),
    );
    comparison.result["coverage"]["inputs"] = json!(
        "independent request-owned inline source compilations with the same compiler; explicit module sets and use reachability; no filesystem or session inputs"
    );
    for side in ["before", "after"] {
        comparison.result["inputs"][side]["source_kind"] = json!("inline");
    }
    comparison.document()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unexpected_comparison_failure_stays_inside_its_response() {
        let failed = isolated(|| panic!("synthetic comparison failure"));
        assert_eq!(failed["command"], "diff");
        assert_eq!(failed["error"]["kind"], "internal");
        assert_eq!(failed["success"], false);
        assert!(failed["result"].is_null());
        let sources = json!({
            "entry": "main", "sources": [{"module": "main", "text": "fn f() {}"}]
        });
        let next = response(&json!({
            "op": "diff", "semantic": true, "before": sources, "after": sources
        }));
        assert_eq!(next["success"], true);
    }
}
