//! Missing-return rejection through the database-backed check/run gates.
use serde_json::{Value, json};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Source(std::path::PathBuf);

impl Source {
    fn new(src: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ontixa_return_completion_{}_{}.ixa",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, src).unwrap();
        Self(path)
    }

    fn invoke(&self, command: &str) -> (Output, Value) {
        let out = Command::new(env!("CARGO_BIN_EXE_ontixa"))
            .args([command, self.0.to_str().unwrap(), "--json"])
            .output()
            .unwrap();
        let json: Value = serde_json::from_slice(&out.stdout).expect("one JSON envelope");
        assert_eq!(json["schema"], 1);
        (out, json)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn check_and_run_reject_discarded_short_circuit_returns_with_exact_diagnostics() {
    for body in [
        "{ false && { return 7; true } };",
        "{ true || { return 7; false } };",
        "let c = false; { c && { return 7; true } };",
        "let c = true; { c || { return 7; false } };",
        "match O::A { O::A => { false && { return 7; true } }, O::B => { return 8; } };",
        "{ 42 };",
        "if true { 42 } else { 7 };",
    ] {
        // A preceding item/trivia checks item-local diagnostic rebasing too.
        let prefix = "data O { A; B; }\n\n// fallthrough\nfn ";
        let file = Source::new(&format!("{prefix}main() -> i32 {{ {body} }}"));
        for command in ["check", "run"] {
            let (out, actual) = file.invoke(command);
            assert_eq!(out.status.code(), Some(1), "{body}: {actual}");
            assert!(out.stderr.is_empty());
            assert_eq!(
                actual,
                json!({
                    "command": command,
                    "diagnostics": [{
                        "code": "E_MISSING_RETURN", "help": [], "labels": [],
                        "message": "function `main` can complete without returning a value",
                        "notes": [],
                        "primary": { "end": prefix.len() + 4, "file": file.0.to_str().unwrap(), "start": prefix.len() },
                        "severity": "error", "subject": "main"
                    }],
                    "error": null, "result": null, "schema": 1, "success": false, "timings": []
                })
            );
            assert_eq!(out.stdout, file.invoke(command).0.stdout);
        }
    }
}

#[test]
fn required_and_skipped_returns_keep_the_correct_runtime_value() {
    for (body, expected) in [
        ("{ false && { return 7; true } }; 42", 42),
        ("{ true && { return 7; true } }; 42", 7),
        ("{ true || { return 7; false } }; return 42;", 42),
        ("{ false || { return 7; false } }; return 42;", 7),
        ("{ { return 7; false } && true };", 7),
        ("{ { return 7; true } || false };", 7),
        ("{ { 42 } }", 42),
        ("{ { return 42; } }", 42),
        ("return 42; let dead = 1;", 42),
        ("let value = { return 7; 0 };", 7),
        ("let mut value = 0; value = { return 7; 1 };", 7),
        ("!{ return 7; false };", 7),
        ("1 + { return 7; 2 };", 7),
        ("if { return 7; true } {}", 7),
        ("[1, { return 7; 2 }];", 7),
        ("[1][{ return 7; 0 }];", 7),
        ("[1][{ return 7; 0 }..1];", 7),
        ("[1][0..({ return 7; 1 })];", 7),
        ("{ return 7; [1] }.len;", 7),
        ("for i in 0..({ return 7; 1 }) {}", 7),
        ("for i in { return 7; [1] } {}", 7),
        ("for i in 0..0 { return 7; }; 42", 42),
        ("for i in 0..1 { return 7; }; 42", 7),
        ("if false { return 7; } else { return 42; };", 42),
    ] {
        let file = Source::new(&format!("fn main() -> i32 {{ {body} }}"));
        let (out, json) = file.invoke("run");
        assert!(out.status.success(), "{body}: {json}");
        assert_eq!(json["result"]["value"], expected, "{body}");
    }
}

#[test]
fn unit_discard_still_returns_unit() {
    let file = Source::new("fn main() { { 42 }; }");
    let (out, json) = file.invoke("run");
    assert!(out.status.success(), "{json}");
    assert_eq!(json["result"]["display"], "unit");
}

#[test]
fn invalid_returns_and_unreachable_children_are_still_execution_errors() {
    for (src, code) in [
        ("fn main() -> i32 { { return true; } }", "E_TYPE_MISMATCH"),
        (
            "fn main() -> i32 { { return 7; true } && 1; }",
            "E_TYPE_MISMATCH",
        ),
        (
            "fn main() -> i32 { return 7; let x: bool = 1; }",
            "E_TYPE_MISMATCH",
        ),
        (
            "fn main() -> i32 { let x: i32; false && { x = 7; true }; x }",
            "E_UNINITIALIZED",
        ),
        ("fn main() -> bool { false & true }", "E_PARSE"),
    ] {
        let file = Source::new(src);
        for command in ["check", "run"] {
            let (out, json) = file.invoke(command);
            assert_eq!(out.status.code(), Some(1), "{src}: {json}");
            assert_eq!(json["success"], false);
            assert_eq!(json["result"], Value::Null);
            assert!(
                json["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|d| d["code"] == code)
            );
        }
    }
}

#[test]
fn return_completion_example_runs_and_has_deterministic_artifacts() {
    let file = Source::new(include_str!("../../../examples/return-completion.ixa"));
    let (out, json) = file.invoke("run");
    assert!(out.status.success(), "{json}");
    assert_eq!(json["result"]["value"], 42);
    for command in ["mir", "graph"] {
        let first = file.invoke(command).0;
        let second = file.invoke(command).0;
        assert!(first.status.success() && second.status.success());
        assert_eq!(first.stdout, second.stdout);
    }
}

#[test]
fn non_copy_tail_carriers_still_escape_and_consume_the_caller_argument() {
    for (defs, argument, access) in [
        (
            "data P { x: i32; } fn keep(p: P) -> P { { p } }",
            "P { x: 42 }",
            "q.x",
        ),
        ("fn keep(p: [i32]) -> [i32] { { p } }", "[42]", "q[0]"),
        (
            "data P { x: i32; } data O { A(P); } fn keep(p: O) -> P { match p { O::A(value) => { value } } }",
            "O::A(P { x: 42 })",
            "q.x",
        ),
    ] {
        let file = Source::new(&format!(
            "{defs} fn main() -> i32 {{ let p = {argument}; let q = keep(p); {access} }}"
        ));
        let (out, value) = file.invoke("run");
        assert!(out.status.success(), "{value}");
        assert_eq!(value["result"]["value"], 42);
        let (out, explain) = file.invoke("explain");
        assert!(out.status.success(), "{explain}");
        let keep = explain["result"]["defs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| d["name"] == "keep")
            .unwrap();
        assert_eq!(keep["params"][0]["behavior"], "escape");
        assert_eq!(keep["params"][0]["escapes"], json!(["return"]));
        for command in ["mir", "graph"] {
            let first = file.invoke(command).0;
            let second = file.invoke(command).0;
            assert!(first.status.success() && second.status.success());
            assert_eq!(first.stdout, second.stdout);
        }
        let reuse = Source::new(&format!(
            "{defs} fn main() -> i32 {{ let p = {argument}; let q = keep(p); let again = keep(p); {access} }}"
        ));
        let (out, errors) = reuse.invoke("check");
        assert_eq!(out.status.code(), Some(1));
        assert!(
            errors["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == "E_USE_AFTER_MOVE")
        );
    }
}
