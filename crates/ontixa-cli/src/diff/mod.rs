//! Read-only comparison of explicitly bounded semantic fields.
//!
//! Each input is independently compiled by the CLI's existing workspace loader.
//! This module does not execute source or interact with editing transactions.

mod snapshot;

use crate::envelope::{CompileFailure, Envelope};
use ontixa_db::Artifacts;
use ontixa_diagnostics::{Diagnostic, diagnostic_json_in};
use ontixa_source::SourceFile;
use serde_json::{Value as Json, json};
use snapshot::{Snapshot, normalize, uncertainties, valid_module};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::ExitCode;

type Compiled = Result<(Vec<SourceFile>, Artifacts), CompileFailure>;

struct Input {
    info: Json,
    snapshot: Option<Snapshot>,
    diagnostics: Vec<Json>,
    human_diagnostics: String,
    code: u8,
}

impl Input {
    fn fail(&mut self, status: &str, message: impl Into<String>, code: u8) {
        self.info["status"] = json!(status);
        self.info["error"] = json!(message.into());
        self.snapshot = None;
        self.code = code;
    }

    fn diagnostics(&mut self, side: &str, diags: &[Diagnostic], files: &[SourceFile]) {
        self.diagnostics = diags
            .iter()
            .map(|d| {
                let mut record = diagnostic_json_in(d, files);
                record["comparison_side"] = json!(side);
                record
            })
            .collect();
        self.human_diagnostics = ontixa_diagnostics::render_all_in(diags, files);
    }
}

fn input(path: &Path, compiled: Compiled, side: &str) -> Input {
    let module = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let mut input = Input {
        info: json!({
            "path": path.display().to_string(),
            "module": module,
            "status": "ok",
            "error": null,
            "uncertainties": [],
        }),
        snapshot: None,
        diagnostics: Vec::new(),
        human_diagnostics: String::new(),
        code: 0,
    };
    match compiled {
        Err(CompileFailure::Io(message)) => input.fail("io", message, 2),
        Err(CompileFailure::Ice(file, diagnostic)) => {
            input.diagnostics(side, std::slice::from_ref(&diagnostic), &[file]);
            input.fail("internal", diagnostic.message, 3);
        }
        Ok((files, mut artifacts)) => {
            artifacts.diags.sort();
            let diagnostics: Vec<_> = artifacts.diags.iter().cloned().collect();
            input.diagnostics(side, &diagnostics, &files);
            if !artifacts.is_valid() {
                input.fail("source_errors", "source diagnostics prevent comparison", 1);
            } else if !valid_module(module) {
                input.fail(
                    "unresolved",
                    "entry module must have an ASCII identifier file stem",
                    1,
                );
            } else {
                match normalize(&artifacts) {
                    Ok(snapshot) => {
                        input.info["uncertainties"] = json!(uncertainties(&snapshot));
                        input.snapshot = Some(snapshot);
                    }
                    Err(message) => input.fail("unresolved", message, 1),
                }
            }
        }
    }
    input
}

fn coverage() -> Json {
    json!({
        "compared": [
            "qualified definition presence and kind",
            "ordered parameter names, resolved types and declared mutability",
            "resolved return types",
            "record/enum shape and ordered fields, variants, discriminants and payload types",
            "inferred parameter behavior and normalized escape target sets"
        ],
        "excluded": [
            "function bodies and runtime behavior",
            "local bindings, call graphs and import spellings",
            "evidence sites, source coordinates, comments and whitespace",
            "unreachable sibling modules",
            "compatibility, safety and semantic equivalence"
        ],
        "identity": "declaring module plus definition name; entry module names must match; renames are removal/addition",
        "inputs": "independent source compilations with the same compiler; existing sibling loading and use reachability",
        "uncertainty": "unknown ownership values are preserved and disclosed per input, including when compared fields are unchanged",
        "claim": "reports changes only in compared fields; no compatibility, safety or behavioral equality guarantee"
    })
}

fn field_changes(path: &str, before: &Json, after: &Json, out: &mut Vec<Json>) {
    if before == after {
        return;
    }
    match (before, after) {
        (Json::Object(a), Json::Object(b)) => {
            let keys: BTreeSet<_> = a.keys().chain(b.keys()).collect();
            for key in keys {
                let field = if path.is_empty() {
                    key.to_string()
                } else {
                    format!("{path}.{key}")
                };
                field_changes(
                    &field,
                    a.get(key).unwrap_or(&Json::Null),
                    b.get(key).unwrap_or(&Json::Null),
                    out,
                );
            }
        }
        (Json::Array(a), Json::Array(b)) if a.len() == b.len() => {
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                field_changes(&format!("{path}[{i}]"), a, b, out);
            }
        }
        _ => out.push(json!({"field": path, "before": before, "after": after})),
    }
}

fn compare(before: &Snapshot, after: &Snapshot) -> Vec<Json> {
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    let mut changes = Vec::new();
    for key in keys {
        let a = before.get(key);
        let b = after.get(key);
        if a == b {
            continue;
        }
        let kind = match (a, b) {
            (None, _) => "added",
            (_, None) => "removed",
            _ => "modified",
        };
        let a = json!(a);
        let b = json!(b);
        let mut fields = Vec::new();
        if kind == "modified" {
            field_changes("", &a, &b, &mut fields);
        }
        changes.push(json!({
            "definition": key, "kind": kind,
            "before": a, "after": b, "fields": fields,
        }));
    }
    changes
}

