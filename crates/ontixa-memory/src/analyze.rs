//! Ownership, borrow, move, and escape inference.
//!
//! The pass runs in two phases:
//!
//! **Phase A — contract inference.** For every function, a taint walk
//! records how each parameter is *used*: read, mutated, moved, or
//! escaped into the return value. Call arguments are judged by the
//! callee's contract — which is itself being inferred — so contracts
//! iterate to a fixpoint over the call graph. The fixpoint runs as a
//! **reverse-dependency worklist**: a function is re-evaluated only
//! when one of its callees' contracts changed. Facts are monotone in
//! callee contracts and `classify` is monotone in facts on a finite
//! lattice (`copy < borrow < borrow_mut < move < escape`), so the
//! iteration converges to the unique least fixpoint — recursion,
//! mutual recursion, and arbitrarily deep chains all terminate
//! structurally. There is no round cap; `Unknown` is never produced
//! by "the compiler got tired" — it is reserved for genuinely
//! unanalyzable constructs (e.g. future indirect calls).
//!
//! **Phase B — enforcement.** With final contracts, a per-binding
//! state walk (`initialized`, `moved`, `maybe-*`) emits
//! `E_USE_AFTER_MOVE` and `E_UNINITIALIZED` diagnostics, merging
//! states across `if` branches.
//!
//! Scoping rules (documented in `docs/memory-model.md`):
//!
//! - `let`/assignment/return/struct-field positions **move** their
//!   values. An expression statement drops (moves) its value.
//! - Field reads are reads; a non-copy field used in a move position
//!   conservatively moves the whole binding (no partial tracking yet).
//! - A value stored into a structure that later escapes makes its
//!   source params escape too — tracked by per-local *carrier* sets.
//! - Copy types (`Ty::is_copy`) are never moved or borrowed; they are
//!   always `Copy`.

use crate::behavior::ParamBehavior;
use crate::place::{Loan, LoanKind, Place, Region, place_of};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_hir::{HirBody, HirExpr, HirExprKind, HirModule, HirPlace, HirStmt, ModuleScope};
use ontixa_source::{DefId, ExprId, InternId, Interner, Span, SymbolId};
use ontixa_types::{ModuleTypes, Ty, TypeTables};
use rustc_hash::{FxHashMap, FxHashSet};

/// The product of ownership inference: the inferred contract of every
/// function parameter, by definition and parameter position.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct OwnershipTables {
    /// `param_behaviors[f][i]` — inferred behavior of `f`'s i-th param.
    pub param_behaviors: FxHashMap<DefId, Vec<ParamBehavior>>,
    /// `summaries[f][i]` — escape exits + evidence for `f`'s i-th param.
    pub summaries: FxHashMap<DefId, Vec<ParamSummary>>,
}

/// What the analyzer *saw* a parameter do — the explanation layer
/// under the contract (escape exits plus the spans that set flags).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParamSummary {
    /// Exits the param's value may take (`Return`, `ViaCall`).
    pub escapes: Vec<EscapeExit>,
    /// Usage sites that produced the contract's flags.
    pub evidence: Vec<Evidence>,
}

/// One observed use of a parameter — the span that set a flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Evidence {
    /// Which fact this site produced.
    pub kind: EvidenceKind,
    /// Where it happened.
    pub at: Span,
}

/// The fact an [`Evidence`] supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    /// The value was observed.
    Read,
    /// Written through a field projection or `borrow_mut` arg.
    Mutated,
    /// Ownership consumed without escaping.
    Moved,
    /// May flow into the return value.
    Escaped,
}

impl EvidenceKind {
    /// Stable string for JSON output.
    pub fn as_str(&self) -> &'static str {
        match self {
            EvidenceKind::Read => "read",
            EvidenceKind::Mutated => "mutated",
            EvidenceKind::Moved => "moved",
            EvidenceKind::Escaped => "escaped",
        }
    }
}

