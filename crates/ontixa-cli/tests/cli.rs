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
    // check is check-only: frontend stages run, backend stages
    // (graph, mir, assemble) must not — that's the check/build split.
    for stage in ["lex+parse", "ast", "hir", "types", "ownership"] {
        assert!(stages.contains(&stage), "missing stage {stage}");
    }
    for stage in ["graph", "mir", "assemble"] {
        assert!(!stages.contains(&stage), "check ran {stage}");
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

// ---------- multi-file workspaces ----------

/// A fresh directory for one test's workspace: `main.ixa` gets the
/// root source, `math.ixa` the dep module.
fn ws_fixture(tag: &str, root: &str, math: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ontixa_ws_{}_{}", tag, std::process::id()));
    std::fs::create_dir_all(&dir).expect("create ws dir");
    std::fs::write(dir.join("math.ixa"), math).expect("write dep");
    std::fs::write(dir.join("main.ixa"), root).expect("write root");
    dir.join("main.ixa")
}

#[test]
fn workspace_qualified_call_runs() {
    let main = ws_fixture(
        "call",
        "use math; fn main() -> i32 { return math::double(21); }",
        "fn double(x: i32) -> i32 { return x * 2; }",
    );
    let out = ontixa(&["run", main.to_str().unwrap(), "--json"]);
    let d = envelope(&out);
    assert_eq!(d["success"], true, "{d}");
    assert_eq!(d["result"]["value"], 42);
}

/// A diagnostic in a dependency file names *that* file and offsets
/// into *its* text — not the root's.
#[test]
fn workspace_dep_diagnostic_names_dep_file() {
    let main = ws_fixture(
        "depdiag",
        "use math; fn main() -> i32 { return math::v(); }",
        "fn v() -> i32 { return nope; }",
    );
    let out = ontixa(&["check", main.to_str().unwrap(), "--json"]);
    let d = envelope(&out);
    assert_eq!(d["success"], false);
    let diags = d["diagnostics"].as_array().unwrap();
    let dep = diags
        .iter()
        .find(|x| {
            x["primary"]["file"]
                .as_str()
                .is_some_and(|f| f.contains("math.ixa"))
        })
        .unwrap_or_else(|| panic!("no dep-file diagnostic: {diags:?}"));
    // `nope`'s span indexes math.ixa's own text.
    let math_src = "fn v() -> i32 { return nope; }";
    let at = math_src.find("nope").unwrap() as u64;
    assert_eq!(dep["primary"]["start"], at);
    assert_eq!(dep["primary"]["end"], at + 4);
}

/// `use`d modules join the daemon's workspace: sibling files are
/// loaded lazily at `check` time, and file-1 queries appear in the
/// evaluated list.
#[test]
fn daemon_workspace_check_loads_siblings() {
    let main = ws_fixture(
        "daemonws",
        "use math; fn main() -> i32 { return math::double(21); }",
        "fn double(x: i32) -> i32 { return x * 2; }",
    );
    let p = main.to_str().unwrap();
    let rs = daemon(&[
        serde_json::json!({"op": "open", "path": p}),
        serde_json::json!({"op": "check", "path": p}),
        serde_json::json!({"op": "check", "path": p}),
    ]);
    assert_eq!(rs[1]["success"], true, "{:?}", rs[1]);
    let ev: Vec<&str> = rs[1]["result"]["evaluated"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|k| k.as_str())
        .collect();
    // File 1 (math) was discovered through `use` and its defs checked.
    assert!(
        ev.iter().any(|k| k.contains("FileId(1)")),
        "no dep-file query evaluated: {ev:?}"
    );
    // Recheck: pure verification.
    assert_eq!(rs[2]["result"]["evaluated"].as_array().unwrap().len(), 0);
}

/// Editing a dep through the daemon re-runs the dep's chain while
/// the root's untouched bodies stay memoized.
#[test]
fn daemon_dep_edit_reruns_only_dep_chain() {
    let main = ws_fixture(
        "daemondep",
        "use math; fn main() -> i32 { return math::double(21); }",
        "fn double(x: i32) -> i32 { return x * 2; }",
    );
    let p = main.to_str().unwrap();
    let dep = main.parent().unwrap().join("math.ixa");
    let dep = dep.to_str().unwrap();
    let edited = "fn double(x: i32) -> i32 { return x * 4; }";
    let rs = daemon(&[
        serde_json::json!({"op": "open", "path": p}),
        serde_json::json!({"op": "check", "path": p}),
        serde_json::json!({"op": "set", "path": dep, "text": edited}),
        serde_json::json!({"op": "check", "path": p}),
    ]);
    assert_eq!(rs[3]["success"], true, "{:?}", rs[3]);
    let ev: Vec<&str> = rs[3]["result"]["evaluated"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|k| k.as_str())
        .collect();
    let bodies: Vec<&&str> = ev.iter().filter(|k| k.contains("HirBody")).collect();
    // Only `double` (file 1) re-lowered; `main` (file 0) stayed
    // memoized — the signature didn't change. DefKeys print as
    // `DefKey(root:file:name)`.
    assert_eq!(bodies.len(), 1, "expected one dirty body: {ev:?}");
    assert!(bodies[0].contains("DefKey(0:1:"), "{ev:?}");
}

// ---------- rename transactions ----------

#[test]
fn rename_preview_lists_edits_and_writes_nothing() {
    let main = ws_fixture(
        "renprev",
        "use math; fn main() -> i32 { return math::double(21); }",
        "fn double(x: i32) -> i32 { return x * 2; }\n\
         fn norm(x: i32) -> i32 { return x; }",
    );
    let math_path = main.parent().unwrap().join("math.ixa");
    let math_before = std::fs::read_to_string(&math_path).unwrap();
    let out = ontixa(&[
        "rename",
        main.to_str().unwrap(),
        "math::double",
        "twice",
        "--json",
    ]);
    let d = envelope(&out);
    assert_eq!(d["success"], true, "{d}");
    assert_eq!(d["result"]["applied"], false);
    let edits = d["result"]["edits"].as_array().unwrap();
    assert_eq!(edits.len(), 2, "{edits:?}");
    assert!(
        edits
            .iter()
            .any(|e| e["file"].as_str().unwrap().contains("math.ixa"))
    );
    assert!(
        edits
            .iter()
            .any(|e| e["file"].as_str().unwrap().contains("main.ixa"))
    );
    assert!(edits.iter().all(|e| e["replace"] == "twice"));
    assert_eq!(out.status.code(), Some(0));
    // Preview never mutates disk.
    assert_eq!(std::fs::read_to_string(&math_path).unwrap(), math_before);
}

#[test]
fn rename_apply_writes_all_files_and_program_still_runs() {
    let main = ws_fixture(
        "renapply",
        "use math; fn main() -> i32 { return math::double(21); }",
        "fn double(x: i32) -> i32 { return x * 2; }",
    );
    let out = ontixa(&[
        "rename",
        main.to_str().unwrap(),
        "math::double",
        "twice",
        "--apply",
        "--json",
    ]);
    let d = envelope(&out);
    assert_eq!(d["success"], true, "{d}");
    assert_eq!(d["result"]["applied"], true);
    let dir = main.parent().unwrap();
    let math = std::fs::read_to_string(dir.join("math.ixa")).unwrap();
    let main_src = std::fs::read_to_string(&main).unwrap();
    assert!(math.contains("fn twice("), "{math}");
    assert!(main_src.contains("math::twice(21)"), "{main_src}");
    // The renamed workspace still compiles and runs.
    let run = ontixa(&["run", main.to_str().unwrap(), "--json"]);
    assert_eq!(envelope(&run)["result"]["value"], 42);
}

#[test]
fn rename_rejections_emit_codes_and_write_nothing() {
    let main = ws_fixture(
        "renrej",
        "use math; fn main() -> i32 { return math::double(21); }",
        "fn double(x: i32) -> i32 { return x * 2; }\n\
         fn norm(x: i32) -> i32 { return x; }",
    );
    let math_path = main.parent().unwrap().join("math.ixa");
    let math_before = std::fs::read_to_string(&math_path).unwrap();
    let reject = |args: &[&str]| {
        let out = ontixa(args);
        let d = envelope(&out);
        assert_eq!(d["success"], false, "{d}");
        assert_eq!(out.status.code(), Some(1));
        d["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["code"].as_str().map(String::from))
            .collect::<Vec<_>>()
    };
    let m = main.to_str().unwrap();
    let conflict = reject(&["rename", m, "math::double", "norm", "--apply", "--json"]);
    assert!(
        conflict.iter().any(|c| c == "E_NAME_CONFLICT"),
        "{conflict:?}"
    );
    let invalid = reject(&["rename", m, "math::double", "123x", "--json"]);
    assert!(invalid.iter().any(|c| c == "E_INVALID_NAME"), "{invalid:?}");
    let unknown = reject(&["rename", m, "math::nope", "x", "--json"]);
    assert!(
        unknown.iter().any(|c| c == "E_UNKNOWN_SYMBOL"),
        "{unknown:?}"
    );
    // Rejected applies never touch disk.
    assert_eq!(std::fs::read_to_string(&math_path).unwrap(), math_before);
}

/// An IO failure mid-apply rolls already-written files back — the
/// workspace is never left half-renamed on disk.
#[test]
fn rename_apply_io_failure_leaves_no_partial_writes() {
    let main = ws_fixture(
        "renio",
        "use math; fn main() -> i32 { return math::double(21); }",
        "fn double(x: i32) -> i32 { return x * 2; }",
    );
    let dir = main.parent().unwrap();
    let math_path = dir.join("math.ixa");
    let main_before = std::fs::read_to_string(&main).unwrap();
    let math_before = std::fs::read_to_string(&math_path).unwrap();
    // A pending transaction journal blocks any new write to the
    // workspace — deterministic on every platform.
    let journal = dir.join(".ontixa-tx-dead.journal");
    std::fs::write(&journal, "{ not valid json").unwrap();
    let out = ontixa(&[
        "rename",
        main.to_str().unwrap(),
        "math::double",
        "twice",
        "--apply",
        "--json",
    ]);
    let d = envelope(&out);
    assert_eq!(d["success"], false, "{d}");
    assert_eq!(d["error"]["kind"], "io");
    assert!(
        d["error"]["message"].as_str().unwrap().contains("pending"),
        "{d}"
    );
    // Nothing was written; the journal is preserved.
    assert_eq!(std::fs::read_to_string(&main).unwrap(), main_before);
    assert_eq!(std::fs::read_to_string(&math_path).unwrap(), math_before);
    assert!(journal.exists());

    // `ontixa recover` reports the corrupt journal as a conflict
    // (exit 1) and preserves it — it never guesses.
    let out = ontixa(&["recover", dir.to_str().unwrap(), "--json"]);
    let d = envelope(&out);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(d["schema"], 1);
    assert_eq!(d["success"], false, "{d}");
    assert_eq!(d["error"]["kind"], "conflict");
    assert_eq!(d["error"]["message"], "recovery has unresolved conflicts");
    assert_eq!(d["result"]["outcomes"][0]["status"], "conflict", "{d}");
    assert_eq!(std::fs::read(&journal).unwrap(), b"{ not valid json");
    assert_eq!(std::fs::read_to_string(&main).unwrap(), main_before);
    assert_eq!(std::fs::read_to_string(&math_path).unwrap(), math_before);
    let human = ontixa(&["recover", dir.to_str().unwrap()]);
    assert_eq!(human.status.code(), Some(1));
    assert_eq!(std::fs::read(&journal).unwrap(), b"{ not valid json");

    // With the journal removed, recovery is a clean no-op.
    std::fs::remove_file(&journal).unwrap();
    let clean = ontixa(&["recover", dir.to_str().unwrap(), "--json"]);
    let d = envelope(&clean);
    assert_eq!(clean.status.code(), Some(0));
    assert_eq!(d["success"], true);
    assert!(d["error"].is_null());
    assert_eq!(d["result"]["outcomes"][0]["status"], "clean");
    let out = ontixa(&["recover", dir.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn recover_json_mixed_outcomes_fail_without_discarding_successful_results() {
    use ontixa_cli::persist::{FailPoint, TxFile, persist_tx_hooks};
    for (tag, fail, expected) in [
        (
            "recover_mixed_rollback",
            FailPoint::StageDie(0),
            "rolled_back",
        ),
        ("recover_mixed_commit", FailPoint::Die(0), "committed"),
    ] {
        let before = b"fn main() -> i32 { return 1; }\r\n";
        let after = b"fn main() -> i32 { return 2; }\r\n";
        let main = ws_fixture(tag, std::str::from_utf8(before).unwrap(), "// unchanged\n");
        let dir = main.parent().unwrap();
        let tx = [TxFile {
            path: main.clone(),
            before: before.to_vec(),
            after: after.to_vec(),
        }];
        let err = persist_tx_hooks(dir, &tx, Some(fail)).unwrap_err();
        let recoverable = err.journal.unwrap();
        let conflict = dir.join(".ontixa-tx-corrupt.journal");
        let evidence = b"{ invalid fixture journal\r\n";
        std::fs::write(&conflict, evidence).unwrap();
        let out = ontixa(&["recover", dir.to_str().unwrap(), "--json"]);
        let d = envelope(&out);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(d["schema"], 1);
        assert_eq!(d["success"], false);
        assert_eq!(d["error"]["kind"], "conflict");
        let outcomes = d["result"]["outcomes"].as_array().unwrap();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.iter().any(|o| o["status"] == expected));
        assert!(outcomes.iter().any(|o| o["status"] == "conflict"));
        assert_eq!(std::fs::read(&conflict).unwrap(), evidence);
        assert_eq!(
            std::fs::read(&main).unwrap(),
            if expected == "committed" {
                after
            } else {
                before
            }
        );
        assert_eq!(
            std::fs::read(dir.join("math.ixa")).unwrap(),
            b"// unchanged\n"
        );
        assert!(
            !recoverable.exists(),
            "successful recovery finalizes its own journal"
        );
    }
}

/// The daemon `rename` op: preview carries the planned revision,
/// apply requires it, and a stale revision is rejected before any
/// mutation — the session's sources are only updated in-memory
/// (`new_sources` are returned for the client to persist).
#[test]
fn daemon_rename_previews_applies_and_guards_stale() {
    let main = ws_fixture(
        "daemonren",
        "use math; fn main() -> i32 { return math::double(21); }",
        "fn double(x: i32) -> i32 { return x * 2; }",
    );
    let p = main.to_str().unwrap();
    let rs = daemon(&[
        serde_json::json!({"op": "open", "path": p}),
        serde_json::json!({"op": "rename", "path": p, "symbol": "math::double", "to": "twice"}),
    ]);
    assert_eq!(rs[1]["success"], true, "{:?}", rs[1]);
    assert_eq!(rs[1]["result"]["applied"], false);
    let rev = rs[1]["result"]["revision"].as_u64().unwrap();
    assert!(rs[1]["result"]["edits"].as_array().unwrap().len() >= 2);

    let rs = daemon(&[
        serde_json::json!({"op": "open", "path": p}),
        // Apply naming the wrong revision → E_STALE_REVISION.
        serde_json::json!({"op": "rename", "path": p, "symbol": "math::double", "to": "twice", "apply": true, "revision": rev + 9}),
        // Apply with the planned revision → applied in-memory.
        serde_json::json!({"op": "rename", "path": p, "symbol": "math::double", "to": "twice", "apply": true, "revision": rev}),
        // The revision bumped — the same plan is now stale.
        serde_json::json!({"op": "rename", "path": p, "symbol": "math::twice", "to": "triple", "apply": true, "revision": rev}),
    ]);
    let stale: Vec<&str> = rs[1]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["code"].as_str())
        .collect();
    assert!(stale.contains(&"E_STALE_REVISION"), "{:?}", rs[1]);
    assert_eq!(rs[2]["success"], true, "{:?}", rs[2]);
    assert_eq!(rs[2]["result"]["applied"], true);
    let srcs = rs[2]["result"]["new_sources"].as_array().unwrap();
    assert_eq!(srcs.len(), 2);
    assert!(
        srcs.iter()
            .all(|s| s["text"].as_str().unwrap().contains("twice"))
    );
    let stale2: Vec<&str> = rs[3]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|x| x["code"].as_str())
        .collect();
    assert!(stale2.contains(&"E_STALE_REVISION"), "{:?}", rs[3]);
    // The daemon never writes disk — math.ixa is untouched.
    let math = std::fs::read_to_string(main.parent().unwrap().join("math.ixa")).unwrap();
    assert!(math.contains("fn double("), "{math}");
}

// ---------- canonical formatting ----------

const MESSY: &str = "fn f( x:i32)->i32{return x;}";
const TIDY: &str = "fn f(x: i32) -> i32 {\n    return x;\n}\n";

/// `fmt` with no flags is the preview: canonical text on stdout,
/// nothing written — the same dry-run shape as `rename`.
#[test]
fn fmt_preview_prints_canonical_source_and_writes_nothing() {
    let f = src_file("fmtprev", MESSY);
    let before = std::fs::read_to_string(&f).unwrap();
    let out = ontixa(&["fmt", f.to_str().unwrap()]);
    assert!(out.status.success(), "{out:?}");
    assert_eq!(stdout_str(&out), TIDY);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), before);
}

