//! The incremental compiler database.
//!
//! [`Db`] is a small memoized query engine (ADR-0008): every pipeline
//! stage is a [`QueryKey`], every result is a memoized [`Entry`]
//! carrying the dependencies it read and the revision at which its
//! value last changed. `set_source` bumps a global revision and
//! rewrites the `Source` input entry; the next demand verifies each
//! cached entry by walking its dependencies and re-evaluates only
//! those whose inputs drifted. When a recomputed value compares equal
//! to the memoized one, `computed_at` is preserved — the change
//! "cuts off" and dependents stay fresh.
//!
//! Per-definition queries (`HirBody`, `BodyTypes`, `MirBody`) are
//! keyed by the stable [`DefKey`], so editing one function can never
//! renumber or invalidate another function's cached work.

use std::sync::Arc;
use std::time::Instant;

use ontixa_ast::AstModule;
use ontixa_diagnostics::{Diagnostic, Diagnostics};
use ontixa_hir::{HirBody, HirModule, ModuleScope};
use ontixa_memory::{OwnershipOracle, OwnershipTables};
use ontixa_mir::{MirBody, MirModule};
use ontixa_semantic::SemanticGraph;
use ontixa_source::{DefKey, FileId, Interner};
use ontixa_types::{ModuleTypes, TypeTables};
use rustc_hash::FxHashMap;

use crate::eval;
use crate::query::{Entry, QueryKey, QueryStats, Value, same_value};

/// One pipeline stage's measured cost.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StageTiming {
    /// Stage name (`lex+parse`, `ast`, `resolve`, `hir`, `types`,
    /// `ownership`, `graph`, `mir`, ...). Per-definition query evals
    /// are aggregated under their stage name.
    pub stage: &'static str,
    /// Wall-clock nanoseconds for the last build.
    pub nanos: u64,
}

/// Everything a compiled workspace produced — the artifacts
/// downstream tools (CLI, daemon, agents) consume.
#[derive(Debug, Clone)]
pub struct Artifacts {
    /// Source revision these artifacts were built from.
    pub built_revision: u64,
    /// Canonical AST of the demanded (root) file.
    pub ast: AstModule,
    /// Canonical ASTs of every file in the workspace, by `FileId`.
    /// Needed to rebase item-relative spans of defs living in
    /// dependency files.
    pub asts: FxHashMap<FileId, AstModule>,
    /// Resolved, lowered HIR (post-typecheck — field indices filled).
    /// `module.scope` spans the whole reachable workspace.
    pub module: HirModule,
    /// Interner that `InternId`s in the module resolve against.
    pub interner: Interner,
    /// Per-body type tables (`types[def]` is `Some` for functions).
    pub types: ModuleTypes,
    /// Inferred ownership contracts.
    pub ownership: OwnershipTables,
    /// Semantic program graph.
    pub graph: SemanticGraph,
    /// Typed MIR.
    pub mir: MirModule,
    /// All diagnostics accumulated across stages.
    pub diags: Diagnostics,
    /// Per-stage build timings.
    pub timings: Vec<StageTiming>,
}

impl Artifacts {
    /// True when no stage produced an error-severity diagnostic.
    pub fn is_valid(&self) -> bool {
        !self.diags.has_errors()
    }
}

/// The result of a check-only demand: diagnostics plus stage timings.
/// Everything `check` needs, without the graph, MIR, or assembled
/// [`Artifacts`] that `compile` builds — see [`Db::check`].
#[derive(Debug, Clone)]
pub struct CheckReport {
    /// Source revision these diagnostics were produced from.
    pub checked_revision: u64,
    /// All diagnostics, sorted for deterministic output.
    pub diags: Diagnostics,
    /// Per-stage timings for the check.
    pub timings: Vec<StageTiming>,
}

impl CheckReport {
    /// True when no stage produced an error-severity diagnostic.
    pub fn is_valid(&self) -> bool {
        !self.diags.has_errors()
    }
}

/// One file's input state. The text is duplicated into the `Source`
/// memo entry — the file slot is only a fallback for out-of-band
/// evaluation.
struct FileSlot {
    text: Arc<str>,
    /// The module name this file provides (`use m;` resolves against
    /// it). Convention: the file's stem.
    module: String,
}