/// Emit a single schema-1 envelope, or a human report with the same coverage.
/// A difference is a successful result (exit 0), not a compatibility verdict.
pub fn run(
    before_path: &Path,
    after_path: &Path,
    before: Compiled,
    after: Compiled,
    json: bool,
) -> ExitCode {
    let mut before = input(before_path, before, "before");
    let mut after = input(after_path, after, "after");
    if before.info["module"] != after.info["module"] {
        let message = format!(
            "entry module names must match: before `{}`, after `{}`; no root aliasing is performed",
            before.info["module"].as_str().unwrap_or(""),
            after.info["module"].as_str().unwrap_or("")
        );
        for input in [&mut before, &mut after] {
            if input.code == 0 {
                input.fail("root_mismatch", &message, 1);
            }
        }
    }
    let changes = match (&before.snapshot, &after.snapshot) {
        (Some(a), Some(b)) => Some(compare(a, b)),
        _ => None,
    };
    let status = match &changes {
        Some(changes) if changes.is_empty() => "unchanged",
        Some(_) => "changed",
        None => "unavailable",
    };
    let code = before.code.max(after.code);
    let result = json!({
        "comparison_schema": 1,
        "coverage": coverage(),
        "inputs": {"before": before.info, "after": after.info},
        "status": status,
        "changes": changes,
    });
    if json {
        let mut envelope = Envelope::new("diff").result(result);
        if code != 0 {
            envelope = envelope.error(
                "comparison_unavailable",
                "comparison unavailable; see result.inputs and side-tagged diagnostics",
                code,
            );
        }
        let (mut document, _) = envelope.into_parts();
        document["diagnostics"] = json!(
            before
                .diagnostics
                .into_iter()
                .chain(after.diagnostics)
                .collect::<Vec<_>>()
        );
        println!("{}", serde_json::to_string_pretty(&document).unwrap());
    } else {
        print_human(&result, &before, &after);
    }
    ExitCode::from(code)
}

fn print_human(result: &Json, before: &Input, after: &Input) {
    println!(
        "Comparing qualified definitions, ordered signatures/data shapes, and inferred parameter behaviors/escape targets"
    );
    println!("Excludes function bodies, runtime behavior, evidence sites, and unreachable modules");
    println!("No compatibility, safety or behavioral equality guarantee");
    for (side, input) in [("before", before), ("after", after)] {
        println!("{side}: {}", input.info["path"].as_str().unwrap_or(""));
        if !input.human_diagnostics.is_empty() {
            eprintln!("{side} diagnostics:\n{}", input.human_diagnostics);
        }
        if let Some(error) = input.info["error"].as_str() {
            eprintln!("{side}: {error}");
        }
        for uncertainty in input.info["uncertainties"].as_array().into_iter().flatten() {
            println!(
                "Uncertain ({side}): {} parameter {} ({}) has unknown ownership behavior",
                uncertainty["definition"].as_str().unwrap_or(""),
                uncertainty["parameter"],
                uncertainty["name"].as_str().unwrap_or("")
            );
        }
    }
    match result["status"].as_str() {
        Some("unavailable") => println!("Comparison unavailable"),
        Some("unchanged") => println!("No changes in compared fields"),
        _ => {
            for change in result["changes"].as_array().into_iter().flatten() {
                let kind = change["kind"].as_str().unwrap_or("");
                let symbol = change["definition"].as_str().unwrap_or("");
                println!("{kind} {symbol}");
                if kind == "modified" {
                    for field in change["fields"].as_array().into_iter().flatten() {
                        println!(
                            "  {}: {} -> {}",
                            field["field"].as_str().unwrap_or(""),
                            field["before"],
                            field["after"]
                        );
                    }
                } else {
                    let record = if kind == "added" { "after" } else { "before" };
                    println!("  {}", change[record]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ontixa_db::Db;
    use ontixa_memory::ParamBehavior;
    use ontixa_source::DefId;

    fn unknown_input(side: &str) -> Input {
        let mut db = Db::new();
        let file = db.add_source_named("main", "fn f(x: i32) -> i32 { x }");
        let mut a = db.compile_owned(file);
        a.ownership.param_behaviors.get_mut(&DefId::new(0)).unwrap()[0] = ParamBehavior::Unknown;
        input(Path::new("main.ixa"), Ok((vec![], a)), side)
    }

    #[test]
    fn equal_unknown_records_retain_uncertainty_on_both_sides() {
        let before = unknown_input("before");
        let after = unknown_input("after");
        assert!(
            compare(
                before.snapshot.as_ref().unwrap(),
                after.snapshot.as_ref().unwrap()
            )
            .is_empty()
        );
        for input in [before, after] {
            assert_eq!(input.code, 0);
            assert_eq!(input.info["uncertainties"][0]["definition"], "main::f");
            assert_eq!(input.info["uncertainties"][0]["parameter"], 0);
            assert_eq!(input.info["uncertainties"][0]["behavior"], "unknown");
        }
    }
}
