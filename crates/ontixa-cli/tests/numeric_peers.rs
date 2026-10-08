//! Peer literal types reach MIR without changing written evaluation order.
use serde_json::Value;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn invoke(command: &str, src: &str) -> (Output, Value) {
    let path = std::env::temp_dir().join(format!(
        "ontixa_numeric_peers_{}_{}.ixa",
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
fn peers_reach_check_mir_and_execution() {
    for (ty, zero, literal) in [("i64", "0", "2"), ("f32", "0.0", "2.0")] {
        for expression in [format!("{literal} + x"), format!("x + {literal}")] {
            let src = format!(
                "fn main() -> bool {{ let x: {ty} = {literal}; let y = {expression}; {zero} < y }}"
            );
            for command in ["check", "mir", "run"] {
                let (out, json) = invoke(command, &src);
                assert!(out.status.success(), "{src}: {json}");
                assert_eq!(json["success"], true);
                if command == "run" {
                    assert_eq!(json["result"]["value"], true);
                }
                if command == "mir" {
                    let locals = json["result"]["mir"]["fns"][0]["locals"]
                        .as_array()
                        .unwrap();
                    assert!(locals.iter().any(|l| l["ty"]["kind"] == ty));
                    assert!(
                        locals
                            .iter()
                            .all(|l| l["ty"]["kind"] == ty || l["ty"]["kind"] == "bool")
                    );
                }
            }
        }
    }
}

#[test]
fn noncommutative_operators_preserve_order_and_evaluate_peers_once() {
    for (expression, expected) in [
        ("20 - { n = n + 1; let result: i64 = 4; result }", 161),
        ("20 / { n = n + 1; let result: i64 = 4; result }", 51),
        ("20 % { n = n + 1; let result: i64 = 6; result }", 21),
        ("{ n = n + 1; let result: i64 = 4; result } - 20", -159),
    ] {
        let src = format!(
            "fn main() -> i64 {{ let mut n: i64 = 0; let value = {expression}; value * 10 + n }}"
        );
        let (out, json) = invoke("run", &src);
        assert!(out.status.success(), "{src}: {json}");
        assert_eq!(json["result"]["value"], expected, "{src}");
    }
    let (out, json) = invoke(
        "run",
        "fn main() -> bool { let x: f32 = 4.0; let y = 20.0 / x; y == 5.0 }",
    );
    assert!(out.status.success(), "{json}");
    assert_eq!(json["result"]["value"], true);
}

#[test]
fn literal_deferral_keeps_initialization_and_short_circuit_rules() {
    for (src, expected) in [
        (
            "fn main() -> i64 { let mut x: i64; let y = 1 + { x = 2; x }; y + x }",
            5,
        ),
        (
            "fn main() -> i64 { let mut x: i64 = 0; let y = false && (0 < { x = 1; x }); x }",
            0,
        ),
        (
            "fn main() -> i64 { let mut x: i64 = 0; let y = true || (0 < { x = 1; x }); x }",
            0,
        ),
        (
            "fn main() -> i64 { let mut x: i64 = 0; let y: i64 = { x = 1; 10 } + { x = x + 1; 20 }; x }",
            2,
        ),
    ] {
        let (out, json) = invoke("run", src);
        assert!(out.status.success(), "{src}: {json}");
        assert_eq!(json["result"]["value"], expected, "{src}");
    }
    for src in [
        "fn main() -> i64 { let x: i64; let y = 1 + x; y }",
        "fn main() -> i64 { let mut x: i64; let y: i64 = x + { x = 1; 1 }; y }",
        "fn main() -> i64 { let mut x: i64; let y = false && (0 < { x = 1; x }); x }",
    ] {
        let (out, json) = invoke("run", src);
        assert_eq!(out.status.code(), Some(1), "{src}: {json}");
        assert!(
            json["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == "E_UNINITIALIZED"),
            "{src}: {json}"
        );
        assert!(json["result"].is_null());
    }
}

#[test]
fn adopted_overflows_and_typed_mismatches_stop_execution() {
    for (src, code) in [
        (
            "fn main() -> bool { let x: i8 = 0; 128 > x }",
            "E_LITERAL_OVERFLOW",
        ),
        (
            "fn main() -> bool { let x: u8 = 0; -1 < x }",
            "E_LITERAL_OVERFLOW",
        ),
        (
            "fn main() -> bool { let x: i64 = 2; let y = 0; y < x }",
            "E_TYPE_MISMATCH",
        ),
        (
            "fn main() -> bool { let x: f32 = 2.0; 0 < x }",
            "E_TYPE_MISMATCH",
        ),
    ] {
        for command in ["check", "run"] {
            let (out, json) = invoke(command, src);
            assert_eq!(out.status.code(), Some(1), "{src}: {json}");
            assert!(
                json["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["code"] == code),
                "{src}: {json}"
            );
            assert!(json["result"].is_null());
        }
    }
}

#[test]
fn context_dependent_peers_keep_existing_signed_minima_and_return_paths() {
    for (src, expected) in [
        (
            "fn main() -> i32 { let x = 0 + { -2147483648 }; x }",
            serde_json::json!(-2147483648_i64),
        ),
        (
            "fn main() -> i32 { let x = 0 + -2147483648; x }",
            serde_json::json!(-2147483648_i64),
        ),
        (
            "fn main() -> bool { 0 > { -2147483648 } }",
            serde_json::json!(true),
        ),
        (
            "fn main() -> bool { let mut x: i32 = 0; 0 > { x = 1; -2147483648 } }",
            serde_json::json!(true),
        ),
        (
            "fn main() -> bool { 0 > if true { -2147483648 } else { 0 } }",
            serde_json::json!(true),
        ),
        (
            "fn main() -> bool { 0 < if true { return true; } else { return false; } }",
            serde_json::json!(true),
        ),
        (
            "fn main() -> i32 { let x = 0 + if true { return 7; } else { return 8; }; x }",
            serde_json::json!(7),
        ),
        (
            "data Choice { A; B; } fn main() -> bool { 0 < match Choice::A { Choice::A => { return true; }, Choice::B => { return false; } } }",
            serde_json::json!(true),
        ),
    ] {
        let (out, json) = invoke("run", src);
        assert!(out.status.success(), "{src}: {json}");
        assert_eq!(json["result"]["value"], expected, "{src}");
    }
}