/// Query-oriented compiler state. One `Db` is one workspace session:
/// the memo table, the persistent [`Interner`], and the persistent
/// [`OwnershipOracle`] all survive across `set_source` edits.
#[derive(Default)]
pub struct Db {
    files: Vec<FileSlot>,
    /// Workspace root per file, installed by `Scope` discovery:
    /// `root_of(dep)` is the file whose `use`-walk reached `dep`.
    /// Files never discovered keep the default (`self`). Top-level
    /// demands reset the demanded file's root to itself — the file
    /// you `check`/`compile` is the root of *its* workspace.
    roots: FxHashMap<FileId, FileId>,
    /// Session-persistent interner — `InternId`s never change meaning
    /// across revisions, which is what makes `DefKey` stable.
    pub(crate) interner: Interner,
    /// Global input revision; bumped by `add_source`/`set_source`.
    pub(crate) revision: u64,
    /// The memo table.
    pub(crate) memo: FxHashMap<QueryKey, Entry>,
    /// Cross-revision ownership-fact cache (see `ontixa-memory`).
    pub(crate) oracle: OwnershipOracle,
    /// Dependency-recording stack; one frame per in-flight eval.
    pub(crate) frames: Vec<Vec<QueryKey>>,
    /// `(key, nanos)` of each eval during the current top-level demand.
    pub(crate) last_run: Vec<(QueryKey, u64)>,
    stats: QueryStats,
}

impl Db {
    /// An empty database.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a source file; returns its index. The file provides
    /// a module named `file{n}` — single-source callers never resolve
    /// `use`s, so the name is only a placeholder.
    pub fn add_source(&mut self, text: impl Into<String>) -> usize {
        let name = format!("file{}", self.files.len());
        self.add_source_named(name, text)
    }

    /// Registers a source file providing module `module` (its file
    /// stem, by convention); returns its index.
    pub fn add_source_named(
        &mut self,
        module: impl Into<String>,
        text: impl Into<String>,
    ) -> usize {
        self.revision += 1;
        let text: Arc<str> = text.into().into();
        self.files.push(FileSlot {
            text: text.clone(),
            module: module.into(),
        });
        let key = QueryKey::Source(FileId::new(self.files.len() as u32 - 1));
        self.memo.insert(
            key,
            Entry {
                value: Value::Text(text),
                deps: Vec::new(),
                diags: Vec::new(),
                computed_at: self.revision,
                verified_at: self.revision,
            },
        );
        self.files.len() - 1
    }

    /// Replaces a file's text. If the text is unchanged, the `Source`
    /// entry keeps its `computed_at` — dependents stay fresh and the
    /// next `compile` is a pure verification pass.
    pub fn set_source(&mut self, file: usize, text: impl Into<String>) {
        self.revision += 1;
        let text: Arc<str> = text.into().into();
        self.files[file].text = text.clone();
        let key = QueryKey::Source(FileId::new(file as u32));
        let computed_at = match self.memo.get(&key) {
            Some(old) if same_value(&old.value, &Value::Text(text.clone())) => old.computed_at,
            _ => self.revision,
        };
        self.memo.insert(
            key,
            Entry {
                value: Value::Text(text),
                deps: Vec::new(),
                diags: Vec::new(),
                computed_at,
                verified_at: self.revision,
            },
        );
    }

    /// The current source text of a file.
    pub fn source(&self, file: usize) -> &str {
        &self.files[file].text
    }

    /// Number of registered files.
    pub(crate) fn file_count(&self) -> usize {
        self.files.len()
    }

    /// The module name a file provides (its stem, by convention).
    pub(crate) fn file_module(&self, f: FileId) -> &str {
        &self.files[f.index()].module
    }

    /// The workspace root a file belongs to. Files never claimed by
    /// a `Scope` discovery root at themselves.
    pub(crate) fn root_of(&self, f: FileId) -> FileId {
        self.roots.get(&f).copied().unwrap_or(f)
    }

    /// Records `root` as the workspace `f` belongs to. Called by
    /// `Scope` evaluation as it discovers reachable files.
    pub(crate) fn set_root(&mut self, f: FileId, root: FileId) {
        self.roots.insert(f, root);
    }

    /// The session interner (for resolving `InternId`s in artifacts).
    pub fn interner(&self) -> &Interner {
        &self.interner
    }

    /// Cumulative query statistics since the `Db` was created.
    pub fn stats(&self) -> &QueryStats {
        &self.stats
    }

    /// Query keys evaluated during the most recent top-level demand —
    /// the observable unit of incremental work. Empty when the last
    /// demand verified everything fresh.
    pub fn last_evaluated(&self) -> Vec<QueryKey> {
        self.last_run.iter().map(|(k, _)| *k).collect()
    }

    /// The persistent ownership oracle — per-body fact memoization
    /// across revisions (`last_collected`/`last_reused` counters).
    pub fn oracle(&self) -> &OwnershipOracle {
        &self.oracle
    }

    /// Compiles (or returns the memoized) artifacts for `file`.
    pub fn compile(&mut self, file: usize) -> &Artifacts {
        let key = QueryKey::Compile(FileId::new(file as u32));
        self.top_demand(key);
        match &self.memo.get(&key).expect("compile evaluated").value {
            Value::Compile(a) => a,
            _ => unreachable!("Compile produced wrong value"),
        }
    }

