//! The milestone-1 type checker.
//!
//! Walks every function body bottom-up, assigning a [`Ty`] to each
//! [`ExprId`]. It also completes the semantic work name resolution
//! deferred: field accesses get their declaration index, struct
//! literals get their field-order map, and `let` bindings get their
//! types. The result — [`TypeTables`] — is what ownership analysis,
//! the semantic graph, and MIR all consume.
//!
//! Design rules:
//!
//! - **Poison unifies with everything.** Once a diagnostic exists for
//!   a node, downstream checks on that node stay quiet. One error
//!   must never cascade into a wall of secondary errors.
//! - **Expected types flow inward.** `let x: i64 = 5` and
//!   `return 5` pass their required type down so integer literals
//!   adopt the context's integer width instead of always defaulting
//!   to `i32`.
//! - **The checker is total.** Every expression gets a type, even if
//!   that type is `Poison`. Later passes never see `Option<Ty>`.

use crate::ty::Ty;
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_hir::{
    BinOp, HirBody, HirExpr, HirExprKind, HirModule, HirPlace, HirStmt, LitValue, ModuleScope,
    Name, UnOp,
};
use ontixa_source::{DefId, ExprId, InternId, Interner, Span, SymbolId};
use rustc_hash::FxHashMap;

/// The product of type checking **one body**: one type per
/// expression in `body.exprs`, the type of every parameter and `let`
/// binding, and resolved field layouts for struct literals.
///
/// Tables are per-body (like the arenas they index) so a body edit
/// can never invalidate another body's types.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct TypeTables {
    /// `expr_types[id]` is the checked type of `body.exprs[id]`.
    pub expr_types: Vec<Ty>,
    /// Type of each parameter and `let` binding symbol (body-local
    /// `SymbolId`s for params and locals).
    pub local_types: FxHashMap<SymbolId, Ty>,
    /// For each `StructLit` expression, the declared field index of
    /// each written field (same order as the written fields).
    pub struct_lit_slots: FxHashMap<ExprId, Vec<u32>>,
}

impl TypeTables {
    /// The type of an expression node in the owning body.
    pub fn ty_of(&self, id: ExprId) -> Ty {
        self.expr_types[id.index()]
    }
}

/// Per-def type tables for a whole module — `tables[def]` is `Some`
/// for functions, `None` for `data` defs.
pub type ModuleTypes = Vec<Option<TypeTables>>;

/// Type-checks every body in a resolved [`HirModule`], filling
/// `Field.field` indices in place and appending diagnostics.
/// Convenience composition of [`check_body`] for whole-module
/// consumers (tests, one-shot compilation).
pub fn check_module(
    module: &mut HirModule,
    interner: &Interner,
    diags: &mut Diagnostics,
) -> ModuleTypes {
    module
        .bodies
        .iter_mut()
        .map(|b| {
            b.as_mut()
                .map(|b| check_body(&module.scope, b, interner, diags))
        })
        .collect()
}

/// Type-checks one function body, filling its `Field.field` indices
/// in place and appending diagnostics. Diagnostics are emitted in
/// the body's item-local coordinate space and tagged
/// `origin = body.def` — callers rebase them (see
/// [`Diagnostics::rebase_tagged`]).
pub fn check_body(
    scope: &ModuleScope,
    body: &mut HirBody,
    interner: &Interner,
    diags: &mut Diagnostics,
) -> TypeTables {
    let mark = diags.len();
    let mut exprs = std::mem::take(&mut body.exprs);
    let mut tables = TypeTables {
        expr_types: vec![Ty::Poison; exprs.len()],
        local_types: FxHashMap::default(),
        struct_lit_slots: FxHashMap::default(),
    };
    {
        let mut ck = Checker {
            scope,
            body,
            exprs: &mut exprs,
            interner,
            diags,
            tables: &mut tables,
            locals: FxHashMap::default(),
            ret: Ty::Unit,
        };
        ck.run_body(body);
        ck.diags.tag_origin_from(mark, body.def);
    }
    body.exprs = exprs;
    tables
}