impl OwnershipTables {
    /// The contract vector of a function (empty for `data` defs).
    pub fn contract(&self, def: DefId) -> &[ParamBehavior] {
        self.param_behaviors
            .get(&def)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// The summaries of a function's params (empty for `data` defs).
    pub fn summary(&self, def: DefId) -> &[ParamSummary] {
        self.summaries.get(&def).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// Contract vectors keyed by def.
pub type ContractTable = FxHashMap<DefId, Vec<ParamBehavior>>;

/// One way a parameter's value may leave its function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeExit {
    /// Flows into the function's return value.
    Return,
    /// Flows into a call whose callee may return it (the value exits
    /// through that callee's own contract).
    ViaCall(DefId),
}

impl EscapeExit {
    /// Stable string for JSON output.
    pub fn as_str(&self, scope: &ModuleScope, interner: &Interner) -> String {
        match self {
            EscapeExit::Return => "return".to_string(),
            EscapeExit::ViaCall(d) => {
                let sym = scope.def(*d).name;
                format!("call `{}`", interner.resolve(scope.symbols.get(sym).name))
            }
        }
    }
}

/// Per-module incremental state for ownership inference — the
/// memoized surface of the fixpoint across compilation sessions.
///
/// The scheme is *hypothesis-verified rounds* (ADR-0008/0010):
/// a fact walk for `f` reads callee contracts through a *view* —
/// the previous run's final contract when one was recorded, else
/// the current in-flight value. Each entry records exactly which
/// `(callee → contract)` pairs it consumed. After the worklist
/// converges, a verification sweep compares every consumed record
/// against the new finals; drift invalidates the affected entries
/// and triggers another round with the drifted contracts as the
/// new hypothesis. Contracts only ascend, so the rounds terminate
/// (at most `lattice height × #params` rounds; in practice 1 for
/// unchanged or body-local edits).
#[derive(Debug, Default)]
pub struct OwnershipOracle {
    /// Final contracts of the last completed run, by def name.
    prev_contracts: FxHashMap<InternId, Vec<ParamBehavior>>,
    /// Memoized per-body facts, by def name.
    facts: FxHashMap<InternId, FactsEntry>,
    /// Bodies actually re-walked during the last run.
    pub last_collected: usize,
    /// Bodies served from the fact memo during the last run.
    pub last_reused: usize,
    /// Outer rounds the last run needed (1 = hypothesis confirmed).
    pub last_rounds: usize,
}

#[derive(Debug)]
struct FactsEntry {
    /// Input stamps the facts were collected under.
    stamps: FactStamps,
    /// Collected facts per parameter.
    facts: Vec<ParamFacts>,
    /// Escape exits + evidence per parameter.
    summaries: Vec<ParamSummary>,
    /// `(callee name, contract vector)` — every callee contract the
    /// collection consumed, at the values it observed through the
    /// view. Valid only while those views hold.
    consumed: Vec<(InternId, Vec<ParamBehavior>)>,
}

/// Stamps identifying the inputs `collect_facts` for a def depends
/// on — supplied by the caller (the database passes query
/// `computed_at` revisions; standalone callers pass zeros, which
/// simply never hits the memo).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FactStamps {
    /// Stamp of `hir_body(def)`.
    pub body: u64,
    /// Stamp of `body_types(def)`.
    pub types: u64,
}

/// The facts a collection produced plus the contract values it read.
struct CollectedFacts {
    facts: Vec<ParamFacts>,
    summaries: Vec<ParamSummary>,
    consumed: Vec<(InternId, Vec<ParamBehavior>)>,
}

/// Infers ownership for the whole module and enforces
/// use-after-move / initialization / mutability rules.
///
/// `types[def]` holds the per-body [`TypeTables`] produced by
/// `check_body`. `stamps` (indexed by `DefId`) and `oracle` enable
/// cross-run fact reuse; pass `None` and a default oracle for
/// one-shot compilation.
pub fn infer_ownership(
    module: &HirModule,
    types: &ModuleTypes,
    interner: &Interner,
    diags: &mut Diagnostics,
    stamps: Option<&[FactStamps]>,
    oracle: &mut OwnershipOracle,
) -> OwnershipTables {
    oracle.last_collected = 0;
    oracle.last_reused = 0;
    oracle.last_rounds = 0;
    let mut hypothesis = std::mem::take(&mut oracle.prev_contracts);

    let (contracts, summaries) = loop {
        oracle.last_rounds += 1;
        let (contracts, summaries, drifted) =
            fixpoint_round(module, types, stamps, &hypothesis, oracle);
        if drifted.is_empty() {
            // Every memoized collection ran under contracts that
            // match the finals — the result is the true fixpoint.
            break (contracts, summaries);
        }
        // Hypothesis was wrong somewhere: drop the drifted entries
        // and rerun under this round's finals.
        for name in drifted {
            oracle.facts.remove(&name);
        }
        hypothesis = contracts
            .iter()
            .map(|(d, c)| (def_name(&module.scope, *d), c.clone()))
            .collect();
    };

    oracle.prev_contracts = contracts
        .iter()
        .map(|(d, c)| (def_name(&module.scope, *d), c.clone()))
        .collect();
    // Forget memos of defs that no longer exist.
    oracle
        .facts
        .retain(|name, _| module.scope.fns.contains_key(name));

    // ---- Phase B: enforcement ----
    for def in module.scope.defs.iter() {
        let Some(sig) = module.scope.fn_sig(def.id) else {
            continue;
        };
        let Some(body) = module.body(def.id) else {
            continue;
        };
        let mut e = Enforcer {
            scope: &module.scope,
            body,
            tables: types[def.id.index()]
                .as_ref()
                .unwrap_or_else(|| empty_tables()),
            contracts: &contracts,
            interner,
            diags,
            state: FxHashMap::default(),
            loans: Vec::new(),
        };
        for p in &sig.params {
            e.state.insert(
                p.symbol,
                BindingState {
                    init: Init::Yes,
                    moved: Moved::No,
                },
            );
        }
        e.eval(body.root, Ctx::Move);
    }

    OwnershipTables {
        param_behaviors: contracts,
        summaries,
    }
}

/// The interned name of a def — the cross-revision identity used by
/// the oracle.
fn def_name(scope: &ModuleScope, def: DefId) -> InternId {
    scope.symbols.get(scope.def(def).name).name
}

/// One fixpoint round: contracts seeded at bottom, a
/// reverse-dependency worklist, and per-body fact collection under
/// the hypothesis view. Returns the round's contracts plus the set
/// of def names whose consumed contracts no longer match the finals
/// (hypothesis drift — a nonempty set forces another round).
fn fixpoint_round(
    module: &HirModule,
    types: &ModuleTypes,
    stamps: Option<&[FactStamps]>,
    hypothesis: &FxHashMap<InternId, Vec<ParamBehavior>>,
    oracle: &mut OwnershipOracle,
) -> (
    ContractTable,
    FxHashMap<DefId, Vec<ParamSummary>>,
    FxHashSet<InternId>,
) {
    let mut contracts: ContractTable = module
        .scope
        .defs
        .iter()
        .filter_map(|d| {
            module.scope.fn_sig(d.id).map(|sig| {
                (
                    d.id,
                    sig.params
                        .iter()
                        .map(|p| {
                            let ty = Ty::from_ref(p.ty);
                            if ty.is_copy() {
                                ParamBehavior::Copy
                            } else {
                                ParamBehavior::Borrow
                            }
                        })
                        .collect(),
                )
            })
        })
        .collect();

    // Reverse edges: callee -> its callers (sorted for deterministic
    // enqueue order; the fixpoint result itself is order-independent).
    let mut callers: FxHashMap<DefId, Vec<DefId>> = FxHashMap::default();
    for def in &module.scope.defs {
        if let Some(body) = module.body(def.id) {
            for callee in callees_of(body) {
                callers.entry(callee).or_default().push(def.id);
            }
        }
    }
    for v in callers.values_mut() {
        v.sort_unstable();
        v.dedup();
    }

    // The view a collection reads through: hypothesis (previous
    // finals) for defs seen last run, the in-flight contract for
    // anything new. Reading through the view is what makes a
    // confirmed hypothesis equal a full recollection.
    let view = |contracts: &ContractTable, callee: DefId| -> Vec<ParamBehavior> {
        let name = def_name(&module.scope, callee);
        hypothesis
            .get(&name)
            .cloned()
            .unwrap_or_else(|| contracts.get(&callee).cloned().unwrap_or_default())
    };

    let mut work: Vec<DefId> = module
        .scope
        .defs
        .iter()
        .filter(|d| module.scope.fn_sig(d.id).is_some() && module.body(d.id).is_some())
        .map(|d| d.id)
        .collect();
    let mut queued: FxHashSet<DefId> = work.iter().copied().collect();
    let mut summaries: FxHashMap<DefId, Vec<ParamSummary>> = FxHashMap::default();
    let mut head = 0;
    while head < work.len() {
        let f = work[head];
        head += 1;
        queued.remove(&f);
        let sig = module.scope.fn_sig(f).unwrap();
        let body = module.body(f).unwrap();
        let fname = def_name(&module.scope, f);
        let (facts, sum) = match memo_take(&module.scope, oracle, stamps, f, fname, |g| {
            view(&contracts, g)
        }) {
            Some(pair) => {
                oracle.last_reused += 1;
                pair
            }
            None => {
                oracle.last_collected += 1;
                let collected = collect_facts(
                    &module.scope,
                    body,
                    types[f.index()].as_ref().unwrap_or_else(|| empty_tables()),
                    |g| view(&contracts, g),
                    sig,
                );
                if let Some(stamps) = stamps {
                    oracle.facts.insert(
                        fname,
                        FactsEntry {
                            stamps: stamps[f.index()],
                            facts: collected.facts.clone(),
                            summaries: collected.summaries.clone(),
                            consumed: collected.consumed,
                        },
                    );
                }
                (collected.facts, collected.summaries)
            }
        };
        summaries.insert(f, sum);
        let mut next = Vec::with_capacity(sig.params.len());
        for (i, p) in sig.params.iter().enumerate() {
            next.push(classify(Ty::from_ref(p.ty), &facts[i]));
        }
        if contracts.get(&f) != Some(&next) {
            contracts.insert(f, next);
            if let Some(cs) = callers.get(&f) {
                for &caller in cs {
                    if queued.insert(caller) {
                        work.push(caller);
                    }
                }
            }
        }
    }

    // Verification: every recorded consumed contract must equal the
    // round's finals. Entries that checked out against the
    // hypothesis mid-round are only sound if the hypothesis held.
    let mut drifted = FxHashSet::default();
    for (fname, entry) in &oracle.facts {
        for (callee_name, recorded) in &entry.consumed {
            let actual = module
                .scope
                .fns
                .get(callee_name)
                .and_then(|d| contracts.get(d));
            if actual != Some(recorded) {
                drifted.insert(*fname);
                break;
            }
        }
    }
    (contracts, summaries, drifted)
}

/// Reuses memoized facts for `def` when its input stamps match and
/// every callee contract it consumed still equals the current view.
fn memo_take(
    scope: &ModuleScope,
    oracle: &OwnershipOracle,
    stamps: Option<&[FactStamps]>,
    def: DefId,
    name: InternId,
    view: impl Fn(DefId) -> Vec<ParamBehavior>,
) -> Option<(Vec<ParamFacts>, Vec<ParamSummary>)> {
    let stamps = stamps?;
    let entry = oracle.facts.get(&name)?;
    if entry.stamps != stamps[def.index()] {
        return None;
    }
    for (callee_name, recorded) in &entry.consumed {
        let callee = scope.fns.get(callee_name)?;
        if view(*callee) != *recorded {
            return None;
        }
    }
    Some((entry.facts.clone(), entry.summaries.clone()))
}

/// Shared empty tables for bodies that lack them (error paths).
fn empty_tables() -> &'static TypeTables {
    static EMPTY: std::sync::OnceLock<TypeTables> = std::sync::OnceLock::new();
    EMPTY.get_or_init(TypeTables::default)
}

