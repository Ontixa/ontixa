//! `ontixa explain` — semantic introspection.
//!
//! Without a symbol: one summary per top-level definition (signature +
//! inferred ownership contracts). With a symbol: resolves the name
//! against the module's semantic symbol table — not source text — and
//! explains the single match, or emits `E_AMBIGUOUS_SYMBOL` /
//! `E_UNKNOWN_SYMBOL` as appropriate.

use crate::envelope::{Envelope, emit_human_diags, print_timings};
use ontixa_db::Artifacts;
use ontixa_diagnostics::{Code, Diagnostic};
use ontixa_hir::{DefKind, HirModule, SymbolKind};
use ontixa_source::{DefId, Interner, SourceFile, SymbolId};
use ontixa_types::Ty;
use serde_json::{Value as Json, json};
use std::process::ExitCode;

/// Runs `explain` on a compiled file.
pub fn run(
    file: &std::path::Path,
    sf: SourceFile,
    mut a: Artifacts,
    symbol: Option<&str>,
    json: bool,
    timings: bool,
) -> ExitCode {
    a.diags.sort();
    let mut extra: Option<Diagnostic> = None;

    let result = match symbol {
        None => json!({
            "file": file.display().to_string(),
            "defs": def_summaries(&a),
        }),
        Some(name) => match resolve_symbol(&a, name) {
            Resolved::Def(def) => json!({"symbol": def_json(&a, def)}),
            Resolved::Sym(owner, sym) => json!({"symbol": sym_json(&a, owner, sym)}),
            Resolved::Ambiguous(cands) => {
                extra = Some(ambiguous(&a, name, &cands));
                Json::Null
            }
            Resolved::Unknown => {
                extra = Some(
                    Diagnostic::error(Code::UnknownSymbol, format!("no symbol named `{name}`"))
                        .subject(name.to_string()),
                );
                Json::Null
            }
        },
    };

    if json {
        let mut e = Envelope::new("explain")
            .diagnostics(&a.diags, &sf)
            .result(result);
        if let Some(d) = &extra {
            e = e.extra_diagnostic(d, &sf);
        }
        if timings {
            e = e.timings(&a.timings);
        }
        e.emit()
    } else {
        if let Some(d) = &extra {
            eprint!("{}", ontixa_diagnostics::render(d, &sf));
            return ExitCode::from(1);
        }
        let code = emit_human_diags(&a, &sf);
        match symbol {
            None => {
                println!("{}", file.display());
                print_defs(&def_summaries(&a));
            }
            Some(_) => print_symbol(&result["symbol"]),
        }
        if timings {
            print_timings(&a);
        }
        code
    }
}

// ---------- symbol resolution ----------

enum Resolved {
    Def(DefId),
    /// Owning def + symbol id. The id may be body-local (param/local)
    /// or module-level (field); `sym_of` resolves it in context.
    Sym(DefId, SymbolId),
    Ambiguous(Vec<(DefId, SymbolId)>),
    Unknown,
}

/// Fetches the `Symbol` for `(owner, sym)` — through the owner's body
/// for local ids, or the module table for fields.
fn sym_of(m: &HirModule, owner: DefId, sym: SymbolId) -> &ontixa_hir::Symbol {
    match m.body(owner) {
        Some(b) => b.symbol(&m.scope, sym),
        None => m.scope.symbols.get(sym),
    }
}

/// Resolves a query name to a semantic symbol. Top-level def names
/// take precedence (they're unique); otherwise every symbol whose
/// name matches is a candidate — params, locals, fields.
fn resolve_symbol(a: &Artifacts, name: &str) -> Resolved {
    if let Some(id) = a.interner.get(name) {
        if let Some(def) = a
            .module
            .scope
            .fns
            .get(&id)
            .or(a.module.scope.datas.get(&id))
        {
            return Resolved::Def(*def);
        }
    }
    let mut cands: Vec<(DefId, SymbolId)> = Vec::new();
    // Module-level symbols: `data` fields (defs already returned).
    for s in a.module.scope.symbols.iter() {
        if s.kind == SymbolKind::Field && a.interner.resolve(s.name) == name {
            if let Some(owner) = s.owner {
                cands.push((owner, s.id));
            }
        }
    }
    // Body-local symbols: params and `let` bindings live in their
    // own body's arena.
    for body in a.module.bodies.iter().flatten() {
        for s in &body.local_symbols {
            if a.interner.resolve(s.name) == name {
                cands.push((body.def, s.id));
            }
        }
    }
    match cands.len() {
        0 => Resolved::Unknown,
        1 => Resolved::Sym(cands[0].0, cands[0].1),
        _ => Resolved::Ambiguous(cands),
    }
}