struct Checker<'a> {
    scope: &'a ModuleScope,
    /// The body being checked — `exprs` holds its taken-out arena;
    /// `body` itself is needed for `local_symbols` and `root`/`def`.
    body: &'a HirBody,
    exprs: &'a mut Vec<HirExpr>,
    interner: &'a Interner,
    diags: &'a mut Diagnostics,
    tables: &'a mut TypeTables,
    /// Types of params and `let` bindings in the current body.
    locals: FxHashMap<SymbolId, Ty>,
    /// Return type of the function being checked.
    ret: Ty,
}

impl Checker<'_> {
    // ---------- infrastructure ----------

    fn node(&self, id: ExprId) -> &HirExpr {
        &self.exprs[id.index()]
    }

    fn set_ty(&mut self, id: ExprId, ty: Ty) -> Ty {
        self.tables.expr_types[id.index()] = ty;
        ty
    }

    /// Renders a type for diagnostics.
    fn show(&self, ty: Ty) -> String {
        match ty {
            Ty::Bool => "bool".into(),
            Ty::I32 => "i32".into(),
            Ty::I64 => "i64".into(),
            Ty::U32 => "u32".into(),
            Ty::U64 => "u64".into(),
            Ty::F32 => "f32".into(),
            Ty::F64 => "f64".into(),
            Ty::Str => "str".into(),
            Ty::Unit => "unit".into(),
            Ty::Struct(d) => self
                .interner
                .resolve(self.scope.symbols.get(self.scope.def(d).name).name)
                .to_string(),
            Ty::Array(e) => format!("[{}]", self.show(e.ty())),
            Ty::Poison => "<error>".into(),
        }
    }

    /// Reports `expected` vs `actual` unless either side is `Poison`.
    /// Returns the type the caller should record for the node.
    fn unify(&mut self, actual: Ty, expected: Ty, span: Span) -> Ty {
        if expected.compatible(actual) {
            if expected == Ty::Poison {
                actual
            } else {
                expected
            }
        } else {
            self.diags.push(
                Diagnostic::error(
                    Code::TypeMismatch,
                    format!(
                        "expected {}, found {}",
                        self.show(expected),
                        self.show(actual)
                    ),
                )
                .primary(span),
            );
            Ty::Poison
        }
    }

    // ---------- bodies ----------

    fn run_body(&mut self, body: &HirBody) {
        self.locals.clear();
        let sig = self
            .scope
            .fn_sig(body.def)
            .expect("a body exists only for fn defs");
        self.ret = Ty::from_ref(sig.ret);
        for p in &sig.params {
            let ty = Ty::from_ref(p.ty);
            self.locals.insert(p.symbol, ty);
            self.tables.local_types.insert(p.symbol, ty);
        }

        self.expr_ty(body.root, Some(self.ret));

        // Return-surface checks.
        if let HirExprKind::Block { tail, .. } = &self.node(body.root).kind {
            match tail {
                Some(t) => {
                    let tail_ty = self.tables.ty_of(*t);
                    let tspan = self.node(*t).span;
                    self.unify(tail_ty, self.ret, tspan);
                }
                None if self.ret != Ty::Unit
                    && self.ret != Ty::Poison
                    && !self.diverges(body.root) =>
                {
                    let name_sym = self.scope.def(body.def).name;
                    let name_span = self.scope.symbols.get(name_sym).span;
                    let name = self.interner.resolve(self.scope.symbols.get(name_sym).name);
                    self.diags.push(
                        Diagnostic::error(
                            Code::MissingReturn,
                            format!("function `{name}` can complete without returning a value"),
                        )
                        .primary(name_span)
                        .subject(name.to_string()),
                    );
                }
                None => {}
            }
        }
    }

    /// Whether control cannot reach the end of `id` — i.e. the
    /// expression yields a value (a block tail) or every path diverges
    /// via `return`. Used for the missing-return check.
    fn diverges(&self, id: ExprId) -> bool {
        match &self.node(id).kind {
            HirExprKind::Block { stmts, tail } => {
                if tail.is_some() {
                    return true;
                }
                match stmts.last() {
                    Some(HirStmt::Return { .. }) => true,
                    Some(HirStmt::Expr { expr, .. }) => self.diverges(*expr),
                    _ => false,
                }
            }
            HirExprKind::If {
                then,
                else_: Some(e),
                ..
            } => self.diverges(*then) && self.diverges(*e),
            _ => false,
        }
    }

    // ---------- statements ----------

    fn stmt(&mut self, stmt: &HirStmt) {
        match stmt {
            HirStmt::Let {
                symbol,
                ty,
                init,
                span,
            } => {
                let annotated = ty.map(Ty::from_ref);
                let init_ty = init.map(|e| self.expr_ty(e, annotated));
                let binding_ty = match (annotated, init_ty) {
                    (Some(a), Some(i)) => {
                        let ispan = self.node(init.unwrap()).span;
                        self.unify(i, a, ispan)
                    }
                    (Some(a), None) => a,
                    (None, Some(i)) => i,
                    (None, None) => {
                        let name = self.body.symbol(self.scope, *symbol).name;
                        self.diags.push(
                            Diagnostic::error(
                                Code::CannotInfer,
                                format!(
                                    "cannot infer the type of `{}`; add an annotation or initializer",
                                    self.interner.resolve(name)
                                ),
                            )
                            .primary(*span),
                        );
                        Ty::Poison
                    }
                };
                self.locals.insert(*symbol, binding_ty);
                self.tables.local_types.insert(*symbol, binding_ty);
            }
            HirStmt::Assign { target, value, .. } => {
                let place_ty = self.place_ty(target);
                let v = self.expr_ty(*value, Some(place_ty));
                let vspan = self.node(*value).span;
                self.unify(v, place_ty, vspan);
            }
            HirStmt::Expr { expr, .. } => {
                self.expr_ty(*expr, None);
            }
            HirStmt::Return { value, span } => {
                let ret = self.ret;
                match value {
                    Some(v) => {
                        let t = self.expr_ty(*v, Some(ret));
                        let vspan = self.node(*v).span;
                        self.unify(t, ret, vspan);
                    }
                    None => {
                        if self.ret != Ty::Unit && self.ret != Ty::Poison {
                            self.diags.push(
                                Diagnostic::error(
                                    Code::TypeMismatch,
                                    format!("expected {}, found unit", self.show(ret)),
                                )
                                .primary(*span),
                            );
                        }
                    }
                }
            }
        }
    }

    /// The type of an assignment place: base symbol type walked through
    /// its field projections.
    fn place_ty(&mut self, place: &HirPlace) -> Ty {
        let mut ty = match self.locals.get(&place.base) {
            Some(t) => *t,
            None => {
                let sym = self.body.symbol(self.scope, place.base);
                let name = self.interner.resolve(sym.name).to_string();
                self.diags.push(
                    Diagnostic::error(
                        Code::Internal,
                        format!("untyped binding `{name}` survived type checking"),
                    )
                    .primary(place.span),
                );
                Ty::Poison
            }
        };
        for field in &place.fields {
            ty = match self.field_ty(ty, field.id, field.span) {
                Some(t) => t,
                None => return Ty::Poison,
            };
        }
        ty
    }

    /// Resolves `name` as a field of `ty` (which must be a struct).
    /// Returns the field's type, or `None` after emitting a diagnostic.
    fn field_ty(&mut self, ty: Ty, name: InternId, span: Span) -> Option<Ty> {
        match ty {
            Ty::Poison => None,
            // `s.len`/`a.len` read fine as expressions (the Field arm
            // intercepts them), but they are not places — `.len = n`
            // must fail here rather than as a generic field error.
            Ty::Str | Ty::Array(_) if self.interner.resolve(name) == "len" => {
                self.diags.push(
                    Diagnostic::error(
                        Code::UnsupportedOperation,
                        "`len` is a read-only property; cannot assign to it",
                    )
                    .primary(span),
                );
                None
            }
            Ty::Struct(def) => {
                let shape = self.scope.data_shape(def).expect("Struct Ty of a data def");
                match shape.field_index.get(&name) {
                    Some(idx) => Some(shape.fields[*idx as usize].ty.into()),
                    None => {
                        let fname = self.interner.resolve(name).to_string();
                        self.diags.push(
                            Diagnostic::error(
                                Code::UnknownField,
                                format!("{} has no field `{fname}`", self.show(ty)),
                            )
                            .primary(span)
                            .subject(fname),
                        );
                        None
                    }
                }
            }
            other => {
                self.diags.push(
                    Diagnostic::error(
                        Code::NotAStruct,
                        format!("cannot access fields of {}", self.show(other)),
                    )
                    .primary(span),
                );
                None
            }
        }
    }

    // ---------- expressions ----------

    /// Checks an expression and records its type. `expected` is the
    /// context's required type, used for literal adoption. The node
    /// kind is cloned so the walk can mutate `self.exprs` in place
    /// (to fill `Field.field`) — expression kinds are small.
    fn expr_ty(&mut self, id: ExprId, expected: Option<Ty>) -> Ty {
        let span = self.node(id).span;
        let ty = match self.node(id).kind.clone() {
            HirExprKind::Literal(lit) => self.literal_ty(&lit, expected, span),
            HirExprKind::Var(sym) => match self.locals.get(&sym) {
                Some(t) => *t,
                None => {
                    let sym = self.body.symbol(self.scope, sym);
                    let name = self.interner.resolve(sym.name).to_string();
                    self.diags.push(
                        Diagnostic::error(
                            Code::Internal,
                            format!("untyped binding `{name}` survived type checking"),
                        )
                        .primary(span),
                    );
                    Ty::Poison
                }
            },
            HirExprKind::Call { def, args } => self.call_ty(def, &args, span),
            HirExprKind::Field { base, name, .. } => {
                let base_ty = self.expr_ty(base, None);
                let is_len = self.interner.resolve(name.id) == "len";
                if is_len && matches!(base_ty, Ty::Str | Ty::Array(_)) {
                    // `len` is a built-in property, not a field —
                    // rewrite the node so MIR and ownership analysis
                    // never treat it as a projection.
                    self.exprs[id.index()].kind = HirExprKind::Len { base };
                    Ty::I32
                } else {
                    match self.field_ty(base_ty, name.id, name.span) {
                        Some(t) => {
                            // Record the resolved field index for MIR.
                            let idx = self.field_index_of(base_ty, name.id);
                            if let HirExprKind::Field { field, .. } =
                                &mut self.exprs[id.index()].kind
                            {
                                *field = idx;
                            }
                            t
                        }
                        None => Ty::Poison,
                    }
                }
            }
            HirExprKind::Len { base } => {
                // Only the checker creates `Len`, and only for
                // `str`/`[T]` bases — any other base means an
                // internal gap.
                match self.expr_ty(base, None) {
                    Ty::Str | Ty::Array(_) => Ty::I32,
                    Ty::Poison => Ty::Poison,
                    other => {
                        self.diags.push(
                            Diagnostic::error(
                                Code::Internal,
                                format!("`len` property on {}", self.show(other)),
                            )
                            .primary(span),
                        );
                        Ty::Poison
                    }
                }
            }
            HirExprKind::Index { base, index } => self.index_ty(base, index),
            HirExprKind::Slice { base, lo, hi } => self.slice_ty(base, lo, hi),
            HirExprKind::ArrayLit { elems } => self.array_lit_ty(&elems, expected, span),
            HirExprKind::Range { lo, hi } => {
                // A `..` range is only valid as a `for` iterable
                // (slice bounds inside `[]` never lower to `Range`).
                // Bounds are still checked for secondary diagnostics.
                for bound in [lo, hi].into_iter().flatten() {
                    self.expr_ty(bound, None);
                }
                self.diags.push(
                    Diagnostic::error(
                        Code::UnsupportedOperation,
                        "a `..` range is only valid as a `for` iterable",
                    )
                    .primary(span),
                );
                Ty::Poison
            }
            HirExprKind::For { var, iter, body } => self.for_ty(var, iter, body),
            HirExprKind::Binary { op, lhs, rhs } => self.binary_ty(op, lhs, rhs, span),
            HirExprKind::Unary { op, expr } => self.unary_ty(op, expr, span),
            HirExprKind::If { cond, then, else_ } => self.if_ty(cond, then, else_, expected, span),
            HirExprKind::Block { stmts, tail } => {
                for s in &stmts {
                    self.stmt(s);
                }
                match tail {
                    Some(t) => self.expr_ty(t, expected),
                    None => Ty::Unit,
                }
            }
            HirExprKind::StructLit { def, fields } => self.struct_lit_ty(id, def, &fields, span),
            HirExprKind::Poison => Ty::Poison,
        };
        self.set_ty(id, ty)
    }

    fn field_index_of(&self, ty: Ty, name: InternId) -> Option<u32> {
        match ty {
            Ty::Struct(def) => self
                .scope
                .data_shape(def)
                .and_then(|s| s.field_index.get(&name).copied()),
            _ => None,
        }
    }

    fn literal_ty(&mut self, lit: &LitValue, expected: Option<Ty>, span: Span) -> Ty {
        match lit {
            LitValue::Int(v) => {
                let ty = match expected {
                    Some(t) if t.is_integer() => t,
                    _ => Ty::I32,
                };
                if !int_fits(*v, ty) {
                    self.diags.push(
                        Diagnostic::error(
                            Code::LiteralOverflow,
                            format!("integer literal `{v}` does not fit {}", self.show(ty)),
                        )
                        .primary(span),
                    );
                    return Ty::Poison;
                }
                ty
            }
            LitValue::Float(_) => match expected {
                Some(t @ (Ty::F32 | Ty::F64)) => t,
                _ => Ty::F64,
            },
            LitValue::Str(_) => Ty::Str,
            LitValue::Bool(_) => Ty::Bool,
        }
    }

    fn call_ty(&mut self, def: DefId, args: &[ExprId], span: Span) -> Ty {
        let sig = self.scope.fn_sig(def).expect("calls resolve to fns");
        let params: Vec<Ty> = sig.params.iter().map(|p| p.ty.into()).collect();
        let ret: Ty = sig.ret.into();
        if args.len() != params.len() {
            self.diags.push(
                Diagnostic::error(
                    Code::ArgCount,
                    format!(
                        "expected {} argument{}, found {}",
                        params.len(),
                        if params.len() == 1 { "" } else { "s" },
                        args.len()
                    ),
                )
                .primary(span),
            );
        }
        for (i, arg) in args.iter().enumerate() {
            let expected = params.get(i).copied();
            let actual = self.expr_ty(*arg, expected);
            if let Some(e) = expected {
                let aspan = self.node(*arg).span;
                self.unify(actual, e, aspan);
            }
        }
        ret
    }

    /// `base[index]`: `str` indexes by character position (the result
    /// is a one-character `str`); `[T]` indexes by element position
    /// (the result is `T`). The index must be an integer.
    fn index_ty(&mut self, base: ExprId, index: ExprId) -> Ty {
        let b = self.expr_ty(base, None);
        let i = self.expr_ty(index, None);
        let mut ok = true;
        let result = match b {
            Ty::Str => Ty::Str,
            Ty::Array(e) => e.ty(),
            Ty::Poison => Ty::Poison,
            other => {
                let bspan = self.node(base).span;
                self.diags.push(
                    Diagnostic::error(
                        Code::UnsupportedOperation,
                        format!("cannot index {}", self.show(other)),
                    )
                    .primary(bspan),
                );
                ok = false;
                Ty::Poison
            }
        };
        if !i.is_integer() && i != Ty::Poison {
            let ispan = self.node(index).span;
            self.diags.push(
                Diagnostic::error(
                    Code::TypeMismatch,
                    format!("index must be an integer, found {}", self.show(i)),
                )
                .primary(ispan),
            );
            ok = false;
        }
        if ok { result } else { Ty::Poison }
    }

    /// `base[lo..hi]`: `str` slices produce `str`; `[T]` slices
    /// produce a fresh `[T]`. Present bounds must be integers.
    fn slice_ty(&mut self, base: ExprId, lo: Option<ExprId>, hi: Option<ExprId>) -> Ty {
        let b = self.expr_ty(base, None);
        let mut ok = true;
        let result = match b {
            Ty::Str => Ty::Str,
            Ty::Array(_) => b,
            Ty::Poison => Ty::Poison,
            other => {
                let bspan = self.node(base).span;
                self.diags.push(
                    Diagnostic::error(
                        Code::UnsupportedOperation,
                        format!("cannot slice {}", self.show(other)),
                    )
                    .primary(bspan),
                );
                ok = false;
                Ty::Poison
            }
        };
        for bound in [lo, hi].into_iter().flatten() {
            let t = self.expr_ty(bound, None);
            if !t.is_integer() && t != Ty::Poison {
                let bspan = self.node(bound).span;
                self.diags.push(
                    Diagnostic::error(
                        Code::TypeMismatch,
                        format!("slice bound must be an integer, found {}", self.show(t)),
                    )
                    .primary(bspan),
                );
                ok = false;
            }
        }
        if ok { result } else { Ty::Poison }
    }

    /// `[e, ...]`: every element must share one type — the first
    /// element's, or the annotation's when given (`let a: [i32] =
    /// [..]`). An empty `[]` needs the annotation — there is nothing
    /// to infer from.
    fn array_lit_ty(&mut self, elems: &[ExprId], expected: Option<Ty>, span: Span) -> Ty {
        let expected_elem = match expected {
            Some(Ty::Array(e)) => Some(e.ty()),
            _ => None,
        };
        let mut elem: Option<Ty> = expected_elem;
        let mut ok = true;
        for &e in elems {
            let t = self.expr_ty(e, expected_elem);
            match elem {
                None => elem = Some(t),
                Some(prev) => {
                    if !prev.compatible(t) {
                        let espan = self.node(e).span;
                        self.diags.push(
                            Diagnostic::error(
                                Code::TypeMismatch,
                                format!(
                                    "array element must be {}, found {}",
                                    self.show(prev),
                                    self.show(t)
                                ),
                            )
                            .primary(espan),
                        );
                        ok = false;
                    }
                }
            }
        }
        let Some(elem) = elem else {
            // `[]` with no `[T]` context — nothing to infer from. When
            // the context itself is poison (`let a: [unit] = []`), the
            // annotation was already diagnosed — don't report twice.
            if expected != Some(Ty::Poison) {
                self.diags.push(
                    Diagnostic::error(
                        Code::CannotInfer,
                        "cannot infer the element type of an empty array literal; \
                         add a type annotation",
                    )
                    .primary(span),
                );
            }
            return Ty::Poison;
        };
        if !ok || elem == Ty::Poison {
            return Ty::Poison;
        }
        match elem.elem() {
            Some(e) => Ty::Array(e),
            None => {
                // Only `unit` and `[_]` elements reach here — `Poison`
                // returned early above.
                let msg = match elem {
                    Ty::Array(_) => "nested arrays are not supported yet".to_string(),
                    _ => format!("arrays cannot hold {} elements", self.show(elem)),
                };
                self.diags
                    .push(Diagnostic::error(Code::UnsupportedOperation, msg).primary(span));
                Ty::Poison
            }
        }
    }

    /// `for x in e { .. }`: `e` must be an array (`x` binds its
    /// element type) or a `lo..hi` integer range (`x` binds the
    /// bound type — `i32` by default). `for` is `unit`-typed.
    fn for_ty(&mut self, var: SymbolId, iter: ExprId, body: ExprId) -> Ty {
        let elem = match self.node(iter).kind.clone() {
            HirExprKind::Range { lo, hi } => self.range_iter_ty(lo, hi, iter),
            _ => match self.expr_ty(iter, None) {
                Ty::Array(e) => e.ty(),
                Ty::Poison => Ty::Poison,
                other => {
                    let ispan = self.node(iter).span;
                    self.diags.push(
                        Diagnostic::error(
                            Code::UnsupportedOperation,
                            format!("cannot iterate over {}", self.show(other)),
                        )
                        .primary(ispan),
                    );
                    Ty::Poison
                }
            },
        };
        self.locals.insert(var, elem);
        self.tables.local_types.insert(var, elem);
        // The body is a statement block — its tail value, if any, is
        // discarded like an `if` without `else`.
        self.expr_ty(body, None);
        Ty::Unit
    }

    /// Element type of a `for i in lo..hi` iterable. Bounds must be
    /// integers of one type; a missing `lo` starts at 0; a missing
    /// `hi` never terminates — `break` does not exist yet, so an
    /// unbounded range is rejected.
    fn range_iter_ty(&mut self, lo: Option<ExprId>, hi: Option<ExprId>, iter: ExprId) -> Ty {
        let mut ok = true;
        if hi.is_none() {
            let ispan = self.node(iter).span;
            self.diags.push(
                Diagnostic::error(
                    Code::UnsupportedOperation,
                    "cannot iterate an unbounded `lo..` range (no `break` yet)",
                )
                .primary(ispan),
            );
            ok = false;
        }
        let mut elem: Option<Ty> = None;
        // A bare integer literal adopts the other bound's type —
        // evaluate a non-literal bound first so `0..n` with `n: i64`
        // treats `0` as `i64` too (not just `n..0`).
        let lo_lit = lo.is_some_and(|e| matches!(self.node(e).kind, HirExprKind::Literal(_)));
        let bounds: Vec<ExprId> = if lo_lit {
            [hi, lo].into_iter().flatten().collect()
        } else {
            [lo, hi].into_iter().flatten().collect()
        };
        for bound in bounds {
            let t = self.expr_ty(bound, elem);
            if t == Ty::Poison {
                ok = false;
                continue;
            }
            if !t.is_integer() {
                let bspan = self.node(bound).span;
                self.diags.push(
                    Diagnostic::error(
                        Code::TypeMismatch,
                        format!("range bound must be an integer, found {}", self.show(t)),
                    )
                    .primary(bspan),
                );
                ok = false;
                continue;
            }
            match elem {
                None => elem = Some(t),
                Some(e) if e != t => {
                    let ispan = self.node(iter).span;
                    self.diags.push(
                        Diagnostic::error(
                            Code::TypeMismatch,
                            format!(
                                "range bounds must share one integer type, found {} and {}",
                                self.show(e),
                                self.show(t)
                            ),
                        )
                        .primary(ispan),
                    );
                    ok = false;
                }
                Some(_) => {}
            }
        }
        if ok {
            elem.unwrap_or(Ty::I32)
        } else {
            Ty::Poison
        }
    }

    fn binary_ty(&mut self, op: BinOp, lhs: ExprId, rhs: ExprId, span: Span) -> Ty {
        let l = self.expr_ty(lhs, None);
        let r = self.expr_ty(rhs, Some(l));
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => {
                if l.is_numeric() && l.compatible(r) {
                    l
                } else if op == BinOp::Add && l == Ty::Str && r == Ty::Str {
                    // String concatenation.
                    Ty::Str
                } else if l == Ty::Poison || r == Ty::Poison {
                    Ty::Poison
                } else {
                    let (code, msg) = if l.compatible(r) {
                        (
                            Code::UnsupportedOperation,
                            format!(
                                "operator `{}` is not supported on {}",
                                op_str(op),
                                self.show(l)
                            ),
                        )
                    } else {
                        (
                            Code::TypeMismatch,
                            format!("expected {}, found {}", self.show(l), self.show(r)),
                        )
                    };
                    self.diags.push(Diagnostic::error(code, msg).primary(span));
                    Ty::Poison
                }
            }
            BinOp::Eq | BinOp::Ne => {
                if !l.compatible(r) {
                    self.diags.push(
                        Diagnostic::error(
                            Code::TypeMismatch,
                            format!("expected {}, found {}", self.show(l), self.show(r)),
                        )
                        .primary(span),
                    );
                    Ty::Poison
                } else if l != Ty::Poison && !(l.is_numeric() || l == Ty::Bool || l == Ty::Str) {
                    self.diags.push(
                        Diagnostic::error(
                            Code::UnsupportedOperation,
                            format!("cannot compare {} values for equality", self.show(l)),
                        )
                        .primary(span),
                    );
                    Ty::Poison
                } else {
                    Ty::Bool
                }
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                if (l.is_numeric() || l == Ty::Str) && l.compatible(r) {
                    // Numeric ordering, or lexicographic `str` ordering.
                    Ty::Bool
                } else if l == Ty::Poison || r == Ty::Poison {
                    Ty::Poison
                } else {
                    let (code, msg) = if l.compatible(r) {
                        (
                            Code::UnsupportedOperation,
                            format!("cannot order-compare {} values", self.show(l)),
                        )
                    } else {
                        (
                            Code::TypeMismatch,
                            format!("expected {}, found {}", self.show(l), self.show(r)),
                        )
                    };
                    self.diags.push(Diagnostic::error(code, msg).primary(span));
                    Ty::Poison
                }
            }
            BinOp::And | BinOp::Or => {
                if l == Ty::Bool && r == Ty::Bool {
                    Ty::Bool
                } else if l == Ty::Poison || r == Ty::Poison {
                    Ty::Poison
                } else {
                    self.diags.push(
                        Diagnostic::error(
                            Code::TypeMismatch,
                            format!(
                                "operator `{}` requires bool operands ({} and {})",
                                op_str(op),
                                self.show(l),
                                self.show(r)
                            ),
                        )
                        .primary(span),
                    );
                    Ty::Poison
                }
            }
        }
    }

    fn unary_ty(&mut self, op: UnOp, expr: ExprId, span: Span) -> Ty {
        let t = self.expr_ty(expr, None);
        match op {
            UnOp::Neg => {
                if t.is_numeric() {
                    t
                } else if t == Ty::Poison {
                    Ty::Poison
                } else {
                    self.diags.push(
                        Diagnostic::error(
                            Code::UnsupportedOperation,
                            format!("cannot negate {}", self.show(t)),
                        )
                        .primary(span),
                    );
                    Ty::Poison
                }
            }
            UnOp::Not => match t {
                Ty::Bool => Ty::Bool,
                Ty::Poison => Ty::Poison,
                _ => {
                    self.diags.push(
                        Diagnostic::error(
                            Code::TypeMismatch,
                            format!("operator `!` requires bool, found {}", self.show(t)),
                        )
                        .primary(span),
                    );
                    Ty::Poison
                }
            },
        }
    }

    fn if_ty(
        &mut self,
        cond: ExprId,
        then: ExprId,
        else_: Option<ExprId>,
        expected: Option<Ty>,
        span: Span,
    ) -> Ty {
        let c = self.expr_ty(cond, Some(Ty::Bool));
        if c != Ty::Bool && c != Ty::Poison {
            self.diags.push(
                Diagnostic::error(
                    Code::TypeMismatch,
                    format!("expected bool, found {}", self.show(c)),
                )
                .primary(self.node(cond).span),
            );
        }
        let t = self.expr_ty(then, expected);
        match else_ {
            Some(e) => {
                let el = self.expr_ty(e, expected);
                self.unify(el, t, span)
            }
            // `if c { ... }` without else discards the then-value.
            None => Ty::Unit,
        }
    }

    fn struct_lit_ty(
        &mut self,
        id: ExprId,
        def: DefId,
        fields: &[(Name, ExprId)],
        span: Span,
    ) -> Ty {
        let shape = self
            .scope
            .data_shape(def)
            .expect("struct lit of a data def");
        let index = &shape.field_index;

        let mut slots = Vec::with_capacity(fields.len());
        let mut seen: FxHashMap<InternId, ()> = FxHashMap::default();
        for (fname, value) in fields {
            match index.get(&fname.id) {
                Some(i) => {
                    if seen.insert(fname.id, ()).is_some() {
                        let n = self.interner.resolve(fname.id).to_string();
                        self.diags.push(
                            Diagnostic::error(
                                Code::DuplicateField,
                                format!("field `{n}` is initialized more than once"),
                            )
                            .primary(fname.span)
                            .subject(n),
                        );
                    }
                    slots.push(*i);
                    let expected: Ty = shape.fields[*i as usize].ty.into();
                    let actual = self.expr_ty(*value, Some(expected));
                    let vspan = self.node(*value).span;
                    self.unify(actual, expected, vspan);
                }
                None => {
                    let n = self.interner.resolve(fname.id).to_string();
                    self.diags.push(
                        Diagnostic::error(
                            Code::UnknownField,
                            format!("{} has no field `{n}`", self.show(Ty::Struct(def))),
                        )
                        .primary(fname.span)
                        .subject(n),
                    );
                    self.expr_ty(*value, None);
                }
            }
        }
        let shape = self
            .scope
            .data_shape(def)
            .expect("struct lit of a data def");
        for (i, f) in shape.fields.iter().enumerate() {
            if !slots.contains(&(i as u32)) {
                let n = self
                    .interner
                    .resolve(self.scope.symbols.get(f.symbol).name)
                    .to_string();
                self.diags.push(
                    Diagnostic::error(Code::MissingField, format!("missing field `{n}`"))
                        .primary(span)
                        .subject(n),
                );
            }
        }
        self.tables.struct_lit_slots.insert(id, slots);
        Ty::Struct(def)
    }
}

fn op_str(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        BinOp::Rem => "%",
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Le => "<=",
        BinOp::Gt => ">",
        BinOp::Ge => ">=",
        BinOp::And => "&&",
        BinOp::Or => "||",
    }
}

fn int_fits(v: i128, ty: Ty) -> bool {
    match ty {
        Ty::I32 => (i32::MIN as i128..=i32::MAX as i128).contains(&v),
        Ty::I64 => (i64::MIN as i128..=i64::MAX as i128).contains(&v),
        Ty::U32 => (0..=u32::MAX as i128).contains(&v),
        Ty::U64 => (0..=u64::MAX as i128).contains(&v),
        _ => true,
    }
}
