//! A discarded value does not return from its enclosing function.
use ontixa_diagnostics::Code;
use ontixa_types::check_src;

fn codes(src: &str) -> Vec<Code> {
    let (_, _, _, diags) = check_src(src);
    diags.iter().map(|d| d.code).collect()
}

#[test]
fn short_circuit_rhs_does_not_guarantee_a_return() {
    // PR #19 made these previously eager returns optional. The old
    // checker mistook the discarded block's boolean tail for a return.
    for expr in [
        "false && { return 7; true }",
        "true || { return 7; false }",
        "true && { return 7; true }",
        "false || { return 7; false }",
        "c && { return 7; true }",
        "c || { return 7; false }",
    ] {
        let src = format!("fn f(c: bool) -> i32 {{ {{ {expr} }}; }}");
        assert_eq!(codes(&src), [Code::MissingReturn], "{src}");
    }
}

#[test]
fn discarded_values_do_not_satisfy_a_non_unit_return() {
    for body in [
        "{ 42 };",
        "{ { 42 } };",
        "if c { 42 } else { 7 };",
        "match O::A { O::A => { 42 }, O::B => { 7 } };",
        "match O::A { O::A => { false && { return 7; true } }, O::B => { return 8; } };",
    ] {
        let src = format!("data O {{ A; B; }} fn f(c: bool) -> i32 {{ {body} }}");
        assert_eq!(codes(&src), [Code::MissingReturn], "{src}");
    }
}

#[test]
fn tails_and_unit_discards_keep_their_existing_meaning() {
    for src in [
        "fn main() -> i32 { 42 }",
        "fn main() -> i8 { { { -128 } } }",
        "fn f(c: bool) -> i32 { if c { 42 } else { 7 } }",
        "fn main() {}",
        "fn main() { return; }",
        "fn main() -> unit { {} }",
        "fn main() { { 42 }; }",
        "fn main() { if true { 42 } else { 7 }; }",
        "data O { A; B; } fn main() { match O::A { O::A => { 42 }, O::B => { 7 } }; }",
    ] {
        assert!(codes(src).is_empty(), "{src}: {:?}", codes(src));
    }
}

#[test]
fn mandatory_evaluation_and_nested_returns_stop_completion() {
    for body in [
        "return 7; let unused = 1;",
        "{ return 7; }",
        "{ { return 7; } }",
        "{ { return 7; false } && true };",
        "{ { return 7; true } || false };",
        "let value = { return 7; 0 };",
        "let mut value = 0; value = { return 7; 1 };",
        "!{ return 7; false };",
        "1 + { return 7; 2 };",
        "if { return 7; true } {}",
        "[1, { return 7; 2 }];",
        "[1][{ return 7; 0 }];",
        "[1][{ return 7; 0 }..1];",
        "[1][0..({ return 7; 1 })];",
        "{ return 7; [1] }.len;",
        "for i in 0..({ return 7; 1 }) {}",
        "for i in { return 7; [1] } {}",
        "if c { return 7; } else { return 8; };",
    ] {
        let src = format!("fn f(c: bool) -> i32 {{ {body} }}");
        assert!(codes(&src).is_empty(), "{src}: {:?}", codes(&src));
    }
    for src in [
        "fn g(x: i32) {} fn main() -> i32 { g({ return 7; 1 }); }",
        "data P { x: i32; } fn main() -> i32 { P { x: { return 7; 1 } }; }",
        "data O { A(i32); } fn main() -> i32 { O::A({ return 7; 1 }); }",
        "data O { A; } fn main() -> i32 { match { return 7; O::A } { O::A => 0 }; }",
        "data O { A; B; } fn f(o: O) -> i32 { match o { O::A => { { return 7; } }, O::B => { return 8; } }; }",
    ] {
        assert!(codes(src).is_empty(), "{src}: {:?}", codes(src));
    }
}

#[test]
fn optional_branches_and_loop_bodies_can_complete() {
    for body in [
        "if c { return 7; };",
        "if c { return 7; } else {};",
        "for i in 0..0 { return 7; };",
        "for i in 0..1 { return 7; };",
        "for i in [1] { return 7; };",
    ] {
        let src = format!("fn f(c: bool) -> i32 {{ {body} }}");
        assert_eq!(codes(&src), [Code::MissingReturn], "{src}");
        let fallback = format!("fn f(c: bool) -> i32 {{ {body} 42 }}");
        assert!(codes(&fallback).is_empty(), "{fallback}");
    }
    let src = "data O { A; B; } fn f(o: O) -> i32 { match o { O::A => { return 7; } }; }";
    assert_eq!(codes(src), [Code::NonExhaustive, Code::MissingReturn]);
    // Calls stay conservative; this is not interprocedural termination analysis.
    assert_eq!(codes("fn f() -> i32 { f(); }"), [Code::MissingReturn]);
}

#[test]
fn completion_does_not_suppress_type_errors() {
    for src in [
        "fn main() -> i32 { false && { return 7; true } }",
        "fn main() -> i32 { return true; }",
        "fn main() -> i32 { return 7; let x: bool = 1; }",
        "fn main() -> i32 { { return 7; true } && 1; }",
        "fn main() -> i32 { false && { return true; true }; 42 }",
        "fn main() -> bool { false && 1 }",
    ] {
        assert!(codes(src).contains(&Code::TypeMismatch), "{src}");
    }
    // Mixed return/value if typing is a separate, pre-existing limitation.
    let mixed = "fn f(c: bool) -> i32 { if c { return 7; } else { 42 } }";
    assert!(codes(mixed).contains(&Code::TypeMismatch));
}