#[test]
fn fmt_json_envelope_reports_would_change() {
    let f = src_file("fmtjson", MESSY);
    let out = ontixa(&["fmt", f.to_str().unwrap(), "--json"]);
    let d = envelope(&out);
    assert_eq!(d["command"], "fmt");
    assert_eq!(d["success"], true);
    assert_eq!(d["result"]["would_change"], true);
    let file = &d["result"]["files"][0];
    assert_eq!(file["changed"], true);
    assert_eq!(file["formatted"], TIDY);
    assert_eq!(file["written"], false);
}

/// `--check` is the CI gate: exit 1 plus the offending paths on
/// stdout, nothing written; an already-canonical file exits 0.
#[test]
fn fmt_check_exits_nonzero_without_writing() {
    let f = src_file("fmtcheck", MESSY);
    let before = std::fs::read_to_string(&f).unwrap();
    let out = ontixa(&["fmt", f.to_str().unwrap(), "--check"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout_str(&out).trim(), f.to_str().unwrap());
    assert_eq!(std::fs::read_to_string(&f).unwrap(), before);
    std::fs::write(&f, TIDY).unwrap();
    let out = ontixa(&["fmt", f.to_str().unwrap(), "--check"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(stdout_str(&out).trim().is_empty());
}

/// In JSON mode a failed `--check` is a verdict, not a command
/// error: `success` stays true, `would_change` carries it, the exit
/// code still gates.
#[test]
fn fmt_check_json_keeps_success_but_exits_1() {
    let f = src_file("fmtcheckjson", MESSY);
    let out = ontixa(&["fmt", f.to_str().unwrap(), "--check", "--json"]);
    let d = envelope(&out);
    assert_eq!(d["success"], true);
    assert_eq!(d["result"]["would_change"], true);
    assert_eq!(out.status.code(), Some(1));
}

/// `--write` persists the canonical form (staged + journaled like
/// `rename --apply`), the formatted program still runs to the same
/// value, and a second `--write` is a no-op — idempotence at the
/// command level.
#[test]
fn fmt_write_persists_and_output_still_runs() {
    let f = src_file(
        "fmtwrite",
        "data P{x:i32;}fn f(p:P)->i32{return p.x;}fn main()->i32{return f(P{x:7});}",
    );
    let out = ontixa(&["fmt", f.to_str().unwrap(), "--write"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let written = std::fs::read_to_string(&f).unwrap();
    assert!(written.contains("data P {"), "{written}");
    let run = ontixa(&["run", f.to_str().unwrap(), "--json"]);
    assert_eq!(envelope(&run)["result"]["value"], 7);
    let again = ontixa(&["fmt", f.to_str().unwrap(), "--write", "--json"]);
    let d = envelope(&again);
    assert_eq!(d["result"]["would_change"], false);
    assert_eq!(d["result"]["files"][0]["written"], false);
}

/// A file that doesn't parse is never rewritten — its diagnostics
/// surface in the envelope and the file is skipped.
#[test]
fn fmt_refuses_parse_errors() {
    let f = src_file("fmterr", "fn f( { return 1; }");
    let before = std::fs::read_to_string(&f).unwrap();
    let out = ontixa(&["fmt", f.to_str().unwrap(), "--write", "--json"]);
    let d = envelope(&out);
    assert_eq!(d["success"], false);
    assert!(!d["diagnostics"].as_array().unwrap().is_empty());
    assert!(d["result"]["files"][0]["formatted"].is_null());
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(std::fs::read_to_string(&f).unwrap(), before);
}

/// Multiple files format independently — one envelope, one entry
/// per input file.
#[test]
fn fmt_multiple_files_report_per_file() {
    let a = src_file("fmtmulti_a", MESSY);
    let b = src_file("fmtmulti_b", TIDY);
    let out = ontixa(&[
        "fmt",
        a.to_str().unwrap(),
        b.to_str().unwrap(),
        "--check",
        "--json",
    ]);
    let d = envelope(&out);
    let files = d["result"]["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["changed"], true);
    assert_eq!(files[1]["changed"], false);
    assert_eq!(out.status.code(), Some(1));
}

/// The daemon `fmt` op returns the canonical text of the bound
/// source without mutating it — the client installs it via `set`.
#[test]
fn daemon_fmt_returns_canonical_text_without_mutating() {
    let f = src_file("daemonfmt", MESSY);
    let p = f.to_str().unwrap();
    let rs = daemon(&[
        serde_json::json!({"op": "set", "path": p, "text": MESSY}),
        serde_json::json!({"op": "fmt", "path": p}),
        serde_json::json!({"op": "fmt", "path": p}),
    ]);
    assert_eq!(rs[1]["command"], "fmt");
    assert_eq!(rs[1]["success"], true);
    assert_eq!(rs[1]["result"]["changed"], true);
    assert_eq!(rs[1]["result"]["formatted"], TIDY);
    // Not mutated: the second demand still reports `changed`.
    assert_eq!(rs[2]["result"]["changed"], true);
}

/// The daemon `rename` op with `"at"` selects a body-local binding
/// positionally — same transaction core, same guards.
#[test]
fn daemon_rename_at_selects_local_positionally() {
    let main = ws_fixture(
        "daemonat",
        "fn main() -> i32 { let count = 2; return count * 2; }",
        "fn double(x: i32) -> i32 { return x * 2; }",
    );
    let p = main.to_str().unwrap();
    let src = std::fs::read_to_string(&main).unwrap();
    let off = src.find("count").unwrap() as u64;
    let rs = daemon(&[
        serde_json::json!({"op": "open", "path": p}),
        serde_json::json!({"op": "rename", "path": p, "at": off, "to": "total"}),
    ]);
    assert_eq!(rs[1]["success"], true, "{:?}", rs[1]);
    assert_eq!(rs[1]["result"]["applied"], false);
    // decl + use — local rename found both through HIR identity.
    assert_eq!(rs[1]["result"]["edits"].as_array().unwrap().len(), 2);
    let rev = rs[1]["result"]["revision"].as_u64().unwrap();

    // Same fixture → same revision; apply with the planned rev.
    let rs = daemon(&[
        serde_json::json!({"op": "open", "path": p}),
        serde_json::json!({"op": "rename", "path": p, "at": off, "to": "total", "apply": true, "revision": rev}),
    ]);
    assert_eq!(rs[1]["success"], true, "{:?}", rs[1]);
    assert_eq!(rs[1]["result"]["applied"], true);
    let new_src = rs[1]["result"]["new_sources"][0]["text"].as_str().unwrap();
    assert!(new_src.contains("let total"), "{new_src}");
    assert!(new_src.contains("return total * 2"), "{new_src}");
    // Disk untouched — the daemon only mutates memory.
    assert!(
        std::fs::read_to_string(&main)
            .unwrap()
            .contains("let count")
    );
}