    /// Compiles `file` and returns the artifacts by value. For
    /// one-shot consumers (the CLI); session consumers should use
    /// [`Db::compile`].
    pub fn compile_owned(&mut self, file: usize) -> Artifacts {
        self.compile(file).clone()
    }

    /// Checks `file` without building run artifacts. Demands
    /// `Diagnostics` only, so `Graph`, `MirBody`, and `Compile`
    /// never evaluate — validation reuses the same pipeline as
    /// `compile` but stops at ownership. The right demand for CLI
    /// `check`, daemon `check`, and rename validation.
    pub fn check(&mut self, file: usize) -> CheckReport {
        let key = QueryKey::Diagnostics(FileId::new(file as u32));
        self.top_demand(key);
        let Value::Diags(diags) = &self.memo.get(&key).expect("diagnostics evaluated").value else {
            unreachable!("Diagnostics produced wrong value")
        };
        let mut out = Diagnostics::new();
        for d in diags {
            out.push(d.clone());
        }
        out.sort();
        CheckReport {
            checked_revision: self.revision,
            diags: out,
            timings: self.stage_timings(),
        }
    }

    /// A function's lowered body, by name. `None` for `data` defs and
    /// unknown names.
    pub fn hir_body(&mut self, file: usize, name: &str) -> Option<&HirBody> {
        let key = self.def_key(file, name, QueryKey::HirBody);
        self.top_demand(key);
        match &self.memo.get(&key)?.value {
            Value::Hir(b) => b.as_ref(),
            _ => unreachable!("HirBody produced wrong value"),
        }
    }

    /// A function's type tables, by name.
    pub fn body_types(&mut self, file: usize, name: &str) -> Option<&TypeTables> {
        let key = self.def_key(file, name, QueryKey::BodyTypes);
        self.top_demand(key);
        match &self.memo.get(&key)?.value {
            Value::Checked(c) => c.as_ref().map(|c| &c.tables),
            _ => unreachable!("BodyTypes produced wrong value"),
        }
    }

    /// A function's MIR body, by name.
    pub fn mir_body(&mut self, file: usize, name: &str) -> Option<&MirBody> {
        let key = self.def_key(file, name, QueryKey::MirBody);
        self.top_demand(key);
        match &self.memo.get(&key)?.value {
            Value::Mir(m) => m.as_ref(),
            _ => unreachable!("MirBody produced wrong value"),
        }
    }

    /// The file's ownership contracts.
    pub fn ownership(&mut self, file: usize) -> Arc<OwnershipTables> {
        let key = QueryKey::Ownership(FileId::new(file as u32));
        match self.top_demand(key) {
            Value::Ownership(o) => o,
            _ => unreachable!("Ownership produced wrong value"),
        }
    }

    /// The file's resolved module scope.
    pub fn scope(&mut self, file: usize) -> Arc<ModuleScope> {
        let key = QueryKey::Scope(FileId::new(file as u32));
        match self.top_demand(key) {
            Value::Scope(s) => s,
            _ => unreachable!("Scope produced wrong value"),
        }
    }

    fn def_key(&mut self, file: usize, name: &str, ctor: fn(DefKey) -> QueryKey) -> QueryKey {
        let name = self.interner.intern(name);
        let f = FileId::new(file as u32);
        // The def resolves under the workspace its file belongs to —
        // `root_of` routes a dependency file to its claiming root.
        ctor(DefKey::new(self.root_of(f), f, name))
    }

    // ---- engine internals ------------------------------------------

    /// A top-level demand: clears the run log, then drives `key` to
    /// freshness. Public accessors funnel through here.
    fn top_demand(&mut self, key: QueryKey) -> Value {
        self.last_run.clear();
        // The demanded file roots its own workspace — `check dep.ixa`
        // means "dep's workspace", even if `dep` was earlier reached
        // as a dependency of some other root.
        let root = key.file();
        self.roots.insert(root, root);
        // Reclaim the workspace's file set from the memoized scope.
        // A dep that was since `check`ed directly has `roots[dep] =
        // dep`; a fresh `Scope(root)` never re-runs `set_root`, so
        // without this reclaim the dep would keep routing to its own
        // single-file workspace for the whole demand.
        if let Some(entry) = self.memo.get(&QueryKey::Scope(root)) {
            if let Value::Scope(s) = &entry.value {
                for f in s.files.clone() {
                    self.roots.insert(f, root);
                }
            }
        }
        self.demand(key)
    }