// ================= Phase A: usage facts =================

/// The set of `fn` defs called from `body`'s expression tree.
/// Used to build the callee→caller worklist edges.
fn callees_of(body: &HirBody) -> FxHashSet<DefId> {
    let mut out = FxHashSet::default();
    let mut stack = vec![body.root];
    while let Some(id) = stack.pop() {
        match &body.expr(id).kind {
            HirExprKind::Call { def, args } => {
                out.insert(*def);
                stack.extend(args.iter().copied());
            }
            HirExprKind::Field { base, .. } => stack.push(*base),
            HirExprKind::Binary { lhs, rhs, .. } => {
                stack.push(*lhs);
                stack.push(*rhs);
            }
            HirExprKind::Unary { expr, .. } => stack.push(*expr),
            HirExprKind::If { cond, then, else_ } => {
                stack.push(*cond);
                stack.push(*then);
                stack.extend(*else_);
            }
            HirExprKind::Block { stmts, tail } => {
                for s in stmts {
                    match s {
                        HirStmt::Let { init: Some(e), .. } => stack.push(*e),
                        HirStmt::Assign { value, .. } => stack.push(*value),
                        HirStmt::Expr { expr, .. } => stack.push(*expr),
                        HirStmt::Return { value: Some(v), .. } => stack.push(*v),
                        _ => {}
                    }
                }
                stack.extend(*tail);
            }
            HirExprKind::StructLit { fields, .. } => {
                stack.extend(fields.iter().map(|(_, e)| *e));
            }
            HirExprKind::Literal(_) | HirExprKind::Var(_) | HirExprKind::Poison => {}
        }
    }
    out
}

