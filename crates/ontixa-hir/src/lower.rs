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
    HirBody, HirExpr, HirExprKind, HirModule, HirPlace, HirStmt, ModuleScope, Name, Symbol,
    SymbolKind, SymbolTable, TypeRef,
};
use ontixa_ast::{AstModule, Block, Expr, Ident, Item, Place, Stmt, TypeExpr};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_source::{DefId, ExprId, InternId, Interner, Span, SymbolId};
use rustc_hash::FxHashMap;

/// Lowers all function bodies in `ast` against the resolved `scope`,
/// producing the complete [`HirModule`].
pub fn lower_bodies(
    ast: &AstModule,
    mut scope: ModuleScope,
    interner: &mut Interner,
    diags: &mut Diagnostics,
) -> HirModule {
    let mut exprs: Vec<HirExpr> = Vec::new();
    let mut bodies: Vec<Option<HirBody>> = (0..scope.defs.len()).map(|_| None).collect();

    // Local symbols are appended to the module table during lowering.
    // The table is moved out so each body lowerer can hold `&mut` to it
    // while keeping an immutable view of the rest of the scope.
    let mut symbol_table = std::mem::take(&mut scope.symbols);

    for (idx, item) in ast.items.iter().enumerate() {
        let Item::Fn(f) = item else { continue };
        let def = DefId::new(idx as u32);
        let mut b = BodyLowerer {
            def,
            scope: &scope,
            symbols: &mut symbol_table,
            interner,
            diags,
            exprs: &mut exprs,
            locals: Vec::new(),
            scopes: vec![FxHashMap::default()],
        };
        // Bind parameters into the outermost scope. `sig.params[i]`
        // corresponds to `f.params[i]` by construction.
        if let Some(sig) = scope.fn_sig(def) {
            for (i, p) in f.params.iter().enumerate() {
                let interned = b.interner.intern(&p.name.name);
                if let Some(param) = sig.params.get(i) {
                    b.scopes[0].insert(interned, param.symbol);
                }
            }
        }
        let root = b.block(&f.body);
        bodies[idx] = Some(HirBody {
            def,
            root,
            locals: b.locals,
        });
    }

    scope.symbols = symbol_table;
    HirModule {
        scope,
        exprs,
        bodies,
    }
}

struct BodyLowerer<'a> {
    def: DefId,
    scope: &'a ModuleScope,
    symbols: &'a mut SymbolTable,
    interner: &'a mut Interner,
    diags: &'a mut Diagnostics,
    exprs: &'a mut Vec<HirExpr>,
    locals: Vec<SymbolId>,
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

    fn declare_local(&mut self, ident: &Ident, mutable: bool) -> SymbolId {
        let interned = self.interner.intern(&ident.name);
        let sym = Symbol {
            id: SymbolId::new(0),
            name: interned,
            kind: SymbolKind::Local,
            mutable,
            owner: Some(self.def),
            span: ident.span,
        };
        let id = self.symbols.push(sym);
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(interned, id);
        }
        self.locals.push(id);
        id
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
                let interned = self.interner.intern(&callee.name);
                let args: Vec<ExprId> = args.iter().filter_map(|a| self.expr(a)).collect();
                match self.scope.fns.get(&interned) {
                    Some(def) => self.alloc_expr(HirExprKind::Call { def: *def, args }, span),
                    None => {
                        let code = if self.scope.datas.contains_key(&interned)
                            || self.lookup(interned).is_some()
                        {
                            Code::NotCallable
                        } else {
                            Code::UnknownSymbol
                        };
                        self.diags.push(
                            Diagnostic::error(code, format!("`{}` is not a function", callee.name))
                                .primary(callee.span)
                                .subject(callee.name.clone()),
                        );
                        self.alloc_expr(HirExprKind::Poison, span)
                    }
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
                let interned = self.interner.intern(&name.name);
                let fields: Vec<(Name, ExprId)> = fields
                    .iter()
                    .filter_map(|f| {
                        let fname = self.name_of(&f.name);
                        self.expr(&f.value).map(|v| (fname, v))
                    })
                    .collect();
                match self.scope.datas.get(&interned) {
                    Some(def) => {
                        self.alloc_expr(HirExprKind::StructLit { def: *def, fields }, span)
                    }
                    None => {
                        self.diags.push(
                            Diagnostic::error(
                                Code::UnknownType,
                                format!("unknown type `{}`", name.name),
                            )
                            .primary(name.span)
                            .subject(name.name.clone()),
                        );
                        self.alloc_expr(HirExprKind::Poison, span)
                    }
                }
            }
            Expr::Error { .. } => self.alloc_expr(HirExprKind::Poison, span),
        })
    }

    fn resolve_type(&mut self, ty: &TypeExpr) -> TypeRef {
        let name = ty.name.name.as_str();
        match name {
            "bool" => TypeRef::Bool,
            "i32" => TypeRef::I32,
            "i64" => TypeRef::I64,
            "u32" => TypeRef::U32,
            "u64" => TypeRef::U64,
            "f32" => TypeRef::F32,
            "f64" => TypeRef::F64,
            "str" => TypeRef::Str,
            "unit" => TypeRef::Unit,
            _ => {
                let interned = self.interner.intern(name);
                match self.scope.datas.get(&interned) {
                    Some(def) => TypeRef::Struct(*def),
                    None => {
                        self.diags.push(
                            Diagnostic::error(Code::UnknownType, format!("unknown type `{name}`"))
                                .primary(ty.name.span)
                                .subject(name.to_string()),
                        );
                        TypeRef::Poison
                    }
                }
            }
        }
    }

    /// Describes a def-level name for a better "unknown binding" message.
    fn describe_def(&self, interned: InternId) -> String {
        if self.scope.fns.contains_key(&interned) {
            " (a function, not a value)".to_string()
        } else if self.scope.datas.contains_key(&interned) {
            " (a type, not a value)".to_string()
        } else {
            String::new()
        }
    }
}
