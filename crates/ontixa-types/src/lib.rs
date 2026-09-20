//! Type representation and the milestone-1 type checker.
//!
//! Pipeline position:
//!
//! ```text
//! HirModule ──▶ check_module ──▶ TypeTables (+ Field.field filled)
//! ```
//!
//! The checker is *total*: every expression gets a [`Ty`], even if it
//! is `Poison`. Downstream passes (ownership inference, MIR) consume
//! [`TypeTables`] and never reason about untyped syntax.

mod check;
mod ty;

pub use check::{ModuleTypes, TypeTables, check_body, check_module};
pub use ty::Ty;

/// Parses, lowers, resolves, and type-checks `src` end-to-end.
/// Returns the HIR module (with field indices filled), the per-body
/// type tables, the interner, and all diagnostics. Convenience for
/// tests and the CLI.
pub fn check_src(
    src: &str,
) -> (
    ontixa_hir::HirModule,
    ModuleTypes,
    ontixa_source::Interner,
    ontixa_diagnostics::Diagnostics,
) {
    let (ast, mut module, interner, mut diags) = ontixa_hir::parse_hir_ast(src);
    let tables = check_module(&mut module, &interner, &mut diags);
    // `check_body` emitted item-relative diagnostics tagged with
    // their owning def — rebase to file-absolute for callers.
    diags.rebase_tagged(|d| ast.items[d.index()].span().start);
    (module, tables, interner, diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ontixa_hir::{HirExprKind, HirStmt};

    /// Finds the `let` binding named `name` in `main` and returns its
    /// inferred type.
    fn local_ty(src: &str, name: &str) -> Ty {
        let (m, tables, mut interner, diags) = check_src(src);
        assert!(diags.is_empty(), "{diags:?}");
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = m.body(main).expect("body");
        let sym = body
            .local_ids()
            .find(|s| body.local_symbols[s.local_index()].name == interner.intern(name))
            .expect("local not found");
        *tables[main.index()]
            .as_ref()
            .unwrap()
            .local_types
            .get(&sym)
            .expect("local type")
    }

    fn codes(diags: &ontixa_diagnostics::Diagnostics) -> Vec<ontixa_diagnostics::Code> {
        diags.iter().map(|d| d.code).collect()
    }

    #[test]
    fn checks_arithmetic_and_defaults_i32() {
        let t = local_ty("fn main() -> i32 { let x = 1 + 2; return x; }", "x");
        assert_eq!(t, Ty::I32);
    }

    #[test]
    fn literal_adopts_annotation() {
        let t = local_ty("fn main() -> i64 { let x: i64 = 5; return x; }", "x");
        assert_eq!(t, Ty::I64);
    }

    #[test]
    fn reports_type_mismatch() {
        let (_, _, _, diags) = check_src("fn main() -> i32 { let x: i32 = true; return x; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
    }

    #[test]
    fn reports_arg_count() {
        let (_, _, _, diags) = check_src(
            "fn g(a: i32, b: i32) -> i32 { return a; } fn main() -> i32 { return g(1); }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::ArgCount));
    }

    #[test]
    fn checks_field_access_and_fills_index() {
        let (m, tables, mut interner, diags) = check_src(
            "data P { x: i32; y: i32; } fn main() -> i32 { let p = P { x: 1, y: 2 }; return p.y; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = m.body(main).expect("body");
        let HirExprKind::Block { stmts, .. } = &body.expr(body.root).kind else {
            panic!()
        };
        let HirStmt::Return { value: Some(v), .. } = &stmts[1] else {
            panic!("expected return")
        };
        // `p.y` resolved to declared field index 1.
        let HirExprKind::Field { field: Some(1), .. } = &body.expr(*v).kind else {
            panic!("expected resolved field, got {:?}", body.expr(*v).kind)
        };
        assert_eq!(tables[main.index()].as_ref().unwrap().ty_of(*v), Ty::I32);
    }

    #[test]
    fn reports_unknown_field() {
        let (_, _, _, diags) =
            check_src("data P { x: i32; } fn main() -> i32 { let p = P { x: 1 }; return p.z; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnknownField));
    }

    #[test]
    fn reports_missing_and_extra_fields() {
        let (_, _, _, diags) = check_src(
            "data P { x: i32; y: i32; } fn main() -> i32 { let p = P { x: 1 }; return 0; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::MissingField));

        let (_, _, _, diags) = check_src(
            "data P { x: i32; } fn main() -> i32 { let p = P { x: 1, z: 2 }; return 0; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnknownField));
    }

    #[test]
    fn reports_missing_return() {
        let (_, _, _, diags) =
            check_src("fn f() -> i32 { let x = 1; } fn main() -> i32 { return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::MissingReturn));
    }

    #[test]
    fn accepts_diverging_body_without_tail() {
        let (_, _, _, diags) =
            check_src("fn f() -> i32 { return 1; } fn main() -> i32 { return 0; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn if_without_else_is_unit() {
        let (_, _, _, diags) = check_src("fn main() -> i32 { if true { return 1; } return 0; }");
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn reports_bad_operand_types() {
        let (_, _, _, diags) = check_src("fn main() -> i32 { let x = true + 1; return 0; }");
        assert!(
            codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation)
                || codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch)
        );
    }

    #[test]
    fn struct_value_type_flows_through_locals() {
        let (m, tables, mut interner, diags) = check_src(
            "data P { x: i32; } fn id(p: P) -> P { return p; } fn main() -> i32 { let q = id(P { x: 1 }); return q.x; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let p_def = m.scope.root_env().datas[&interner.intern("P")];
        let main = m.scope.root_env().fns[&interner.intern("main")];
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
        assert_eq!(
            tables[main.index()].as_ref().unwrap().ty_of(*init),
            Ty::Struct(p_def)
        );
    }
}
