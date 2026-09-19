//! HIR → MIR lowering.
//!
//! Expressions compile to [`Operand`]s; temporaries hold intermediate
//! results; `if` expressions build a diamond (`Branch` → join block).
//! Statements after a `return` land in a fresh unreachable block —
//! they stay in the graph for tooling but are never executed.
//!
//! Borrowed arguments are preserved as places: when the callee's
//! contract is `borrow`/`borrow_mut`, an argument written as a
//! variable or field access stays `Operand::Place`, so the executor
//! shares the caller's storage instead of copying.

use crate::mir::{
    BasicBlock, BlockId, Const, Local, LocalDecl, MirBody, MirModule, MirStmt, Operand, Place,
    Rvalue, Terminator,
};
use ontixa_hir::{HirBody, HirExprKind, HirModule, HirPlace, HirStmt, ModuleScope};
use ontixa_memory::OwnershipTables;
use ontixa_source::{ExprId, SymbolId};
use ontixa_types::{ModuleTypes, Ty, TypeTables};
use rustc_hash::FxHashMap;

/// Lowers the whole module to MIR — convenience composition of
/// [`lower_fn`] for whole-module consumers.
pub fn lower_mir(
    module: &HirModule,
    types: &ModuleTypes,
    ownership: &OwnershipTables,
) -> MirModule {
    let mut fns: Vec<Option<MirBody>> = (0..module.scope.defs.len()).map(|_| None).collect();
    for def in &module.scope.defs {
        let (Some(body), Some(tables)) = (
            module.body(def.id),
            types.get(def.id.index()).and_then(|t| t.as_ref()),
        ) else {
            continue;
        };
        if module.scope.fn_sig(def.id).is_none() {
            continue;
        }
        fns[def.id.index()] = Some(lower_fn(&module.scope, body, tables, ownership));
    }
    MirModule { fns }
}

/// Lowers one HIR body to a [`MirBody`]. `ownership` carries the
/// inferred contracts of this function's params and of every callee
/// it may call.
pub fn lower_fn(
    scope: &ModuleScope,
    body: &HirBody,
    types: &TypeTables,
    ownership: &OwnershipTables,
) -> MirBody {
    let sig = scope.fn_sig(body.def).expect("a body exists only for fns");
    let mut l = FnLowerer {
        scope,
        body,
        types,
        ownership,
        locals: Vec::new(),
        local_of: FxHashMap::default(),
        blocks: Vec::new(),
        cur: BlockId(0),
        cur_closed: false,
    };
    // Params occupy locals 0..n in order.
    for p in &sig.params {
        let local = l.alloc_local(Some(p.symbol), Ty::from_ref(p.ty));
        l.local_of.insert(p.symbol, local);
    }
    l.cur = l.new_block(); // entry = block 0
    let result = l.eval(body.root);
    if l.cur_is_open() {
        l.close(Terminator::Return(result));
    }
    // Any block still holding the placeholder terminator falls off
    // the end — a unit return (only reachable in error paths).
    for b in l.blocks.iter_mut() {
        if matches!(b.term, Terminator::Goto(BlockId(u32::MAX))) {
            b.term = Terminator::Return(Operand::Const(Const::Unit));
        }
    }
    MirBody {
        def: body.def,
        params: sig.params.iter().map(|p| (p.symbol, p.ty.into())).collect(),
        param_behaviors: ownership.contract(body.def).to_vec(),
        ret: sig.ret.into(),
        locals: l.locals,
        blocks: l.blocks,
    }
}

struct FnLowerer<'a> {
    scope: &'a ModuleScope,
    body: &'a HirBody,
    types: &'a TypeTables,
    ownership: &'a OwnershipTables,
    locals: Vec<LocalDecl>,
    local_of: FxHashMap<SymbolId, Local>,
    blocks: Vec<BasicBlock>,
    cur: BlockId,
    /// True after `return`/`Branch` closed `cur` — the next emission
    /// lazily opens a fresh (unreachable) block.
    cur_closed: bool,
}

// The "pending" terminator is `Terminator::Goto(u32::MAX)`, written by
// `new_block` and always overwritten by `close`; `lower_mir` sweeps any
// survivors (unreachable paths) into `Return(unit)`.

