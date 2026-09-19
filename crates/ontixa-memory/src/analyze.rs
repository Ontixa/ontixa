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
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_hir::{HirExpr, HirExprKind, HirModule, HirPlace, HirStmt};
use ontixa_source::{DefId, ExprId, Interner, Span, SymbolId};
use ontixa_types::{Ty, TypeTables};
use rustc_hash::{FxHashMap, FxHashSet};

/// The product of ownership inference: the inferred contract of every
/// function parameter, by definition and parameter position.
#[derive(Debug, Default)]
pub struct OwnershipTables {
    /// `param_behaviors[f][i]` — inferred behavior of `f`'s i-th param.
    pub param_behaviors: FxHashMap<DefId, Vec<ParamBehavior>>,
}

impl OwnershipTables {
    /// The contract vector of a function (empty for `data` defs).
    pub fn contract(&self, def: DefId) -> &[ParamBehavior] {
        self.param_behaviors
            .get(&def)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }
}

type ContractTable = FxHashMap<DefId, Vec<ParamBehavior>>;

/// Infers ownership for the whole module and enforces
/// use-after-move / initialization / mutability rules.
pub fn infer_ownership(
    module: &HirModule,
    tables: &TypeTables,
    interner: &Interner,
    diags: &mut Diagnostics,
) -> OwnershipTables {
    // ---- Phase A: contract fixpoint ----
    //
    // Bottom values: `Copy` for copy types (fixed forever — a copy
    // type can never need a stronger contract), `Borrow` for
    // non-copy types (the weakest contract). Because callee contracts
    // only strengthen and `classify` is monotone, every contract
    // converges after at most a few strengthenings; the worklist is
    // therefore finite by construction.
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
        if module.body(def.id).is_none() {
            continue;
        }
        for callee in callees_of(module, module.body(def.id).unwrap().root) {
            callers.entry(callee).or_default().push(def.id);
        }
    }
    for v in callers.values_mut() {
        v.sort_unstable();
        v.dedup();
    }

    let mut work: Vec<DefId> = module
        .scope
        .defs
        .iter()
        .filter(|d| module.scope.fn_sig(d.id).is_some() && module.body(d.id).is_some())
        .map(|d| d.id)
        .collect();
    let mut queued: FxHashSet<DefId> = work.iter().copied().collect();
    let mut head = 0;
    while head < work.len() {
        let f = work[head];
        head += 1;
        queued.remove(&f);
        let sig = module.scope.fn_sig(f).unwrap();
        let body = module.body(f).unwrap();
        let facts = collect_facts(module, tables, &contracts, body, sig);
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

    // ---- Phase B: enforcement ----
    for def in module.scope.defs.iter() {
        let Some(sig) = module.scope.fn_sig(def.id) else {
            continue;
        };
        let Some(body) = module.body(def.id) else {
            continue;
        };
        let mut e = Enforcer {
            module,
            tables,
            contracts: &contracts,
            interner,
            diags,
            state: FxHashMap::default(),
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
    }
}

// ================= Phase A: usage facts =================

/// The set of `fn` defs called from the expression tree at `root`.
/// Used to build the callee→caller worklist edges.
fn callees_of(module: &HirModule, root: ExprId) -> FxHashSet<DefId> {
    let mut out = FxHashSet::default();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        match &module.expr(id).kind {
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
#[derive(Debug, Default, Clone)]
struct ParamFacts {
    read: bool,
    mutated: bool,
    moved: bool,
    escaped: bool,
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

/// Collects parameter usage facts for one body under `contracts`.
fn collect_facts(
    module: &HirModule,
    tables: &TypeTables,
    contracts: &ContractTable,
    body: &ontixa_hir::HirBody,
    sig: &ontixa_hir::FnSig,
) -> Vec<ParamFacts> {
    let mut c = FactCollector {
        module,
        tables,
        contracts,
        param_index: sig
            .params
            .iter()
            .enumerate()
            .map(|(i, p)| (p.symbol, i as u32))
            .collect(),
        facts: vec![ParamFacts::default(); sig.params.len()],
        carriers: FxHashMap::default(),
    };
    let root_carriers = c.eval(body.root, Ctx::Move);
    for i in root_carriers {
        c.facts[i as usize].escaped = true;
    }
    c.facts
}

struct FactCollector<'a> {
    module: &'a HirModule,
    tables: &'a TypeTables,
    contracts: &'a ContractTable,
    /// Param symbol → parameter position.
    param_index: FxHashMap<SymbolId, u32>,
    facts: Vec<ParamFacts>,
    /// Local → param indices whose value the local may carry.
    carriers: FxHashMap<SymbolId, Carriers>,
}

impl FactCollector<'_> {
    fn expr(&self, id: ExprId) -> &HirExpr {
        self.module.expr(id)
    }

    /// Flags a symbol as read/mutated/moved when it is a parameter.
    fn flag(&mut self, sym: SymbolId, ctx: Ctx) {
        if let Some(i) = self.param_index.get(&sym) {
            let f = &mut self.facts[*i as usize];
            match ctx {
                Ctx::Read => f.read = true,
                Ctx::Move => f.moved = true,
            }
        }
    }

    fn flag_mutated(&mut self, sym: SymbolId) {
        if let Some(i) = self.param_index.get(&sym) {
            self.facts[*i as usize].mutated = true;
        }
    }

    /// The param indices an expression's value may carry.
    fn eval(&mut self, id: ExprId, ctx: Ctx) -> Carriers {
        match self.expr(id).kind.clone() {
            HirExprKind::Literal(_) | HirExprKind::Poison => Carriers::default(),
            HirExprKind::Var(sym) => {
                self.flag(sym, ctx);
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
                let contract = self.contracts.get(&def).cloned().unwrap_or_default();
                for (i, arg) in args.iter().enumerate() {
                    let b = contract.get(i).copied().unwrap_or(ParamBehavior::Move);
                    match b {
                        ParamBehavior::Borrow | ParamBehavior::Copy => {
                            self.eval(*arg, Ctx::Read);
                        }
                        ParamBehavior::BorrowMut => {
                            self.eval(*arg, Ctx::Read);
                            if let Some(sym) = self.root_var(*arg) {
                                self.flag_mutated(sym);
                            }
                        }
                        ParamBehavior::Move | ParamBehavior::Unknown => {
                            self.eval(*arg, Ctx::Move);
                        }
                        ParamBehavior::Escape => {
                            let c = self.eval(*arg, Ctx::Move);
                            // The callee may return this argument's
                            // value — so it flows onward through us.
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
                    self.flag_mutated(target.base);
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
                        for i in c {
                            self.facts[i as usize].escaped = true;
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
    module: &'a HirModule,
    tables: &'a TypeTables,
    contracts: &'a ContractTable,
    interner: &'a Interner,
    diags: &'a mut Diagnostics,
    state: FxHashMap<SymbolId, BindingState>,
}

impl Enforcer<'_> {
    fn expr(&self, id: ExprId) -> &HirExpr {
        self.module.expr(id)
    }

    fn name(&self, sym: SymbolId) -> &str {
        self.interner
            .resolve(self.module.scope.symbols.get(sym).name)
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
                for (i, arg) in args.iter().enumerate() {
                    let b = contract.get(i).copied().unwrap_or(ParamBehavior::Move);
                    match b {
                        ParamBehavior::Borrow | ParamBehavior::Copy => {
                            self.eval(*arg, Ctx::Read);
                        }
                        ParamBehavior::BorrowMut => {
                            self.eval(*arg, Ctx::Read);
                            if let Some(sym) = self.root_var(*arg) {
                                let span = self.expr(*arg).span;
                                self.use_var(sym, span, Ctx::Read);
                                // A mutable borrow requires mutable
                                // authority over the place.
                                if !self.module.scope.symbols.get(sym).mutable {
                                    let name = self.name(sym).to_string();
                                    self.diags.push(
                                        Diagnostic::error(
                                            Code::MutableBorrowOfImmutable,
                                            format!(
                                                "`{name}` is passed to a parameter that mutates it, but `{name}` is not declared `mut`"
                                            ),
                                        )
                                        .primary(span)
                                        .label(
                                            self.module.scope.symbols.get(sym).span,
                                            format!("`{name}` declared here without `mut`"),
                                        )
                                        .help(format!(
                                            "declare it with `mut`: `let mut {name} = ...`"
                                        ))
                                        .subject(name),
                                    );
                                }
                            }
                        }
                        _ => self.eval(*arg, Ctx::Move),
                    }
                }
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
        let sym = self.module.scope.symbols.get(place.base);
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

    fn root_var(&self, id: ExprId) -> Option<SymbolId> {
        match &self.expr(id).kind {
            HirExprKind::Var(sym) => Some(*sym),
            HirExprKind::Field { base, .. } => self.root_var(*base),
            _ => None,
        }
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