/// Per-parameter usage facts gathered by the taint walk — the
/// *multi-dimensional* ownership domain (see `docs/ownership-lattice.md`):
///
/// | flag      | dimension        | meaning                          |
/// | --------- | ---------------- | -------------------------------- |
/// | `read`    | access ≥ read    | the value was observed           |
/// | `mutated` | access = write   | written through field projection |
/// | `moved`   | consumption      | ownership consumed, not escaped  |
/// | `escaped` | escape           | may flow into the return value   |
///
/// `classify` derives the public [`ParamBehavior`] contract from
/// these facts. Flags only ever go `false → true` during one walk,
/// and callee contracts only strengthen between walks — the two
/// monotonicities together are what make the fixpoint finite.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ParamFacts {
    /// The value was observed.
    pub read: bool,
    /// Written through a field projection (or a mutable-borrow arg).
    pub mutated: bool,
    /// Ownership consumed without escaping.
    pub moved: bool,
    /// May flow into the return value.
    pub escaped: bool,
}

/// Evaluation context: whether the expression's value is being
/// consumed (`Move`) or merely observed (`Read`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    Move,
    Read,
}

/// The set of parameter indices whose value may flow through an
/// expression or be carried by a local.
type Carriers = FxHashSet<u32>;

fn classify(ty: Ty, f: &ParamFacts) -> ParamBehavior {
    if ty.is_copy() {
        return ParamBehavior::Copy;
    }
    if f.escaped {
        ParamBehavior::Escape
    } else if f.moved {
        ParamBehavior::Move
    } else if f.mutated {
        ParamBehavior::BorrowMut
    } else if f.read {
        ParamBehavior::Borrow
    } else {
        // An unused non-copy parameter is still consumed: ownership
        // transfers to the callee and the value drops at scope end.
        ParamBehavior::Move
    }
}

/// Collects parameter usage facts for one body, reading callee
/// contracts through `view` (the hypothesis or the in-flight table).
/// Returns the facts plus the `(callee, contract)` pairs the walk
/// consumed — the memo validity record.
fn collect_facts(
    scope: &ModuleScope,
    body: &HirBody,
    tables: &TypeTables,
    view: impl Fn(DefId) -> Vec<ParamBehavior>,
    sig: &ontixa_hir::FnSig,
) -> CollectedFacts {
    let mut c = FactCollector {
        scope,
        body,
        tables,
        view: &view,
        param_index: sig
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| (p.symbol, i as u32))
            .collect(),
        facts: vec![ParamFacts::default(); sig.params.len()],
        summaries: vec![ParamSummary::default(); sig.params.len()],
        carriers: FxHashMap::default(),
        consumed: Vec::new(),
    };
    let root_carriers = c.eval(body.root, Ctx::Move);
    let root_span = body.expr(body.root).span;
    for i in root_carriers {
        c.facts[i as usize].escaped = true;
        c.mark_escape(i, EscapeExit::Return, root_span);
    }
    CollectedFacts {
        facts: c.facts,
        summaries: c.summaries,
        consumed: c.consumed,
    }
}

