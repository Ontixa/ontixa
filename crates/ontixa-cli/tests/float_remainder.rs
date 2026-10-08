//! Floating remainder through the real CLI, database and interpreter.
use serde_json::Value;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

fn invoke(command: &str, src: &str, json: bool) -> Output {
    let path = std::env::temp_dir().join(format!(
        "ontixa_float_remainder_{}_{}.ixa",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, src).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_ontixa"));
    cmd.args([command, path.to_str().unwrap()]);
    if json {
        cmd.arg("--json");
    }
    let out = cmd.output().unwrap();
    std::fs::remove_file(path).unwrap();
    out
}

fn envelope(out: &Output) -> Value {
    let json: Value = serde_json::from_slice(&out.stdout).expect("one JSON envelope");
    assert_eq!(json["schema"], 1);
    assert!(out.stderr.is_empty(), "{out:?}");
    json
}

#[test]
fn float_remainder_checks_and_runs_at_both_widths() {
    for ty in ["f32", "f64"] {
        for (expr, expected) in [
            ("5.5 % 2.0", 1.5_f64),
            ("-5.5 % 2.0", -1.5),
            ("5.5 % -2.0", 1.5),
            ("-5.5 % -2.0", -1.5),
            ("-4.0 % 2.0", -0.0),
            ("5.5 % (1.0 / 0.0)", 5.5),
        ] {
            let src = format!("fn main() -> {ty} {{ {expr} }}");
            for command in ["check", "run"] {
                let out = invoke(command, &src, true);
                let json = envelope(&out);
                assert!(out.status.success(), "{src}: {json}");
                assert_eq!(json["success"], true);
                assert_eq!(json["diagnostics"], serde_json::json!([]));
                if command == "run" {
                    assert_eq!(
                        json["result"]["value"].as_f64().unwrap().to_bits(),
                        expected.to_bits(),
                        "{src}"
                    );
                }
            }
        }
    }
}

#[test]
fn nan_remainder_uses_existing_float_output() {
    for ty in ["f32", "f64"] {
        for expr in ["5.5 % 0.0", "(1.0 / 0.0) % 2.0", "5.5 % (0.0 / 0.0)"] {
            let src = format!("fn main() -> {ty} {{ {expr} }}");
            let out = invoke("run", &src, true);
            let json = envelope(&out);
            assert!(out.status.success(), "{src}: {json}");
            assert_eq!(json["success"], true);
            assert_eq!(json["diagnostics"], serde_json::json!([]));
            // JSON has no NaN number; this is the existing float serializer.
            assert!(json["result"]["value"].is_null());
            let out = invoke("run", &src, false);
            assert!(out.status.success(), "{out:?}");
            assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "NaN");
        }
    }
}

#[test]
fn remainder_still_rejects_mixed_types() {
    for src in [
        "fn main() -> f32 { let a: f32 = 5.5; let b: f64 = 2.0; a % b }",
        "fn main() -> f64 { let a: f64 = 5.5; let b: i32 = 2; a % b }",
    ] {
        for command in ["check", "run"] {
            let out = invoke(command, src, true);
            let json = envelope(&out);
            assert_eq!(out.status.code(), Some(1), "{json}");
            assert_eq!(json["success"], false);
            assert!(json["result"].is_null());
            assert!(
                json["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["code"] == "E_TYPE_MISMATCH"),
                "{json}"
            );
        }
    }
}

#[test]
fn remainder_example_runs_and_integer_behavior_is_preserved() {
    let out = invoke(
        "run",
        include_str!("../../../examples/float-remainder.ixa"),
        true,
    );
    assert!(out.status.success(), "{}", envelope(&out));
    assert_eq!(envelope(&out)["result"]["value"], 42.0);

    let out = invoke("run", "fn main() -> i32 { -5 % 2 }", true);
    assert!(out.status.success(), "{}", envelope(&out));
    assert_eq!(envelope(&out)["result"]["value"], -1);

    let out = invoke("run", "fn main() -> i32 { 5 % 0 }", true);
    let json = envelope(&out);
    assert_eq!(out.status.code(), Some(2), "{json}");
    assert_eq!(json["success"], false);
    assert_eq!(json["error"]["kind"], "runtime");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("remainder by zero")
    );
}
