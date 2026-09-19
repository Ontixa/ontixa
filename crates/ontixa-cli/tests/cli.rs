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
fn borrow_conflict_is_a_structured_diagnostic() {
    // `both(q, q)`: arg 0 creates a mutable loan on q, arg 1 a shared
    // one — overlapping places under a mut loan must conflict.
    let src = "data P { x: i32; }\n\
               fn both(mut a: P, b: P) -> i32 { a.x = a.x + 1; return b.x; }\n\
               fn main() -> i32 { let mut q = P { x: 1 }; return both(q, q); }";
    let f = src_file("borrowconflict", src);
    let out = ontixa(&["check", f.to_str().unwrap(), "--json"]);
    let d = envelope(&out);
    assert_eq!(d["success"], false);
    let codes: Vec<&str> = d["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["code"].as_str())
        .collect();
    assert!(
        codes.contains(&"E_BORROW_CONFLICT"),
        "expected E_BORROW_CONFLICT in {codes:?}"
    );
}

#[test]
fn json_output_is_byte_deterministic() {
    // Same input → same bytes: an agent can diff artifacts
    // meaningfully. (`--timings` excluded — nanos are wall-clock.)
    let f = src_file("determinism", SRC);
    let p = f.to_str().unwrap();
    for cmd in ["check", "graph", "ast", "mir"] {
        let a = ontixa(&[cmd, p, "--json"]);
        let b = ontixa(&[cmd, p, "--json"]);
        assert_eq!(a.stdout, b.stdout, "{cmd} --json not byte-stable");
    }
}

#[test]
fn human_check_has_no_json_on_stdout() {
    let f = src_file("human", SRC);
    let out = ontixa(&["check", f.to_str().unwrap()]);
    assert!(out.status.success());
    // Human mode: no JSON document on stdout at all.
    assert!(stdout_str(&out).trim().is_empty());
}

// ---------- ontixad: the persistent daemon ----------

const DAEMON: &str = env!("CARGO_BIN_EXE_ontixad");

/// Feeds `requests` (one JSON per line) to ontixad; returns one
/// parsed envelope per line of stdout.
fn daemon(requests: &[Value]) -> Vec<Value> {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = Command::new(DAEMON)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ontixad");
    let mut input = String::new();
    for r in requests {
        input.push_str(&r.to_string());
        input.push('\n');
    }
    input.push_str("{\"op\":\"shutdown\"}\n");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(input.as_bytes())
        .expect("write requests");
    let out = child.wait_with_output().expect("wait ontixad");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).expect("each response is one JSON document"))
        .collect()
}

#[test]
fn daemon_recheck_evaluates_nothing() {
    let f = src_file("daemon", SRC);
    let p = f.to_str().unwrap();
    let rs = daemon(&[
        serde_json::json!({"op": "open", "path": p}),
        serde_json::json!({"op": "check", "path": p}),
        serde_json::json!({"op": "check", "path": p}),
    ]);
    assert_eq!(rs.len(), 3);
    assert_eq!(rs[0]["success"], true);
    // Cold check evaluates queries; the same-source recheck is pure
    // verification — zero evals.
    assert!(!rs[1]["result"]["evaluated"].as_array().unwrap().is_empty());
    assert_eq!(rs[2]["result"]["evaluated"].as_array().unwrap().len(), 0);
}

/// `read` LAST so an in-body edit shifts no other def's spans
/// (span-sensitivity is a documented limitation).
const SRC_LAST: &str = "data P { x: i32; }\n\
                        fn bump(mut p: P) { p.x = p.x + 1; }\n\
                        fn main() -> i32 { let mut q = P { x: 1 }; bump(q); return read(q) + 5; }\n\
                        fn read(p: P) -> i32 { return p.x; }";

#[test]
fn daemon_edit_reruns_only_dirty_chain() {
    let f = src_file("daemonedit", SRC_LAST);
    let p = f.to_str().unwrap();
    // Edit inside `read`'s body — the last def, so nothing else shifts.
    let edited = SRC_LAST.replace("return p.x;", "return p.x ;");
    let rs = daemon(&[
        serde_json::json!({"op": "set", "path": p, "text": SRC_LAST}),
        serde_json::json!({"op": "check", "path": p}),
        serde_json::json!({"op": "set", "path": p, "text": edited}),
        serde_json::json!({"op": "check", "path": p}),
    ]);
    let ev = rs[3]["result"]["evaluated"].as_array().unwrap();
    let evs: Vec<&str> = ev.iter().filter_map(|k| k.as_str()).collect();
    // Exactly one body chain re-ran (read's) — bump and main are
    // verified fresh without re-evaluating.
    let bodies: Vec<&&str> = evs.iter().filter(|k| k.contains("HirBody")).collect();
    assert_eq!(bodies.len(), 1, "expected one dirty body: {evs:?}");
    assert!(evs.iter().any(|k| k.contains("Parse")), "{evs:?}");
}

#[test]
fn daemon_trailing_comment_cuts_off_at_ast() {
    let f = src_file("daemoncomment", SRC_LAST);
    let p = f.to_str().unwrap();
    let commented = format!("{SRC_LAST}\n// a note");
    let rs = daemon(&[
        serde_json::json!({"op": "set", "path": p, "text": SRC_LAST}),
        serde_json::json!({"op": "check", "path": p}),
        serde_json::json!({"op": "set", "path": p, "text": commented}),
        serde_json::json!({"op": "check", "path": p}),
    ]);
    let ev = rs[3]["result"]["evaluated"].as_array().unwrap();
    let evs: Vec<&str> = ev.iter().filter_map(|k| k.as_str()).collect();
    // A comment-only edit re-parses but cuts off at the AST —
    // no body's semantic chain runs.
    for pat in ["HirBody", "BodyTypes", "MirBody", "Scope"] {
        assert!(
            !evs.iter().any(|k| k.contains(pat)),
            "{pat} unexpectedly re-ran: {evs:?}"
        );
    }
    assert!(evs.iter().any(|k| k.contains("Parse")), "{evs:?}");
}

#[test]
fn daemon_explain_and_stats() {
    let f = src_file("daemonexpl", SRC);
    let p = f.to_str().unwrap();
    let rs = daemon(&[
        serde_json::json!({"op": "open", "path": p}),
        serde_json::json!({"op": "explain", "path": p, "symbol": "read"}),
        serde_json::json!({"op": "stats"}),
    ]);
    let sym = &rs[1]["result"]["symbol"];
    assert_eq!(sym["name"], "read");
    assert_eq!(sym["params"][0]["behavior"], "borrow");
    // Evidence: read's param was observed at a specific span.
    assert_eq!(sym["params"][0]["evidence"][0]["kind"], "read");
    let q = &rs[2]["result"]["queries"];
    assert!(q["executed"].is_object());
    assert!(rs[2]["result"]["oracle"]["rounds"].as_u64().unwrap() >= 1);
}

#[test]
fn daemon_bad_lines_get_envelopes_not_crashes() {
    use std::process::Stdio;
    let mut child = Command::new(DAEMON)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn ontixad");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"not json\n{\"op\":\"bogus\"}\n{\"op\":\"shutdown\"}\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let lines: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["error"]["kind"], "io");
    assert_eq!(lines[1]["error"]["kind"], "io");
}