impl FnLowerer<'_> {
    // ---------- infrastructure ----------

    fn alloc_local(&mut self, sym: Option<SymbolId>, ty: Ty) -> Local {
        let l = Local(self.locals.len() as u32);
        self.locals.push(LocalDecl { sym, ty });
        l
    }

    fn temp(&mut self, ty: Ty) -> Place {
        Place::local(self.alloc_local(None, ty))
    }

    fn local(&mut self, sym: SymbolId, ty: Ty) -> Local {
        match self.local_of.get(&sym) {
            Some(l) => *l,
            None => {
                let l = self.alloc_local(Some(sym), ty);
                self.local_of.insert(sym, l);
                l
            }
        }
    }

    fn new_block(&mut self) -> BlockId {
        let id = BlockId(self.blocks.len() as u32);
        self.blocks.push(BasicBlock {
            id,
            stmts: Vec::new(),
            term: Terminator::Goto(BlockId(u32::MAX)), // placeholder
        });
        id
    }

    fn ensure_open(&mut self) {
        if self.cur_closed {
            self.cur = self.new_block();
            self.cur_closed = false;
        }
    }

    fn emit(&mut self, stmt: MirStmt) {
        self.ensure_open();
        self.blocks[self.cur.0 as usize].stmts.push(stmt);
    }

    fn close(&mut self, term: Terminator) {
        self.ensure_open();
        self.blocks[self.cur.0 as usize].term = term;
        self.cur_closed = true;
    }

    /// Whether the current block still has its placeholder terminator.
    fn cur_is_open(&self) -> bool {
        matches!(
            self.blocks[self.cur.0 as usize].term,
            Terminator::Goto(BlockId(u32::MAX))
        )
    }

    /// The type the type-checker assigned to an expression.
    fn ty_of(&self, id: ExprId) -> Ty {
        self.types.ty_of(id)
    }

    // ---------- places ----------

    /// The storage place an expression refers to. Only `Var` and
    /// `Field` expressions are places; anything else is evaluated into
    /// a fresh temporary.
    fn eval_place(&mut self, id: ExprId) -> Place {
        match self.body.expr(id).kind.clone() {
            HirExprKind::Var(sym) => {
                let ty = self.ty_of(id);
                Place::local(self.local(sym, ty))
            }
            HirExprKind::Field { base, field, .. } => {
                let mut p = self.eval_place(base);
                if let Some(i) = field {
                    p.proj.push(i);
                }
                p
            }
            _ => {
                let op = self.eval(id);
                self.ensure_place(op, self.ty_of(id))
            }
        }
    }

    fn ensure_place(&mut self, op: Operand, ty: Ty) -> Place {
        match op {
            Operand::Place(p) => p,
            Operand::Const(_) => {
                let t = self.temp(ty);
                self.emit(MirStmt::Assign {
                    dst: t.clone(),
                    val: Rvalue::Use(op),
                });
                t
            }
        }
    }

    /// A `HirPlace` (assignment target) as a MIR place.
    fn hir_place(&mut self, place: &HirPlace) -> Place {
        let base_ty = self
            .types
            .local_types
            .get(&place.base)
            .copied()
            .unwrap_or(Ty::Poison);
        let l = self.local(place.base, base_ty);
        let mut p = Place::local(l);
        let mut ty = base_ty;
        for f in &place.fields {
            if let Ty::Struct(def) = ty {
                if let Some(shape) = self.scope.data_shape(def) {
                    if let Some(i) = shape.field_index.get(&f.id) {
                        p.proj.push(*i);
                        ty = shape.fields[*i as usize].ty.into();
                        continue;
                    }
                }
            }
            // Unresolvable projection — the checker already diagnosed.
            ty = Ty::Poison;
        }
        p
    }

    // ---------- statements ----------

    fn stmt(&mut self, stmt: &HirStmt) {
        match stmt {
            HirStmt::Let { symbol, init, .. } => {
                let ty = self
                    .types
                    .local_types
                    .get(symbol)
                    .copied()
                    .unwrap_or(Ty::Poison);
                let l = self.local(*symbol, ty);
                if let Some(i) = init {
                    let op = self.eval(*i);
                    self.emit(MirStmt::Assign {
                        dst: Place::local(l),
                        val: Rvalue::Use(op),
                    });
                }
            }
            HirStmt::Assign { target, value, .. } => {
                let op = self.eval(*value);
                let dst = self.hir_place(target);
                self.emit(MirStmt::Assign {
                    dst,
                    val: Rvalue::Use(op),
                });
            }
            HirStmt::Expr { expr, .. } => {
                self.eval(*expr);
            }
            HirStmt::Return { value, .. } => {
                let op = match value {
                    Some(v) => self.eval(*v),
                    None => Operand::Const(Const::Unit),
                };
                self.close(Terminator::Return(op));
                // Anything written next lands in a lazily-created
                // unreachable block — kept for tooling, never executed.
            }
        }
    }

    // ---------- expressions ----------

    /// Evaluates an expression, appending any needed statements to the
    /// current block, and returns the operand holding its value.
    fn eval(&mut self, id: ExprId) -> Operand {
        match self.body.expr(id).kind.clone() {
            HirExprKind::Literal(lit) => Operand::Const(match lit {
                ontixa_hir::Literal::Int(v) => Const::Int(v),
                ontixa_hir::Literal::Float(v) => Const::Float(v),
                ontixa_hir::Literal::Str(s) => Const::Str(s),
                ontixa_hir::Literal::Bool(b) => Const::Bool(b),
            }),
            HirExprKind::Var(_) | HirExprKind::Field { .. } => Operand::Place(self.eval_place(id)),
            HirExprKind::Binary { op, lhs, rhs } => {
                let l = self.eval(lhs);
                let r = self.eval(rhs);
                let t = self.temp(self.ty_of(id));
                self.emit(MirStmt::Assign {
                    dst: t.clone(),
                    val: Rvalue::Binary { op, lhs: l, rhs: r },
                });
                Operand::Place(t)
            }
            HirExprKind::Unary { op, expr } => {
                let e = self.eval(expr);
                let t = self.temp(self.ty_of(id));
                self.emit(MirStmt::Assign {
                    dst: t.clone(),
                    val: Rvalue::Unary { op, operand: e },
                });
                Operand::Place(t)
            }
            HirExprKind::Call { def, args } => {
                let ops: Vec<Operand> = args.iter().map(|a| self.eval(*a)).collect();
                let t = self.temp(self.ty_of(id));
                self.emit(MirStmt::Assign {
                    dst: t.clone(),
                    val: Rvalue::Call {
                        def,
                        args: ops,
                        contract: self.ownership.contract(def).to_vec(),
                    },
                });
                Operand::Place(t)
            }
            HirExprKind::If { cond, then, else_ } => {
                let c = self.eval(cond);
                let ty = self.ty_of(id);
                let result = if ty == Ty::Unit {
                    None
                } else {
                    Some(self.temp(ty))
                };
                let then_bb = self.new_block();
                let else_bb = else_.map(|_| self.new_block());
                let join_bb = self.new_block();
                self.close(Terminator::Branch {
                    cond: c,
                    then: then_bb,
                    else_: else_bb.unwrap_or(join_bb),
                });

                self.cur = then_bb;
                self.cur_closed = false;
                let t = self.eval(then);
                if let Some(r) = &result {
                    self.emit(MirStmt::Assign {
                        dst: r.clone(),
                        val: Rvalue::Use(t),
                    });
                }
                if !self.cur_closed {
                    self.close(Terminator::Goto(join_bb));
                }

                if let (Some(e), Some(ebb)) = (else_, else_bb) {
                    self.cur = ebb;
                    self.cur_closed = false;
                    let ev = self.eval(e);
                    if let Some(r) = &result {
                        self.emit(MirStmt::Assign {
                            dst: r.clone(),
                            val: Rvalue::Use(ev),
                        });
                    }
                    if !self.cur_closed {
                        self.close(Terminator::Goto(join_bb));
                    }
                }

                self.cur = join_bb;
                self.cur_closed = false;
                result.map_or(Operand::Const(Const::Unit), Operand::Place)
            }
            HirExprKind::Block { stmts, tail } => {
                for s in &stmts {
                    self.stmt(s);
                }
                match tail {
                    Some(t) => self.eval(t),
                    None => Operand::Const(Const::Unit),
                }
            }
            HirExprKind::StructLit { def, fields } => {
                let slots = self
                    .types
                    .struct_lit_slots
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();
                let ops: Vec<(u32, Operand)> = fields
                    .iter()
                    .enumerate()
                    .map(|(i, (_, v))| {
                        let slot = slots.get(i).copied().unwrap_or(i as u32);
                        (slot, self.eval(*v))
                    })
                    .collect();
                let t = self.temp(self.ty_of(id));
                self.emit(MirStmt::Assign {
                    dst: t.clone(),
                    val: Rvalue::StructLit { def, fields: ops },
                });
                Operand::Place(t)
            }
            HirExprKind::Poison => Operand::Const(Const::Unit),
        }
    }
}
