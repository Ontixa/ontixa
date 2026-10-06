use ontixa_interpreter::{RuntimeError, Value, run_src};

fn value(src: &str) -> Value {
    let (value, _, _, _, diags) = run_src(src, "main").expect("run");
    assert!(diags.is_empty(), "{diags:?}");
    value
}

#[test]
fn short_circuit_skips_traps_and_checks_boolean_truth_tables() {
    for (expr, expected) in [
        ("false && (1 / 0 == 0)", false),
        ("true || (1 / 0 == 0)", true),
        ("false && false", false),
        ("false && true", false),
        ("true && false", false),
        ("true && true", true),
        ("false || false", false),
        ("false || true", true),
        ("true || false", true),
        ("true || true", true),
        ("true || false && false", true),
        ("(true || false) && false", false),
        ("false || true && true", true),
    ] {
        let src = format!("fn main() -> bool {{ return {expr}; }}");
        assert!(
            matches!(value(&src), Value::Bool(b) if b == expected),
            "{expr}"
        );
    }
    assert!(matches!(
        value("fn main() -> bool { let s = \"\"; return s.len > 0 && s[0] == \"x\"; }"),
        Value::Bool(false)
    ));
}

#[test]
fn short_circuit_required_rhs_still_traps() {
    for expr in ["true && (1 / 0 == 0)", "false || (1 / 0 == 0)"] {
        let src = format!("fn main() -> bool {{ return {expr}; }}");
        assert!(matches!(run_src(&src, "main"), Err(RuntimeError::Trap(_))));
    }
}

#[test]
fn short_circuit_evaluates_operands_once_in_order() {
    for (expr, expected) in [
        ("false && tick(p, true)", 0),
        ("true || tick(p, false)", 0),
        ("true && tick(p, true)", 1),
        ("false || tick(p, true)", 1),
        ("tick(p, false) && tick(p, true)", 1),
        ("tick(p, true) || tick(p, false)", 1),
        ("tick(p, true) && tick(p, false)", 2),
        ("tick(p, false) || tick(p, true)", 2),
        ("true || false && tick(p, true)", 0),
        ("(false || tick(p, true)) && tick(p, true)", 2),
    ] {
        let src = format!(
            "data P {{ x: i32; }}
             fn tick(mut p: P, result: bool) -> bool {{ p.x = p.x + 1; return result; }}
             fn main() -> i32 {{ let mut p = P {{ x: 0 }}; let b = {expr}; return p.x; }}"
        );
        assert!(
            matches!(value(&src), Value::Int(n) if n == expected),
            "{expr}"
        );
    }
    // The branch reads the old value of LHS before RHS changes its binding.
    assert!(matches!(
        value("fn main() -> bool { let mut a = true; return a && { a = false; true }; }"),
        Value::Bool(true)
    ));
}

#[test]
fn short_circuit_preserves_early_returns() {
    for (expr, expected) in [
        ("false && { return true; false }", false),
        ("true || { return false; true }", true),
        ("true && { return false; true }", false),
        ("false || { return true; false }", true),
        ("{ return true; false } && false", true),
        ("false || { if true { return true; } false }", true),
        ("true && { if false { return false; } true }", true),
    ] {
        let src = format!("fn main() -> bool {{ return {expr}; }}");
        assert!(
            matches!(value(&src), Value::Bool(b) if b == expected),
            "{expr}"
        );
    }
}
