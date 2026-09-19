//! Query evaluation — what each [`QueryKey`] runs when its memoized
//! value is missing or stale.
//!
//! Evaluators never read inputs directly; they call `db.demand(dep)`
//! for everything they consume, which records the dependency edge the
//! freshness verifier later walks. Determinism: defs are processed in
//! `DefId` (declaration) order and diagnostics are collected in
//! demand order.

use std::sync::Arc;

use ontixa_ast::{AstModule, Item};
use ontixa_diagnostics::{Diagnostic, Diagnostics};
use ontixa_hir::{Def, HirBody, HirModule, ModuleScope};
use ontixa_memory::{FactStamps, OwnershipTables};
use ontixa_mir::{MirBody, MirModule};
use ontixa_semantic::SemanticGraph;
use ontixa_source::{DefKey, FileId, InternId};
use ontixa_types::ModuleTypes;
use rustc_hash::FxHashMap;

use crate::db::{Artifacts, Db, StageTiming};
use crate::query::{CheckedBody, QueryKey, Value};

/// Evaluates `key`, returning its value plus the diagnostics emitted
/// during the evaluation itself (dependencies' diagnostics live on
/// their own entries).
pub(crate) fn eval(db: &mut Db, key: QueryKey) -> (Value, Vec<Diagnostic>) {
    let mut diags = Diagnostics::new();
    let value = match key {
        // `Source` values are seeded by `add_source`/`set_source` and
        // never go stale; this arm exists only to keep eval total.
        QueryKey::Source(f) => Value::Text(db.file_text(f)),
        QueryKey::Parse(f) => {
            let text = text(db, f);
            let (root, d) = ontixa_syntax::parse_file(&text);
            diags.extend(d);
            Value::Tree(root)
        }
        QueryKey::Ast(f) => {
            let tree = db.demand(QueryKey::Parse(f));
            let Value::Tree(root) = tree else {
                unreachable!("Parse produced wrong value")
            };
            Value::Ast(ontixa_ast::lower_module(&root, &mut diags))
        }
        QueryKey::Scope(f) => {
            let module_ast = ast(db, f);
            let scope = ontixa_hir::resolve_module(&module_ast, &mut db.interner, &mut diags);
            Value::Scope(Arc::new(scope))
        }
        QueryKey::AstItem(k) => {
            let module_ast = ast(db, k.file);
            let item = module_ast.items.iter().find_map(|i| {
                let name = match i {
                    Item::Fn(f) => &f.name.name,
                    Item::Data(d) => &d.name.name,
                };
                (db.interner.intern(name) == k.name).then(|| i.clone())
            });
            Value::Item(item)
        }
        QueryKey::HirBody(k) => Value::Hir(hir_body(db, k, &mut diags)),
        QueryKey::BodyTypes(k) => Value::Checked(body_types(db, k, &mut diags)),
        QueryKey::Ownership(f) => Value::Ownership(ownership(db, f, &mut diags)),
        QueryKey::MirBody(k) => Value::Mir(mir_body(db, k)),
        QueryKey::Graph(f) => Value::Graph(graph(db, f)),
        QueryKey::Diagnostics(f) => Value::Diags(diagnostics(db, f)),
        QueryKey::Compile(f) => Value::Compile(compile(db, f)),
    };
    (value, diags.into_vec())
}

// ---- typed demands --------------------------------------------------
// Every evaluator unwraps through these so the variant matching lives
// in exactly one place.

fn text(db: &mut Db, f: FileId) -> Arc<str> {
    match db.demand(QueryKey::Source(f)) {
        Value::Text(t) => t,
        _ => unreachable!("Source produced wrong value"),
    }
}

fn ast(db: &mut Db, f: FileId) -> AstModule {
    match db.demand(QueryKey::Ast(f)) {
        Value::Ast(a) => a,
        _ => unreachable!("Ast produced wrong value"),
    }
}

fn scope(db: &mut Db, f: FileId) -> Arc<ModuleScope> {
    match db.demand(QueryKey::Scope(f)) {
        Value::Scope(s) => s,
        _ => unreachable!("Scope produced wrong value"),
    }
}

fn hir(db: &mut Db, k: DefKey) -> Option<HirBody> {
    match db.demand(QueryKey::HirBody(k)) {
        Value::Hir(b) => b,
        _ => unreachable!("HirBody produced wrong value"),
    }
}