    /// Records `key` as a dependency of the in-flight eval (if any),
    /// brings it fresh, and returns its value.
    pub(crate) fn demand(&mut self, key: QueryKey) -> Value {
        if let Some(frame) = self.frames.last_mut() {
            frame.push(key);
        }
        self.ensure_fresh(key);
        self.memo.get(&key).expect("evaluated entry").value.clone()
    }

    /// Brings `key` to the current revision: verifies each
    /// dependency's `computed_at` against the entry's own, and
    /// re-evaluates only when a dependency actually changed value.
    fn ensure_fresh(&mut self, key: QueryKey) {
        let Some(entry) = self.memo.get(&key) else {
            self.run_eval(key);
            return;
        };
        if entry.verified_at == self.revision {
            QueryStats::bump(&mut self.stats.reused, key);
            return;
        }
        // A dep makes this entry dirty when it changed *after the
        // entry's last verification* — not after the entry's last
        // value change. Comparing against `computed_at` would
        // re-evaluate forever an entry that recomputed to an equal
        // value (early cutoff): its `computed_at` stays behind its
        // deps' forever, so the check would never heal.
        let verified_at = entry.verified_at;
        let deps = entry.deps.clone();
        let mut dirty = false;
        for dep in deps {
            self.ensure_fresh(dep);
            let changed = self
                .memo
                .get(&dep)
                .is_none_or(|d| d.computed_at > verified_at);
            if changed {
                dirty = true;
                break;
            }
        }
        if dirty {
            self.run_eval(key);
        } else {
            self.memo.get_mut(&key).expect("verified entry").verified_at = self.revision;
            QueryStats::bump(&mut self.stats.reused, key);
        }
    }

    /// Runs `key`'s evaluator and installs the result, preserving
    /// `computed_at` when the value is unchanged (early cutoff).
    fn run_eval(&mut self, key: QueryKey) {
        self.frames.push(Vec::new());
        let t0 = Instant::now();
        let (value, mut diags) = eval::eval(self, key);
        let nanos = t0.elapsed().as_nanos() as u64;
        let deps = self.frames.pop().unwrap_or_default();
        // Entries record their *transitive* diagnostics — dependencies'
        // first (demand order), then this eval's own — so a consumer
        // asking for "the diagnostics of X" never needs to walk the
        // dep graph itself.
        let mut all: Vec<Diagnostic> = Vec::new();
        for dep in &deps {
            all.extend(self.entry_diags(*dep).iter().cloned());
        }
        all.append(&mut diags);
        let diags = all;
        let computed_at = match self.memo.get(&key) {
            Some(old) if same_value(&old.value, &value) => old.computed_at,
            _ => self.revision,
        };
        self.memo.insert(
            key,
            Entry {
                value,
                deps,
                diags,
                computed_at,
                verified_at: self.revision,
            },
        );
        self.last_run.push((key, nanos));
        QueryStats::bump(&mut self.stats.executed, key);
        *self
            .stats
            .eval_nanos
            .entry(key.name().to_string())
            .or_insert(0) += nanos;
    }

    /// The revision a query's value last changed at (`0` = never).
    pub(crate) fn stamp(&self, key: QueryKey) -> u64 {
        self.memo.get(&key).map_or(0, |e| e.computed_at)
    }

    /// Diagnostics recorded on a query's entry.
    pub(crate) fn entry_diags(&self, key: QueryKey) -> &[Diagnostic] {
        self.memo.get(&key).map_or(&[], |e| e.diags.as_slice())
    }

    /// Dependencies demanded so far by the in-flight eval.
    pub(crate) fn deps_so_far(&self) -> &[QueryKey] {
        self.frames.last().map_or(&[], |f| f.as_slice())
    }

    /// The per-query eval timings of the current demand (cloned —
    /// the log stays intact so `last_evaluated` still reports every
    /// eval that ran).
    pub(crate) fn take_last_run(&mut self) -> Vec<(QueryKey, u64)> {
        self.last_run.clone()
    }

    /// Aggregates the last demand's per-query timings into per-stage
    /// timings (multiple `hir`/`types`/`mir` evals sum into one stage
    /// entry, in demand order).
    pub(crate) fn stage_timings(&mut self) -> Vec<StageTiming> {
        let mut order = Vec::new();
        let mut agg: FxHashMap<&'static str, u64> = FxHashMap::default();
        for (k, nanos) in self.take_last_run() {
            if let Some(v) = agg.get_mut(k.name()) {
                *v += nanos;
            } else {
                agg.insert(k.name(), nanos);
                order.push(k.name());
            }
        }
        order
            .into_iter()
            .map(|stage| StageTiming {
                stage,
                nanos: agg[stage],
            })
            .collect()
    }

    /// A file's text (fallback for the `Source` eval arm).
    pub(crate) fn file_text(&self, f: FileId) -> Arc<str> {
        self.files[f.index()].text.clone()
    }
}
