//! Query vocabulary for the incremental engine: keys, memoized
//! values, dependency records, and statistics.
//!
//! Design: ADR-0008 (`docs/adr/0008-incremental-engine.md`). Queries
//! form a DAG rooted at [`QueryKey::Compile`]; per-definition queries
//! are keyed by the stable [`DefKey`] so editing one function can
//! never invalidate another function's memoized results.

use std::collections::BTreeMap;
use std::sync::Arc;

use ontixa_ast::{AstModule, Item};
use ontixa_diagnostics::Diagnostic;
use ontixa_hir::{HirBody, ModuleScope};
use ontixa_memory::OwnershipTables;
use ontixa_mir::MirBody;
use ontixa_semantic::SemanticGraph;
use ontixa_source::{DefKey, FileId};
use ontixa_syntax::SyntaxNode;
use ontixa_types::TypeTables;

/// Identifies a memoized computation.
///
/// File-granular keys carry a [`FileId`]; per-definition keys carry a
/// [`DefKey`]. `DefKey` (file + interned name) is stable across
/// revisions, unlike `DefId`, which is a per-scope dense index that
/// shifts when definitions are added, removed, or reordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryKey {
    /// Raw source text — the only true input, written by `set_source`.
    Source(FileId),
    /// `parse_file`: text → lossless syntax tree.
    Parse(FileId),
    /// `lower_module`: syntax tree → canonical AST.
    Ast(FileId),
    /// `resolve_module`: AST → module scope (defs, signatures, symbol
    /// table of module-level entities).
    Scope(FileId),
    /// One item's AST, extracted by name. This is the narrow waist
    /// between "the file's AST changed" and "this function's AST
    /// changed" — an unchanged item cuts off before `hir_body` runs.
    AstItem(DefKey),
    /// `lower_body`: one `fn` item → its [`HirBody`] (body-local
    /// expression and symbol arenas).
    HirBody(DefKey),
    /// `check_body`: one body → [`CheckedBody`] (field indices filled
    /// in the body's arena, plus the body's `TypeTables`).
    BodyTypes(DefKey),
    /// `infer_ownership`: module-wide contract fixpoint. Fact
    /// collection inside is memoized per body through the persistent
    /// [`OwnershipOracle`](ontixa_memory::OwnershipOracle), so a
    /// body-local edit only re-walks that body.
    Ownership(FileId),
    /// `lower_fn`: one checked body → MIR.
    MirBody(DefKey),
    /// `build_graph`: semantic program graph of a file.
    Graph(FileId),
    /// Every diagnostic emitted while producing a file's artifacts.
    Diagnostics(FileId),
    /// The assembled [`Artifacts`](crate::Artifacts) bundle.
    Compile(FileId),
}

impl QueryKey {
    /// Stable name for statistics and stage timings.
    pub fn name(self) -> &'static str {
        match self {
            QueryKey::Source(_) => "source",
            QueryKey::Parse(_) => "lex+parse",
            QueryKey::Ast(_) => "ast",
            QueryKey::Scope(_) => "resolve",
            QueryKey::AstItem(_) => "ast-item",
            QueryKey::HirBody(_) => "hir",
            QueryKey::BodyTypes(_) => "types",
            QueryKey::Ownership(_) => "ownership",
            QueryKey::MirBody(_) => "mir",
            QueryKey::Graph(_) => "graph",
            QueryKey::Diagnostics(_) => "diagnostics",
            QueryKey::Compile(_) => "assemble",
        }
    }

    /// The file this query's result belongs to.
    pub fn file(self) -> FileId {
        match self {
            QueryKey::Source(f)
            | QueryKey::Parse(f)
            | QueryKey::Ast(f)
            | QueryKey::Scope(f)
            | QueryKey::Ownership(f)
            | QueryKey::Graph(f)
            | QueryKey::Diagnostics(f)
            | QueryKey::Compile(f) => f,
            QueryKey::HirBody(k)
            | QueryKey::AstItem(k)
            | QueryKey::BodyTypes(k)
            | QueryKey::MirBody(k) => k.file,
        }
    }
}

