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

/// Runs `explain` on a compiled workspace. `sfs` holds the
/// workspace's source files — `files[0]` is the root.
pub fn run(
    file: &std::path::Path,
    sfs: Vec<SourceFile>,
    mut a: Artifacts,
    symbol: Option<&str>,
    json: bool,
    timings: bool,
) -> ExitCode {
    a.diags.sort();
    let (result, extra) = explain_result(&a, symbol, &file.display().to_string());

    if json {
        let mut e = Envelope::new("explain")
            .diagnostics(&a.diags, &sfs)
            .result(result);
        if let Some(d) = &extra {
            e = e.extra_diagnostic(d, &sfs);
        }
        if timings {
            e = e.timings(&a.timings);
        }
        e.emit()
    } else {
        if let Some(d) = &extra {
            eprint!("{}", ontixa_diagnostics::render_in(d, &sfs));
            return ExitCode::from(1);
        }
        let code = emit_human_diags(&a.diags, &sfs);
        match symbol {
            None => {
                println!("{}", file.display());
                print_defs(&def_summaries(&a));
            }
            Some(_) => print_symbol(&result["symbol"]),
        }
        if timings {
            print_timings(&a.timings);
        }
        code
    }
}

/// The pure query: symbol name → `(result, extra_diagnostic)`.
/// Daemon/CLI agnostic — `run` wraps it in output modes.
pub fn explain_result(
    a: &Artifacts,
    symbol: Option<&str>,
    file_name: &str,
) -> (Json, Option<Diagnostic>) {
    match symbol {
        None => (
            json!({
                "file": file_name,
                "defs": def_summaries(a),
            }),
            None,
        ),
        Some(name) => match resolve_symbol(a, name) {
            Resolved::Def(def) => (json!({"symbol": def_json(a, def)}), None),
            Resolved::Sym(owner, sym) => (json!({"symbol": sym_json(a, owner, sym)}), None),
            Resolved::Ambiguous(cands) => (Json::Null, Some(ambiguous(a, name, &cands))),
            Resolved::Unknown => (
                Json::Null,
                Some(
                    Diagnostic::error(Code::UnknownSymbol, format!("no symbol named `{name}`"))
                        .subject(name.to_string()),
                ),
            ),
        },
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

/// Resolves a query name to a semantic symbol. `m::x` selects a def
/// in module `m`'s file directly; a bare name matches top-level defs
/// in every reachable file (ambiguous when several modules define
/// it); otherwise every symbol whose name matches is a candidate —
/// params, locals, fields.
fn resolve_symbol(a: &Artifacts, name: &str) -> Resolved {
    if let Some((m, member)) = name.split_once("::") {
        let scope = &a.module.scope;
        let def = a
            .interner
            .get(m)
            .and_then(|mi| {
                scope
                    .files
                    .iter()
                    .find(|f| scope.file_name(**f) == Some(mi))
            })
            .and_then(|f| scope.env(*f))
            .and_then(|e| {
                a.interner
                    .get(member)
                    .and_then(|id| e.fns.get(&id).or_else(|| e.datas.get(&id)))
            })
            .copied();
        return match def {
            Some(d) => Resolved::Def(d),
            None => Resolved::Unknown,
        };
    }
    if let Some(id) = a.interner.get(name) {
        // Def names are unique per file, not per workspace — a bare
        // name may legitimately resolve in several modules.
        let mut defs: Vec<DefId> = a
            .module
            .scope
            .files
            .iter()
            .filter_map(|f| a.module.scope.env(*f))
            .flat_map(|e| e.fns.get(&id).into_iter().chain(e.datas.get(&id)))
            .copied()
            .collect();
        defs.sort();
        defs.dedup();
        match defs.len() {
            1 => return Resolved::Def(defs[0]),
            0 => {}
            _ => {
                let cands = defs
                    .iter()
                    .map(|d| (*d, a.module.scope.def(*d).name))
                    .collect();
                return Resolved::Ambiguous(cands);
            }
        }
    }
    let mut cands: Vec<(DefId, SymbolId)> = Vec::new();
    // Module-level symbols: `data` fields and enum variants (defs
    // already returned).
    for s in a.module.scope.symbols.iter() {
        if matches!(s.kind, SymbolKind::Field | SymbolKind::Variant)
            && a.interner.resolve(s.name) == name
        {
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

/// The absolute start offset of `def`'s source item — the base for
/// converting its item-relative symbol/evidence spans to
/// file-absolute for output. The def's own file supplies the AST.
fn item_base(a: &Artifacts, def: DefId) -> u32 {
    let d = a.module.scope.def(def);
    a.asts[&d.file].items[d.item as usize].span().start
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
        let abs = s.span.abs(item_base(a, owner));
        d = d.label(abs, desc.clone());
        cand_json.push(json!({
            "kind": kind_str(s.kind),
            "owner": owner_name,
            "span": {"start": abs.start, "end": abs.end},
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
        SymbolKind::Variant => match s.owner {
            Some(o) => format!("variant `{name}` of `{}`", def_name(m, interner, o)),
            None => format!("variant `{name}`"),
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
        SymbolKind::Variant => "variant",
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
        Ty::Array(e) => format!("[{}]", ty_name(m, interner, e.ty())),
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
                    if let Some(sum) = a.ownership.summary(def).get(i) {
                        if !sum.escapes.is_empty() {
                            param["escapes"] = json!(
                                sum.escapes
                                    .iter()
                                    .map(|e| e.as_str(&m.scope, &a.interner))
                                    .collect::<Vec<_>>()
                            );
                        }
                        if !sum.evidence.is_empty() {
                            param["evidence"] = evidence_json(sum, item_base(a, def));
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
            let variants: Vec<Json> = shape
                .variants
                .iter()
                .map(|v| {
                    json!({
                        "name": a.interner.resolve(m.scope.symbols.get(v.symbol).name),
                        "discriminant": v.index,
                        "payload": v
                            .payload
                            .iter()
                            .map(|t| ty_name(m, &a.interner, Ty::from_ref(*t)))
                            .collect::<Vec<_>>(),
                    })
                })
                .collect();
            json!({
                "kind": "data",
                "name": name,
                "shape": shape.shape_name(),
                "fields": fields,
                "variants": variants,
            })
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
    let base = item_base(a, owner);
    let span = s.span.abs(base);
    let mut obj = json!({
        "kind": kind_str(s.kind),
        "name": name,
        "mutable": s.mutable,
        "owner": def_name(m, &a.interner, owner),
        "span": {"start": span.start, "end": span.end},
    });
    if let Some(t) = ty {
        obj["type"] = json!(ty_name(m, &a.interner, t));
    }
    if let Some(sig) = m.scope.fn_sig(owner) {
        if let Some(i) = sig.params.iter().position(|p| p.symbol == sym) {
            obj["behavior"] = json!(a.ownership.contract(owner)[i].as_str());
            if let Some(sum) = a.ownership.summary(owner).get(i) {
                if !sum.escapes.is_empty() {
                    obj["escapes"] = json!(
                        sum.escapes
                            .iter()
                            .map(|e| e.as_str(&m.scope, &a.interner))
                            .collect::<Vec<_>>()
                    );
                }
                if !sum.evidence.is_empty() {
                    obj["evidence"] = evidence_json(sum, base);
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
        if let Some(v) = shape.variants.iter().find(|v| v.symbol == sym) {
            // A variant's type is its enum; payload types are the
            // constructor's argument list.
            obj["type"] = json!(ty_name(m, &a.interner, Ty::Struct(owner)));
            obj["discriminant"] = json!(v.index);
            obj["payload"] = json!(
                v.payload
                    .iter()
                    .map(|t| ty_name(m, &a.interner, Ty::from_ref(*t)))
                    .collect::<Vec<_>>()
            );
        }
    }
    obj
}

fn evidence_json(s: &ontixa_memory::ParamSummary, base: u32) -> Json {
    json!(
        s.evidence
            .iter()
            .map(|e| {
                let at = e.at.abs(base);
                json!({
                    "kind": e.kind.as_str(),
                    "span": {"start": at.start, "end": at.end},
                })
            })
            .collect::<Vec<_>>()
    )
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
            let variants: Vec<String> = d["variants"]
                .as_array()
                .map(|vs| {
                    vs.iter()
                        .map(|v| {
                            let payload: Vec<&str> = v["payload"]
                                .as_array()
                                .map(|ps| ps.iter().filter_map(|t| t.as_str()).collect())
                                .unwrap_or_default();
                            if payload.is_empty() {
                                v["name"].as_str().unwrap_or("?").to_string()
                            } else {
                                format!(
                                    "{}({})",
                                    v["name"].as_str().unwrap_or("?"),
                                    payload.join(", ")
                                )
                            }
                        })
                        .collect()
                })
                .unwrap_or_default();
            if !variants.is_empty() {
                println!(
                    "  data {} {{ {} }}",
                    d["name"].as_str().unwrap_or("?"),
                    variants.join(", ")
                );
                continue;
            }
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
    if let Some(d) = s["discriminant"].as_u64() {
        println!("  discriminant: {d}");
    }
    if let Some(p) = s["payload"].as_array() {
        let ts: Vec<&str> = p.iter().filter_map(|t| t.as_str()).collect();
        println!("  payload: ({})", ts.join(", "));
    }
    if let Some(b) = s["behavior"].as_str() {
        println!("  behavior: {b}");
    }
    if let Some(evs) = s["evidence"].as_array() {
        for e in evs {
            let k = e["kind"].as_str().unwrap_or("?");
            let sp = &e["span"];
            println!(
                "    {k} at {}..{}",
                sp["start"].as_u64().unwrap_or(0),
                sp["end"].as_u64().unwrap_or(0)
            );
        }
    }
    if let Some(es) = s["escapes"].as_array() {
        let list: Vec<&str> = es.iter().filter_map(|e| e.as_str()).collect();
        if !list.is_empty() {
            println!("  escapes: {}", list.join(", "));
        }
    }
}