struct FactCollector<'a, 'v> {
    scope: &'a ModuleScope,
    body: &'a HirBody,
    tables: &'a TypeTables,
    /// Contract lookup: hypothesis-first during incremental rounds.
    view: &'v dyn Fn(DefId) -> Vec<ParamBehavior>,
    /// Param symbol → parameter position.
    param_index: FxHashMap<SymbolId, u32>,
    facts: Vec<ParamFacts>,
    /// Param index → exits its value may take (`Return`, `ViaCall`).
    summaries: Vec<ParamSummary>,
    /// Local → param indices whose value the local may carry.
    carriers: FxHashMap<SymbolId, Carriers>,
    /// `(callee name, contract)` read during this walk, in order.
    consumed: Vec<(InternId, Vec<ParamBehavior>)>,
}

impl FactCollector<'_, '_> {
    fn expr(&self, id: ExprId) -> &HirExpr {
        self.body.expr(id)
    }

    /// The contract the callee is viewed under, recorded as consumed.
    fn contract_of(&mut self, def: DefId) -> Vec<ParamBehavior> {
        let c = (self.view)(def);
        let name = self.scope.symbols.get(self.scope.def(def).name).name;
        if let Some(e) = self.consumed.iter_mut().find(|(n, _)| *n == name) {
            e.1 = c.clone();
        } else {
            self.consumed.push((name, c.clone()));
        }
        c
    }

    /// Flags a symbol as read/moved when it is a parameter, recording
    /// the site as evidence.
    fn flag(&mut self, sym: SymbolId, ctx: Ctx, at: Span) {
        if let Some(&i) = self.param_index.get(&sym) {
            let i = i as usize;
            let kind = match ctx {
                Ctx::Read => {
                    self.facts[i].read = true;
                    EvidenceKind::Read
                }
                Ctx::Move => {
                    self.facts[i].moved = true;
                    EvidenceKind::Moved
                }
            };
            Self::record(&mut self.summaries[i].evidence, kind, at);
        }
    }

    fn flag_mutated(&mut self, sym: SymbolId, at: Span) {
        if let Some(&i) = self.param_index.get(&sym) {
            self.facts[i as usize].mutated = true;
            Self::record(
                &mut self.summaries[i as usize].evidence,
                EvidenceKind::Mutated,
                at,
            );
        }
    }

    /// Records an escape exit for a param index (deduplicated) and
    /// the evidence that produced it.
    fn mark_escape(&mut self, i: u32, exit: EscapeExit, at: Span) {
        let s = &mut self.summaries[i as usize];
        if !s.escapes.contains(&exit) {
            s.escapes.push(exit);
        }
        Self::record(&mut s.evidence, EvidenceKind::Escaped, at);
    }

    /// Records an evidence site (deduplicated by kind + span).
    fn record(evidence: &mut Vec<Evidence>, kind: EvidenceKind, at: Span) {
        let e = Evidence { kind, at };
        if !evidence.contains(&e) {
            evidence.push(e);
        }
    }

    /// The param indices an expression's value may carry.
    fn eval(&mut self, id: ExprId, ctx: Ctx) -> Carriers {
        match self.expr(id).kind.clone() {
            HirExprKind::Literal(_) | HirExprKind::Poison => Carriers::default(),
            HirExprKind::Var(sym) => {
                let at = self.expr(id).span;
                self.flag(sym, ctx, at);
                if let Some(i) = self.param_index.get(&sym) {
                    let mut s = Carriers::default();
                    s.insert(*i);
                    s
                } else {
                    self.carriers.get(&sym).cloned().unwrap_or_default()
                }
            }
            HirExprKind::Field { base, .. } => {
                // A `Copy`-typed field is only *read*: `return p.x`
                // does not move `p`. A non-copy field in a move
                // position conservatively moves the whole binding.
                if self.tables.ty_of(id).is_copy() {
                    self.eval(base, Ctx::Read);
                    Carriers::default()
                } else {
                    self.eval(base, ctx)
                }
            }
            HirExprKind::Call { def, args } => {
                let mut out = Carriers::default();
                let contract = self.contract_of(def);
                for (i, arg) in args.iter().enumerate() {
                    let b = contract.get(i).copied().unwrap_or(ParamBehavior::Move);
                    match b {
                        ParamBehavior::Borrow | ParamBehavior::Copy => {
                            self.eval(*arg, Ctx::Read);
                        }
                        ParamBehavior::BorrowMut => {
                            self.eval(*arg, Ctx::Read);
                            if let Some(sym) = self.root_var(*arg) {
                                self.flag_mutated(sym, self.expr(*arg).span);
                            }
                        }
                        ParamBehavior::Move | ParamBehavior::Unknown => {
                            self.eval(*arg, Ctx::Move);
                        }
                        ParamBehavior::Escape => {
                            let c = self.eval(*arg, Ctx::Move);
                            // The callee may return this argument's
                            // value — so it flows onward through us.
                            for &i in &c {
                                self.mark_escape(i, EscapeExit::ViaCall(def), self.expr(id).span);
                            }
                            out.extend(c);
                        }
                    }
                }
                out
            }
            HirExprKind::Binary { lhs, rhs, .. } => {
                self.eval(lhs, Ctx::Read);
                self.eval(rhs, Ctx::Read);
                Carriers::default()
            }
            HirExprKind::Unary { expr, .. } => {
                self.eval(expr, Ctx::Read);
                Carriers::default()
            }
            HirExprKind::If { cond, then, else_ } => {
                self.eval(cond, Ctx::Read);
                let mut out = self.eval(then, ctx);
                if let Some(e) = else_ {
                    out.extend(self.eval(e, ctx));
                }
                out
            }
            HirExprKind::Block { stmts, tail } => {
                self.walk_stmts(&stmts);
                match tail {
                    Some(t) => self.eval(t, ctx),
                    None => Carriers::default(),
                }
            }
            HirExprKind::StructLit { fields, .. } => {
                let mut out = Carriers::default();
                for (_, v) in &fields {
                    out.extend(self.eval(*v, Ctx::Move));
                }
                out
            }
        }
    }

