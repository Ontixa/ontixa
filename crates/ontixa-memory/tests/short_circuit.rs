use ontixa_diagnostics::Code;
use ontixa_memory::analyze_src;

fn codes(src: &str) -> Vec<Code> {
    let (_, _, _, _, diags) = analyze_src(src);
    diags.iter().map(|d| d.code).collect()
}

#[test]
fn short_circuit_rhs_initialization_is_not_definite() {
    for expr in [
        "false && { x = 42; true }",
        "true || { x = 42; false }",
        "true && { x = 42; true }",
        "false || { x = 42; false }",
        "false || true && { x = 42; true }",
    ] {
        let src = format!("fn main() -> i32 {{ let x: i32; let b = {expr}; return x; }}");
        assert!(codes(&src).contains(&Code::Uninitialized), "{expr}");
    }
}

#[test]
fn short_circuit_lhs_initialization_is_definite() {
    for op in ["&&", "||"] {
        let src = format!(
            "fn main() -> i32 {{ let x: i32; let b = {{ x = 42; true }} {op} (x == 42); return x; }}"
        );
        assert!(codes(&src).is_empty(), "{src}");
    }
}

#[test]
fn short_circuit_rhs_reinitialization_cannot_revive_a_moved_value() {
    for op in ["&&", "||"] {
        let src = format!(
            "data P {{ x: i32; }} fn take(p: P) -> bool {{ return true; }}
             fn main() -> i32 {{ let mut p = P {{ x: 1 }}; take(p);
               let b = false {op} {{ p = P {{ x: 42 }}; true }}; return p.x; }}"
        );
        assert!(codes(&src).contains(&Code::UseAfterMove), "{op}");
    }
}

#[test]
fn short_circuit_conditional_moves_stay_maybe_moved() {
    for op in ["&&", "||"] {
        let src = format!(
            "data P {{ x: i32; }} fn take(p: P) -> bool {{ return true; }}
             fn main() -> i32 {{ let p = P {{ x: 1 }}; let b = false {op} take(p); return p.x; }}"
        );
        let (_, _, _, _, diags) = analyze_src(&src);
        assert!(
            diags
                .iter()
                .any(|d| d.code == Code::UseAfterMove && d.message.contains("may have been moved")),
            "{diags:?}"
        );
    }
}

#[test]
fn short_circuit_keeps_both_operands_checked() {
    for expr in ["false && 1", "true || 1", "1 && false", "1 || true"] {
        let src = format!("fn main() -> bool {{ return {expr}; }}");
        assert!(codes(&src).contains(&Code::TypeMismatch), "{expr}");
    }
    for expr in ["false && (x == 0)", "true || (x == 0)"] {
        let src = format!("fn main() -> bool {{ let x: i32; return {expr}; }}");
        assert!(codes(&src).contains(&Code::Uninitialized), "{expr}");
    }
}

#[test]
fn short_circuit_conditional_init_keeps_write_once_rule() {
    let src = "fn main() -> i32 { let x: i32; let b = false && { x = 1; true }; x = 2; return x; }";
    assert!(codes(src).contains(&Code::ImmutableAssignment));
}

#[test]
fn short_circuit_return_only_rhs_does_not_move_skipped_value() {
    for rhs in [
        "{ take(p); return 17; false }",
        "{ take(p); return 17; let z = true; z }",
        "{ take(p); let z = { return 17; true }; z }",
        "{ take(p); if c { return 17; } else { return 18; } false }",
    ] {
        for op in ["&&", "||"] {
            let src = format!(
                "data P {{ x: i32; }} fn take(p: P) -> bool {{ return true; }}
                 fn f(c: bool) -> i32 {{ let p = P {{ x: 42 }}; let b = c {op} {rhs}; return p.x; }}"
            );
            assert!(codes(&src).is_empty(), "{src}: {:?}", codes(&src));
        }
    }
}
