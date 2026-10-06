//! Only paths that reach an if's continuation contribute ownership state.
use ontixa_diagnostics::Code;
use ontixa_memory::analyze_src;

fn codes(src: &str) -> Vec<Code> {
    let (_, _, _, _, diags) = analyze_src(src);
    diags.iter().map(|d| d.code).collect()
}

fn owned(body: &str) -> String {
    format!("data P {{ x: i32; }} data O {{ A; B; }} fn keep(p: P, c: bool) -> P {{ {body} }}")
}

#[test]
fn returning_moves_do_not_reach_the_continuation() {
    for body in [
        "if c { return p; }; p",
        "if c {} else { return p; }; p",
        "if c { return p; } else { p }",
        "if c { p } else { return p; }",
        "if c { if c { return p; } else { return p; }; }; p",
        "if c { match O::A { O::A => { return p; }, O::B => { return p; } }; }; p",
        "if { if c { return p; }; false } { return p; }; p",
        "if c && { return p; true } { return p; }; p",
        "if c { return p; } else { return p; }; p",
    ] {
        let src = owned(body);
        assert!(codes(&src).is_empty(), "{src}: {:?}", codes(&src));
    }
}

#[test]
fn initialization_and_reinitialization_in_the_only_continuing_arm_are_definite() {
    for branch in [
        "if c { x = 42; } else { return 7; }",
        "if c { return 7; } else { x = 42; }",
        "if c { if c { x = 42; } else { return 8; }; } else { return 7; }",
    ] {
        let src = format!("fn f(c: bool) -> i32 {{ let x: i32; {branch}; x }}");
        assert!(codes(&src).is_empty(), "{src}: {:?}", codes(&src));
    }
    for branch in [
        "if c { p = P { x: 42 }; } else { return q; }",
        "if c { return q; } else { p = P { x: 42 }; }",
    ] {
        let src = format!(
            "data P {{ x: i32; }} fn f(mut p: P, c: bool) -> P {{ let q = p; {branch}; p }}"
        );
        assert!(codes(&src).is_empty(), "{src}: {:?}", codes(&src));
    }
}

#[test]
fn genuine_moves_on_continuing_paths_are_rejected() {
    for body in [
        "if c { let q = p; } else { return p; }; p",
        "if c { return p; } else { let q = p; }; p",
        "if c { let q = p; }; p",
        "if c { if c { let q = p; }; } else { return p; }; p",
        "if c { false && { let q = p; true }; } else { return p; }; p",
        "if c { let q = p; for i in 0..0 { return q; }; } else { return p; }; p",
    ] {
        let src = owned(body);
        assert!(codes(&src).contains(&Code::UseAfterMove), "{src}");
    }
}

#[test]
fn optional_initialization_stays_uninitialized() {
    for branch in [
        "if c { x = 42; }",
        "if c { return 7; } else {}",
        "if c { if c { x = 42; }; } else { return 7; }",
        "if c { false && { x = 42; true }; } else { return 7; }",
        "if c { for i in 0..0 { x = 42; }; } else { return 7; }",
    ] {
        let src = format!("fn f(c: bool) -> i32 {{ let x: i32; {branch}; x }}");
        assert!(codes(&src).contains(&Code::Uninitialized), "{src}");
    }
}

#[test]
fn returning_arms_keep_move_initialization_and_loan_diagnostics() {
    let src = owned("if c { let q = p; return p; }; p");
    assert!(codes(&src).contains(&Code::UseAfterMove));
    assert!(
        codes("fn f() -> i32 { let x: i32; if false { return x; } else { 42 } }")
            .contains(&Code::Uninitialized)
    );
    for (call, code) in [
        ("mix(q, q)", Code::BorrowConflict),
        ("take(q, q)", Code::MoveWhileBorrowed),
    ] {
        for arms in [
            format!("if false {{ return {call}; }} else {{ 42 }}"),
            format!("if true {{ 42 }} else {{ return {call}; }}"),
        ] {
            let src = format!(
                "data P {{ x: i32; }}
                fn mix(a: P, mut b: P) -> i32 {{ b.x = a.x; b.x }}
                fn take(a: P, b: P) -> i32 {{ let r = b; a.x + r.x }}
                fn main() -> i32 {{ let mut q = P {{ x: 42 }}; {arms} }}"
            );
            assert!(codes(&src).contains(&code), "{src}: {:?}", codes(&src));
        }
    }
}
