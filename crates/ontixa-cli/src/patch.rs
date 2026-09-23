//! `ontixa patch` — structured semantic-patch preview and apply,
//! shared between the CLI subcommand and the `ontixad` daemon op.
//!
//! A patch is a transaction planned by [`Db::plan_patch`]: a bounded
//! list of ops resolved through the workspace scope, shadow-compiled
//! on a scratch `Db`, and applied in one revision after the stale
//! guard passes.
//!
//! The spec is JSON — `{"ops": [...]}` or a bare `[...]`:

//! ```json
//! {"ops": [
//!   {"op": "replace_body", "symbol": "m::f", "body": "{ return x + 1; }"},
//!   {"op": "remove_def",   "symbol": "m::dead"},
//!   {"op": "add_def",      "module": "m",
//!    "text": "fn g() -> i32 { return 1; }"},
//!   {"op": "rename_param", "symbol": "m::f", "param": "x", "to": "acc"},
//!   {"op": "set_param_type", "symbol": "m::f", "param": "x", "ty": "i64"},
//!   {"op": "set_ret_type", "symbol": "m::f", "ty": "i32"},
//!   {"op": "add_use",    "module": "m", "path": "dep::T", "as": "T2"},
//!   {"op": "remove_use", "module": "m", "path": "dep"},
//!   {"op": "add_field",  "symbol": "m::D", "field": "c", "ty": "i32"},
//!   {"op": "remove_field", "symbol": "m::D", "field": "dead"},
//!   {"op": "rename_field", "symbol": "m::D", "field": "a", "to": "b"},
//!   {"op": "set_field_type", "symbol": "m::D", "field": "a", "ty": "i64"}
//! ]}
//! ```
//!
//! `module` names the file to edit — a reachable module stem, or the
//! workspace root when absent. `symbol` names a top-level `fn`/`data`
//! resolved through the workspace scope (`x` or `m::x`); `param` and
//! `field` name existing members of that target, `to`/`as`/`ty` carry
//! new text. `set_ret_type`'s `ty` is required — `"ty": null` is the
//! remove-the-annotation form. `path` is `m` or `m::x` and never
//! carries `as` — the alias is the separate `as` field.
//!
//! Malformed specs surface as `E_MALFORMED_PATCH` diagnostics — the
//! same rejection channel every semantic failure uses.

use crate::envelope::Envelope;
use ontixa_db::{PatchError, PatchOp, PatchPlan, PatchReport};
use ontixa_diagnostics::{Code, Diagnostic};
use ontixa_source::SourceFile;
use serde_json::{Value as Json, json};

/// The display name of file `f` in `sfs` (indexed by `FileId`).
fn file_name(sfs: &[SourceFile], f: usize) -> String {
    sfs.get(f)
        .map(|s| s.name().to_string())
        .unwrap_or_else(|| f.to_string())
}

/// An `E_MALFORMED_PATCH` diagnostic for a spec that is not a
/// well-formed op list — the same rejection channel the engine's
/// structural checks use.
fn malformed(message: impl Into<String>) -> Box<Diagnostic> {
    Box::new(Diagnostic::error(Code::MalformedPatch, message.into()))
}

/// Parses a patch spec document into the engine's op list. Accepts
/// `{"ops": [...]}` or a bare `[...]` — both appear in the wild
/// (files vs inline daemon fields).
pub fn parse_ops(spec: &Json) -> Result<Vec<PatchOp>, Box<Diagnostic>> {
    let ops = match spec {
        Json::Array(_) => spec,
        Json::Object(_) => &spec["ops"],
        _ => &Json::Null,
    };
    let Json::Array(ops) = ops else {
        return Err(malformed(
            "a patch spec must be `{\"ops\": [...]}` or a bare op array",
        ));
    };
    ops.iter()
        .enumerate()
        .map(|(i, op)| op_from_json(i, op))
        .collect()
}

