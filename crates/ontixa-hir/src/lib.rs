//! The High-level IR: resolved names, symbols, and definition tables.
//!
//! Pipeline position:
//!
//! ```text
//! AstModule ──▶ resolve_module ──▶ ModuleScope ──▶ lower_bodies ──▶ HirModule
//! ```
//!
//! HIR is the boundary between *syntax* and *semantics*. Everything
//! downstream (type checking, ownership inference, the semantic graph)
//! works on resolved [`SymbolId`]s and [`DefId`]s rather than text, so
//! later passes never re-resolve names or guess at intent.
//!
//! Resolution rules for milestone 1:
//!
//! - One module per file; no imports.
//! - `fn` and `data` share one top-level namespace; first definition
//!   wins on duplicates (a diagnostic is emitted).
//! - Type positions resolve to a builtin primitive or a `data` def.
//! - Inside bodies, `let` bindings and params are lexically scoped;
//!   calls resolve only to `fn` defs, struct literals only to `data`
//!   defs — a value position never silently fabricates a def.
//! - Unresolved names produce a diagnostic plus a `Poison` node so the
//!   pipeline can keep running and report more errors in one pass.

mod hir;
mod lower;
mod resolve;

pub use hir::{
    BinOp, DataShape, Def, DefKind, ElemRef, FieldDef, FileEnv, FnSig, HirBody, HirExpr,
    HirExprKind, HirModule, HirPlace, HirStmt, LitValue, Literal, ModuleScope, Name, ParamDef,
    Symbol, SymbolKind, SymbolTable, TypeRef, UnOp,
};
pub use lower::{lower_bodies, lower_body};
pub use resolve::{WorkspaceFile, resolve_module, resolve_workspace};

/// Lowers an [`ontixa_ast::AstModule`] to a fully resolved
/// [`HirModule`], appending name-resolution diagnostics to `diags`.
///
/// Per-definition passes emit item-relative diagnostics tagged with
/// their owning def; this whole-module path rebases them to
/// file-absolute before returning, matching what the database's
/// `Diagnostics(file)` query produces.
pub fn lower_hir(
    ast: &ontixa_ast::AstModule,
    interner: &mut ontixa_source::Interner,
    diags: &mut ontixa_diagnostics::Diagnostics,
) -> HirModule {
    let scope = resolve_module(ast, interner, diags);
    let module = lower_bodies(ast, scope, interner, diags);
    // `Def.item` locates the owning item — its absolute start is the
    // rebase base for that def's tagged diagnostics.
    diags.rebase_tagged(|d| ast.items[module.scope.def(d).item as usize].span().start);
    module
}

/// Parses and lowers `src` end-to-end (lex → parse → AST → HIR),
/// returning the HIR module, the [`Interner`] that holds every name
/// referenced by the module's `InternId`s, and all diagnostics
/// gathered along the way. Convenience for tests and the CLI.
pub fn parse_hir(
    src: &str,
) -> (
    HirModule,
    ontixa_source::Interner,
    ontixa_diagnostics::Diagnostics,
) {
    let (_, module, interner, diags) = parse_hir_ast(src);
    (module, interner, diags)
}