/// Builds the ambiguity diagnostic: one label per candidate so both
/// humans and machines see every match.
fn ambiguous(a: &Artifacts, name: &str, cands: &[(DefId, SymbolId)]) -> Diagnostic {
    let mut d = Diagnostic::error(
        Code::AmbiguousSymbol,
        format!("`{name}` is ambiguous: {} candidates", cands.len()),
    )
    .subject(name.to_string());
    let mut cand_json = Vec::new();
    for &(owner, sym) in cands {
        let s = sym_of(&a.module, owner, sym);
        let owner_name = def_name(&a.module, &a.interner, owner);
        let desc = describe_symbol(&a.module, &a.interner, s);
        d = d.label(s.span, desc.clone());
        cand_json.push(json!({
            "kind": kind_str(s.kind),
            "owner": owner_name,
            "span": {"start": s.span.start, "end": s.span.end},
            "description": desc,
        }));
    }
    d.detail("candidates", Json::Array(cand_json))
}

fn describe_symbol(m: &HirModule, interner: &Interner, s: &ontixa_hir::Symbol) -> String {
    let name = interner.resolve(s.name);
    match s.kind {
        SymbolKind::Param | SymbolKind::Local => match s.owner {
            Some(o) => format!(
                "{} `{name}` of `{}`",
                kind_str(s.kind),
                def_name(m, interner, o)
            ),
            None => format!("{} `{name}`", kind_str(s.kind)),
        },
        SymbolKind::Field => match s.owner {
            Some(o) => format!("field `{name}` of `{}`", def_name(m, interner, o)),
            None => format!("field `{name}`"),
        },
        SymbolKind::Function => format!("fn `{name}`"),
        SymbolKind::Data => format!("data `{name}`"),
    }
}

fn kind_str(k: SymbolKind) -> &'static str {
    match k {
        SymbolKind::Function => "fn",
        SymbolKind::Data => "data",
        SymbolKind::Field => "field",
        SymbolKind::Param => "param",
        SymbolKind::Local => "local",
    }
}

// ---------- JSON summaries ----------

fn def_name(m: &HirModule, interner: &Interner, def: DefId) -> String {
    interner
        .resolve(m.scope.symbols.get(m.scope.def(def).name).name)
        .to_string()
}

fn ty_name(m: &HirModule, interner: &Interner, ty: Ty) -> String {
    match ty {
        Ty::Struct(d) => def_name(m, interner, d),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// A `fn` def as `{name, params:[{name,type,mutable,behavior}], returns}`.
fn def_json(a: &Artifacts, def: DefId) -> Json {
    let m = &a.module;
    let d = m.scope.def(def);
    let name = def_name(m, &a.interner, def);
    match &d.kind {
        DefKind::Function(sig) => {
            let contract = a.ownership.contract(def);
            let params: Vec<Json> = sig
                .params
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    // `p.name`/`p.mutable` are on the ParamDef itself —
                    // `p.symbol` is a body-local id only meaningful
                    // inside the fn's own arena.
                    let mut param = json!({
                        "name": a.interner.resolve(p.name),
                        "type": ty_name(m, &a.interner, Ty::from_ref(p.ty)),
                        "mutable": p.mutable,
                        "behavior": contract.get(i).map(|b| b.as_str()).unwrap_or("unknown"),
                    });
                    if let Some(exits) = a.ownership.escapes.get(&def).and_then(|e| e.get(i)) {
                        if !exits.is_empty() {
                            param["escapes"] = json!(
                                exits
                                    .iter()
                                    .map(|e| e.as_str(&m.scope, &a.interner))
                                    .collect::<Vec<_>>()
                            );
                        }
                    }
                    param
                })
                .collect();
            json!({
                "kind": "fn",
                "name": name,
                "params": params,
                "returns": ty_name(m, &a.interner, Ty::from_ref(sig.ret)),
            })
        }
        DefKind::Data(shape) => {
            let fields: Vec<Json> = shape
                .fields
                .iter()
                .map(|f| {
                    json!({
                        "name": a.interner.resolve(m.scope.symbols.get(f.symbol).name),
                        "type": ty_name(m, &a.interner, Ty::from_ref(f.ty)),
                    })
                })
                .collect();
            json!({"kind": "data", "name": name, "fields": fields})
        }
    }
}

