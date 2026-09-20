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
use ontixa_hir::{HirBody, HirModule, ModuleScope, WorkspaceFile};
use ontixa_memory::{FactStamps, OwnershipTables};
use ontixa_mir::{MirBody, MirModule};
use ontixa_semantic::SemanticGraph;
use ontixa_source::{DefKey, FileId, InternId};
use ontixa_types::ModuleTypes;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::db::{Artifacts, Db};
use crate::query::{CheckedBody, QueryKey, Value};

/// Evaluates `key`, returning its value plus the diagnostics emitted
/// during the evaluation itself (dependencies' diagnostics live on
/// their own entries).
pub(crate) fn eval(db: &mut Db, key: QueryKey) -> (Value, Vec<Diagnostic>) {
    let mut diags = Diagnostics::new();
    // Every diagnostic this eval emits belongs to the key's file —
    // workspace passes re-stamp per file as they go.
    diags.set_file(Some(key.file()));
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
            Value::Ast(Arc::new(ontixa_ast::lower_module(&root, &mut diags)))
        }
        QueryKey::Scope(f) => Value::Scope(Arc::new(scope_eval(db, f, &mut diags))),
        QueryKey::AstItem(k) => {
            let module_ast = ast(db, k.file);
            let item = module_ast.items.iter().find_map(|i| {
                let name = match i {
                    Item::Fn(f) => &f.name.name,
                    Item::Data(d) => &d.name.name,
                };
                // The stored value is item-relative: an edit that
                // only shifts the item's absolute offset recomputes
                // an equal value and cuts off, so `hir_body` and
                // everything below it never re-evaluates.
                (db.interner.intern(name) == k.name)
                    .then(|| ontixa_ast::rebase_item(i, i.span().start))
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

fn ast(db: &mut Db, f: FileId) -> Arc<AstModule> {
    match db.demand(QueryKey::Ast(f)) {
        Value::Ast(a) => a,
        _ => unreachable!("Ast produced wrong value"),
    }
}

/// The workspace scope containing `f`: `Scope` entries are keyed by
/// workspace root, and `root_of` routes a dependency file to the
/// root that discovered it.
fn scope(db: &mut Db, f: FileId) -> Arc<ModuleScope> {
    let root = db.root_of(f);
    match db.demand(QueryKey::Scope(root)) {
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
    let root = db.root_of(f);
    match db.demand(QueryKey::Ownership(root)) {
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
    let root = db.root_of(f);
    match db.demand(QueryKey::Graph(root)) {
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

/// `lower_body` for the fn named by `key`. `None` when the key names
/// a `data` def or a name that no longer resolves (stale caller —
/// callers always discover keys through `scope`). Depends on the
/// item's own AST slice, so an edit elsewhere in the file never
/// re-lowers this body.
fn hir_body(db: &mut Db, key: DefKey, diags: &mut Diagnostics) -> Option<HirBody> {
    let scope = scope(db, key.root);
    let item = match db.demand(QueryKey::AstItem(key)) {
        Value::Item(i) => i,
        _ => unreachable!("AstItem produced wrong value"),
    }?;
    let &def = scope.env(key.file)?.fns.get(&key.name)?;
    let Item::Fn(decl) = item else { return None };
    Some(ontixa_hir::lower_body(
        &decl,
        def,
        &scope,
        &mut db.interner,
        diags,
    ))
}

/// `resolve_workspace` rooted at `root`: discovers the reachable
/// file set by walking `use` declarations over the module-name table
/// (every registered file provides the module named by its stem),
/// then runs the three-pass resolver. Demanding each file's `Ast`
/// records the dependency edge — an edit to a `use` list or a
/// dependency's signatures re-runs resolution.
fn scope_eval(db: &mut Db, root: FileId, diags: &mut Diagnostics) -> ModuleScope {
    // Module table: name → file. First registration wins on a
    // duplicate stem; the resolver reports the ambiguity itself.
    let mut module_of: FxHashMap<InternId, FileId> = FxHashMap::default();
    for i in 0..db.file_count() {
        let f = FileId::new(i as u32);
        let module = db.file_module(f).to_string();
        let name = db.interner.intern(&module);
        module_of.entry(name).or_insert(f);
    }
    // Breadth-first discovery over `use` decls, root first — the
    // `files` order is part of the scope value's determinism.
    let mut order = vec![root];
    let mut seen: FxHashSet<FileId> = [root].into_iter().collect();
    let mut head = 0;
    while head < order.len() {
        let f = order[head];
        head += 1;
        let module_ast = ast(db, f);
        for u in &module_ast.uses {
            let Some(seg0) = u.path.segs.first() else {
                continue;
            };
            let name = db.interner.intern(&seg0.name);
            if let Some(&dep) = module_of.get(&name) {
                if dep != f && seen.insert(dep) {
                    order.push(dep);
                }
            }
        }
    }
    for &f in &order {
        db.set_root(f, root);
    }
    let asts: Vec<Arc<AstModule>> = order.iter().map(|&f| ast(db, f)).collect();
    // Owned copies — `file_module` borrows `db`, which
    // `resolve_workspace` needs mutably for the interner.
    let modules: Vec<String> = order
        .iter()
        .map(|&f| db.file_module(f).to_string())
        .collect();
    let files: Vec<WorkspaceFile> = order
        .iter()
        .enumerate()
        .map(|(i, &file)| WorkspaceFile {
            file,
            module: &modules[i],
            ast: &asts[i],
        })
        .collect();
    ontixa_hir::resolve_workspace(root, &files, &mut db.interner, diags)
}

/// `check_body` for one function — the per-definition unit of
/// type-inference reuse.
fn body_types(db: &mut Db, key: DefKey, diags: &mut Diagnostics) -> Option<CheckedBody> {
    let scope = scope(db, key.root);
    let mut body = hir(db, key)?;
    let tables = ontixa_types::check_body(&scope, &mut body, &db.interner, diags);
    Some(CheckedBody { body, tables })
}

/// Assembles the `HirModule` + `ModuleTypes` views workspace-wide
/// passes (ownership, graph) still consume — from per-definition
/// query results, not a monolithic re-lower.
fn assemble(db: &mut Db, scope: &ModuleScope) -> (HirModule, ModuleTypes) {
    let mut bodies = Vec::with_capacity(scope.defs.len());
    let mut types = Vec::with_capacity(scope.defs.len());
    for def in &scope.defs {
        if scope.fn_sig(def.id).is_none() {
            bodies.push(None);
            types.push(None);
            continue;
        }
        match checked(db, scope.def_key(def.id)) {
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
        let key = scope.def_key(def.id);
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
    let scope = scope(db, key.root);
    let c = checked(db, key)?;
    let own = ownership_t(db, key.root);
    Some(ontixa_mir::lower_fn(&scope, &c.body, &c.tables, &own))
}

/// The workspace's semantic program graph (keyed under its root
/// file). Node spans are file-absolute for consumers — `bases[def]`
/// is the absolute start of `def`'s item *in its own file*, so a
/// def in a dependency rebases against that file's coordinates.
fn graph(db: &mut Db, f: FileId) -> SemanticGraph {
    let scope = scope(db, f);
    let (module, types) = assemble(db, &scope);
    let own = ownership_t(db, f);
    let mut bases = vec![0u32; scope.defs.len()];
    let mut asts: FxHashMap<FileId, Arc<AstModule>> = FxHashMap::default();
    for def in &scope.defs {
        let a = asts.entry(def.file).or_insert_with(|| ast(db, def.file));
        bases[def.id.index()] = a.items[def.item as usize].span().start;
    }
    ontixa_semantic::build_graph(&module, &types, &own, &db.interner, &bases)
}

/// Every diagnostic emitted anywhere in this workspace's pipeline,
/// collected from dependency entries in demand order. Item-relative
/// diagnostics (`origin`-tagged) are rebased to file-absolute here —
/// each def's own file supplies the item base — and stamped with the
/// def's `file` so consumers know which source text the spans index.
fn diagnostics(db: &mut Db, f: FileId) -> Vec<Diagnostic> {
    let _ = db.demand(QueryKey::Parse(f));
    let sc = scope(db, f);
    // Demand every reachable file's AST — its parse/lower diags are
    // part of the workspace's diagnostics even when it has no defs.
    let mut asts: FxHashMap<FileId, Arc<AstModule>> = FxHashMap::default();
    for &file in &sc.files {
        asts.insert(file, ast(db, file));
    }
    for def in &sc.defs {
        if sc.fn_sig(def.id).is_some() {
            let _ = checked(db, sc.def_key(def.id));
        }
    }
    let _ = ownership_t(db, f);
    // Entries store transitive diagnostics, so each dep's diags may
    // repeat an earlier dep's — dedupe by value, keeping first-seen
    // (pipeline) order. Rebasing happens before dedupe so two defs'
    // identical item-relative diagnostics don't collapse.
    let mut out: Vec<Diagnostic> = Vec::new();
    for dep in db.deps_so_far().to_vec() {
        for d in db.entry_diags(dep) {
            let d = match d.origin {
                Some(did) => {
                    let def = sc.def(did);
                    let base = asts[&def.file].items[def.item as usize].span().start;
                    let mut d = d.rebased(base);
                    // The origin's file is authoritative — an
                    // eval-level stamp names the *query's* file,
                    // which for a dep's def is not the root.
                    d.file = Some(def.file);
                    d
                }
                None => d.clone(),
            };
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

/// The assembled artifact bundle — `compile` is a *pure join* of
/// per-definition and workspace-level query results.
fn compile(db: &mut Db, f: FileId) -> Arc<Artifacts> {
    let sc = scope(db, f);
    let mut asts: FxHashMap<FileId, AstModule> = FxHashMap::default();
    for &file in &sc.files {
        asts.insert(file, (*ast(db, file)).clone());
    }
    let module_ast = asts[&f].clone();
    let (module, types) = assemble(db, &sc);
    let ownership = ownership_t(db, f);
    let graph = graph_v(db, f);
    let mut mirs = Vec::with_capacity(sc.defs.len());
    for def in &sc.defs {
        if sc.fn_sig(def.id).is_none() {
            mirs.push(None);
            continue;
        }
        mirs.push(mir(db, sc.def_key(def.id)));
    }
    let mut diags = Diagnostics::new();
    for d in diags_v(db, f) {
        diags.push(d);
    }
    diags.sort();
    let timings = db.stage_timings();
    Arc::new(Artifacts {
        built_revision: db.revision,
        ast: module_ast,
        asts,
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
