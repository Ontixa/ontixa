//! Contextual returning blocks through public diagnostics, MIR, and execution.
use serde_json::{Value, json};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Source(std::path::PathBuf);

impl Source {
    fn new(source: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "ontixa_returning_blocks_{}_{}.ixa",
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
fn returning_blocks_execute_before_their_enclosing_operation() {
    for body in [
        "id({ return 42; })",
        "let x: i32 = { return 42; }; x",
        "let mut x = 0; x = { return 42; }; x",
        "return { return 42; };",
        "1 + { return 42; }",
        "{ return 42; } + 1",
        "-{ return 42; }",
        "let xs: [i32] = [{ return 42; }]; xs[0]",
        "P { x: { return 42; } }.x",
        "O::A({ return 42; }); 0",
        "id({ { return 42; }; })",
        "id({ if true { return 42; } else { return 7; }; })",
        "id({ match O::A(0) { O::A(x) => { return 42; } }; })",
        "if { return 42; } { 1 } else { 2 }",
    ] {
        let file = Source::new(&format!(
            "data P {{ x: i32; }} data O {{ A(i32); }} fn id(x: i32) -> i32 {{ 1 / 0 }} fn main() -> i32 {{ {body} }}"
        ));
        let (out, checked) = file.invoke("check");
        assert!(out.status.success(), "{body}: {checked}");
        let (out, run) = file.invoke("run");
        assert!(out.status.success(), "{body}: {run}");
        assert_eq!(run["result"]["value"], 42, "{body}");
    }
}

#[test]
fn conditional_returns_execute_only_on_the_selected_path() {
    for (body, on_true, on_false) in [
        ("c && { return 42; }; 7", 42, 7),
        ("c || { return 42; }; 7", 7, 42),
        ("id({ if c { return 42; } else { return 7; }; })", 42, 7),
        ("if c { id({ return 42; }) } else { 7 }", 42, 7),
    ] {
        for (flag, expected) in [(true, on_true), (false, on_false)] {
            let file = Source::new(&format!(
                "fn id(x: i32) -> i32 {{ 1 / 0 }} fn choose(c: bool) -> i32 {{ {body} }} fn main() -> i32 {{ choose({flag}) }}"
            ));
            let (out, run) = file.invoke("run");
            assert!(out.status.success(), "{body}, {flag}: {run}");
            assert_eq!(run["result"]["value"], expected, "{body}, {flag}");
        }
    }
}

#[test]
fn reachable_mir_keeps_returns_and_never_uses_a_returning_blocks_unit_operand() {
    let file = Source::new("fn id(x: i32) -> i32 { x } fn main() -> i32 { id({ return 42; }) }");
    let (out, result) = file.invoke("mir");
    assert!(out.status.success(), "{result}");
    let entry = &result["result"]["mir"]["fns"][1]["blocks"][0];
    assert_eq!(entry["stmts"], json!([]));
    assert_eq!(
        entry["term"],
        json!({ "kind": "Return", "value": { "kind": "Int", "value": 42 } })
    );

    for op in ["&&", "||"] {
        let file = Source::new(&format!(
            "fn choose(c: bool) -> i32 {{ c {op} {{ return 42; }}; 7 }}"
        ));
        let (out, result) = file.invoke("mir");
        assert!(out.status.success(), "{result}");
        let blocks = result["result"]["mir"]["fns"][0]["blocks"]
            .as_array()
            .unwrap();
        let mut pending = vec![json!(0)];
        let mut visited = Vec::new();
        let mut returns = Vec::new();
        while let Some(id) = pending.pop() {
            if visited.contains(&id) {
                continue;
            }
            let block = blocks.iter().find(|b| b["id"] == id).unwrap();
            visited.push(id);
            for stmt in block["stmts"].as_array().unwrap() {
                assert_ne!(stmt["val"]["kind"], "Unit", "{block}");
            }
            let term = &block["term"];
            match term["kind"].as_str().unwrap() {
                "Branch" => pending.extend([term["then"].clone(), term["else_"].clone()]),
                "Goto" => pending.push(term["target"].clone()),
                "Return" => returns.push(term["value"]["value"].as_i64().unwrap()),
                other => panic!("unexpected terminator {other}"),
            }
        }
        returns.sort();
        assert_eq!(returns, [7, 42]);
    }
}

#[test]
fn owned_returns_keep_escape_facts_and_consume_the_caller_value() {
    for (defs, ty, value, access) in [
        ("data P { x: i32; }", "P", "P { x: 42 }", "q.x"),
        ("", "[i32]", "[42]", "q[0]"),
    ] {
        for body in [
            "id({ return p; })",
            "if c { id({ return p; }) } else { p }",
            "c && { return p; }; p",
            "c || { return p; }; p",
        ] {
            let keep = format!(
                "{defs} fn id(p: {ty}) -> {ty} {{ p }} fn keep(p: {ty}, c: bool) -> {ty} {{ {body} }}"
            );
            for flag in [true, false] {
                let file = Source::new(&format!(
                    "{keep} fn main() -> i32 {{ let p = {value}; let q = keep(p, {flag}); {access} }}"
                ));
                let (out, run) = file.invoke("run");
                assert!(out.status.success(), "{body}, {flag}: {run}");
                assert_eq!(run["result"]["value"], 42);
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
            rejects(
                &format!(
                    "{keep} fn main() -> i32 {{ let p = {value}; let q = keep(p, false); let again = keep(p, true); {access} }}"
                ),
                "E_USE_AFTER_MOVE",
            );
        }
    }
}

#[test]
fn type_and_ownership_errors_remain_public_check_and_run_gates() {
    for (body, code) in [
        ("id({ return false; })", "E_TYPE_MISMATCH"),
        ("id({ return 42; let dead: bool = 1; })", "E_TYPE_MISMATCH"),
        ("id({ return 42; false })", "E_TYPE_MISMATCH"),
        ("id({ if true { return 42; }; })", "E_TYPE_MISMATCH"),
        ("id({ for i in 0..1 { return 42; }; })", "E_TYPE_MISMATCH"),
        ("{ false && { return 42; } };", "E_MISSING_RETURN"),
        ("{ 42 };", "E_MISSING_RETURN"),
        (
            "let x: i32; false && { x = 1; return 42; }; x",
            "E_UNINITIALIZED",
        ),
        ("id({ let x: i32; return x; })", "E_UNINITIALIZED"),
    ] {
        rejects(
            &format!("fn id(x: i32) -> i32 {{ x }} fn main() -> i32 {{ {body} }}"),
            code,
        );
    }
    for body in [
        "if c { let q = p; } else { id({ return p; }); }; p",
        "c && { let q = p; true }; id({ return p; })",
        "id({ let q = p; return p; })",
    ] {
        rejects(
            &format!(
                "data P {{ x: i32; }} fn id(p: P) -> P {{ p }} fn keep(p: P, c: bool) -> P {{ {body} }} fn main() -> i32 {{ keep(P {{ x: 42 }}, true).x }}"
            ),
            "E_USE_AFTER_MOVE",
        );
    }
    for (call, code) in [
        ("mix(q, q)", "E_BORROW_CONFLICT"),
        ("take(q, q)", "E_MOVE_WHILE_BORROWED"),
    ] {
        rejects(
            &format!(
                "data P {{ x: i32; }} fn two(a: P, b: P) -> i32 {{ a.x + b.x }} fn mix(a: P, mut b: P) -> i32 {{ b.x = a.x; b.x }} fn take(a: P, b: P) -> i32 {{ let moved = b; a.x + moved.x }} fn main() -> i32 {{ let mut q = P {{ x: 42 }}; two(q, {{ return {call}; }}) }}"
            ),
            code,
        );
    }
}

#[test]
fn returning_blocks_example_has_deterministic_public_artifacts() {
    let file = Source::new(include_str!("../../../examples/returning-blocks.ixa"));
    let (out, run) = file.invoke("run");
    assert!(out.status.success(), "{run}");
    assert_eq!(run["result"]["value"], 42);
    for command in ["check", "mir", "graph", "explain"] {
        let first = file.invoke(command).0;
        let second = file.invoke(command).0;
        assert!(first.status.success() && second.status.success());
        assert_eq!(first.stdout, second.stdout);
    }
}