fn checked(db: &mut Db, k: DefKey) -> Option<CheckedBody> {
    match db.demand(QueryKey::BodyTypes(k)) {
        Value::Checked(c) => c,
        _ => unreachable!("BodyTypes produced wrong value"),
    }
}

fn ownership_t(db: &mut Db, f: FileId) -> Arc<OwnershipTables> {
    match db.demand(QueryKey::Ownership(f)) {
        Value::Ownership(o) => o,
        _ => unreachable!("Ownership produced wrong value"),
    }
}

fn mir(db: &mut Db, k: DefKey) -> Option<MirBody> {
    match db.demand(QueryKey::MirBody(k)) {
        Value::Mir(m) => m,
        _ => unreachable!("MirBody produced wrong value"),
    }
}

fn graph_v(db: &mut Db, f: FileId) -> SemanticGraph {
    match db.demand(QueryKey::Graph(f)) {
        Value::Graph(g) => g,
        _ => unreachable!("Graph produced wrong value"),
    }
}

fn diags_v(db: &mut Db, f: FileId) -> Vec<Diagnostic> {
    match db.demand(QueryKey::Diagnostics(f)) {
        Value::Diags(d) => d,
        _ => unreachable!("Diagnostics produced wrong value"),
    }
}

// ---- evaluators -----------------------------------------------------

/// The interned name of a def (its `DefKey` component).
fn def_name(scope: &ModuleScope, def: &Def) -> InternId {
    scope.symbols.get(def.name).name
}

/// `lower_body` for the fn named by `key`. `None` when the key names
/// a `data` def or a name that no longer resolves (stale caller —
/// callers always discover keys through `scope`). Depends on the
/// item's own AST slice, so an edit elsewhere in the file never
/// re-lowers this body.
fn hir_body(db: &mut Db, key: DefKey, diags: &mut Diagnostics) -> Option<HirBody> {
    let scope = scope(db, key.file);
    let item = match db.demand(QueryKey::AstItem(key)) {
        Value::Item(i) => i,
        _ => unreachable!("AstItem produced wrong value"),
    }?;
    let &def = scope.fns.get(&key.name)?;
    let Item::Fn(decl) = item else { return None };
    Some(ontixa_hir::lower_body(
        &decl,
        def,
        &scope,
        &mut db.interner,
        diags,
    ))
}

/// `check_body` for one function — the per-definition unit of
/// type-inference reuse.
fn body_types(db: &mut Db, key: DefKey, diags: &mut Diagnostics) -> Option<CheckedBody> {
    let scope = scope(db, key.file);
    let mut body = hir(db, key)?;
    let tables = ontixa_types::check_body(&scope, &mut body, &db.interner, diags);
    Some(CheckedBody { body, tables })
}

/// Assembles the `HirModule` + `ModuleTypes` views module-wide passes
/// (ownership, graph) still consume — from per-definition query
/// results, not a monolithic re-lower.
fn assemble(db: &mut Db, f: FileId, scope: &ModuleScope) -> (HirModule, ModuleTypes) {
    let mut bodies = Vec::with_capacity(scope.defs.len());
    let mut types = Vec::with_capacity(scope.defs.len());
    for def in &scope.defs {
        if scope.fn_sig(def.id).is_none() {
            bodies.push(None);
            types.push(None);
            continue;
        }
        match checked(db, DefKey::new(f, def_name(scope, def))) {
            Some(c) => {
                bodies.push(Some(c.body));
                types.push(Some(c.tables));
            }
            None => {
                bodies.push(None);
                types.push(None);
            }
        }
    }
    (
        HirModule {
            scope: scope.clone(),
            bodies,
        },
        types,
    )
}

