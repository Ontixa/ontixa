//! A tail-less block that cannot complete can use an existing expected type.
use ontixa_diagnostics::Code;
use ontixa_hir::{HirExprKind, HirStmt};
use ontixa_types::{Ty, check_src};

fn codes(src: &str) -> Vec<Code> {
    let (_, _, _, diags) = check_src(src);
    diags.iter().map(|d| d.code).collect()
}

#[test]
fn required_contexts_accept_tail_less_returning_blocks() {
    for body in [
        "id({ return 42; })",
        "let x: i32 = { return 42; }; x",
        "let mut x = 0; x = { return 42; }; x",
        "return { return 42; };",
        "1 + { return 42; }",
        "{ return 42; } + 1",
        "-{ return 42; }",
        "let xs: [i32] = [{ return 42; }]; xs[0]",
        "P { x: { return 42; } }.x",
        "O::A({ return 42; }); 0",
        "id({ { return 42; }; })",
        "id({ let x = { return 42; }; })",
        "id({ if c { return 42; } else { return 7; }; })",
        "id({ match O::A(0) { O::A(x) => { return 42; } }; })",
        "if { return 42; } { 1 } else { 2 }",
        "c && { return 42; }; 7",
        "c || { return 42; }; 7",
    ] {
        let src = format!(
            "data P {{ x: i32; }} data O {{ A(i32); }} fn id(x: i32) -> i32 {{ x }} fn f(c: bool) -> i32 {{ {body} }}"
        );
        assert!(codes(&src).is_empty(), "{src}: {:?}", codes(&src));
    }
}

#[test]
fn contextual_block_type_is_recorded_without_changing_unannotated_inference() {
    let src = "fn id(x: i64) -> i64 { x } fn main() -> i64 { let dead = { return 42; }; id({ return 42; }) }";
    let (module, tables, mut interner, diags) = check_src(src);
    assert!(diags.is_empty(), "{diags:?}");
    let main = module.scope.root_env().fns[&interner.intern("main")];
    let body = module.body(main).unwrap();
    let table = tables[main.index()].as_ref().unwrap();
    let HirExprKind::Block { stmts, tail } = &body.expr(body.root).kind else {
        panic!("expected function block")
    };
    let HirStmt::Let { symbol, init, .. } = &stmts[0] else {
        panic!("expected binding")
    };
    assert_eq!(table.local_types[symbol], Ty::Unit);
    assert_eq!(table.ty_of(init.unwrap()), Ty::Unit);
    let HirExprKind::Call { args, .. } = &body.expr(tail.unwrap()).kind else {
        panic!("expected call")
    };
    assert_eq!(table.ty_of(args[0]), Ty::I64);
}

#[test]
fn payloads_and_dead_children_retain_their_own_diagnostics() {
    for (body, code) in [
        ("id({ return false; })", Code::TypeMismatch),
        ("id({ return; })", Code::TypeMismatch),
        ("id({ return 42; let dead: bool = 1; })", Code::TypeMismatch),
        ("id({ return 42; return false; })", Code::TypeMismatch),
        (
            "id({ return 42; let dead: i8 = 128; })",
            Code::LiteralOverflow,
        ),
        ("id({ return 42; let dead = []; })", Code::CannotInfer),
        ("id({ return 42; false })", Code::TypeMismatch),
        (
            "let x = { return 42; }; let y: i32 = x; 0",
            Code::TypeMismatch,
        ),
        ("let xs = [1, { return 42; }]; xs[0]", Code::TypeMismatch),
        ("{ return 42; } && true; 0", Code::TypeMismatch),
    ] {
        let src = format!("fn id(x: i32) -> i32 {{ x }} fn main() -> i32 {{ {body} }}");
        assert!(codes(&src).contains(&code), "{src}: {:?}", codes(&src));
    }
    // Context is i64 but the return payload still uses the function's i8.
    let src = "fn id(x: i64) -> i8 { 0 } fn main() -> i8 { id({ return 128; }) }";
    assert_eq!(codes(src), [Code::LiteralOverflow]);
}

#[test]
fn completing_and_optional_blocks_remain_unit() {
    for block in [
        "{}",
        "{ 42; }",
        "{ if c { return 42; }; }",
        "{ if c { return 42; } else {}; }",
        "{ c && { return 42; }; }",
        "{ c || { return 42; }; }",
        "{ true && { return 42; }; }",
        "{ false || { return 42; }; }",
        "{ for i in 0..0 { return 42; }; }",
        "{ for i in 0..1 { return 42; }; }",
        "{ id(42); }",
    ] {
        let src = format!("fn id(x: i32) -> i32 {{ x }} fn f(c: bool) -> i32 {{ id({block}) }}");
        assert_eq!(codes(&src), [Code::TypeMismatch], "{src}");
    }
    let src = "data O { A; B; } fn id(x: i32) -> i32 { x } fn main() -> i32 { id({ match O::A { O::A => { return 42; } }; }) }";
    assert!(codes(src).contains(&Code::NonExhaustive));
    assert!(codes(src).contains(&Code::TypeMismatch));
}

#[test]
fn short_circuit_and_discarded_values_do_not_gain_a_function_return() {
    for body in [
        "{ 42 };",
        "{ false && { return 42; } };",
        "{ true || { return 42; } };",
        "{ true && { return 42; } };",
        "{ false || { return 42; } };",
        "{ if c { 42 } else { 7 } };",
    ] {
        let src = format!("fn f(c: bool) -> i32 {{ {body} }}");
        assert_eq!(codes(&src), [Code::MissingReturn], "{src}");
    }
    for src in [
        "fn main() { return { return; }; }",
        "fn main() { let x: unit = { return; }; }",
        "fn main() { { 42 }; }",
    ] {
        assert!(codes(src).is_empty(), "{src}: {:?}", codes(src));
    }
}
