//! Exercise the same semantics through the database-backed CLI and JSON envelope.
use serde_json::Value;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn invoke(command: &str, src: &str) -> (Output, Value) {
    let path = std::env::temp_dir().join(format!(
        "ontixa_short_circuit_{}_{}.ixa",
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
fn short_circuit_run_json_skips_rhs_and_preserves_required_traps() {
    for (expr, expected) in [
        ("false && (1 / 0 == 0)", false),
        ("true || (1 / 0 == 0)", true),
    ] {
        let src = format!("fn main() -> bool {{ return {expr}; }}");
        let (out, json) = invoke("run", &src);
        assert!(out.status.success(), "{json}");
        assert_eq!(json["success"], true);
        assert_eq!(json["result"]["value"], expected);
    }
    for expr in ["true && (1 / 0 == 0)", "false || (1 / 0 == 0)"] {
        let src = format!("fn main() -> bool {{ return {expr}; }}");
        let (out, json) = invoke("run", &src);
        assert_eq!(out.status.code(), Some(2));
        assert_eq!(json["success"], false);
        assert!(json.to_string().contains("division by zero"));
    }
}

#[test]
fn short_circuit_check_json_rejects_conditional_initialization_and_invalid_rhs() {
    for (src, code) in [
        (
            "fn main() -> i32 { let x: i32; let b = false && { x = 42; true }; return x; }",
            "E_UNINITIALIZED",
        ),
        (
            "fn main() -> bool { return true || 42; }",
            "E_TYPE_MISMATCH",
        ),
        ("fn main() -> bool { return false & true; }", "E_PARSE"),
        ("fn main() -> bool { return false | true; }", "E_PARSE"),
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
fn short_circuit_example_runs_and_mir_is_deterministic() {
    let src = include_str!("../../../examples/short-circuit.ixa");
    let (out, json) = invoke("run", src);
    assert!(out.status.success(), "{json}");
    assert_eq!(json["result"]["value"], 42);
    // Use one filename for byte stability (the envelope includes it).
    let file =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/short-circuit.ixa");
    let dump = || {
        Command::new(env!("CARGO_BIN_EXE_ontixa"))
            .args(["mir", file.to_str().unwrap(), "--json"])
            .output()
            .unwrap()
    };
    let first = dump();
    let second = dump();
    assert!(first.status.success() && second.status.success());
    assert_eq!(first.stdout, second.stdout);
}
