//! Returning branches impose no constraint on an if's value type.
use ontixa_diagnostics::Code;
use ontixa_types::check_src;

fn codes(src: &str) -> Vec<Code> {
    let (_, _, _, diags) = check_src(src);
    diags.iter().map(|d| d.code).collect()
}

#[test]
fn only_continuing_branches_constrain_the_if_result() {
    for body in [
        "if c { return 7; } else { 42 }",
        "if c { 42 } else { return 7; }",
        "if c { return 7; } else { return 42; }",
        "let x: i32 = if c { return 7; } else { return 42; }; x",
        "if c { return 7; false } else { 42 }",
        "if c { 42 } else { return 7; false }",
        "if c { { return 7; } } else { 42 }",
        "if c { if c { return 7; } else { return 8; } } else { 42 }",
        "if c { return 7; } else { if c { return 8; } else { 42 } }",
        "if c { match O::A { O::A => { return 7; }, O::B => { return 8; } } } else { 42 }",
        "if c { { return 7; true } && false } else { 42 }",
        "if { return 7; true } { 42 } else { 8 }",
    ] {
        let src = format!("data O {{ A; B; }} fn f(c: bool) -> i32 {{ {body} }}");
        assert!(codes(&src).is_empty(), "{src}: {:?}", codes(&src));
    }
}

#[test]
fn expected_types_reach_continuing_branches_and_return_payloads() {
    for src in [
        "fn f(c: bool) -> i8 { if c { return -128; } else { -127 } }",
        "fn f(c: bool) -> i8 { if c { -127 } else { return -128; } }",
        "fn f(c: bool) -> [i32] { if c { return []; } else { [] } }",
        "fn f(c: bool) -> [i32] { if c { [] } else { return []; } }",
        "fn f(c: bool) -> [i32] { let a: [i32] = if c { return []; } else { return []; }; a }",
        "fn f(c: bool) { if c { return; } else {} }",
    ] {
        assert!(codes(src).is_empty(), "{src}: {:?}", codes(src));
    }
    for src in [
        "fn f(c: bool) -> i8 { if c { return -128; } else { 128 } }",
        "fn f(c: bool) -> i8 { if c { -127 } else { return -129; } }",
    ] {
        assert!(codes(src).contains(&Code::LiteralOverflow), "{src}");
    }
}

#[test]
fn returning_and_dead_children_are_still_type_checked() {
    for body in [
        "if c { return false; } else { 42 }",
        "if c { 42 } else { return false; }",
        "if c { return 7; let x: bool = 1; } else { 42 }",
        "if c { 42 } else { return 7; let x: bool = 1; }",
        "if c { return 7; } else { false }",
        "if c { 42 } else { false }",
        "if 1 { return 7; } else { 42 }",
        "if { return 7; 0 } { 42 } else { 8 }",
        "if { return 7; true } { 42 } else { false }",
        "if c { if c { return 7; } else { return false; } } else { 42 }",
    ] {
        let src = format!("fn f(c: bool) -> i32 {{ {body} }}");
        assert!(codes(&src).contains(&Code::TypeMismatch), "{src}");
    }
}

#[test]
fn optional_paths_do_not_become_returning_branches() {
    for body in [
        "if c { false && { return 7; true } } else { 42 }",
        "if c { true || { return 7; false } } else { 42 }",
        "if c { for i in 0..0 { return 7; } } else { 42 }",
        "if c { for i in 0..1 { return 7; } } else { 42 }",
        "if c { if c { return 7; } } else { 42 }",
    ] {
        let src = format!("fn f(c: bool) -> i32 {{ {body} }}");
        assert!(codes(&src).contains(&Code::TypeMismatch), "{src}");
    }
    let src = "data O { A; B; } fn f(c: bool) -> i32 { if c { match O::A { O::A => { return 7; } } } else { 42 } }";
    assert!(codes(src).contains(&Code::NonExhaustive));
}

#[test]
fn no_else_still_discards_the_then_value_and_can_complete() {
    for body in ["if c { 42 }", "if c { return 7; }"] {
        let src = format!("fn f(c: bool) -> i32 {{ {body} }}");
        assert!(codes(&src).contains(&Code::TypeMismatch), "{src}");
        let discarded = format!("fn f(c: bool) -> i32 {{ {body}; }}");
        assert_eq!(codes(&discarded), [Code::MissingReturn], "{discarded}");
        let fallback = format!("fn f(c: bool) -> i32 {{ {body}; 42 }}");
        assert!(codes(&fallback).is_empty(), "{fallback}");
    }
}
