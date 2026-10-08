//! Numeric contexts reach arithmetic leaves without coercing typed values.
use ontixa_diagnostics::Code;
use ontixa_hir::HirExprKind;
use ontixa_types::{Ty, check_src};

fn codes(src: &str) -> Vec<Code> {
    let (_, _, _, diags) = check_src(src);
    diags.iter().map(|d| d.code).collect()
}

#[test]
fn arithmetic_adopts_each_numeric_return_type_through_nested_operators() {
    for (name, expected, expression) in [
        ("i8", Ty::I8, "(40 + 4) * 2 / 2 - 5 % 3"),
        ("i16", Ty::I16, "(40 + 4) * 2 / 2 - 5 % 3"),
        ("i32", Ty::I32, "(40 + 4) * 2 / 2 - 5 % 3"),
        ("i64", Ty::I64, "(40 + 4) * 2 / 2 - 5 % 3"),
        ("isize", Ty::Isize, "(40 + 4) * 2 / 2 - 5 % 3"),
        ("u8", Ty::U8, "(40 + 4) * 2 / 2 - 5 % 3"),
        ("u16", Ty::U16, "(40 + 4) * 2 / 2 - 5 % 3"),
        ("u32", Ty::U32, "(40 + 4) * 2 / 2 - 5 % 3"),
        ("u64", Ty::U64, "(40 + 4) * 2 / 2 - 5 % 3"),
        ("usize", Ty::Usize, "(40 + 4) * 2 / 2 - 5 % 3"),
        ("f32", Ty::F32, "(40.0 + 4.0) * 2.0 / 2.0 - 5.0 % 3.0"),
        ("f64", Ty::F64, "(40.0 + 4.0) * 2.0 / 2.0 - 5.0 % 3.0"),
    ] {
        let src = format!("fn f() -> {name} {{ {expression} }}");
        let (module, tables, _, diags) = check_src(&src);
        assert!(diags.is_empty(), "{src}: {diags:?}");
        let body = module.bodies.iter().flatten().next().unwrap();
        let types = tables[body.def.index()].as_ref().unwrap();
        for expr in &body.exprs {
            assert_eq!(types.ty_of(expr.id), expected, "{src}: {expr:?}");
        }
    }
}

#[test]
fn context_reaches_each_value_position() {
    for src in [
        "fn f() -> i64 { return 40 + 2; }",
        "fn f() -> i64 { let x: i64 = 40 + 2; x }",
        "fn f() -> i64 { let mut x: i64 = 0; x = 40 + 2; x }",
        "fn take(x: i64) -> i64 { x } fn f() -> i64 { take(40 + 2) }",
        "data P { x: i64; } fn f() -> i64 { let p = P { x: 40 + 2 }; p.x }",
        "data P { x: i64; } fn f() -> i64 { let mut p = P { x: 0 }; p.x = 40 + 2; p.x }",
        "data O { Some(i64); None; } fn f() -> O { O::Some(40 + 2) }",
        "fn f() -> [i64] { [40 + 2, 6 * 7] }",
        "fn f(c: bool) -> i64 { if c { 40 + 2 } else { 6 * 7 } }",
        "fn f(c: bool) -> i64 { if c { return 40 + 2; } else { 6 * 7 } }",
        "fn f(c: bool) -> i8 { if c { if c { return -128 + 1; } else { 6 * 7 } } else { -(40 + 2) } }",
        "data O { A; B; } fn f(o: O) -> i64 { match o { O::A => 40 + 2, O::B => 6 * 7 } }",
        "fn f() -> i64 { { { 40 + 2 } } }",
        "fn f() -> f32 { -(40.0 + 2.0) }",
        "fn f() -> f32 { -42.0 }",
        "fn f() -> i8 { -(40 + 2) }",
        "fn f() -> i8 { -128 + 1 }",
    ] {
        assert!(codes(src).is_empty(), "{src}: {:?}", codes(src));
    }
}

#[test]
fn unannotated_arithmetic_keeps_its_default_types() {
    for (src, expected) in [
        ("fn f() { let x = 40 + 2; }", Ty::I32),
        ("fn f() { let x = -(40.0 + 2.0); }", Ty::F64),
    ] {
        let (module, tables, _, diags) = check_src(src);
        assert!(diags.is_empty(), "{diags:?}");
        let body = module.bodies.iter().flatten().next().unwrap();
        let types = tables[body.def.index()].as_ref().unwrap();
        for expr in &body.exprs {
            if matches!(
                expr.kind,
                HirExprKind::Binary { .. } | HirExprKind::Unary { .. }
            ) {
                assert_eq!(types.ty_of(expr.id), expected);
            }
        }
    }
}

