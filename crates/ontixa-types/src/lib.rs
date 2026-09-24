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
pub use ty::{ElemTy, Ty};

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
    use ontixa_hir::{HirExprKind, HirPat, HirStmt};

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
    fn string_concat_and_ordering() {
        assert_eq!(
            local_ty("fn main() -> i32 { let s = \"a\" + \"b\"; return 0; }", "s"),
            Ty::Str
        );
        assert_eq!(
            local_ty("fn main() -> i32 { let b = \"a\" < \"b\"; return 0; }", "b"),
            Ty::Bool
        );
    }

    #[test]
    fn string_len_index_and_slice_types() {
        assert_eq!(
            local_ty(
                "fn main() -> i32 { let s = \"abc\"; let n = s.len; return n; }",
                "n"
            ),
            Ty::I32
        );
        assert_eq!(
            local_ty(
                "fn main() -> i32 { let s = \"abc\"; let c = s[0]; return 0; }",
                "c"
            ),
            Ty::Str
        );
        assert_eq!(
            local_ty(
                "fn main() -> i32 { let s = \"abc\"; let t = s[1..2]; return 0; }",
                "t"
            ),
            Ty::Str
        );
    }

    /// `str.len` must be rewritten to a dedicated node — never left
    /// looking like a field projection for MIR or ownership passes.
    #[test]
    fn len_becomes_strlen_node() {
        let (m, _, mut interner, diags) =
            check_src("fn main() -> i32 { let s = \"abc\"; return s.len; }");
        assert!(diags.is_empty(), "{diags:?}");
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = m.body(main).expect("body");
        let HirExprKind::Block { stmts, .. } = &body.expr(body.root).kind else {
            panic!()
        };
        let HirStmt::Return { value: Some(v), .. } = &stmts[1] else {
            panic!("expected return")
        };
        assert!(matches!(body.expr(*v).kind, HirExprKind::Len { .. }));
    }

    #[test]
    fn rejects_bad_string_ops() {
        // Indexing a non-string.
        let (_, _, _, diags) = check_src("fn main() -> i32 { let x = 5; return x[0]; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // Non-integer index.
        let (_, _, _, diags) =
            check_src("fn main() -> i32 { let s = \"a\"; let c = s[1.5]; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
        // Non-integer slice bound.
        let (_, _, _, diags) =
            check_src("fn main() -> i32 { let s = \"a\"; let c = s[true..]; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
        // Slicing a non-string.
        let (_, _, _, diags) =
            check_src("fn main() -> i32 { let x = 5; let c = x[0..1]; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // `str + i32` mismatches.
        let (_, _, _, diags) = check_src("fn main() -> i32 { let s = \"a\" + 1; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
        // `str - str` is not a thing.
        let (_, _, _, diags) = check_src("fn main() -> i32 { let s = \"a\" - \"b\"; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // `s.len` is read-only.
        let (_, _, _, diags) =
            check_src("fn main() -> i32 { let mut s = \"a\"; s.len = 3; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
    }

    // ---- arrays and `for` -------------------------------------------

    #[test]
    fn array_literal_infers_elem_and_annotation_wins() {
        assert_eq!(
            local_ty("fn main() -> i32 { let a = [1, 2, 3]; return a[0]; }", "a"),
            Ty::Array(ElemTy::I32)
        );
        // The annotation picks the element type; elements unify to it.
        assert_eq!(
            local_ty("fn main() -> i32 { let a: [i64] = [1, 2]; return 0; }", "a"),
            Ty::Array(ElemTy::I64)
        );
        // Empty literal takes its type from the annotation.
        assert_eq!(
            local_ty("fn main() -> i32 { let a: [str] = []; return a.len; }", "a"),
            Ty::Array(ElemTy::Str)
        );
    }

    #[test]
    fn array_index_len_and_slice_types() {
        assert_eq!(
            local_ty(
                "fn main() -> i32 { let a = [1, 2]; let x = a[0]; return x; }",
                "x"
            ),
            Ty::I32
        );
        assert_eq!(
            local_ty(
                "fn main() -> i32 { let a = [1, 2]; let n = a.len; return n; }",
                "n"
            ),
            Ty::I32
        );
        assert_eq!(
            local_ty(
                "fn main() -> i32 { let a = [1, 2]; let s = a[1..]; return s.len; }",
                "s"
            ),
            Ty::Array(ElemTy::I32)
        );
    }

    #[test]
    fn array_literal_must_be_homogeneous() {
        let (_, _, _, diags) = check_src("fn main() -> i32 { let a = [1, \"x\"]; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
    }

    #[test]
    fn empty_array_needs_annotation() {
        let (_, _, _, diags) = check_src("fn main() -> i32 { let a = []; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::CannotInfer));
        // One diagnostic — a poison annotation must not double-report.
        let (_, _, _, diags) = check_src("fn main() -> i32 { let a: [unit] = []; return 0; }");
        assert_eq!(
            codes(&diags)
                .iter()
                .filter(|c| **c == ontixa_diagnostics::Code::CannotInfer)
                .count(),
            0,
            "{diags:?}"
        );
    }

    #[test]
    fn rejects_bad_array_shapes() {
        // Nested arrays.
        let (_, _, _, diags) = check_src("fn main() -> i32 { let a = [[1]]; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // Non-integer index.
        let (_, _, _, diags) =
            check_src("fn main() -> i32 { let a = [1]; let x = a[true]; return x; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
        // Indexing a non-array non-string.
        let (_, _, _, diags) = check_src("fn main() -> i32 { let x = 5; return x[0]; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // `==` between arrays is not supported.
        let (_, _, _, diags) = check_src(
            "fn main() -> i32 { let a = [1]; let b = [1]; return if a == b { 1 } else { 0 }; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // `.len` is read-only.
        let (_, _, _, diags) =
            check_src("fn main() -> i32 { let mut a = [1]; a.len = 3; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
    }

    #[test]
    fn for_checks_iterable_and_scopes_var() {
        // Range loop: `i` is i32 by default, `for` is unit.
        let (_, _, _, diags) =
            check_src("fn main() -> i32 { for i in 0..3 { let z = i; } return 0; }");
        assert!(diags.is_empty(), "{diags:?}");
        // `for` over an array binds the element type.
        let t = local_ty(
            "fn main() -> i32 { let a = [\"a\"]; for x in a { let y = x; } return 0; }",
            "y",
        );
        assert_eq!(t, Ty::Str);
        // The var does not leak past the body.
        let (_, _, _, diags) = check_src("fn main() -> i32 { for i in 0..3 { } return i; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnknownSymbol));
    }

    #[test]
    fn for_rejects_bad_iterables() {
        // Not iterable.
        let (_, _, _, diags) = check_src("fn main() -> i32 { for x in 42 { } return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // `str` is not iterable (indexing exists but looping a string
        // is not an M3 feature).
        let (_, _, _, diags) = check_src("fn main() -> i32 { for x in \"ab\" { } return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // Unbounded range — there is no `break`, so this cannot
        // terminate and is rejected.
        let (_, _, _, diags) = check_src("fn main() -> i32 { for i in 0.. { } return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // `..` alone: neither bound present.
        let (_, _, _, diags) = check_src("fn main() -> i32 { for i in .. { } return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // A literal bound adopts the other bound's type — `0` becomes
        // `i64` here, so this is fine.
        let (_, _, _, diags) =
            check_src("fn main() -> i32 { let n: i64 = 3; for i in 0..n { } return 0; }");
        assert!(diags.is_empty(), "{diags:?}");
        // Two typed bounds of different integer types still mismatch.
        let (_, _, _, diags) = check_src(
            "fn main() -> i32 { let a: i32 = 0; let b: i64 = 9; for i in a..b { } return 0; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
        // Non-integer bounds.
        let (_, _, _, diags) = check_src("fn main() -> i32 { for i in 0.0..3 { } return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
    }

    #[test]
    fn range_is_only_valid_in_for() {
        let (_, _, _, diags) = check_src("fn main() -> i32 { let r = 0..3; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        let (_, _, _, diags) =
            check_src("fn main() -> i32 { return if 0..3 == 0 { 1 } else { 0 }; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
    }

    /// `a.len` rewrites to `Len` for arrays exactly like for `str`.
    #[test]
    fn array_len_becomes_len_node() {
        let (m, _, mut interner, diags) =
            check_src("fn main() -> i32 { let a = [1]; return a.len; }");
        assert!(diags.is_empty(), "{diags:?}");
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = m.body(main).expect("body");
        let HirExprKind::Block { stmts, .. } = &body.expr(body.root).kind else {
            panic!()
        };
        let HirStmt::Return { value: Some(v), .. } = &stmts[1] else {
            panic!("expected return")
        };
        assert!(matches!(body.expr(*v).kind, HirExprKind::Len { .. }));
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

    // ---- enum `data` and `match` ------------------------------------

    #[test]
    fn variant_construction_types_to_enum() {
        let (m, tables, mut interner, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(1); return 0; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let opt = m.scope.root_env().datas[&interner.intern("Opt")];
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
            Ty::Struct(opt)
        );
    }

    #[test]
    fn match_binds_payload_and_unifies_result() {
        let (m, tables, mut interner, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(1); return match o { Opt::Some(v) => v, Opt::None => 0 }; }",
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
        let HirExprKind::Match { arms, .. } = &body.expr(*v).kind else {
            panic!("expected match")
        };
        let HirPat::Variant { binds, .. } = &arms[0].pat else {
            panic!("expected variant pattern")
        };
        let bind = binds[0].expect("binding");
        assert_eq!(
            tables[main.index()].as_ref().unwrap().local_types[&bind],
            Ty::I32
        );
        assert_eq!(tables[main.index()].as_ref().unwrap().ty_of(*v), Ty::I32);
    }

    #[test]
    fn match_requires_exhaustive_arms() {
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(1); return match o { Opt::Some(v) => v }; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::NonExhaustive));
        // A catch-all arm makes it exhaustive again.
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(1); return match o { Opt::Some(v) => v, _ => 0 }; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn match_rejects_bad_scrutinees() {
        // Record `data` is not matchable.
        let (_, _, _, diags) = check_src(
            "data P { x: i32; } fn main() -> i32 { let p = P { x: 1 }; return match p { _ => 0 }; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // Primitive scrutinee.
        let (_, _, _, diags) = check_src("fn main() -> i32 { return match 5 { _ => 0 }; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // Variant of a different enum mismatches the scrutinee.
        let (_, _, _, diags) = check_src(
            "data A { X; } data B { Y; } fn main() -> i32 { let a = A::X; return match a { B::Y => 0, _ => 1 }; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
    }

    #[test]
    fn match_checks_pattern_arity() {
        // Too many payload bindings.
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(1); return match o { Opt::Some(a, b) => a, Opt::None => 0 }; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::ArgCount));
        // Too few.
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(1); return match o { Opt::Some => 0, Opt::None => 1 }; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::ArgCount));
    }

    #[test]
    fn match_arm_bodies_must_unify() {
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(1); return match o { Opt::Some(v) => v, Opt::None => true }; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
    }

    #[test]
    fn unreachable_arms_warn() {
        // After a catch-all, every later arm is dead.
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(1); return match o { _ => 0, Opt::None => 1 }; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnreachableArm));
        // A repeated variant arm is dead too.
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(1); return match o { Opt::Some(a) => a, Opt::Some(b) => b, Opt::None => 0 }; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnreachableArm));
    }

    #[test]
    fn variant_constructor_checks_arity_and_types() {
        // Wrong arity.
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(); return 0; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::ArgCount));
        // Wrong payload type.
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(\"x\"); return 0; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
        // Unit variants take no call parens — `Opt::None` alone is a value.
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::None; return match o { Opt::Some(v) => v, Opt::None => 0 }; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }

    #[test]
    fn enum_cannot_be_record_constructed() {
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt { x: 1 }; return 0; }",
        );
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
    }

    // ---- `char` ------------------------------------------------------

    #[test]
    fn char_literal_types_and_compares() {
        assert_eq!(
            local_ty("fn main() -> i32 { let c = 'x'; return 0; }", "c"),
            Ty::Char
        );
        assert_eq!(
            local_ty("fn main() -> i32 { let c: char = '\\n'; return 0; }", "c"),
            Ty::Char
        );
        // Equality and scalar ordering.
        let (_, _, _, diags) =
            check_src("fn main() -> i32 { let a = 'a' < 'z'; let b = 'x' == 'x'; return 0; }");
        assert!(diags.is_empty(), "{diags:?}");
        // Arrays of char and `for` binding.
        assert_eq!(
            local_ty(
                "fn main() -> i32 { let a = ['a', 'b']; for c in a { let y = c; } return 0; }",
                "y"
            ),
            Ty::Char
        );
    }

    #[test]
    fn char_rejects_bad_ops_and_arity() {
        // No arithmetic on chars.
        let (_, _, _, diags) = check_src("fn main() -> i32 { let c = 'a' + 'b'; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::UnsupportedOperation));
        // `char` vs `str` mismatches.
        let (_, _, _, diags) = check_src("fn main() -> i32 { let c: char = \"a\"; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
        let (_, _, _, diags) = check_src("fn main() -> i32 { let s: str = 'a'; return 0; }");
        assert!(codes(&diags).contains(&ontixa_diagnostics::Code::TypeMismatch));
        // Empty and multi-character literals.
        let (_, _, _, diags) = check_src("fn main() -> i32 { let c = ''; return 0; }");
        assert!(diags.has_errors());
        let (_, _, _, diags) = check_src("fn main() -> i32 { let c = 'ab'; return 0; }");
        assert!(diags.has_errors());
    }

    #[test]
    fn exhaustive_match_of_returns_diverges() {
        // Every arm returns — the function needs no tail.
        let (_, _, _, diags) = check_src(
            "data Opt { Some(i32); None; } fn f(o: Opt) -> i32 { match o { Opt::Some(v) => { return v; }, Opt::None => { return 0; } } } fn main() -> i32 { return f(Opt::None); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
    }
}