/// Same as [`parse_hir`] but also returns the parsed AST. Downstream
/// convenience pipelines (`check_src`, `analyze_src`) keep it alive
/// so they can rebase item-relative diagnostics emitted by the
/// per-definition passes.
pub fn parse_hir_ast(
    src: &str,
) -> (
    ontixa_ast::AstModule,
    HirModule,
    ontixa_source::Interner,
    ontixa_diagnostics::Diagnostics,
) {
    let (ast, mut diags) = ontixa_ast::parse_ast(src);
    let mut interner = ontixa_source::Interner::new();
    let module = lower_hir(&ast, &mut interner, &mut diags);
    (ast, module, interner, diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ontixa_source::{DefId, Interner};

    fn fn_def(m: &HirModule, name: &str, interner: &mut Interner) -> DefId {
        let id = interner.intern(name);
        *m.scope
            .root_env()
            .fns
            .get(&id)
            .expect("function not resolved")
    }

    #[test]
    fn resolves_fn_and_data_defs() {
        let (m, mut interner, diags) = parse_hir(
            "data P { x: i32; } fn f(p: P) -> i32 { return p.x; } fn main() -> i32 { return 0; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let env = m.scope.root_env();
        assert!(env.datas.contains_key(&interner.intern("P")));
        assert!(env.fns.contains_key(&interner.intern("f")));
        assert!(env.fns.contains_key(&interner.intern("main")));
        assert_eq!(m.scope.defs.len(), 3);
    }

    #[test]
    fn resolves_param_and_field_types() {
        let (m, mut interner, _) =
            parse_hir("data P { x: i32; } fn f(p: P) -> i32 { return p.x; }");
        let f = fn_def(&m, "f", &mut interner);
        let sig = m.scope.fn_sig(f).expect("fn sig");
        let p_def = m.scope.root_env().datas[&interner.intern("P")];
        assert_eq!(sig.params[0].ty, TypeRef::Struct(p_def));
        assert_eq!(sig.ret, TypeRef::I32);
        let shape = m.scope.data_shape(p_def).expect("data shape");
        assert_eq!(shape.fields.len(), 1);
        assert_eq!(shape.fields[0].ty, TypeRef::I32);
    }

    #[test]
    fn resolves_calls_and_locals() {
        let (m, mut interner, diags) = parse_hir(
            "fn g(x: i32) -> i32 { return x + 1; } fn main() -> i32 { let y = g(2); return y; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let main = fn_def(&m, "main", &mut interner);
        let body = m.body(main).expect("body");
        let root = body.expr(body.root);
        let HirExprKind::Block { stmts, .. } = &root.kind else {
            panic!("expected block root")
        };
        let HirStmt::Let {
            init: Some(init), ..
        } = &stmts[0]
        else {
            panic!("expected let")
        };
        let HirExprKind::Call { def, args } = &body.expr(*init).kind else {
            panic!("expected call")
        };
        assert_eq!(*def, fn_def(&m, "g", &mut interner));
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn resolves_struct_literal_to_data_def() {
        let (m, mut interner, diags) =
            parse_hir("data P { x: i32; } fn main() -> i32 { let p = P { x: 1 }; return p.x; }");
        assert!(diags.is_empty(), "{diags:?}");
        let main = fn_def(&m, "main", &mut interner);
        let body = m.body(main).expect("body");
        let HirExprKind::Block { stmts, .. } = &body.expr(body.root).kind else {
            panic!()
        };
        let HirStmt::Let {
            init: Some(init), ..
        } = &stmts[0]
        else {
            panic!()
        };
        let HirExprKind::StructLit { def, fields } = &body.expr(*init).kind else {
            panic!("expected struct lit")
        };
        assert_eq!(*def, m.scope.root_env().datas[&interner.intern("P")]);
        assert_eq!(fields.len(), 1);
    }

    #[test]
    fn lowers_index_and_slice_exprs() {
        let (m, mut interner, diags) = parse_hir("fn f(s: str) -> str { return s[0] + s[1..]; }");
        assert!(diags.is_empty(), "{diags:?}");
        let f = fn_def(&m, "f", &mut interner);
        let body = m.body(f).expect("body");
        let HirExprKind::Block { stmts, .. } = &body.expr(body.root).kind else {
            panic!()
        };
        let HirStmt::Return { value: Some(v), .. } = &stmts[0] else {
            panic!("expected return")
        };
        let HirExprKind::Binary { lhs, rhs, .. } = &body.expr(*v).kind else {
            panic!("expected binary")
        };
        assert!(matches!(body.expr(*lhs).kind, HirExprKind::Index { .. }));
        let HirExprKind::Slice { lo, hi, .. } = &body.expr(*rhs).kind else {
            panic!("expected slice")
        };
        assert!(lo.is_some() && hi.is_none());
    }

    #[test]
    fn reports_unknown_binding() {
        let (_, _, diags) = parse_hir("fn main() -> i32 { return nope; }");
        assert_eq!(diags.iter().count(), 1);
        let d = diags.iter().next().unwrap();
        assert_eq!(d.code, ontixa_diagnostics::Code::UnknownSymbol);
        assert_eq!(d.subject.as_deref(), Some("nope"));
    }

    #[test]
    fn reports_unknown_type() {
        let (_, _, diags) = parse_hir("fn f(x: Missing) -> i32 { return 0; }");
        assert!(
            diags
                .iter()
                .any(|d| d.code == ontixa_diagnostics::Code::UnknownType)
        );
    }

    #[test]
    fn reports_duplicate_defs() {
        let (_, _, diags) = parse_hir("fn f() -> i32 { return 0; } fn f() -> i32 { return 1; }");
        assert!(
            diags
                .iter()
                .any(|d| d.code == ontixa_diagnostics::Code::DuplicateDef)
        );
    }

    #[test]
    fn reports_calling_a_value() {
        let (_, _, diags) = parse_hir("fn main() -> i32 { let x = 1; return x(2); }");
        assert!(
            diags
                .iter()
                .any(|d| d.code == ontixa_diagnostics::Code::NotCallable)
        );
    }

    #[test]
    fn lexical_scoping_shadows_correctly() {
        let (m, mut interner, diags) = parse_hir(
            "fn main() -> i32 { let x = 1; { let x = 2; let y = x; } let z = x; return z; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let main = fn_def(&m, "main", &mut interner);
        let body = m.body(main).expect("body");
        // Two distinct `x` locals plus `y` and `z` → 4 locals.
        assert_eq!(body.local_ids().count(), 4);
        let names: Vec<_> = body
            .local_symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Local)
            .map(|s| s.name)
            .collect();
        let x = interner.intern("x");
        let xs: Vec<_> = names.iter().filter(|n| **n == x).collect();
        assert_eq!(xs.len(), 2);
    }

    #[test]
    fn struct_lit_unknown_type_reports() {
        let (_, _, diags) = parse_hir("fn main() -> i32 { let p = Q { x: 1 }; return 0; }");
        assert!(
            diags
                .iter()
                .any(|d| d.code == ontixa_diagnostics::Code::UnknownType)
        );
    }

    #[test]
    fn resolves_array_type_refs() {
        let (m, mut interner, diags) =
            parse_hir("data P { x: i32; } fn f(a: [i64], b: [P]) -> [i32] { return a[0..]; }");
        assert!(diags.is_empty(), "{diags:?}");
        let f = fn_def(&m, "f", &mut interner);
        let sig = m.scope.fn_sig(f).expect("fn sig");
        assert_eq!(sig.params[0].ty, TypeRef::Array { elem: ElemRef::I64 });
        let p = m.scope.root_env().datas[&interner.intern("P")];
        assert_eq!(
            sig.params[1].ty,
            TypeRef::Array {
                elem: ElemRef::Struct(p)
            }
        );
        assert_eq!(sig.ret, TypeRef::Array { elem: ElemRef::I32 });
    }

    #[test]
    fn rejects_bad_element_types() {
        // Nested arrays and arrays of `unit` are rejected at resolution.
        let (_, _, diags) = parse_hir("fn f(a: [[i32]]) -> i32 { return 0; }");
        assert!(diags.has_errors());
        let (_, _, diags) = parse_hir("fn f(a: [unit]) -> i32 { return 0; }");
        assert!(diags.has_errors());
        let (_, _, diags) = parse_hir("fn f(a: [Nope]) -> i32 { return 0; }");
        assert!(
            diags
                .iter()
                .any(|d| d.code == ontixa_diagnostics::Code::UnknownType)
        );
    }

    #[test]
    fn lowers_array_for_range_nodes() {
        let (m, mut interner, diags) = parse_hir(
            "fn f() -> i32 { let a = [1, 2]; for i in 0..a.len { } for x in a { } return 0; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let f = fn_def(&m, "f", &mut interner);
        let body = m.body(f).expect("body");
        let HirExprKind::Block { stmts, .. } = &body.expr(body.root).kind else {
            panic!()
        };
        let HirStmt::Let {
            init: Some(init), ..
        } = &stmts[0]
        else {
            panic!("expected let")
        };
        assert!(matches!(
            body.expr(*init).kind,
            HirExprKind::ArrayLit { .. }
        ));
        for s in &stmts[1..3] {
            let HirStmt::Expr { expr, .. } = s else {
                panic!("expected expr stmt")
            };
            let HirExprKind::For { iter, .. } = &body.expr(*expr).kind else {
                panic!("expected for, got {:?}", body.expr(*expr).kind)
            };
            match &body.expr(*iter).kind {
                HirExprKind::Range {
                    lo: Some(_),
                    hi: Some(_),
                } => {}
                HirExprKind::Var(_) => {}
                other => panic!("unexpected iterable kind {other:?}"),
            }
        }
    }

    /// The loop variable is a fresh `Local` symbol scoped to the loop:
    /// `x` does not leak past the body and shadows an outer `x`.
    #[test]
    fn for_var_is_loop_scoped() {
        let (m, mut interner, diags) = parse_hir(
            "fn f() -> i32 { let x = 9; for x in 0..2 { let y = x; } let z = x; return z; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let f = fn_def(&m, "f", &mut interner);
        let body = m.body(f).expect("body");
        // locals: outer x, loop x, y, z → the loop `x` is a distinct
        // symbol even though it shadows the outer binding.
        let x = interner.intern("x");
        let xs: Vec<_> = body.local_symbols.iter().filter(|s| s.name == x).collect();
        assert_eq!(xs.len(), 2);
        // `let z = x` must see the *outer* x, not the loop's.
        let HirExprKind::Block { stmts, .. } = &body.expr(body.root).kind else {
            panic!()
        };
        let HirStmt::Let {
            init: Some(init), ..
        } = &stmts[2]
        else {
            panic!("expected let z")
        };
        let HirExprKind::Var(sym) = &body.expr(*init).kind else {
            panic!("expected var")
        };
        assert_eq!(*sym, xs[0].id);
    }

    /// `for mut` marks the loop symbol mutable; plain `for` does not.
    #[test]
    fn for_mut_marks_symbol_mutable() {
        let (m, mut interner, diags) =
            parse_hir("fn f() -> i32 { for i in 0..1 { } for mut j in 0..1 { } return 0; }");
        assert!(diags.is_empty(), "{diags:?}");
        let f = fn_def(&m, "f", &mut interner);
        let body = m.body(f).expect("body");
        let i = interner.intern("i");
        let j = interner.intern("j");
        let sym = |n| {
            body.local_symbols
                .iter()
                .find(|s| s.name == n)
                .expect("loop var")
        };
        assert!(!sym(i).mutable);
        assert!(sym(j).mutable);
    }
}