/// A non-def symbol (param / local / field) as an explanation record.
/// `owner` is the def the symbol belongs to — required because
/// body-local ids are only meaningful within their owning body.
fn sym_json(a: &Artifacts, owner: DefId, sym: SymbolId) -> Json {
    let m = &a.module;
    let s = sym_of(m, owner, sym);
    let name = a.interner.resolve(s.name);
    let ty = a
        .types
        .get(owner.index())
        .and_then(|t| t.as_ref())
        .and_then(|t| t.local_types.get(&sym))
        .copied();
    let mut obj = json!({
        "kind": kind_str(s.kind),
        "name": name,
        "mutable": s.mutable,
        "owner": def_name(m, &a.interner, owner),
        "span": {"start": s.span.start, "end": s.span.end},
    });
    if let Some(t) = ty {
        obj["type"] = json!(ty_name(m, &a.interner, t));
    }
    if let Some(sig) = m.scope.fn_sig(owner) {
        if let Some(i) = sig.params.iter().position(|p| p.symbol == sym) {
            obj["behavior"] = json!(a.ownership.contract(owner)[i].as_str());
            if let Some(exits) = a.ownership.escapes.get(&owner).and_then(|e| e.get(i)) {
                if !exits.is_empty() {
                    obj["escapes"] = json!(
                        exits
                            .iter()
                            .map(|e| e.as_str(&m.scope, &a.interner))
                            .collect::<Vec<_>>()
                    );
                }
            }
            if ty.is_none() {
                obj["type"] = json!(ty_name(m, &a.interner, Ty::from_ref(sig.params[i].ty)));
            }
        }
    }
    if let Some(shape) = m.scope.data_shape(owner) {
        if let Some(f) = shape.fields.iter().find(|f| f.symbol == sym) {
            if ty.is_none() {
                obj["type"] = json!(ty_name(m, &a.interner, Ty::from_ref(f.ty)));
            }
        }
    }
    obj
}

fn def_summaries(a: &Artifacts) -> Vec<Json> {
    a.module
        .scope
        .defs
        .iter()
        .map(|d| def_json(a, d.id))
        .collect()
}

// ---------- human output ----------

fn print_defs(defs: &[Json]) {
    for d in defs {
        if d["kind"] == "fn" {
            let params: Vec<String> = d["params"]
                .as_array()
                .map(|ps| {
                    ps.iter()
                        .map(|p| {
                            let mut_s = if p["mutable"].as_bool().unwrap_or(false) {
                                "mut "
                            } else {
                                ""
                            };
                            format!(
                                "{mut_s}{}: {} [{}]",
                                p["name"].as_str().unwrap_or("?"),
                                p["type"].as_str().unwrap_or("?"),
                                p["behavior"].as_str().unwrap_or("?"),
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            println!(
                "  fn {}({}) -> {}",
                d["name"].as_str().unwrap_or("?"),
                params.join(", "),
                d["returns"].as_str().unwrap_or("unit"),
            );
        } else {
            let fields: Vec<String> = d["fields"]
                .as_array()
                .map(|fs| {
                    fs.iter()
                        .map(|f| {
                            format!(
                                "{}: {}",
                                f["name"].as_str().unwrap_or("?"),
                                f["type"].as_str().unwrap_or("?")
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            println!(
                "  data {} {{ {} }}",
                d["name"].as_str().unwrap_or("?"),
                fields.join(", ")
            );
        }
    }
}

fn print_symbol(s: &Json) {
    if s.is_null() {
        return;
    }
    // Defs share the def_summaries shape — reuse the same renderer.
    if matches!(s["kind"].as_str(), Some("fn" | "data")) {
        print_defs(std::slice::from_ref(s));
        return;
    }
    println!("symbol: {}", s["name"].as_str().unwrap_or("?"));
    println!("  kind: {}", s["kind"].as_str().unwrap_or("?"));
    if let Some(o) = s["owner"].as_str() {
        println!("  owner: {o}");
    }
    if let Some(t) = s["type"].as_str() {
        println!("  type: {t}");
    }
    if let Some(m) = s["mutable"].as_bool() {
        println!("  binding: {}", if m { "mutable" } else { "immutable" });
    }
    if let Some(b) = s["behavior"].as_str() {
        println!("  behavior: {b}");
    }
    if let Some(es) = s["escapes"].as_array() {
        let list: Vec<&str> = es.iter().filter_map(|e| e.as_str()).collect();
        if !list.is_empty() {
            println!("  escapes: {}", list.join(", "));
        }
    }
}
