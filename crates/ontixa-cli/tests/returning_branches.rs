//! Mixed returning/value branches through the public check, run, and explain gates.
use serde_json::{Value, json};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Source(std::path::PathBuf);

impl Source {
    fn new(source: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ontixa_returning_branches_{}_{}.ixa",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, source).unwrap();
        Self(path)
    }

    fn invoke(&self, command: &str) -> (Output, Value) {
        let out = Command::new(env!("CARGO_BIN_EXE_ontixa"))
            .args([command, self.0.to_str().unwrap(), "--json"])
            .output()
            .unwrap();
        let json: Value = serde_json::from_slice(&out.stdout).expect("one JSON envelope");
        assert_eq!(json["schema"], 1);
        assert!(out.stderr.is_empty());
        (out, json)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn rejects(source: &str, code: &str) {
    let file = Source::new(source);
    for command in ["check", "run"] {
        let (out, json) = file.invoke(command);
        assert_eq!(out.status.code(), Some(1), "{source}: {json}");
        assert_eq!(json["success"], false);
        assert_eq!(json["result"], Value::Null);
        assert!(
            json["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == code),
            "{source}: {json}"
        );
    }
}

#[test]
fn returning_and_continuing_branches_run_in_both_orientations() {
    for (body, on_true, on_false) in [
        ("if c { return 42; } else { 7 }", 42, 7),
        ("if c { 42 } else { return 7; }", 42, 7),
        ("if c { return 42; } else { return 7; }", 42, 7),
        ("if c { return 7; }; 42", 7, 42),
        ("let x: i32; if c { x = 42; } else { return 7; }; x", 42, 7),
        ("let x: i32; if c { return 7; } else { x = 42; }; x", 7, 42),
        (
            "if c { if c { return 42; } else { return 1; } } else { 7 }",
            42,
            7,
        ),
    ] {
        for (flag, expected) in [(true, on_true), (false, on_false)] {
            let file = Source::new(&format!(
                "fn choose(c: bool) -> i32 {{ {body} }} fn main() -> i32 {{ choose({flag}) }}"
            ));
            let (out, actual) = file.invoke("run");
            assert!(out.status.success(), "{body}, {flag}: {actual}");
            assert_eq!(actual["result"]["value"], expected, "{body}, {flag}");
        }
    }
}

#[test]
fn mixed_branch_mir_assigns_the_live_i32_result_and_keeps_the_early_return() {
    for (body, returns_from_then, returned, continued) in [
        ("if c { return 42; } else { 7 }", true, 42, 7),
        ("if c { 42 } else { return 7; }", false, 7, 42),
    ] {
        let file = Source::new(&format!("fn choose(c: bool) -> i32 {{ {body} }}"));
        let (out, result) = file.invoke("mir");
        assert!(out.status.success(), "{result}");
        let function = &result["result"]["mir"]["fns"][0];
        let blocks = function["blocks"].as_array().unwrap();
        let branch = &blocks
            .iter()
            .find(|b| b["term"]["kind"] == "Branch")
            .unwrap()["term"];
        let block = |id: &Value| blocks.iter().find(|b| &b["id"] == id).unwrap();
        let (return_key, live_key) = if returns_from_then {
            ("then", "else_")
        } else {
            ("else_", "then")
        };
        let returning = block(&branch[return_key]);
        assert_eq!(returning["stmts"], json!([]));
        assert_eq!(
            returning["term"],
            json!({ "kind": "Return", "value": { "kind": "Int", "value": returned } })
        );
        let live = block(&branch[live_key]);
        assert_eq!(live["stmts"].as_array().unwrap().len(), 1);
        let assign = &live["stmts"][0];
        assert_eq!(assign["kind"], "Assign");
        assert_eq!(assign["val"], json!({ "kind": "Int", "value": continued }));
        let local = assign["dst"]["local"].as_u64().unwrap() as usize;
        assert_eq!(function["locals"][local]["ty"], json!({ "kind": "i32" }));
        assert_eq!(live["term"]["kind"], "Goto");
        let join = block(&live["term"]["target"]);
        assert_eq!(
            join["term"],
            json!({ "kind": "Return", "value": { "kind": "Place", "local": local, "proj": [] } })
        );
    }
}

#[test]
fn owned_guards_and_mixed_tails_escape_and_consume_caller_values() {
    for (defs, ty, value, access) in [
        ("data P { x: i32; }", "P", "P { x: 42 }", "q.x"),
        ("", "[i32]", "[42]", "q[0]"),
    ] {
        for body in [
            "if c { return p; }; p",
            "if c {} else { return p; }; p",
            "if c { return p; } else { p }",
            "if c { p } else { return p; }",
        ] {
            let keep = format!("{defs} fn keep(p: {ty}, c: bool) -> {ty} {{ {body} }}");
            for flag in [true, false] {
                let file = Source::new(&format!(
                    "{keep} fn main() -> i32 {{ let p = {value}; let q = keep(p, {flag}); {access} }}"
                ));
                let (out, result) = file.invoke("run");
                assert!(out.status.success(), "{body}, {flag}: {result}");
                assert_eq!(result["result"]["value"], 42);
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
                assert!(
                    keep["params"][0]["evidence"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|e| e["kind"] == "escaped")
                );
            }
            let reuse = format!(
                "{keep} fn main() -> i32 {{ let p = {value}; let q = keep(p, false); let again = keep(p, true); {access} }}"
            );
            rejects(&reuse, "E_USE_AFTER_MOVE");
            let (_, rejected) = Source::new(&reuse).invoke("check");
            assert_eq!(
                rejected["diagnostics"].as_array().unwrap().len(),
                1,
                "{rejected}"
            );
        }
    }
}

#[test]
fn nested_returning_guards_preserve_the_continuing_owner() {
    for body in [
        "if c { if c { return p; } else { return p; }; }; p",
        "if c { match O::A { O::A => { return p; }, O::B => { return p; } }; }; p",
        "if c && { return p; true } { return p; }; p",
    ] {
        for flag in [true, false] {
            let file = Source::new(&format!(
                "data P {{ x: i32; }} data O {{ A; B; }} fn keep(p: P, c: bool) -> P {{ {body} }} fn main() -> i32 {{ keep(P {{ x: 42 }}, {flag}).x }}"
            ));
            let (out, result) = file.invoke("run");
            assert!(out.status.success(), "{body}, {flag}: {result}");
            assert_eq!(result["result"]["value"], 42);
        }
    }
}

#[test]
fn unreachable_return_payloads_and_children_still_reject_errors() {
    for body in [
        "if c { return false; } else { 42 }",
        "if c { 42 } else { return false; }",
        "if c { return 42; let dead: bool = 1; } else { 7 }",
        "if c { return 42; } else { false }",
        "if { return 42; true } { 7 } else { false }",
    ] {
        rejects(
            &format!("fn choose(c: bool) -> i32 {{ {body} }} fn main() -> i32 {{ choose(false) }}"),
            "E_TYPE_MISMATCH",
        );
    }
    let defs = "data P { x: i32; } fn mix(a: P, mut b: P) -> i32 { b.x = a.x; b.x } fn take(a: P, b: P) -> i32 { let moved = b; a.x + moved.x }";
    for (call, code) in [
        ("mix(q, q)", "E_BORROW_CONFLICT"),
        ("take(q, q)", "E_MOVE_WHILE_BORROWED"),
    ] {
        for body in [
            format!("if false {{ return {call}; }} else {{ 42 }}"),
            format!("if true {{ 42 }} else {{ return {call}; }}"),
        ] {
            rejects(
                &format!("{defs} fn main() -> i32 {{ let mut q = P {{ x: 42 }}; {body} }}"),
                code,
            );
        }
    }
}

#[test]
fn outer_call_argument_loan_survives_a_returning_second_argument() {
    rejects(
        "data P { x: i32; } fn two(a: P, b: P) -> i32 { a.x + b.x } fn take(a: P, b: P) -> i32 { let moved = b; a.x + moved.x } fn main() -> i32 { let q = P { x: 42 }; two(q, if false { return take(q, q); } else { q }) }",
        "E_MOVE_WHILE_BORROWED",
    );
}

#[test]
fn paths_that_can_continue_keep_missing_return_and_ownership_errors() {
    for (body, code) in [
        ("if c { return 42; };", "E_MISSING_RETURN"),
        ("let x: i32; if c { x = 42; }; x", "E_UNINITIALIZED"),
        (
            "let x: i32; if c { return 7; } else {}; x",
            "E_UNINITIALIZED",
        ),
        (
            "let x: i32; if c { false && { x = 42; true }; } else { return 7; }; x",
            "E_UNINITIALIZED",
        ),
        (
            "if c { for i in 0..0 { return 7; } } else { return 42; };",
            "E_MISSING_RETURN",
        ),
    ] {
        rejects(
            &format!("fn choose(c: bool) -> i32 {{ {body} }} fn main() -> i32 {{ choose(false) }}"),
            code,
        );
    }
    for body in [
        "if c { let q = p; } else { return p; }; p",
        "if c { return p; } else { let q = p; }; p",
        "if c { false && { let q = p; true }; } else { return p; }; p",
        "if c { let q = p; return p; }; p",
    ] {
        rejects(
            &format!(
                "data P {{ x: i32; }} fn keep(p: P, c: bool) -> P {{ {body} }} fn main() -> i32 {{ keep(P {{ x: 42 }}, false).x }}"
            ),
            "E_USE_AFTER_MOVE",
        );
    }
}

#[test]
fn returning_branches_example_runs_with_deterministic_artifacts() {
    let file = Source::new(include_str!("../../../examples/returning-branches.ixa"));
    let (out, result) = file.invoke("run");
    assert!(out.status.success(), "{result}");
    assert_eq!(result["result"]["value"], 42);
    for command in ["mir", "graph", "explain"] {
        let first = file.invoke(command).0;
        let second = file.invoke(command).0;
        assert!(first.status.success() && second.status.success());
        assert_eq!(first.stdout, second.stdout);
    }
}
