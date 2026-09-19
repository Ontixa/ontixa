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
    BinOp, DataShape, Def, DefKind, FieldDef, FnSig, HirBody, HirExpr, HirExprKind, HirModule,
    HirPlace, HirStmt, LitValue, Literal, ModuleScope, Name, ParamDef, Symbol, SymbolKind,
    SymbolTable, TypeRef, UnOp,
};
pub use lower::{lower_bodies, lower_body};
pub use resolve::resolve_module;

/// Lowers an [`ontixa_ast::AstModule`] to a fully resolved
/// [`HirModule`], appending name-resolution diagnostics to `diags`.
pub fn lower_hir(
    ast: &ontixa_ast::AstModule,
    interner: &mut ontixa_source::Interner,
    diags: &mut ontixa_diagnostics::Diagnostics,
) -> HirModule {
    let scope = resolve_module(ast, interner, diags);
    lower_bodies(ast, scope, interner, diags)
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
    let (ast, mut diags) = ontixa_ast::parse_ast(src);
    let mut interner = ontixa_source::Interner::new();
    let module = lower_hir(&ast, &mut interner, &mut diags);
    (module, interner, diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ontixa_source::{DefId, Interner};

    fn fn_def(m: &HirModule, name: &str, interner: &mut Interner) -> DefId {
        let id = interner.intern(name);
        *m.scope.fns.get(&id).expect("function not resolved")
    }

    #[test]
    fn resolves_fn_and_data_defs() {
        let (m, mut interner, diags) = parse_hir(
            "data P { x: i32; } fn f(p: P) -> i32 { return p.x; } fn main() -> i32 { return 0; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(m.scope.datas.contains_key(&interner.intern("P")));
        assert!(m.scope.fns.contains_key(&interner.intern("f")));
        assert!(m.scope.fns.contains_key(&interner.intern("main")));
        assert_eq!(m.scope.defs.len(), 3);
    }

    #[test]
    fn resolves_param_and_field_types() {
        let (m, mut interner, _) =
            parse_hir("data P { x: i32; } fn f(p: P) -> i32 { return p.x; }");
        let f = fn_def(&m, "f", &mut interner);
        let sig = m.scope.fn_sig(f).expect("fn sig");
        let p_def = m.scope.datas[&interner.intern("P")];
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
        assert_eq!(*def, m.scope.datas[&interner.intern("P")]);
        assert_eq!(fields.len(), 1);
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
}