/// The output of `check_body`: the body with `Field.field` indices
/// filled in place, plus the body's type tables. Downstream passes
/// (MIR lowering, the semantic graph) consume this — not the raw
/// `hir_body` result — so they always see checked field access.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckedBody {
    /// The body, with struct-literal and field-access indices resolved.
    pub body: HirBody,
    /// Per-expression and per-local types of this body.
    pub tables: TypeTables,
}

/// Type-erased memoized value; each variant serves one `QueryKey`
/// shape. Large values are `Arc`'d so demanding them is cheap.
#[derive(Debug, Clone)]
pub(crate) enum Value {
    /// `Source`: file text.
    Text(Arc<str>),
    /// `Parse`: lossless syntax tree (rowan nodes are internally
    /// `Arc`'d — cheap to clone).
    Tree(SyntaxNode),
    /// `Ast`: canonical AST — `Arc`'d because every `AstItem` demand
    /// reads it; an owned value would copy the whole module per def.
    Ast(Arc<AstModule>),
    /// `Scope`: module scope.
    Scope(Arc<ModuleScope>),
    /// `AstItem`: `None` when no item has that name.
    Item(Option<Item>),
    /// `HirBody`: `None` when the key names a `data` def or nothing.
    Hir(Option<HirBody>),
    /// `BodyTypes`: `None` when there is no such function.
    Checked(Option<CheckedBody>),
    /// `Ownership`: module-wide contracts.
    Ownership(Arc<OwnershipTables>),
    /// `MirBody`: `None` for `data` defs / missing functions.
    Mir(Option<MirBody>),
    /// `Graph`: the file's semantic program graph.
    Graph(SemanticGraph),
    /// `Diagnostics`: every diagnostic emitted by the file's pipeline.
    Diags(Vec<Diagnostic>),
    /// `Compile`: the assembled artifact bundle.
    Compile(Arc<crate::Artifacts>),
}

/// Early-cutoff comparison: `true` when a recomputed value equals the
/// memoized one, letting dependents keep their freshness.
///
/// `Tree` is never "equal": rowan nodes expose no cheap structural
/// equality, and the AST cutoff directly above it already catches
/// whitespace-only edits. `Compile` is a terminal node — nothing
/// depends on it, so comparing it would buy nothing.
pub(crate) fn same_value(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Text(x), Value::Text(y)) => x == y,
        (Value::Ast(x), Value::Ast(y)) => **x == **y,
        (Value::Scope(x), Value::Scope(y)) => **x == **y,
        (Value::Item(x), Value::Item(y)) => x == y,
        (Value::Hir(x), Value::Hir(y)) => x == y,
        (Value::Checked(x), Value::Checked(y)) => x == y,
        (Value::Ownership(x), Value::Ownership(y)) => **x == **y,
        (Value::Mir(x), Value::Mir(y)) => x == y,
        (Value::Graph(x), Value::Graph(y)) => x == y,
        (Value::Diags(x), Value::Diags(y)) => x == y,
        _ => false,
    }
}

/// One memoized result plus its dependency fingerprint.
#[derive(Debug)]
pub(crate) struct Entry {
    /// The memoized value.
    pub value: Value,
    /// Direct dependencies, in demand order.
    pub deps: Vec<QueryKey>,
    /// Diagnostics this query's evaluation emitted.
    pub diags: Vec<Diagnostic>,
    /// Revision at which `value` last *changed*.
    pub computed_at: u64,
    /// Revision at which freshness was last verified.
    pub verified_at: u64,
}

/// Cumulative per-query-kind execution statistics — the objective
/// evidence that incrementality works.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct QueryStats {
    /// Times each query kind was actually evaluated.
    pub executed: BTreeMap<String, u64>,
    /// Times a memoized entry served without re-evaluating
    /// (same-revision hits and verified-fresh hits).
    pub reused: BTreeMap<String, u64>,
    /// Cumulative evaluation nanoseconds per query kind.
    pub eval_nanos: BTreeMap<String, u64>,
}

impl QueryStats {
    pub(crate) fn bump(map: &mut BTreeMap<String, u64>, key: QueryKey) {
        *map.entry(key.name().to_string()).or_insert(0) += 1;
    }

    /// `(executed, reused)` totals across all query kinds.
    pub fn totals(&self) -> (u64, u64) {
        (self.executed.values().sum(), self.reused.values().sum())
    }
}