    fn walk_stmts(&mut self, stmts: &[HirStmt]) {
        for stmt in stmts {
            match stmt {
                HirStmt::Let { symbol, init, .. } => {
                    let c = match init {
                        Some(e) => self.eval(*e, Ctx::Move),
                        None => Carriers::default(),
                    };
                    self.carriers.insert(*symbol, c);
                }
                HirStmt::Assign { target, value, .. } => {
                    let c = self.eval(*value, Ctx::Move);
                    self.flag_mutated(target.base, target.span);
                    if !c.is_empty() {
                        self.carriers.entry(target.base).or_default().extend(c);
                    }
                }
                HirStmt::Expr { expr, .. } => {
                    self.eval(*expr, Ctx::Move);
                }
                HirStmt::Return { value, .. } => {
                    if let Some(v) = value {
                        let c = self.eval(*v, Ctx::Move);
                        let at = self.expr(*v).span;
                        for i in c {
                            self.facts[i as usize].escaped = true;
                            self.mark_escape(i, EscapeExit::Return, at);
                        }
                    }
                }
            }
        }
    }

    /// The binding at the root of an expression, if it is a place
    /// (`x`, `x.f.g`). Used for mutable-borrow arguments.
    fn root_var(&self, id: ExprId) -> Option<SymbolId> {
        match &self.expr(id).kind {
            HirExprKind::Var(sym) => Some(*sym),
            HirExprKind::Field { base, .. } => self.root_var(*base),
            _ => None,
        }
    }
}

// ================= Phase B: enforcement =================

/// Whether a binding currently holds a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Init {
    Yes,
    Maybe,
    Never,
}

/// Whether a binding's value has moved away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Moved {
    No,
    Maybe(Span),
    Yes(Span),
}

/// Per-binding ownership state during the enforcement walk.
#[derive(Debug, Clone, Copy)]
struct BindingState {
    init: Init,
    moved: Moved,
}

struct Enforcer<'a> {
    scope: &'a ModuleScope,
    body: &'a HirBody,
    tables: &'a TypeTables,
    contracts: &'a ContractTable,
    interner: &'a Interner,
    diags: &'a mut Diagnostics,
    state: FxHashMap<SymbolId, BindingState>,
    /// Loans live inside the innermost call's extent.
    loans: Vec<Loan>,
}