/// One op object → [`PatchOp`]. Unknown kinds, missing required
/// fields, and wrong-typed fields are all `E_MALFORMED_PATCH` —
/// the spec is data an agent writes, so every rejection names the
/// exact field and op index.
fn op_from_json(i: usize, op: &Json) -> Result<PatchOp, Box<Diagnostic>> {
    let err = |msg: String| Err(malformed(format!("ops[{i}]: {msg}")));
    let Json::Object(_) = op else {
        return err("an op must be an object like `{\"op\": \"remove_def\", ...}`".into());
    };
    let Some(kind) = op["op"].as_str() else {
        return err(
            "missing `op` kind (expected \"replace_body\" | \"remove_def\" \
                    | \"add_def\" | \"rename_param\" | \"set_param_type\" \
                    | \"set_ret_type\" | \"add_use\" | \"remove_use\" \
                    | \"add_field\" | \"remove_field\" | \"rename_field\" \
                    | \"set_field_type\")"
                .into(),
        );
    };
    let need_str = |field: &str| -> Result<String, Box<Diagnostic>> {
        op[field]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| malformed(format!("ops[{i}] {kind}: missing string field `{field}`")))
    };
    // Optional string field: absent or `null` → `None`; any other
    // non-string type rejects.
    let opt_str = |field: &str| -> Result<Option<String>, Box<Diagnostic>> {
        match op.get(field) {
            None | Some(Json::Null) => Ok(None),
            Some(Json::String(s)) => Ok(Some(s.clone())),
            _ => Err(malformed(format!(
                "ops[{i}] {kind}: `{field}` must be a string or absent"
            ))),
        }
    };
    // Required-but-nullable field: present as a string or as an
    // explicit `null` (the "remove" form); absent rejects — the
    // remove form must be written deliberately.
    let req_opt_str = |field: &str| -> Result<Option<String>, Box<Diagnostic>> {
        match op.get(field) {
            None => Err(malformed(format!(
                "ops[{i}] {kind}: missing field `{field}` (a type string, or `null` to remove)"
            ))),
            Some(Json::Null) => Ok(None),
            Some(Json::String(s)) => Ok(Some(s.clone())),
            _ => Err(malformed(format!(
                "ops[{i}] {kind}: `{field}` must be a type string or `null`"
            ))),
        }
    };
    match kind {
        "replace_body" => Ok(PatchOp::ReplaceBody {
            symbol: need_str("symbol")?,
            body: need_str("body")?,
        }),
        "remove_def" => Ok(PatchOp::RemoveDef {
            symbol: need_str("symbol")?,
        }),
        "add_def" => Ok(PatchOp::AddDef {
            module: opt_str("module")?,
            text: need_str("text")?,
        }),
        "rename_param" => Ok(PatchOp::RenameParam {
            symbol: need_str("symbol")?,
            param: need_str("param")?,
            to: need_str("to")?,
        }),
        "set_param_type" => Ok(PatchOp::SetParamType {
            symbol: need_str("symbol")?,
            param: need_str("param")?,
            ty: need_str("ty")?,
        }),
        "set_ret_type" => Ok(PatchOp::SetRetType {
            symbol: need_str("symbol")?,
            ty: req_opt_str("ty")?,
        }),
        "add_use" => Ok(PatchOp::AddUse {
            module: opt_str("module")?,
            path: need_str("path")?,
            alias: opt_str("as")?,
        }),
        "remove_use" => Ok(PatchOp::RemoveUse {
            module: opt_str("module")?,
            path: need_str("path")?,
            alias: opt_str("as")?,
        }),
        "add_field" => Ok(PatchOp::AddField {
            symbol: need_str("symbol")?,
            field: need_str("field")?,
            ty: need_str("ty")?,
        }),
        "remove_field" => Ok(PatchOp::RemoveField {
            symbol: need_str("symbol")?,
            field: need_str("field")?,
        }),
        "rename_field" => Ok(PatchOp::RenameField {
            symbol: need_str("symbol")?,
            field: need_str("field")?,
            to: need_str("to")?,
        }),
        "set_field_type" => Ok(PatchOp::SetFieldType {
            symbol: need_str("symbol")?,
            field: need_str("field")?,
            ty: need_str("ty")?,
        }),
        other => err(format!("unknown op `{other}`")),
    }
}

/// The plan as a JSON payload: op summaries plus every edit with
/// its file, span, and producing op index.
pub fn plan_json(plan: &PatchPlan, sfs: &[SourceFile], applied: bool) -> Json {
    json!({
        "ops": plan.ops(),
        "revision": plan.revision(),
        "applied": applied,
        "edits": plan
            .edits()
            .iter()
            .map(|e| json!({
                "file": file_name(sfs, e.file.index()),
                "op": e.op,
                "span": {"start": e.span.start, "end": e.span.end},
                "replace": e.replace,
            }))
            .collect::<Vec<_>>(),
        "files": plan
            .new_sources()
            .iter()
            .map(|(f, _)| file_name(sfs, *f))
            .collect::<Vec<_>>(),
    })
}

/// The apply report merged into the result payload.
pub fn applied_json(plan: &PatchPlan, report: &PatchReport, sfs: &[SourceFile]) -> Json {
    let mut j = plan_json(plan, sfs, true);
    j["applied_files"] = json!(
        report
            .files
            .iter()
            .map(|f| file_name(sfs, *f))
            .collect::<Vec<_>>()
    );
    j["applied_revision"] = json!(report.revision);
    j["ops_applied"] = json!(report.ops);
    j
}

/// The rejection as a ready envelope document (for the daemon,
/// which serializes envelopes itself).
pub fn rejection_json(e: &PatchError, sfs: &[SourceFile]) -> Json {
    let mut env = Envelope::new("patch");
    for d in e.diagnostics() {
        env = env.extra_diagnostic(&d, sfs);
    }
    env.into_parts().0
}

/// Persists every `new_sources` entry through the staged
/// transaction engine in [`crate::persist`] — identical protocol to
/// a rename apply: disk stale guard, sibling staging, journaled
/// swap, rollback on failure.
pub fn persist(plan: &PatchPlan, sfs: &[SourceFile]) -> Result<(), String> {
    crate::persist::persist_sources(plan.new_sources(), sfs).map_err(|e| e.to_string())
}

/// Human preview of a plan: one line per op, then one per edit,
/// stable order.
pub fn print_preview(plan: &PatchPlan, sfs: &[SourceFile]) {
    println!(
        "patch: {} op(s), {} edit(s) in {} file(s)",
        plan.ops().len(),
        plan.edits().len(),
        plan.new_sources().len(),
    );
    for (i, op) in plan.ops().iter().enumerate() {
        println!("  ops[{i}] {op}");
    }
    for e in plan.edits() {
        println!(
            "  {}:{}..{}  → {:?}",
            file_name(sfs, e.file.index()),
            e.span.start,
            e.span.end,
            e.replace,
        );
    }
}
