//! Exercise numeric contexts through the database, MIR and interpreter.
use serde_json::{Value, json};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn invoke(command: &str, src: &str) -> (Output, Value) {
    let path = std::env::temp_dir().join(format!(
        "ontixa_numeric_context_{}_{}.ixa",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, src).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_ontixa"))
        .args([command, path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    let json: Value = serde_json::from_slice(&out.stdout).expect("one JSON envelope");
    assert_eq!(json["schema"], 1);
    (out, json)
}

#[test]
fn numeric_arithmetic_runs_at_each_declared_width() {
    for (ty, expression, expected) in [
        ("i8", "-128 + (6 * 7)", json!(-86)),
        ("i16", "(40 + 4) / 2 + 20", json!(42)),
        ("i32", "40 + 2", json!(42)),
        ("i64", "2147483648 + 1", json!(2147483649_i64)),
        ("i64", "-9223372036854775808 + 0", json!(i64::MIN)),
        ("isize", "40 + 2", json!(42)),
        ("u8", "255 - (7 % 4)", json!(252)),
        ("u16", "40 + 2", json!(42)),
        ("u32", "40 + 2", json!(42)),
        ("u64", "18446744073709551615 + 0", json!(u64::MAX)),
        ("usize", "40 + 2", json!(42)),
        ("f32", "-(40.0 + 2.0)", json!(-42.0)),
        ("f64", "-(40.0 + 2.0)", json!(-42.0)),
    ] {
        let src = format!("fn main() -> {ty} {{ {expression} }}");
        let (out, json) = invoke("run", &src);
        assert!(out.status.success(), "{src}: {json}");
        assert_eq!(json["success"], true);
        assert_eq!(json["result"]["value"], expected, "{src}");
    }
}

#[test]
fn invalid_literals_and_mixed_types_stop_before_execution() {
    for (src, code) in [
        ("fn main() -> i8 { 128 + 0 }", "E_LITERAL_OVERFLOW"),
        ("fn main() -> u8 { -1 + 1 }", "E_LITERAL_OVERFLOW"),
        (
            "fn main() -> i64 { let x: i32 = 1; x + 2 }",
            "E_TYPE_MISMATCH",
        ),
    ] {
        for command in ["check", "run"] {
            let (out, json) = invoke(command, src);
            assert_eq!(out.status.code(), Some(1), "{json}");
            assert_eq!(json["success"], false);
            assert!(
                json["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["code"] == code),
                "{json}"
            );
        }
    }
}

#[test]
fn numeric_context_example_and_typed_traps() {
    let (out, json) = invoke("run", include_str!("../../../examples/numeric-context.ixa"));
    assert!(out.status.success(), "{json}");
    assert_eq!(json["result"]["value"], 42);
    let (out, json) = invoke("run", "fn main() -> i64 { 42 / 0 }");
    assert_eq!(out.status.code(), Some(2), "{json}");
    assert!(json.to_string().contains("division by zero"));
}

#[test]
fn nested_branch_values_and_early_returns_keep_numeric_context() {
    let template = "fn choose(c: bool) -> i8 { if c { if c { return -128 + 1; } else { 6 * 7 } } else { -(40 + 2) } } fn main() -> i8 { choose(CONDITION) }";
    for (condition, expected) in [("true", -127), ("false", -42)] {
        let (out, json) = invoke("run", &template.replace("CONDITION", condition));
        assert!(out.status.success(), "{json}");
        assert_eq!(json["result"]["value"], expected);
    }
}

#[test]
fn default_integer_minimum_reaches_check_mir_and_execution() {
    for (expression, expected) in [
        ("-2147483648", -2147483648_i64),
        ("-(2147483648)", -2147483648),
        // Arithmetic still uses the interpreter's i128 superset.
        ("-(-2147483648)", 2147483648),
        ("-2147483648 + 0", -2147483648),
        ("0 + -2147483648", -2147483648),
        ("-2147483648 + 1", -2147483647),
        ("-1", -1),
    ] {
        let src = format!("fn main() -> i32 {{ let x = {expression}; x }}");
        for command in ["check", "mir", "run"] {
            let (out, json) = invoke(command, &src);
            assert!(out.status.success(), "{src}: {json}");
            assert_eq!(json["success"], true);
            if command == "mir" {
                let locals = json["result"]["mir"]["fns"][0]["locals"]
                    .as_array()
                    .unwrap();
                assert!(!locals.is_empty());
                assert!(locals.iter().all(|local| local["ty"]["kind"] == "i32"));
            }
            if command == "run" {
                assert_eq!(json["result"]["value"], expected, "{src}");
            }
        }
    }
}

#[test]
fn invalid_default_integer_literals_poison_mir_and_stop_execution() {
    for expression in ["-2147483649", "2147483648", "-(2147483648 + 0)"] {
        let src = format!("fn main() -> i32 {{ let x = {expression}; x }}");
        for command in ["check", "mir", "run"] {
            let (out, json) = invoke(command, &src);
            assert_eq!(out.status.code(), Some(1), "{src}: {json}");
            assert_eq!(json["success"], false);
            if command == "mir" {
                let locals = json["result"]["mir"]["fns"][0]["locals"]
                    .as_array()
                    .unwrap();
                assert!(locals.iter().any(|local| local["ty"]["kind"] == "poison"));
            } else {
                assert!(json["result"].is_null());
            }
            let diagnostics = json["diagnostics"].as_array().unwrap();
            assert_eq!(diagnostics.len(), 1, "{src}: {json}");
            assert_eq!(diagnostics[0]["code"], "E_LITERAL_OVERFLOW");
        }
    }
}
