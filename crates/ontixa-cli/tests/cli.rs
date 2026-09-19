//! Acceptance gate: every `--json` command emits exactly one valid
//! JSON envelope on stdout — parseable by a single `from_slice`, no
//! trailing documents, no logs, no ANSI.

use serde_json::Value;
use std::io::Write;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_ontixa");

const SRC: &str = "data P { x: i32; }\n\
                   fn read(p: P) -> i32 { return p.x; }\n\
                   fn bump(mut p: P) { p.x = p.x + 1; }\n\
                   fn main() -> i32 { let mut q = P { x: 1 }; bump(q); return read(q) + 5; }";

/// Writes `src` to a uniquely named temp file; returns its path.
fn src_file(name: &str, src: &str) -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("ontixa_cli_test_{name}_{}.ixa", std::process::id()));
    let mut f = std::fs::File::create(&path).expect("create temp source");
    f.write_all(src.as_bytes()).expect("write temp source");
    path
}

fn ontixa(args: &[&str]) -> Output {
    Command::new(BIN).args(args).output().expect("spawn ontixa")
}

/// The whole stdout of a JSON-mode invocation must be one document.
fn envelope(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not one JSON document: {e}\nstdout: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn stdout_str(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn check_json_is_one_document() {
    let f = src_file("check", SRC);
    let out = ontixa(&["check", f.to_str().unwrap(), "--json", "--timings"]);
    let d = envelope(&out);
    assert_eq!(d["schema"], 1);
    assert_eq!(d["command"], "check");
    assert_eq!(d["success"], true);
    assert!(d["diagnostics"].is_array());
    let stages: Vec<&str> = d["timings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["stage"].as_str())
        .collect();
    for stage in ["lex+parse", "ast", "hir", "types", "ownership", "mir"] {
        assert!(stages.contains(&stage), "missing stage {stage}");
    }
    assert!(out.status.success());
}

#[test]
fn run_json_is_one_document_with_value() {
    let f = src_file("run", SRC);
    let out = ontixa(&["run", f.to_str().unwrap(), "--json", "--timings"]);
    let d = envelope(&out);
    assert_eq!(d["command"], "run");
    assert_eq!(d["success"], true);
    assert_eq!(d["result"]["entry"], "main");
    assert_eq!(d["result"]["value"], 7); // 1 + 1 (bump) + 5
    assert!(out.status.success());
}

#[test]
fn dump_commands_emit_envelopes() {
    for (cmd, key) in [("ast", "ast"), ("mir", "mir"), ("graph", "graph")] {
        let f = src_file(cmd, SRC);
        let out = ontixa(&[cmd, f.to_str().unwrap(), "--json"]);
        let d = envelope(&out);
        assert_eq!(d["command"], cmd);
        assert!(d["result"][key].is_object() || d["result"][key].is_array());
    }
}

#[test]
fn tokens_json_is_one_document() {
    let f = src_file("tokens", SRC);
    let out = ontixa(&["tokens", f.to_str().unwrap(), "--json"]);
    let d = envelope(&out);
    assert_eq!(d["command"], "tokens");
    assert!(d["result"]["tokens"].as_array().unwrap().len() > 10);
}

#[test]
fn explain_json_lists_defs() {
    let f = src_file("explain", SRC);
    let out = ontixa(&["explain", f.to_str().unwrap(), "--json"]);
    let d = envelope(&out);
    let defs = d["result"]["defs"].as_array().unwrap();
    assert_eq!(defs.len(), 4);
    // `read` must carry its inferred contract.
    let read = defs.iter().find(|d| d["name"] == "read").unwrap();
    assert_eq!(read["params"][0]["behavior"], "borrow");
}

#[test]
fn explain_symbol_resolves_semantically() {
    let f = src_file("explainsym", SRC);
    let out = ontixa(&["explain", f.to_str().unwrap(), "read", "--json"]);
    let d = envelope(&out);
    assert_eq!(d["result"]["symbol"]["kind"], "fn");
    assert_eq!(d["result"]["symbol"]["params"][0]["behavior"], "borrow");
    assert!(out.status.success());
}

#[test]
fn explain_ambiguous_symbol_is_a_diagnostic() {
    let f = src_file("explainambig", SRC);
    let out = ontixa(&["explain", f.to_str().unwrap(), "p", "--json"]);
    let d = envelope(&out);
    assert_eq!(d["success"], false);
    let codes: Vec<&str> = d["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["code"].as_str())
        .collect();
    assert!(codes.contains(&"E_AMBIGUOUS_SYMBOL"), "{codes:?}");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn check_error_still_one_document() {
    let f = src_file(
        "err",
        "data P { x: i32; }\nfn eat(p: P) -> i32 { return 0; }\nfn main() -> i32 { let q = P { x: 1 }; let a = eat(q); let b = eat(q); return a + b; }",
    );
    let out = ontixa(&["check", f.to_str().unwrap(), "--json"]);
    let d = envelope(&out);
    assert_eq!(d["success"], false);
    let codes: Vec<&str> = d["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["code"].as_str())
        .collect();
    assert!(codes.contains(&"E_USE_AFTER_MOVE"), "{codes:?}");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn io_error_is_an_envelope() {
    let out = ontixa(&["check", "definitely/not/a/file.ixa", "--json"]);
    let d = envelope(&out);
    assert_eq!(d["success"], false);
    assert_eq!(d["error"]["kind"], "io");
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn human_check_has_no_json_on_stdout() {
    let f = src_file("human", SRC);
    let out = ontixa(&["check", f.to_str().unwrap()]);
    assert!(out.status.success());
    // Human mode: no JSON document on stdout at all.
    assert!(stdout_str(&out).trim().is_empty());
}
