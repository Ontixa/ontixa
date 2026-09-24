//! AST → HIR body lowering with expression-level name resolution.
//!
//! Each function body is lowered with a lexical scope stack. Names
//! resolve to (innermost-first):
//!
//! 1. `let` bindings in enclosing blocks,
//! 2. function parameters,
//! 3. module definitions (in call/struct-literal position only —
//!    definitions are not values in milestone 1).
//!
//! Unresolved names produce diagnostics and `Poison` nodes rather than
//! guesses, so later passes never see fabricated semantics.

use crate::hir::{
    ElemRef, FileEnv, HirArm, HirBody, HirExpr, HirExprKind, HirModule, HirPat, HirPlace, HirStmt,
    ModuleScope, Name, Symbol, SymbolKind, TypeRef,
};
use ontixa_ast::{
    AstModule, Block, Expr, FnDecl, Ident, Item, Path, Pattern, Place, Stmt, TypeExpr,
};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_source::{DefId, ExprId, InternId, Interner, Span, SymbolId};
use rustc_hash::FxHashMap;

/// Lowers all function bodies in `ast` against the resolved `scope`,
/// producing the complete [`HirModule`].
///
/// Each body is lowered independently by [`lower_body`] into its own
/// expression and local-symbol arenas — an edit to one function can
/// never renumber another's ids.
pub fn lower_bodies(
    ast: &AstModule,
    scope: ModuleScope,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> HirModule {
    let mut bodies: Vec<Option<HirBody>> = (0..scope.defs.len()).map(|_| None).collect();
    for (idx, item) in ast.items.iter().enumerate() {
        let Item::Fn(f) = item else { continue };
        let def = DefId::new(idx as u32);
        // Lower from the item-relative copy, matching the database's
        // `AstItem` query — `Scope` spans and body spans share the
        // item-local coordinate space.
        let Item::Fn(rel) = ontixa_ast::rebase_item(item, f.span.start) else {
            unreachable!()
        };
        bodies[idx] = Some(lower_body(&rel, def, &scope, interner, diags));
    }
    HirModule { scope, bodies }
}

/// Lowers one function body against the resolved module `scope`.
///
/// `f`'s spans must be **item-relative** (see
/// [`ontixa_ast::rebase_item`]) — the returned body's spans and the
/// diagnostics emitted here are item-local, and diagnostics are
/// tagged `origin = def` so a collector can rebase them.
///
/// The scope is borrowed immutably: everything the body declares —
/// its params and its `let` bindings — lives in the returned
/// [`HirBody`]'s own arenas, never in the module table.
pub fn lower_body(
    f: &FnDecl,
    def: DefId,
    scope: &ModuleScope,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> HirBody {
    let mark = diags.len();
    let mut b = BodyLowerer {
        def,
        file: scope.def(def).file,
        scope,
        interner,
        diags,
        exprs: Vec::new(),
        local_symbols: Vec::new(),
        scopes: vec![FxHashMap::default()],
    };
    // Params occupy local arena slots 0..n — matching
    // `sig.params[i].symbol == SymbolId::local(i)`, which name
    // resolution fixed before any body existed.
    for p in &f.params {
        let interned = b.interner.intern(&p.name.name);
        let id = b.declare_sym(Symbol {
            id: SymbolId::new(0),
            name: interned,
            kind: SymbolKind::Param,
            mutable: p.mutable,
            owner: Some(def),
            span: p.name.span,
        });
        b.scopes[0].insert(interned, id);
    }
    let root = b.block(&f.body);
    // Recover the moved-out `diags` handle to tag this body's
    // diagnostics before returning.
    let BodyLowerer {
        exprs,
        local_symbols,
        diags,
        ..
    } = b;
    diags.tag_origin_from(mark, def);
    HirBody {
        def,
        root,
        exprs,
        local_symbols,
    }
}

/// How a `path` failed to resolve — selects the diagnostic shape.
#[derive(Debug, Clone, Copy)]
enum PathErr {
    /// The `m` in `m::x` is not a bound module.
    UnknownModule,
    /// `m` exists but has no member `x`.
    UnknownMember,
    /// Unqualified name not found, or a path longer than `m::x`.
    NotFound,
}

/// How a `T::V`/`m::T::V` path failed to name a `data` variant.
#[derive(Debug, Clone, Copy)]
enum VariantErr {
    /// The `m` in `m::T::V` is not a bound module.
    UnknownModule,
    /// Wrong shape, or the type segment is not a `data` def.
    NotFound,
    /// The data def exists but has no such variant.
    NotVariant(DefId),
}

struct BodyLowerer<'a> {
    def: DefId,
    /// The file this body lives in — selects the [`FileEnv`] its
    /// names resolve through.
    file: ontixa_source::FileId,
    scope: &'a ModuleScope,
    interner: &'a mut Interner,
    diags: &'a mut Diagnostics,
    /// This body's own expression arena.
    exprs: Vec<HirExpr>,
    /// This body's own symbol arena (params then `let` bindings).
    local_symbols: Vec<Symbol>,
    scopes: Vec<FxHashMap<InternId, SymbolId>>,
}

impl BodyLowerer<'_> {
    // ---------- infrastructure ----------

    fn alloc_expr(&mut self, kind: HirExprKind, span: Span) -> ExprId {
        let id = ExprId::new(self.exprs.len() as u32);
        self.exprs.push(HirExpr { id, kind, span });
        id
    }

    fn name_of(&mut self, ident: &Ident) -> Name {
        Name {
            id: self.interner.intern(&ident.name),
            span: ident.span,
        }
    }

    fn lookup(&self, id: InternId) -> Option<SymbolId> {
        for scope in self.scopes.iter().rev() {
            if let Some(sym) = scope.get(&id) {
                return Some(*sym);
            }
        }
        None
    }

    /// Pushes a symbol into the body arena, assigning `local(i)`.
    fn declare_sym(&mut self, mut sym: Symbol) -> SymbolId {
        let id = SymbolId::local(self.local_symbols.len() as u32);
        sym.id = id;
        self.local_symbols.push(sym);
        id
    }

    fn declare_local(&mut self, ident: &Ident, mutable: bool) -> SymbolId {
        let interned = self.interner.intern(&ident.name);
        let id = self.declare_sym(Symbol {
            id: SymbolId::new(0),
            name: interned,
            kind: SymbolKind::Local,
            mutable,
            owner: Some(self.def),
            span: ident.span,
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(interned, id);
        }
        id
    }

    /// This file's name environment — what its own defs and `use`
    /// declarations brought into scope.
    fn env(&self) -> &FileEnv {
        self.scope
            .env(self.file)
            .expect("body lowered outside its workspace")
    }

    /// Resolves a `path` (`x` or `m::x`) to a definition through this
    /// file's env. Any def kind may come back — callers check.
    fn path_def(&mut self, path: &Path) -> Result<DefId, PathErr> {
        match path.segs.as_slice() {
            [one] => {
                let interned = self.interner.intern(&one.name);
                let env = self.env();
                env.fns
                    .get(&interned)
                    .or_else(|| env.datas.get(&interned))
                    .or_else(|| env.imports.get(&interned))
                    .copied()
                    .ok_or(PathErr::NotFound)
            }
            [module, member] => {
                let module_id = self.interner.intern(&module.name);
                let Some(&module_file) = self.env().modules.get(&module_id) else {
                    return Err(PathErr::UnknownModule);
                };
                let member_id = self.interner.intern(&member.name);
                self.scope
                    .env(module_file)
                    .and_then(|e| e.fns.get(&member_id).or_else(|| e.datas.get(&member_id)))
                    .copied()
                    .ok_or(PathErr::UnknownMember)
            }
            _ => Err(PathErr::NotFound),
        }
    }

    /// Resolves a variant path `T::V` or `m::T::V` to `(data DefId,
    /// discriminant index)`. `T` resolves through this file's data
    /// env (own `data` defs and `use m::T` member imports); `m::T::V`
    /// reaches into a bound module's data defs.
    fn resolve_variant(&mut self, path: &Path) -> Result<(DefId, u32), VariantErr> {
        match path.segs.as_slice() {
            [t, v] => {
                let tid = self.interner.intern(&t.name);
                let env = self.env();
                let data = env.datas.get(&tid).copied().or_else(|| {
                    env.imports
                        .get(&tid)
                        .copied()
                        .filter(|d| self.scope.data_shape(*d).is_some())
                });
                let Some(data) = data else {
                    return Err(VariantErr::NotFound);
                };
                self.variant_index_of(data, v)
            }
            [m, t, v] => {
                let mid = self.interner.intern(&m.name);
                let Some(&mfile) = self.env().modules.get(&mid) else {
                    return Err(VariantErr::UnknownModule);
                };
                let tid = self.interner.intern(&t.name);
                let data = self
                    .scope
                    .env(mfile)
                    .and_then(|e| e.datas.get(&tid).copied());
                let Some(data) = data else {
                    return Err(VariantErr::NotFound);
                };
                self.variant_index_of(data, v)
            }
            _ => Err(VariantErr::NotFound),
        }
    }

    /// Looks up variant `v` in `data`'s shape.
    fn variant_index_of(&mut self, data: DefId, v: &Ident) -> Result<(DefId, u32), VariantErr> {
        let vid = self.interner.intern(&v.name);
        let shape = self
            .scope
            .data_shape(data)
            .expect("resolve_variant only returns data defs");
        match shape.variant_index.get(&vid) {
            Some(&idx) => Ok((data, idx)),
            None => Err(VariantErr::NotVariant(data)),
        }
    }

    /// The diagnostic for a failed [`Self::resolve_variant`].
    fn variant_error(&mut self, path: &Path, err: VariantErr) {
        let segs = &path.segs;
        let d = match err {
            VariantErr::UnknownModule => Diagnostic::error(
                Code::UnknownModule,
                format!("unknown module `{}`", segs[0].name),
            )
            .primary(segs[0].span)
            .subject(segs[0].name.clone()),
            VariantErr::NotVariant(def) => {
                let data = self
                    .interner
                    .resolve(self.scope.symbols.get(self.scope.def(def).name).name)
                    .to_string();
                let last = segs.last().expect("variant paths have segments");
                Diagnostic::error(
                    Code::UnknownSymbol,
                    format!("`{data}` has no variant `{}`", last.name),
                )
                .primary(last.span)
                .subject(last.name.clone())
            }
            VariantErr::NotFound => Diagnostic::error(
                Code::UnknownSymbol,
                format!("`{}` does not name a `data` variant", path.display()),
            )
            .primary(path.span)
            .subject(path.display()),
        };
        self.diags.push(d);
    }

    /// The diagnostic for a failed [`Self::path_def`] — `code` covers
    /// the member/name misses; an unknown module is always
    /// `E_UNKNOWN_MODULE`.
    fn path_error(&mut self, path: &Path, err: PathErr, what: &str, code: Code) {
        let segs = &path.segs;
        let d = match err {
            PathErr::UnknownModule => Diagnostic::error(
                Code::UnknownModule,
                format!("unknown module `{}`", segs[0].name),
            )
            .primary(segs[0].span)
            .subject(segs[0].name.clone()),
            PathErr::UnknownMember => Diagnostic::error(
                code,
                format!("module `{}` has no {what} `{}`", segs[0].name, segs[1].name),
            )
            .primary(segs[1].span)
            .subject(segs[1].name.clone()),
            PathErr::NotFound => {
                Diagnostic::error(code, format!("unknown {} `{}`", what, path.display()))
                    .primary(path.span)
                    .subject(path.display())
            }
        };
        self.diags.push(d);
    }

    // ---------- blocks & statements ----------

    fn block(&mut self, block: &Block) -> ExprId {
        self.scopes.push(FxHashMap::default());
        let mut stmts = Vec::new();
        for s in &block.stmts {
            if let Some(stmt) = self.stmt(s) {
                stmts.push(stmt);
            }
        }
        let tail = block.tail.as_deref().and_then(|e| self.expr(e));
        self.scopes.pop();
        self.alloc_expr(HirExprKind::Block { stmts, tail }, block.span)
    }

    fn stmt(&mut self, stmt: &Stmt) -> Option<HirStmt> {
        Some(match stmt {
            Stmt::Let {
                name,
                mutable,
                ty,
                init,
                span,
            } => {
                // The initializer is evaluated before the binding exists
                // (`let x = x` cannot self-reference).
                let init = init.as_ref().and_then(|e| self.expr(e));
                let ty = ty.as_ref().map(|t| self.resolve_type(t));
                let symbol = self.declare_local(name, *mutable);
                HirStmt::Let {
                    symbol,
                    ty,
                    init,
                    span: *span,
                }
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                let value = self.expr(value)?;
                let target = self.place(target)?;
                HirStmt::Assign {
                    target,
                    value,
                    span: *span,
                }
            }
            Stmt::Expr { expr, has_semi, .. } => {
                let expr = self.expr(expr)?;
                HirStmt::Expr {
                    expr,
                    has_semi: *has_semi,
                }
            }
            Stmt::Return { value, span } => {
                let value = value.as_ref().and_then(|e| self.expr(e));
                HirStmt::Return { value, span: *span }
            }
        })
    }

    fn place(&mut self, place: &Place) -> Option<HirPlace> {
        let interned = self.interner.intern(&place.base.name);
        match self.lookup(interned) {
            Some(base) => Some(HirPlace {
                base,
                fields: place.fields.iter().map(|f| self.name_of(f)).collect(),
                span: place
                    .base
                    .span
                    .covering(place.fields.last().map_or(place.base.span, |f| f.span)),
            }),
            None => {
                self.diags.push(
                    Diagnostic::error(
                        Code::UnknownSymbol,
                        format!("unknown binding `{}`", place.base.name),
                    )
                    .primary(place.base.span)
                    .subject(place.base.name.clone()),
                );
                None
            }
        }
    }

    // ---------- expressions ----------

    fn expr(&mut self, expr: &Expr) -> Option<ExprId> {
        let span = expr.span();
        Some(match expr {
            Expr::Literal { value, .. } => {
                self.alloc_expr(HirExprKind::Literal(value.clone()), span)
            }
            Expr::Var { name } => {
                let interned = self.interner.intern(&name.name);
                match self.lookup(interned) {
                    Some(sym) => self.alloc_expr(HirExprKind::Var(sym), span),
                    None => {
                        let what = self.describe_def(interned);
                        self.diags.push(
                            Diagnostic::error(
                                Code::UnknownSymbol,
                                format!("unknown binding `{}`{what}", name.name),
                            )
                            .primary(name.span)
                            .subject(name.name.clone()),
                        );
                        self.alloc_expr(HirExprKind::Poison, span)
                    }
                }
            }
            Expr::Call { callee, args, .. } => {
                let args: Vec<ExprId> = args.iter().filter_map(|a| self.expr(a)).collect();
                match self.path_def(callee) {
                    Ok(d) if self.scope.fn_sig(d).is_some() => {
                        self.alloc_expr(HirExprKind::Call { def: d, args }, span)
                    }
                    Ok(_) => {
                        self.diags.push(
                            Diagnostic::error(
                                Code::NotCallable,
                                format!("`{}` is not a function", callee.display()),
                            )
                            .primary(callee.span)
                            .subject(callee.display()),
                        );
                        self.alloc_expr(HirExprKind::Poison, span)
                    }
                    Err(PathErr::NotFound) if callee.segs.len() == 1 => {
                        // Preserve the historical message: a local or
                        // data bearing the callee's name still reports
                        // "not a function", otherwise "unknown symbol".
                        let interned = self.interner.intern(&callee.segs[0].name);
                        let code = if self.lookup(interned).is_some() {
                            Code::NotCallable
                        } else {
                            Code::UnknownSymbol
                        };
                        self.diags.push(
                            Diagnostic::error(
                                code,
                                format!("`{}` is not a function", callee.display()),
                            )
                            .primary(callee.span)
                            .subject(callee.display()),
                        );
                        self.alloc_expr(HirExprKind::Poison, span)
                    }
                    Err(e) => match self.resolve_variant(callee) {
                        // `T::V(args)` / `m::T::V(args)` — a variant
                        // constructor, not a function call.
                        Ok((d, v)) => self.alloc_expr(
                            HirExprKind::VariantLit {
                                def: d,
                                variant: v,
                                args,
                            },
                            span,
                        ),
                        Err(e2 @ VariantErr::NotVariant(_)) => {
                            self.variant_error(callee, e2);
                            self.alloc_expr(HirExprKind::Poison, span)
                        }
                        Err(_) => {
                            self.path_error(callee, e, "function", Code::UnknownSymbol);
                            self.alloc_expr(HirExprKind::Poison, span)
                        }
                    },
                }
            }
            Expr::Path { path } => {
                match self.path_def(path) {
                    Ok(d) => {
                        let what = if self.scope.fn_sig(d).is_some() {
                            "a function"
                        } else {
                            "a type"
                        };
                        self.diags.push(
                            Diagnostic::error(
                                Code::UnknownSymbol,
                                format!("`{}` is {what}, not a value", path.display()),
                            )
                            .primary(path.span)
                            .subject(path.display()),
                        );
                        self.alloc_expr(HirExprKind::Poison, span)
                    }
                    Err(e) => match self.resolve_variant(path) {
                        // A bare `T::V` constructs a variant with no
                        // payload arguments — arity errors come from
                        // the checker.
                        Ok((d, v)) => self.alloc_expr(
                            HirExprKind::VariantLit {
                                def: d,
                                variant: v,
                                args: Vec::new(),
                            },
                            span,
                        ),
                        Err(e2 @ VariantErr::NotVariant(_)) => {
                            self.variant_error(path, e2);
                            self.alloc_expr(HirExprKind::Poison, span)
                        }
                        Err(_) => {
                            self.path_error(path, e, "symbol", Code::UnknownSymbol);
                            self.alloc_expr(HirExprKind::Poison, span)
                        }
                    },
                }
            }
            Expr::Field { base, name, .. } => {
                let base = self.expr(base)?;
                let fname = self.name_of(name);
                self.alloc_expr(
                    HirExprKind::Field {
                        base,
                        name: fname,
                        field: None,
                    },
                    span,
                )
            }
            Expr::Index { base, index, .. } => {
                let base = self.expr(base)?;
                let index = self.expr(index)?;
                self.alloc_expr(HirExprKind::Index { base, index }, span)
            }
            Expr::Slice { base, lo, hi, .. } => {
                let base = self.expr(base)?;
                let lo = lo.as_deref().and_then(|e| self.expr(e));
                let hi = hi.as_deref().and_then(|e| self.expr(e));
                self.alloc_expr(HirExprKind::Slice { base, lo, hi }, span)
            }
            Expr::ArrayLit { elems, .. } => {
                let elems: Vec<ExprId> = elems.iter().filter_map(|e| self.expr(e)).collect();
                self.alloc_expr(HirExprKind::ArrayLit { elems }, span)
            }
            Expr::Range { lo, hi, .. } => {
                let lo = lo.as_deref().and_then(|e| self.expr(e));
                let hi = hi.as_deref().and_then(|e| self.expr(e));
                self.alloc_expr(HirExprKind::Range { lo, hi }, span)
            }
            Expr::For {
                var,
                mutable,
                iter,
                body,
                ..
            } => {
                // The iterable resolves before the loop variable exists
                // — `for x in x` reads an outer `x`. The variable then
                // scopes over the body only.
                let iter = self.expr(iter)?;
                self.scopes.push(FxHashMap::default());
                let var = self.declare_local(var, *mutable);
                let body = self.block(body);
                self.scopes.pop();
                self.alloc_expr(HirExprKind::For { var, iter, body }, span)
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                let scrutinee = self.expr(scrutinee)?;
                let mut hir_arms = Vec::with_capacity(arms.len());
                for arm in arms {
                    // Each arm is its own scope: pattern bindings are
                    // visible in the arm body only, never across arms.
                    self.scopes.push(FxHashMap::default());
                    let pat = self.pattern(&arm.pat);
                    let body = self
                        .expr(&arm.body)
                        .unwrap_or_else(|| self.alloc_expr(HirExprKind::Poison, arm.span));
                    self.scopes.pop();
                    hir_arms.push(HirArm {
                        pat,
                        body,
                        span: arm.span,
                    });
                }
                self.alloc_expr(
                    HirExprKind::Match {
                        scrutinee,
                        arms: hir_arms,
                    },
                    span,
                )
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                let lhs = self.expr(lhs)?;
                let rhs = self.expr(rhs)?;
                self.alloc_expr(HirExprKind::Binary { op: *op, lhs, rhs }, span)
            }
            Expr::Unary { op, expr, .. } => {
                let inner = self.expr(expr)?;
                self.alloc_expr(
                    HirExprKind::Unary {
                        op: *op,
                        expr: inner,
                    },
                    span,
                )
            }
            Expr::If {
                cond, then, else_, ..
            } => {
                let cond = self.expr(cond)?;
                let then = self.block(then);
                let else_ = match else_.as_deref() {
                    Some(Expr::Block { block, .. }) => Some(self.block(block)),
                    Some(other) => self.expr(other),
                    None => None,
                };
                self.alloc_expr(HirExprKind::If { cond, then, else_ }, span)
            }
            Expr::Block { block, .. } => self.block(block),
            Expr::StructLit { name, fields, .. } => {
                let fields: Vec<(Name, ExprId)> = fields
                    .iter()
                    .filter_map(|f| {
                        let fname = self.name_of(&f.name);
                        self.expr(&f.value).map(|v| (fname, v))
                    })
                    .collect();
                match self.path_def(name) {
                    Ok(d) if self.scope.data_shape(d).is_some() => {
                        self.alloc_expr(HirExprKind::StructLit { def: d, fields }, span)
                    }
                    Ok(_) => {
                        self.diags.push(
                            Diagnostic::error(
                                Code::UnknownType,
                                format!("`{}` is a function, not a type", name.display()),
                            )
                            .primary(name.span)
                            .subject(name.display()),
                        );
                        self.alloc_expr(HirExprKind::Poison, span)
                    }
                    Err(e) => {
                        self.path_error(name, e, "type", Code::UnknownType);
                        self.alloc_expr(HirExprKind::Poison, span)
                    }
                }
            }
            Expr::Error { .. } => self.alloc_expr(HirExprKind::Poison, span),
        })
    }

    fn resolve_type(&mut self, ty: &TypeExpr) -> TypeRef {
        let path = match ty {
            TypeExpr::Array { elem, span } => {
                return match self.resolve_type(elem) {
                    TypeRef::Poison => TypeRef::Poison,
                    TypeRef::Array { .. } => {
                        self.diags.push(
                            Diagnostic::error(
                                Code::UnsupportedOperation,
                                "nested array types are not supported yet",
                            )
                            .primary(*span),
                        );
                        TypeRef::Poison
                    }
                    TypeRef::Unit => {
                        self.diags.push(
                            Diagnostic::error(
                                Code::UnsupportedOperation,
                                "arrays cannot hold `unit` elements",
                            )
                            .primary(*span),
                        );
                        TypeRef::Poison
                    }
                    leaf => TypeRef::Array {
                        elem: ElemRef::of(leaf).expect("leaf types are valid elements"),
                    },
                };
            }
            TypeExpr::Named { path } => path,
        };
        let segs = &path.segs;
        if segs.len() == 1 {
            let name = segs[0].name.as_str();
            match name {
                "bool" => return TypeRef::Bool,
                "i8" => return TypeRef::I8,
                "i16" => return TypeRef::I16,
                "i32" => return TypeRef::I32,
                "i64" => return TypeRef::I64,
                "isize" => return TypeRef::Isize,
                "u8" => return TypeRef::U8,
                "u16" => return TypeRef::U16,
                "u32" => return TypeRef::U32,
                "u64" => return TypeRef::U64,
                "usize" => return TypeRef::Usize,
                "f32" => return TypeRef::F32,
                "f64" => return TypeRef::F64,
                "str" => return TypeRef::Str,
                "char" => return TypeRef::Char,
                "unit" => return TypeRef::Unit,
                _ => {}
            }
        }
        match self.path_def(path) {
            Ok(d) if self.scope.data_shape(d).is_some() => TypeRef::Struct(d),
            Ok(_) => {
                self.diags.push(
                    Diagnostic::error(
                        Code::UnknownType,
                        format!("`{}` is a function, not a type", path.display()),
                    )
                    .primary(path.span)
                    .subject(path.display()),
                );
                TypeRef::Poison
            }
            Err(e) => {
                self.path_error(path, e, "type", Code::UnknownType);
                TypeRef::Poison
            }
        }
    }

    /// Lowers a match pattern inside the current (arm) scope.
    /// `T::V(b, ..)` resolves the variant and declares each payload
    /// binding (`_` slots bind nothing); a bare `x` declares a binding
    /// for the whole scrutinee and `_` is the wildcard.
    fn pattern(&mut self, pat: &Pattern) -> HirPat {
        match pat {
            Pattern::Bind { name } => {
                if name.name == "_" {
                    HirPat::Bind {
                        sym: None,
                        span: name.span,
                    }
                } else {
                    HirPat::Bind {
                        sym: Some(self.declare_local(name, false)),
                        span: name.span,
                    }
                }
            }
            Pattern::Variant { path, binds, span } => match self.resolve_variant(path) {
                Ok((def, variant)) => {
                    let binds = binds
                        .iter()
                        .map(|b| {
                            if b.name == "_" {
                                None
                            } else {
                                Some(self.declare_local(b, false))
                            }
                        })
                        .collect();
                    HirPat::Variant {
                        def,
                        variant,
                        binds,
                        span: *span,
                    }
                }
                Err(e) => {
                    self.variant_error(path, e);
                    HirPat::Poison
                }
            },
        }
    }

    /// Describes a def-level name for a better "unknown binding" message.
    fn describe_def(&self, interned: InternId) -> String {
        let env = self.env();
        if env.fns.contains_key(&interned)
            || env
                .imports
                .get(&interned)
                .is_some_and(|d| self.scope.fn_sig(*d).is_some())
        {
            " (a function, not a value)".to_string()
        } else if env.datas.contains_key(&interned)
            || env
                .imports
                .get(&interned)
                .is_some_and(|d| self.scope.data_shape(*d).is_some())
        {
            " (a type, not a value)".to_string()
        } else {
            String::new()
        }
    }
}
