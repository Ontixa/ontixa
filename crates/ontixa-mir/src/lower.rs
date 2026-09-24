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
use ontixa_hir::{HirArm, HirBody, HirExprKind, HirModule, HirPat, HirPlace, HirStmt, ModuleScope};
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
        let local = l.alloc_local(Some(p.symbol), Ty::from_ref(p.ty), p.mutable);
        l.local_of.insert(p.symbol, local);
    }
    l.cur = l.new_block(); // entry = block 0
    let result = l.eval(body.root);
    if l.cur_is_open() {
        l.close(Terminator::Return { value: result });
    }
    // Any block still holding the placeholder terminator falls off
    // the end — a unit return (only reachable in error paths).
    for b in l.blocks.iter_mut() {
        if matches!(
            b.term,
            Terminator::Goto {
                target: BlockId(u32::MAX)
            }
        ) {
            b.term = Terminator::Return {
                value: Operand::Const(Const::Unit),
            };
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

// The "pending" terminator is `Goto { target: u32::MAX }`, written by
// `new_block` and always overwritten by `close`; `lower_mir` sweeps any
// survivors (unreachable paths) into `Return(unit)`.

impl FnLowerer<'_> {
    // ---------- infrastructure ----------

    fn alloc_local(&mut self, sym: Option<SymbolId>, ty: Ty, mutable: bool) -> Local {
        let l = Local(self.locals.len() as u32);
        self.locals.push(LocalDecl { sym, ty, mutable });
        l
    }

    fn temp(&mut self, ty: Ty) -> Place {
        // Compiler temporaries are always internally writable.
        Place::local(self.alloc_local(None, ty, true))
    }

    fn local(&mut self, sym: SymbolId, ty: Ty) -> Local {
        match self.local_of.get(&sym) {
            Some(l) => *l,
            None => {
                let mutable = self.body.symbol(self.scope, sym).mutable;
                let l = self.alloc_local(Some(sym), ty, mutable);
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
            term: Terminator::Goto {
                target: BlockId(u32::MAX),
            }, // placeholder
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
            Terminator::Goto {
                target: BlockId(u32::MAX)
            }
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
                self.close(Terminator::Return { value: op });
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
            HirExprKind::Len { base } => {
                let b = self.eval(base);
                let t = self.temp(self.ty_of(id));
                self.emit(MirStmt::Assign {
                    dst: t.clone(),
                    val: Rvalue::Len { base: b },
                });
                Operand::Place(t)
            }
            HirExprKind::ArrayLit { elems } => {
                let ops: Vec<Operand> = elems.iter().map(|e| self.eval(*e)).collect();
                let t = self.temp(self.ty_of(id));
                self.emit(MirStmt::Assign {
                    dst: t.clone(),
                    val: Rvalue::ArrayLit { elems: ops },
                });
                Operand::Place(t)
            }
            HirExprKind::Range { .. } => {
                // The checker rejects ranges outside `for`; defensive.
                Operand::Const(Const::Unit)
            }
            HirExprKind::For { var, iter, body } => {
                self.for_loop(var, iter, body);
                Operand::Const(Const::Unit)
            }
            HirExprKind::Index { base, index } => {
                let b = self.eval(base);
                let i = self.eval(index);
                let t = self.temp(self.ty_of(id));
                self.emit(MirStmt::Assign {
                    dst: t.clone(),
                    val: Rvalue::Index { base: b, index: i },
                });
                Operand::Place(t)
            }
            HirExprKind::Slice { base, lo, hi } => {
                let b = self.eval(base);
                let lo = lo.map(|l| self.eval(l));
                let hi = hi.map(|h| self.eval(h));
                let t = self.temp(self.ty_of(id));
                self.emit(MirStmt::Assign {
                    dst: t.clone(),
                    val: Rvalue::Slice { base: b, lo, hi },
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
                    self.close(Terminator::Goto { target: join_bb });
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
                        self.close(Terminator::Goto { target: join_bb });
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
            HirExprKind::VariantLit { def, variant, args } => {
                let ops: Vec<Operand> = args.iter().map(|a| self.eval(*a)).collect();
                let t = self.temp(self.ty_of(id));
                self.emit(MirStmt::Assign {
                    dst: t.clone(),
                    val: Rvalue::VariantLit {
                        def,
                        variant,
                        args: ops,
                    },
                });
                Operand::Place(t)
            }
            HirExprKind::Match { scrutinee, arms } => self.match_expr(id, scrutinee, &arms),
            HirExprKind::Poison => Operand::Const(Const::Unit),
        }
    }

    /// `match e { pat => v, .. }` compiles to a discriminant switch:
    ///
    /// ```text
    ///   <scrutinee evaluated once into an operand>
    ///   Match(scrutinee, [(disc, arm_bb), ..], default_bb?)
    ///   arm_bb: [binds = VariantPayload(scrutinee, i)]; <body>;
    ///           result = v; Goto(join)
    ///   join:   (result temp reads as the match value)
    /// ```
    ///
    /// `x`/`_` arms land on `default`; a `Poison` pattern's block is
    /// emitted but unreachable (a diagnostic already exists). Arms
    /// that end in `return` never reach the join — like `if` branch
    /// tails.
    fn match_expr(&mut self, id: ExprId, scrutinee: ExprId, arms: &[HirArm]) -> Operand {
        let scrut_op = self.eval(scrutinee);
        let scrut_ty = self.ty_of(scrutinee);
        let ty = self.ty_of(id);
        let result = if ty == Ty::Unit {
            None
        } else {
            Some(self.temp(ty))
        };
        let join = self.new_block();
        let mut arms_map: Vec<(u32, BlockId)> = Vec::new();
        let mut default: Option<BlockId> = None;
        let mut arm_bbs = Vec::with_capacity(arms.len());
        for arm in arms {
            let bb = self.new_block();
            arm_bbs.push(bb);
            match &arm.pat {
                HirPat::Variant { variant, .. } => arms_map.push((*variant, bb)),
                HirPat::Bind { .. } if default.is_none() => default = Some(bb),
                _ => {}
            }
        }
        self.close(Terminator::Match {
            scrutinee: scrut_op.clone(),
            arms: arms_map,
            default,
        });
        for (arm, bb) in arms.iter().zip(arm_bbs) {
            self.cur = bb;
            self.cur_closed = false;
            match &arm.pat {
                HirPat::Variant { binds, .. } => {
                    for (i, b) in binds.iter().enumerate() {
                        if let Some(sym) = b {
                            let bty = self
                                .types
                                .local_types
                                .get(sym)
                                .copied()
                                .unwrap_or(Ty::Poison);
                            let l = self.local(*sym, bty);
                            self.emit(MirStmt::Assign {
                                dst: Place::local(l),
                                val: Rvalue::VariantPayload {
                                    base: scrut_op.clone(),
                                    index: i as u32,
                                },
                            });
                        }
                    }
                }
                HirPat::Bind { sym: Some(sym), .. } => {
                    // `x => ..` binds the whole scrutinee value —
                    // an owned copy, since matching only borrows.
                    let l = self.local(*sym, scrut_ty);
                    self.emit(MirStmt::Assign {
                        dst: Place::local(l),
                        val: Rvalue::Duplicate(scrut_op.clone()),
                    });
                }
                _ => {}
            }
            let v = self.eval(arm.body);
            if let Some(r) = &result {
                self.emit(MirStmt::Assign {
                    dst: r.clone(),
                    val: Rvalue::Use(v),
                });
            }
            if !self.cur_closed {
                self.close(Terminator::Goto { target: join });
            }
        }
        self.cur = join;
        self.cur_closed = false;
        result.map_or(Operand::Const(Const::Unit), Operand::Place)
    }

    /// `for x in iter { body }` desugars to a counted loop:
    ///
    /// ```text
    ///   <iter evaluated once into temps>
    ///   var = first
    ///   head: cond = var < end (or idx < len)
    ///         Branch(cond -> body_bb, exit_bb)
    ///   body_bb: [x = arr[idx]]; <body>; var/idx += 1; Goto(head)
    ///   exit_bb: (unit)
    /// ```
    ///
    /// Both iterable forms fix their extent before the first
    /// iteration: range bounds are snapshotted into temps and array
    /// iteration walks a copy of the array, so writes inside the body
    /// cannot invalidate the loop.
    fn for_loop(&mut self, var: SymbolId, iter: ExprId, body: ExprId) {
        let var_ty = self
            .types
            .local_types
            .get(&var)
            .copied()
            .unwrap_or(Ty::Poison);
        // The loop machinery rewrites the var's slot each iteration —
        // internally writable regardless of the source `mut`.
        let var_local = self.alloc_local(Some(var), var_ty, true);
        self.local_of.insert(var, var_local);
        let var_place = Place::local(var_local);

        // Iteration state: (counter place, limit operand, per-iteration
        // var binding for the array form).
        let (counter, limit, elem_read) = match self.body.expr(iter).kind.clone() {
            HirExprKind::Range { lo, hi } => {
                let lo_op = match lo {
                    Some(l) => self.eval(l),
                    None => Operand::Const(Const::Int(0)),
                };
                let lo_t = self.temp(var_ty);
                self.emit(MirStmt::Assign {
                    dst: lo_t.clone(),
                    val: Rvalue::Use(lo_op),
                });
                let hi_op = hi
                    .map(|h| self.eval(h))
                    .unwrap_or(Operand::Const(Const::Int(0)));
                let hi_t = self.temp(var_ty);
                self.emit(MirStmt::Assign {
                    dst: hi_t.clone(),
                    val: Rvalue::Use(hi_op),
                });
                // The var *is* the counter for a range loop.
                self.emit(MirStmt::Assign {
                    dst: var_place.clone(),
                    val: Rvalue::Use(Operand::Place(lo_t)),
                });
                (var_place.clone(), Operand::Place(hi_t), None)
            }
            _ => {
                // Array iteration: snapshot the array, then count
                // `idx` against its (fixed) length.
                let arr_op = self.eval(iter);
                let arr = self.temp(self.ty_of(iter));
                self.emit(MirStmt::Assign {
                    dst: arr.clone(),
                    val: Rvalue::Use(arr_op),
                });
                let len = self.temp(Ty::I32);
                self.emit(MirStmt::Assign {
                    dst: len.clone(),
                    val: Rvalue::Len {
                        base: Operand::Place(arr.clone()),
                    },
                });
                let idx = self.temp(Ty::I32);
                self.emit(MirStmt::Assign {
                    dst: idx.clone(),
                    val: Rvalue::Use(Operand::Const(Const::Int(0))),
                });
                (idx, Operand::Place(len), Some(arr))
            }
        };

        let head_bb = self.new_block();
        let body_bb = self.new_block();
        let exit_bb = self.new_block();
        self.close(Terminator::Goto { target: head_bb });

        // Head: `counter < limit`.
        self.cur = head_bb;
        self.cur_closed = false;
        let cond = self.temp(Ty::Bool);
        self.emit(MirStmt::Assign {
            dst: cond.clone(),
            val: Rvalue::Binary {
                op: ontixa_hir::BinOp::Lt,
                lhs: Operand::Place(counter.clone()),
                rhs: limit,
            },
        });
        self.close(Terminator::Branch {
            cond: Operand::Place(cond),
            then: body_bb,
            else_: exit_bb,
        });

        // Body: bind the element, run the block, bump the counter.
        self.cur = body_bb;
        self.cur_closed = false;
        if let Some(arr) = elem_read {
            self.emit(MirStmt::Assign {
                dst: var_place.clone(),
                val: Rvalue::Index {
                    base: Operand::Place(arr),
                    index: Operand::Place(counter.clone()),
                },
            });
        }
        self.eval(body);
        if self.cur_is_open() {
            // `counter` is a bare local — `var` for ranges, the `idx`
            // temp for arrays.
            self.emit(MirStmt::Assign {
                dst: counter.clone(),
                val: Rvalue::Binary {
                    op: ontixa_hir::BinOp::Add,
                    lhs: Operand::Place(counter),
                    rhs: Operand::Const(Const::Int(1)),
                },
            });
            self.close(Terminator::Goto { target: head_bb });
        }

        self.cur = exit_bb;
        self.cur_closed = false;
    }
}