impl Enforcer<'_> {
    fn expr(&self, id: ExprId) -> &HirExpr {
        self.body.expr(id)
    }

    fn name(&self, sym: SymbolId) -> &str {
        self.interner
            .resolve(self.body.symbol(self.scope, sym).name)
    }

    fn ty(&self, sym: SymbolId) -> Ty {
        self.tables
            .local_types
            .get(&sym)
            .copied()
            .unwrap_or(Ty::Poison)
    }

    /// Verifies a binding holds an unmoved value; records a move when
    /// the context consumes it.
    fn use_var(&mut self, sym: SymbolId, span: Span, ctx: Ctx) {
        let state = *self.state.entry(sym).or_insert(BindingState {
            init: Init::Never,
            moved: Moved::No,
        });
        match state.init {
            Init::Never => self.diags.push(
                Diagnostic::error(
                    Code::Uninitialized,
                    format!("`{}` is used before it is initialized", self.name(sym)),
                )
                .primary(span)
                .subject(self.name(sym).to_string()),
            ),
            Init::Maybe => self.diags.push(
                Diagnostic::error(
                    Code::Uninitialized,
                    format!("`{}` may not be initialized on all paths", self.name(sym)),
                )
                .primary(span)
                .subject(self.name(sym).to_string()),
            ),
            Init::Yes => {}
        }
        match state.moved {
            Moved::Yes(at) => self.diags.push(
                Diagnostic::error(
                    Code::UseAfterMove,
                    format!("`{}` is used after its value moved", self.name(sym)),
                )
                .primary(span)
                .label(at, "value moved here")
                .subject(self.name(sym).to_string()),
            ),
            Moved::Maybe(at) => self.diags.push(
                Diagnostic::error(
                    Code::UseAfterMove,
                    format!(
                        "`{}` may have been moved on a previous path",
                        self.name(sym)
                    ),
                )
                .primary(span)
                .label(at, "value moved here")
                .subject(self.name(sym).to_string()),
            ),
            Moved::No => {}
        }
        if ctx == Ctx::Move && !self.ty(sym).is_copy() {
            self.state
                .entry(sym)
                .and_modify(|s| s.moved = Moved::Yes(span));
        }
    }

    fn eval(&mut self, id: ExprId, ctx: Ctx) {
        // A move out of a place that is currently borrowed is a
        // dedicated error — checked before any state update.
        if ctx == Ctx::Move && !self.tables.ty_of(id).is_copy() {
            if let Some(p) = place_of(self.body, id) {
                self.check_move_loans(&p);
            }
        }
        match self.expr(id).kind.clone() {
            HirExprKind::Literal(_) | HirExprKind::Poison => {}
            HirExprKind::Var(sym) => {
                let span = self.expr(id).span;
                self.use_var(sym, span, ctx);
            }
            HirExprKind::Field { base, .. } => {
                if self.tables.ty_of(id).is_copy() {
                    self.eval(base, Ctx::Read);
                } else {
                    self.eval(base, ctx);
                }
            }
            HirExprKind::Call { def, args } => {
                let contract = self.contracts.get(&def).cloned().unwrap_or_default();
                // Loans created for this call's arguments live until
                // the call returns — the region is the call itself.
                let mark = self.loans.len();
                for (i, arg) in args.iter().enumerate() {
                    let b = contract.get(i).copied().unwrap_or(ParamBehavior::Move);
                    match b {
                        ParamBehavior::Borrow => {
                            self.eval(*arg, Ctx::Read);
                            if let Some(p) = place_of(self.body, *arg) {
                                self.add_loan(p, LoanKind::Shared, Region::Call(id));
                            }
                        }
                        ParamBehavior::Copy => self.eval(*arg, Ctx::Read),
                        ParamBehavior::BorrowMut => {
                            self.eval(*arg, Ctx::Read);
                            if let Some(p) = place_of(self.body, *arg) {
                                // A mutable borrow requires mutable
                                // authority over the place.
                                let sym = p.base;
                                if !self.body.symbol(self.scope, sym).mutable {
                                    let name = self.place_name(&p);
                                    self.diags.push(
                                        Diagnostic::error(
                                            Code::MutableBorrowOfImmutable,
                                            format!(
                                                "`{name}` is passed to a parameter that mutates it, but `{name}` is not declared `mut`"
                                            ),
                                        )
                                        .primary(p.span)
                                        .label(
                                            self.body.symbol(self.scope, sym).span,
                                            format!("`{name}` declared here without `mut`"),
                                        )
                                        .help(format!(
                                            "declare it with `mut`: `let mut {name} = ...`"
                                        ))
                                        .subject(name),
                                    );
                                }
                                self.add_loan(p, LoanKind::Mut, Region::Call(id));
                            }
                        }
                        _ => self.eval(*arg, Ctx::Move),
                    }
                }
                self.loans.truncate(mark);
            }
            HirExprKind::Binary { lhs, rhs, .. } => {
                self.eval(lhs, Ctx::Read);
                self.eval(rhs, Ctx::Read);
            }
            HirExprKind::Unary { expr, .. } => self.eval(expr, Ctx::Read),
            HirExprKind::If { cond, then, else_ } => {
                self.eval(cond, Ctx::Read);
                let saved = self.state.clone();
                self.eval(then, ctx);
                let then_state = std::mem::replace(&mut self.state, saved);
                if let Some(e) = else_ {
                    self.eval(e, ctx);
                }
                self.state = merge(self.state.clone(), then_state);
            }
            HirExprKind::Block { stmts, tail } => {
                for s in &stmts {
                    self.stmt(s);
                }
                if let Some(t) = tail {
                    self.eval(t, ctx);
                }
            }
            HirExprKind::StructLit { fields, .. } => {
                for (_, v) in &fields {
                    self.eval(*v, Ctx::Move);
                }
            }
        }
    }

    fn stmt(&mut self, stmt: &HirStmt) {
        match stmt {
            HirStmt::Let { symbol, init, .. } => match init {
                Some(e) => {
                    self.eval(*e, Ctx::Move);
                    self.state.insert(
                        *symbol,
                        BindingState {
                            init: Init::Yes,
                            moved: Moved::No,
                        },
                    );
                }
                None => {
                    self.state.insert(
                        *symbol,
                        BindingState {
                            init: Init::Never,
                            moved: Moved::No,
                        },
                    );
                }
            },
            HirStmt::Assign { target, value, .. } => {
                self.eval(*value, Ctx::Move);
                self.assign_place(target);
            }
            HirStmt::Expr { expr, .. } => self.eval(*expr, Ctx::Move),
            HirStmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.eval(*v, Ctx::Move);
                }
            }
        }
    }

    fn assign_place(&mut self, place: &HirPlace) {
        let sym = self.body.symbol(self.scope, place.base);
        let state = self
            .state
            .get(&place.base)
            .copied()
            .unwrap_or(BindingState {
                init: Init::Never,
                moved: Moved::No,
            });
        // Deferred first initialization (`let x; x = v;`) is allowed
        // without `mut` — it is a write-once initialization, not a
        // mutation. Every other write requires a `mut` binding.
        let first_init =
            place.fields.is_empty() && state.init == Init::Never && state.moved == Moved::No;
        if !sym.mutable && !first_init {
            let name = self.name(place.base).to_string();
            self.diags.push(
                Diagnostic::error(
                    Code::ImmutableAssignment,
                    format!("cannot assign to `{name}`: it is not declared `mut`"),
                )
                .primary(place.span)
                .label(sym.span, format!("`{name}` declared here without `mut`"))
                .help(format!("declare it with `mut`: `let mut {name} = ...`"))
                .subject(name),
            );
        }
        if place.fields.is_empty() {
            // A whole-binding assignment is legal in any state — it
            // initializes or reinitializes the binding outright.
            self.state.insert(
                place.base,
                BindingState {
                    init: Init::Yes,
                    moved: Moved::No,
                },
            );
        } else {
            // Writing `x.f` writes *into* the value — `x` must hold
            // one (moved or uninitialized values have no fields).
            self.use_var(place.base, place.span, Ctx::Read);
            self.state
                .entry(place.base)
                .and_modify(|s| s.init = Init::Yes);
        }
    }

    /// `x.f.g` rendered with the caller's binding name for messages.
    fn place_name(&self, p: &Place) -> String {
        format!("{}{}", self.name(p.base), p.describe(self.interner))
    }

    /// Pushes a loan, checking it against every overlapping live loan:
    /// shared+shared coexist; anything touching a mutable loan, and a
    /// mutable loan over an already-shared place, is a conflict.
    fn add_loan(&mut self, place: Place, kind: LoanKind, region: Region) {
        if let Some(live) = self.loans.iter().find(|l| {
            l.place.overlaps(&place) && (l.kind == LoanKind::Mut || kind == LoanKind::Mut)
        }) {
            let name = self.place_name(&place);
            let (new_k, live_k) = (kind_str(kind), kind_str(live.kind));
            self.diags.push(
                Diagnostic::error(
                    Code::BorrowConflict,
                    format!("cannot {new_k} `{name}`: it is already {live_k} borrowed"),
                )
                .primary(place.span)
                .label(
                    live.at,
                    format!(
                        "`{}` {} borrowed here",
                        self.place_name(&live.place),
                        live_k
                    ),
                )
                .subject(name),
            );
            return;
        }
        self.loans.push(Loan {
            at: place.span,
            place,
            kind,
            region,
        });
    }

    /// Moving a place while a live loan overlaps it is forbidden —
    /// the loan's region covers this program point.
    fn check_move_loans(&mut self, p: &Place) {
        if let Some(live) = self.loans.iter().find(|l| l.place.overlaps(p)) {
            let name = self.place_name(p);
            let at = live.at;
            let loaned = self.place_name(&live.place);
            let k = kind_str(live.kind);
            self.diags.push(
                Diagnostic::error(
                    Code::MoveWhileBorrowed,
                    format!("cannot move `{name}`: it is {k} borrowed"),
                )
                .primary(p.span)
                .label(at, format!("`{loaned}` {k} borrowed here"))
                .subject(name),
            );
        }
    }
}