/// The ownership fixpoint. Per-body fact memoization lives in
/// `db.oracle`; here we build the stamps each body was last checked
/// at, so the oracle can decide which bodies need re-walking.
fn ownership(db: &mut Db, f: FileId, diags: &mut Diagnostics) -> Arc<OwnershipTables> {
    let scope = scope(db, f);
    let mut stamps = Vec::with_capacity(scope.defs.len());
    let mut bodies = Vec::with_capacity(scope.defs.len());
    let mut types: ModuleTypes = Vec::with_capacity(scope.defs.len());
    for def in &scope.defs {
        if scope.fn_sig(def.id).is_none() {
            stamps.push(FactStamps { body: 0, types: 0 });
            bodies.push(None);
            types.push(None);
            continue;
        }
        let key = DefKey::new(f, def_name(&scope, def));
        // Demand the raw body first: its stamp is what the oracle
        // compares against to reuse collected facts.
        let _ = hir(db, key);
        let body_stamp = db.stamp(QueryKey::HirBody(key));
        let c = checked(db, key);
        let types_stamp = db.stamp(QueryKey::BodyTypes(key));
        stamps.push(FactStamps {
            body: body_stamp,
            types: types_stamp,
        });
        bodies.push(c.as_ref().map(|c| c.body.clone()));
        types.push(c.map(|c| c.tables));
    }
    let module = HirModule {
        scope: (*scope).clone(),
        bodies,
    };
    Arc::new(ontixa_memory::infer_ownership(
        &module,
        &types,
        &db.interner,
        diags,
        Some(&stamps),
        &mut db.oracle,
    ))
}

/// `lower_fn` for one function.
fn mir_body(db: &mut Db, key: DefKey) -> Option<MirBody> {
    let scope = scope(db, key.file);
    let c = checked(db, key)?;
    let own = ownership_t(db, key.file);
    Some(ontixa_mir::lower_fn(&scope, &c.body, &c.tables, &own))
}

/// The file's semantic program graph.
fn graph(db: &mut Db, f: FileId) -> SemanticGraph {
    let scope = scope(db, f);
    let (module, types) = assemble(db, f, &scope);
    let own = ownership_t(db, f);
    ontixa_semantic::build_graph(&module, &types, &own, &db.interner)
}

/// Every diagnostic emitted anywhere in this file's pipeline,
/// collected from dependency entries in demand order.
fn diagnostics(db: &mut Db, f: FileId) -> Vec<Diagnostic> {
    let _ = db.demand(QueryKey::Parse(f));
    let _ = ast(db, f);
    let sc = scope(db, f);
    for def in &sc.defs {
        if sc.fn_sig(def.id).is_some() {
            let _ = checked(db, DefKey::new(f, def_name(&sc, def)));
        }
    }
    let _ = ownership_t(db, f);
    // Entries store transitive diagnostics, so each dep's diags may
    // repeat an earlier dep's — dedupe by value, keeping first-seen
    // (pipeline) order.
    let mut out: Vec<Diagnostic> = Vec::new();
    for dep in db.deps_so_far().to_vec() {
        for d in db.entry_diags(dep) {
            if !out.contains(d) {
                out.push(d.clone());
            }
        }
    }
    out
}

/// The assembled artifact bundle — `compile` is a *pure join* of
/// per-definition and file-level query results.
fn compile(db: &mut Db, f: FileId) -> Arc<Artifacts> {
    let module_ast = ast(db, f);
    let sc = scope(db, f);
    let (module, types) = assemble(db, f, &sc);
    let ownership = ownership_t(db, f);
    let graph = graph_v(db, f);
    let mut mirs = Vec::with_capacity(sc.defs.len());
    for def in &sc.defs {
        if sc.fn_sig(def.id).is_none() {
            mirs.push(None);
            continue;
        }
        mirs.push(mir(db, DefKey::new(f, def_name(&sc, def))));
    }
    let mut diags = Diagnostics::new();
    for d in diags_v(db, f) {
        diags.push(d);
    }
    diags.sort();
    // Aggregate per-query timings into per-stage timings (multiple
    // `hir`/`types`/`mir` query evals sum into one stage entry).
    let mut order = Vec::new();
    let mut agg: FxHashMap<&'static str, u64> = FxHashMap::default();
    for (k, nanos) in db.take_last_run() {
        if let Some(v) = agg.get_mut(k.name()) {
            *v += nanos;
        } else {
            agg.insert(k.name(), nanos);
            order.push(k.name());
        }
    }
    let timings = order
        .into_iter()
        .map(|stage| StageTiming {
            stage,
            nanos: agg[stage],
        })
        .collect();
    Arc::new(Artifacts {
        built_revision: db.revision,
        ast: module_ast,
        module,
        interner: db.interner.clone(),
        types,
        ownership: (*ownership).clone(),
        graph,
        mir: MirModule { fns: mirs },
        diags,
        timings,
    })
}