#[test]
fn literal_overflow_is_checked_at_the_adopted_width() {
    for src in [
        "fn f() -> i8 { 128 + 0 }",
        "fn f() -> i8 { 0 + 128 }",
        "fn f() -> i8 { 1 + (2 * 128) }",
        "fn f() -> i8 { -129 + 1 }",
        "fn f() -> u8 { -1 + 1 }",
        "fn f() -> u8 { 0 + 256 }",
        "fn f() -> i8 { -(128 + 0) }",
        "fn f() -> i64 { -9223372036854775809 + 0 }",
        "fn f() -> u64 { 18446744073709551616 + 0 }",
    ] {
        assert!(
            codes(src).contains(&Code::LiteralOverflow),
            "{src}: {:?}",
            codes(src)
        );
    }
    for src in [
        "fn f() -> i64 { 2147483648 + 1 }",
        "fn f() -> u64 { 18446744073709551615 + 0 }",
        "fn f() -> i8 { -128 + 0 }",
        "fn f() -> i64 { -9223372036854775808 + 0 }",
    ] {
        assert!(codes(src).is_empty(), "{src}: {:?}", codes(src));
    }
}

#[test]
fn context_does_not_coerce_typed_operands_or_change_operator_domains() {
    for src in [
        "fn f(x: i32) -> i64 { x + 1 }",
        "fn f(x: i32) -> i64 { 1 + x }",
        "fn f(x: i32) -> i64 { -x }",
        "fn f(x: i16) -> i8 { 1 + x }",
        "fn f(x: f64) -> f32 { -x }",
        "fn f() -> f32 { 1 + 2.0 }",
        "fn f() -> i8 { 1 + true }",
        "fn f() -> i64 { 1 == 2 }",
        "fn f() -> i64 { 1 < 2 }",
        "fn f() -> i64 { true && false }",
        "fn f() -> i64 { \"a\" + \"b\" }",
        "fn f() -> i64 { !1 }",
    ] {
        assert!(
            codes(src).contains(&Code::TypeMismatch),
            "{src}: {:?}",
            codes(src)
        );
    }
    for src in [
        "fn f() -> bool { (1 + 2 == 3) && !(4.0 < 2.0) }",
        "fn f() -> str { \"a\" + \"b\" }",
        "fn f() -> bool { 'a' < 'b' }",
    ] {
        assert!(codes(src).is_empty(), "{src}: {:?}", codes(src));
    }
    assert!(codes("fn f() -> i8 { true + false }").contains(&Code::UnsupportedOperation));
}

#[test]
fn default_negated_integer_literals_include_the_i32_minimum() {
    for expression in [
        "-2147483648",
        "-(2147483648)",
        "((-2147483648))",
        "-(-2147483648)",
        "-2147483648 + 0",
        "0 + -2147483648",
        "-2147483648 + 1",
        "-2147483647",
        "-1",
        "-0",
    ] {
        let src = format!("fn f() {{ let x = {expression}; }}");
        let (module, tables, _, diags) = check_src(&src);
        assert!(diags.is_empty(), "{src}: {diags:?}");
        let body = module.bodies.iter().flatten().next().unwrap();
        let types = tables[body.def.index()].as_ref().unwrap();
        assert!(types.local_types.values().all(|ty| *ty == Ty::I32));
        for expr in &body.exprs {
            if !matches!(expr.kind, HirExprKind::Block { .. }) {
                assert_eq!(types.ty_of(expr.id), Ty::I32, "{src}: {expr:?}");
            }
        }
    }
}

#[test]
fn default_sign_checking_preserves_overflow_context_and_family_rules() {
    for (src, expected) in [
        ("fn f() { let x = -2147483649; }", Code::LiteralOverflow),
        ("fn f() { let x = 2147483648; }", Code::LiteralOverflow),
        (
            "fn f() { let x = -(2147483648 + 0); }",
            Code::LiteralOverflow,
        ),
        (
            "fn f() { let x = -(-(2147483648 + 0)); }",
            Code::LiteralOverflow,
        ),
        ("fn f() -> i8 { -129 }", Code::LiteralOverflow),
        ("fn f() -> u8 { -1 }", Code::LiteralOverflow),
        ("fn f() -> f64 { -2147483648 }", Code::LiteralOverflow),
        ("fn f() -> f64 { -1 }", Code::TypeMismatch),
        ("fn f() -> bool { -1 }", Code::TypeMismatch),
        (
            "fn f() -> i64 { let x = -2147483648; x }",
            Code::TypeMismatch,
        ),
    ] {
        assert_eq!(codes(src), [expected], "{src}");
    }
}