fn kind_str(k: LoanKind) -> &'static str {
    match k {
        LoanKind::Shared => "shared",
        LoanKind::Mut => "mutably",
    }
}

/// Joins two binding-state maps after an `if` (one entry per branch).
fn merge(
    mut a: FxHashMap<SymbolId, BindingState>,
    b: FxHashMap<SymbolId, BindingState>,
) -> FxHashMap<SymbolId, BindingState> {
    for (sym, sb) in b {
        a.entry(sym)
            .and_modify(|sa| {
                sa.init = join_init(sa.init, sb.init);
                sa.moved = join_moved(sa.moved, sb.moved);
            })
            .or_insert(sb);
    }
    a
}

fn join_init(a: Init, b: Init) -> Init {
    match (a, b) {
        (Init::Yes, Init::Yes) => Init::Yes,
        (Init::Never, Init::Never) => Init::Never,
        _ => Init::Maybe,
    }
}

fn join_moved(a: Moved, b: Moved) -> Moved {
    match (a, b) {
        (Moved::No, Moved::No) => Moved::No,
        (Moved::Yes(s), Moved::Yes(_)) => Moved::Yes(s),
        (Moved::Maybe(s), Moved::Maybe(_)) => Moved::Maybe(s),
        (Moved::Maybe(s), _) | (_, Moved::Maybe(s)) => Moved::Maybe(s),
        (Moved::Yes(s), _) | (_, Moved::Yes(s)) => Moved::Maybe(s),
    }
}
