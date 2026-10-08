//! Numeric peers guide literal syntax without converting existing values.
use ontixa_diagnostics::Code;
use ontixa_hir::HirExprKind;
use ontixa_types::{Ty, check_src};

fn codes(src: &str) -> Vec<Code> {
    let (_, _, _, diags) = check_src(src);
    diags.iter().map(|d| d.code).collect()
}

#[test]
fn either_literal_side_adopts_each_numeric_peer_width_and_operator() {
    for (name, ty, literal) in [
        ("i8", Ty::I8, "2"),
        ("i16", Ty::I16, "2"),
        ("i32", Ty::I32, "2"),
        ("i64", Ty::I64, "2"),
        ("isize", Ty::Isize, "2"),
        ("u8", Ty::U8, "2"),
        ("u16", Ty::U16, "2"),
        ("u32", Ty::U32, "2"),
        ("u64", Ty::U64, "2"),
        ("usize", Ty::Usize, "2"),
        ("f32", Ty::F32, "2.0"),
        ("f64", Ty::F64, "2.0"),
    ] {
        for op in ["+", "-", "*", "/", "%", "==", "!=", "<", "<=", ">", ">="] {
            for expression in [format!("{literal} {op} x"), format!("x {op} {literal}")] {
                let src = format!("fn f(x: {name}) {{ let result = {expression}; }}");
                let (module, tables, _, diags) = check_src(&src);
                assert!(diags.is_empty(), "{src}: {diags:?}");
                let body = module.bodies.iter().flatten().next().unwrap();
                let types = tables[body.def.index()].as_ref().unwrap();
                for expr in &body.exprs {
                    match expr.kind {
                        HirExprKind::Literal(_) | HirExprKind::Var(_) => {
                            assert_eq!(types.ty_of(expr.id), ty, "{src}");
                        }
                        HirExprKind::Binary { .. } => {
                            let expected = if ["+", "-", "*", "/", "%"].contains(&op) {
                                ty
                            } else {
                                Ty::Bool
                            };
                            assert_eq!(types.ty_of(expr.id), expected, "{src}");
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

#[test]
fn grouped_negated_literals_and_independent_computed_peers_work() {
    for src in [
        "fn f(x: i8) -> bool { (-128) < x }",
        "fn f(x: i64) -> bool { -9223372036854775808 < x }",
        "fn f(x: u64) -> bool { 18446744073709551615 == x }",
        "fn f(x: f32) -> bool { (-1.0) < x }",
        "fn f(x: i64) -> bool { (0) < (x + 1) }",
        "fn f(x: i64) -> bool { 0 < -x }",
        "fn value() -> i64 { 2 } fn f() -> bool { 0 < value() }",
        "data P { n: i64; } fn f(p: P) -> bool { 0 < p.n }",
        "fn f(x: [i64]) -> bool { 0 < x[0] }",
        "fn f(x: str) -> bool { 0 <= x.len }",
        "fn f(x: i64) -> bool { 0 < { let y = x + 1; y } }",
        "fn f(x: i64) -> bool { 0 < { let y = x + 1; -(y + 1) } }",
    ] {
        assert!(codes(src).is_empty(), "{src}: {:?}", codes(src));
    }
}

#[test]
fn typed_values_families_and_declared_context_remain_strict() {
    for src in [
        "fn f(x: i64, y: i32) -> bool { x == y }",
        "fn f(x: i64, y: i32) -> bool { y == x }",
        "fn f(x: f32, y: f64) -> bool { x == y }",
        "fn f(x: f32, y: f64) -> bool { y == x }",
        "fn f(x: i64) -> bool { let y = 0; y < x }",
        "fn f(x: i64) -> bool { let y = 0; x > y }",
        "fn f(x: i64) -> bool { 0.0 < x }",
        "fn f(x: i64) -> bool { x > 0.0 }",
        "fn f(x: f32) -> bool { 0 < x }",
        "fn f(x: f32) -> bool { x > 0 }",
        "fn f(x: i32) -> i64 { 1 + x }",
        "fn f(x: i64) -> i32 { 1 + x }",
        "fn f(x: f64) -> f32 { 1.0 + x }",
        "fn f(x: i64) -> bool { (1 + 2) < x }",
        "fn f(x: i64) -> bool { -(1 + 2) < x }",
        "fn f(x: i64) -> bool { -(-1) < x }",
        "fn f(x: i64) -> bool { { 0 } < x }",
        "fn f(x: i64) -> bool { 0 < (1 + x) }",
        "fn f(x: i64) -> bool { 0 < if true { x } else { x } }",
        "fn f(x: bool) -> bool { 1 == x }",
        "fn f() -> bool { 0 && true }",
    ] {
        assert!(
            codes(src).contains(&Code::TypeMismatch),
            "{src}: {:?}",
            codes(src)
        );
    }
    for src in [
        "fn f(x: i64) -> i64 { 1 + x }",
        "fn f(x: i64) -> bool { x > (1 + 2) }",
        "fn f() -> str { \"a\" + \"b\" }",
        "fn f() -> bool { 'a' < 'b' }",
    ] {
        assert!(codes(src).is_empty(), "{src}: {:?}", codes(src));
    }
    // Declared i8 context wins over the i16 peer and checks 128 at i8.
    assert!(codes("fn f(x: i16) -> i8 { 128 + x }").contains(&Code::LiteralOverflow));
}

#[test]
fn adopted_literal_ranges_and_signed_minima_stay_exact() {
    for (name, good, bad) in [
        ("i8", "-128", "-129"),
        ("i16", "-32768", "-32769"),
        ("i32", "-2147483648", "-2147483649"),
        ("i8", "127", "128"),
        ("i16", "32767", "32768"),
        ("i32", "2147483647", "2147483648"),
        ("i64", "9223372036854775807", "9223372036854775808"),
        ("isize", "9223372036854775807", "9223372036854775808"),
        ("i64", "-9223372036854775808", "-9223372036854775809"),
        ("isize", "-9223372036854775808", "-9223372036854775809"),
        ("u8", "255", "256"),
        ("u16", "65535", "65536"),
        ("u32", "4294967295", "4294967296"),
        ("u64", "18446744073709551615", "18446744073709551616"),
        ("usize", "18446744073709551615", "18446744073709551616"),
    ] {
        for (literal, valid) in [(good, true), (bad, false)] {
            for expr in [format!("{literal} == x"), format!("x == {literal}")] {
                let src = format!("fn f(x: {name}) -> bool {{ {expr} }}");
                let diagnostics = codes(&src);
                if valid {
                    assert!(diagnostics.is_empty(), "{src}: {diagnostics:?}");
                } else {
                    assert_eq!(diagnostics, [Code::LiteralOverflow], "{src}");
                }
            }
        }
    }
    for src in [
        "fn f(x: i8) -> bool { 128 > x }",
        "fn f(x: u8) -> bool { -1 < x }",
        "fn f(x: u8) -> bool { x > -1 }",
        "fn f(x: u64) -> bool { -1 < x }",
        "fn f() -> i8 { -(128 + 0) }",
    ] {
        assert!(
            codes(src).contains(&Code::LiteralOverflow),
            "{src}: {:?}",
            codes(src)
        );
    }
}

#[test]
fn two_literals_and_compound_defaults_keep_their_widths() {
    for (expr, ty) in [
        ("1 + 2", Ty::I32),
        ("-1 + 2", Ty::I32),
        ("1.0 + 2.0", Ty::F64),
        ("-1.0 + 2.0", Ty::F64),
        ("1 + (2 + 3)", Ty::I32),
        ("1.0 + (2.0 + 3.0)", Ty::F64),
    ] {
        let src = format!("fn f() {{ let result = {expr}; }}");
        let (module, tables, _, diags) = check_src(&src);
        assert!(diags.is_empty(), "{src}: {diags:?}");
        let body = module.bodies.iter().flatten().next().unwrap();
        let types = tables[body.def.index()].as_ref().unwrap();
        for expr in &body.exprs {
            if !matches!(expr.kind, HirExprKind::Block { .. }) {
                assert_eq!(types.ty_of(expr.id), ty, "{src}");
            }
        }
    }
    assert!(codes("fn f() -> bool { 2147483648 == 2147483648 }").contains(&Code::LiteralOverflow));
}
