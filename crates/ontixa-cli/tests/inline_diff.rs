//! The daemon comparison is a bounded, inline-only, session-independent query.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DAEMON: &str = env!("CARGO_BIN_EXE_ontixad");
const CLI: &str = env!("CARGO_BIN_EXE_ontixa");
static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        loop {
            let path = std::env::temp_dir().join(format!(
                "ontixa_inline_diff_{}_{}_{}",
                std::process::id(),
                nonce,
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create isolated workspace: {error}"),
            }
        }
    }

    fn write(&self, relative: &str, source: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, source).unwrap();
        path
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn daemon(cwd: &Path, requests: &[Value]) -> Vec<Value> {
    let input = requests
        .iter()
        .map(|r| format!("{r}\n"))
        .collect::<String>();
    daemon_lines(cwd, input, requests.len())
}

fn daemon_lines(cwd: &Path, mut input: String, expected: usize) -> Vec<Value> {
    input.push_str("{\"op\":\"shutdown\"}\n");
    let mut child = Command::new(DAEMON)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn ontixad");
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let output = std::thread::spawn(move || {
        let mut b = Vec::new();
        stdout.read_to_end(&mut b).unwrap();
        b
    });
    let errors = std::thread::spawn(move || {
        let mut b = Vec::new();
        stderr.read_to_end(&mut b).unwrap();
        b
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("inline comparison did not finish without executing source");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    writer.join().unwrap().unwrap();
    let stdout = String::from_utf8(output.join().unwrap()).unwrap();
    let stderr = errors.join().unwrap();
    assert!(status.success(), "daemon failed: {status}: {stdout}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    let documents: Vec<Value> = stdout
        .lines()
        .map(|line| {
            let doc: Value = serde_json::from_str(line).expect("one JSON response per line");
            assert_eq!(
                line,
                serde_json::to_string(&doc).unwrap(),
                "response must be compact"
            );
            assert_eq!(doc["schema"], 1);
            doc
        })
        .collect();
    assert_eq!(
        documents.len(),
        expected,
        "exactly one response per request: {stdout}"
    );
    documents
}

fn side(entry: &str, sources: &[(&str, &str)]) -> Value {
    json!({"entry": entry, "sources": sources.iter().map(|(module, text)|
        json!({"module": module, "text": text})).collect::<Vec<_>>()})
}

fn request(before: Value, after: Value) -> Value {
    json!({"op": "diff", "semantic": true, "before": before, "after": after})
}

fn single(before: &str, after: &str) -> Value {
    request(
        side("main", &[("main", before)]),
        side("main", &[("main", after)]),
    )
}

#[test]
fn positional_arrays_are_not_workspace_or_source_objects() {
    let workspace = Workspace::new();
    let valid = side("main", &[("main", "fn f() {}")]);
    let arrays = [
        json!(["main", [{"module": "main", "text": "fn f() {}"}]]),
        json!({"entry": "main", "sources": [["main", "fn f() {}"]]}),
        json!(["main", [["main", "fn f() {}"]]]),
    ];
    let mut requests = Vec::new();
    for array in arrays {
        requests.push(request(array.clone(), valid.clone()));
        requests.push(request(valid.clone(), array));
    }
    requests.push(request(valid.clone(), valid));
    let docs = daemon(&workspace.0, &requests);
    for doc in &docs[..6] {
        assert_eq!(doc["command"], "diff");
        assert_eq!(doc["success"], false);
        assert_eq!(doc["error"]["kind"], "invalid_comparison_request");
        assert!(doc["result"].is_null());
    }
    comparison(&docs[6], "unchanged");
}

fn comparison(doc: &Value, status: &str) {
    assert_eq!(doc["command"], "diff", "{doc}");
    assert_eq!(doc["timings"], json!([]));
    assert_eq!(doc["result"]["comparison_schema"], 1);
    assert!(doc["result"]["coverage"].is_object());
    assert_eq!(doc["result"]["status"], status, "{doc}");
    assert_eq!(doc["success"], status != "unavailable", "{doc}");
    for name in ["before", "after"] {
        assert_eq!(doc["result"]["inputs"][name]["source_kind"], "inline");
    }
    if status == "unavailable" {
        assert_eq!(doc["error"]["kind"], "comparison_unavailable");
        assert!(doc["result"]["changes"].is_null());
    } else {
        assert!(doc["error"].is_null());
        for name in ["before", "after"] {
            assert_eq!(doc["result"]["inputs"][name]["status"], "ok");
            assert!(doc["result"]["inputs"][name]["error"].is_null());
        }
    }
}

#[test]
fn inline_contract_changes_propagate_to_callers() {
    let workspace = Workspace::new();
    let before = "data P { x: i32; } fn read(p: P) -> i32 { return p.x; } fn relay(p: P) -> i32 { return read(p); }";
    let after = before.replace("return p.x;", "return 0;");
    let docs = daemon(&workspace.0, &[single(before, &after)]);
    comparison(&docs[0], "changed");
    let changes = docs[0]["result"]["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2);
    for (change, name) in changes.iter().zip(["main::read", "main::relay"]) {
        assert_eq!(change["definition"], name);
        assert_eq!(change["kind"], "modified");
        assert_eq!(
            change["fields"],
            json!([{"field": "params[0].behavior", "before": "borrow", "after": "move"}])
        );
    }
    for name in ["before", "after"] {
        assert_eq!(
            docs[0]["result"]["inputs"][name]["path"],
            format!("{name}/main.ixa")
        );
        assert_eq!(docs[0]["result"]["inputs"][name]["module"], "main");
    }
}

#[test]
fn trivia_and_body_results_are_equal_and_source_is_never_executed() {
    let workspace = Workspace::new();
    let docs = daemon(
        &workspace.0,
        &[
            single("", ""),
            single(
                "fn f() -> i32 { return 0; } data P { x: i32; }",
                "// trivia and declaration order\ndata P {x:i32;} fn f( )->i32{return 0;}",
            ),
            single(
                "fn f() -> i32 { return 0; }",
                "fn f() -> i32 { return 1 / 0; }",
            ),
            single(
                "fn f() -> i32 { return 0; }",
                "fn f() -> i32 { for i in 0..2147483647 {} return 42; }",
            ),
        ],
    );
    for doc in docs {
        comparison(&doc, "unchanged");
        assert_eq!(doc["result"]["changes"], json!([]));
    }
}

#[test]
fn ascii_module_identifiers_are_preserved_in_identity_and_paths() {
    let workspace = Workspace::new();
    let input = side("_Main2", &[("_Main2", "fn f() -> i32 { return 0; }")]);
    let changed = side("_Main2", &[("_Main2", "fn f() -> i64 { return 0; }")]);
    let docs = daemon(&workspace.0, &[request(input, changed)]);
    comparison(&docs[0], "changed");
    assert_eq!(docs[0]["result"]["changes"][0]["definition"], "_Main2::f");
    for name in ["before", "after"] {
        assert_eq!(docs[0]["result"]["inputs"][name]["module"], "_Main2");
        assert_eq!(
            docs[0]["result"]["inputs"][name]["path"],
            format!("{name}/_Main2.ixa")
        );
    }
}

#[test]
fn inline_modules_are_transitive_order_independent_and_reachability_bounded() {
    let workspace = Workspace::new();
    let before = side(
        "main",
        &[
            ("main", "use bridge; fn main() {}"),
            ("bridge", "use leaf;"),
            ("leaf", "fn value(x: i32) -> i32 { return x; }"),
            ("unused", "this is not valid source {"),
        ],
    );
    let after = side(
        "main",
        &[
            ("leaf", "fn value(x: i64) -> i64 { return x; }"),
            ("unused", "fn extra() {}"),
            ("bridge", "use leaf;"),
            ("main", "use bridge; fn main() {}"),
        ],
    );
    let mut reordered = before.clone();
    reordered["sources"].as_array_mut().unwrap().reverse();
    let docs = daemon(
        &workspace.0,
        &[request(before.clone(), after), request(before, reordered)],
    );
    comparison(&docs[0], "changed");
    let changes = docs[0]["result"]["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0]["definition"], "leaf::value");
    assert_eq!(
        changes[0]["fields"],
        json!([
            {"field": "params[0].type", "before": "i32", "after": "i64"},
            {"field": "returns", "before": "i32", "after": "i64"},
        ])
    );
    comparison(&docs[1], "unchanged");
    assert_eq!(docs[1]["result"]["changes"], json!([]));
}

#[test]
fn inline_selected_fields_match_the_filesystem_cli_comparison() {
    let workspace = Workspace::new();
    let before = [
        (
            "main",
            "use worker; use types; fn relay(p: types::P) -> i32 { return worker::read(p); } data Record { x: i32; y: bool; } data Choice { A(i32); B; } fn gone() {}",
        ),
        ("types", "data P { x: i32; }"),
        (
            "worker",
            "use types; fn read(p: types::P) -> i32 { return p.x; }",
        ),
    ];
    let after = [
        (
            "worker",
            "use types; fn read(p: types::P) -> i32 { return 0; }",
        ),
        (
            "main",
            "use types; use worker; fn relay(p: types::P) -> i32 { return worker::read(p); } data Record { y: bool; x: i32; } data Choice { B; A(i64); } fn added() {}",
        ),
        ("types", "data P { x: i32; }"),
    ];
    for (name, sources) in [("before", &before), ("after", &after)] {
        for (module, text) in sources {
            workspace.write(&format!("{name}/{module}.ixa"), text);
        }
    }
    let cli = Command::new(CLI)
        .args(["diff", "--semantic"])
        .arg(workspace.0.join("before/main.ixa"))
        .arg(workspace.0.join("after/main.ixa"))
        .arg("--json")
        .output()
        .unwrap();
    assert!(cli.status.success(), "{cli:?}");
    assert!(cli.stderr.is_empty());
    let cli: Value = serde_json::from_slice(&cli.stdout).unwrap();
    let docs = daemon(
        &workspace.0,
        &[request(side("main", &before), side("main", &after))],
    );
    comparison(&docs[0], "changed");
    assert_eq!(docs[0]["result"]["changes"], cli["result"]["changes"]);
    assert_eq!(docs[0]["result"]["changes"].as_array().unwrap().len(), 6);
    for field in ["compared", "excluded", "identity", "claim"] {
        assert_eq!(
            docs[0]["result"]["coverage"][field],
            cli["result"]["coverage"][field]
        );
    }
}

#[test]
fn source_errors_on_either_or_both_sides_are_attributed() {
    let workspace = Workspace::new();
    let valid = "fn main() {}";
    for (invalid, code) in [
        ("fn broken( {", "E_PARSE"),
        ("fn main() -> i32 { return true; }", "E_TYPE_MISMATCH"),
        ("fn f() {} fn f() {}", "E_DUPLICATE_DEF"),
        ("use absent; fn main() {}", "E_UNKNOWN_MODULE"),
    ] {
        let docs = daemon(
            &workspace.0,
            &[
                single(invalid, valid),
                single(valid, invalid),
                single(invalid, invalid),
            ],
        );
        for (doc, bad_sides) in
            docs.iter()
                .zip([&["before"][..], &["after"][..], &["before", "after"][..]])
        {
            comparison(doc, "unavailable");
            let diagnostics = doc["diagnostics"].as_array().unwrap();
            for name in ["before", "after"] {
                let status = if bad_sides.contains(&name) {
                    "source_errors"
                } else {
                    "ok"
                };
                assert_eq!(doc["result"]["inputs"][name]["status"], status, "{doc}");
                if bad_sides.contains(&name) {
                    assert!(
                        diagnostics
                            .iter()
                            .any(|d| d["comparison_side"] == name && d["code"] == code),
                        "missing {name} {code}: {doc}"
                    );
                }
            }
            for diagnostic in diagnostics {
                let side = diagnostic["comparison_side"].as_str().unwrap();
                assert!(bad_sides.contains(&side), "{doc}");
                assert_eq!(diagnostic["primary"]["file"], format!("{side}/main.ixa"));
            }
        }
    }
}

#[test]
fn dependency_diagnostics_use_the_side_and_that_modules_offsets() {
    let workspace = Workspace::new();
    let text = "fn broken() -> i32 { return nope; }";
    let input = side("main", &[("main", "use dep; fn main() {}"), ("dep", text)]);
    let docs = daemon(&workspace.0, &[request(input.clone(), input)]);
    comparison(&docs[0], "unavailable");
    for name in ["before", "after"] {
        let diagnostic = docs[0]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|d| {
                d["comparison_side"] == name && d["primary"]["file"] == format!("{name}/dep.ixa")
            })
            .unwrap_or_else(|| panic!("missing {name} dependency diagnostic: {}", docs[0]));
        let start = text.find("nope").unwrap();
        assert_eq!(diagnostic["primary"]["start"], start);
        assert_eq!(diagnostic["primary"]["end"], start + 4);
    }
}

#[test]
fn parse_errors_keep_each_module_and_side_even_with_identical_dependency_errors() {
    let workspace = Workspace::new();
    let root_text = "use a; use b; fn f( {";
    let dep_text = "fn f( {";
    let input = side(
        "main",
        &[("b", dep_text), ("main", root_text), ("a", dep_text)],
    );
    let docs = daemon(&workspace.0, &[request(input.clone(), input)]);
    comparison(&docs[0], "unavailable");
    let diagnostics = docs[0]["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 18);
    for side in ["before", "after"] {
        for (module, text) in [("main", root_text), ("a", dep_text), ("b", dep_text)] {
            let spans: Vec<_> = diagnostics
                .iter()
                .filter(|d| {
                    d["comparison_side"] == side
                        && d["primary"]["file"] == format!("{side}/{module}.ixa")
                })
                .map(|d| {
                    assert_eq!(d["code"], "E_PARSE");
                    (
                        d["primary"]["start"].as_u64().unwrap(),
                        d["primary"]["end"].as_u64().unwrap(),
                    )
                })
                .collect();
            let end = text.len() as u64;
            assert_eq!(spans, vec![(end - 1, end), (end, end), (end, end)]);
        }
    }
    assert!(std::fs::read_dir(&workspace.0).unwrap().next().is_none());
}

#[test]
fn malformed_inline_shapes_names_and_selectors_are_rejected_then_recover() {
    let workspace = Workspace::new();
    let valid = single("fn main() {}", "fn main() {}");
    let mut invalid = Vec::new();
    for (pointer, replacement) in [
        ("/semantic", json!(false)),
        ("/semantic", json!("true")),
        ("/before", json!(null)),
        ("/after", json!([])),
        ("/before/entry", json!(0)),
        ("/before/sources", json!([])),
        ("/before/sources", json!({"main": "fn main() {}"})),
        ("/before/sources/0", json!(null)),
        ("/before/sources/0/text", json!(null)),
        ("/before/sources/0/module", json!(true)),
        ("/before/sources/0/module", json!("other")),
    ] {
        let mut bad = valid.clone();
        *bad.pointer_mut(pointer).unwrap() = replacement;
        invalid.push(bad);
    }
    for (object, key) in [
        ("", "semantic"),
        ("", "after"),
        ("/before", "entry"),
        ("/after", "sources"),
        ("/before/sources/0", "text"),
    ] {
        let mut bad = valid.clone();
        bad.pointer_mut(object)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(key);
        invalid.push(bad);
    }
    for name in [
        "",
        "0main",
        "../main",
        "main/dep",
        "main\\dep",
        "main.ixa",
        "máin",
        "with space",
    ] {
        for pointer in ["/before/entry", "/after/sources/0/module"] {
            let mut bad = valid.clone();
            *bad.pointer_mut(pointer).unwrap() = json!(name);
            invalid.push(bad);
        }
    }
    for name in ["before", "after"] {
        let mut bad = valid.clone();
        bad[name]["sources"]
            .as_array_mut()
            .unwrap()
            .push(json!({"module": "main", "text": "fn duplicate() {}"}));
        invalid.push(bad);
    }
    for (object, key, value) in [
        ("", "path", json!("main.ixa")),
        ("", "symbol", json!("main")),
        ("", "at", json!(0)),
        ("", "apply", json!(true)),
        ("/before", "path", json!("main.ixa")),
        ("/after/sources/0", "path", json!("main.ixa")),
    ] {
        let mut bad = valid.clone();
        bad.pointer_mut(object)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(key.into(), value);
        invalid.push(bad);
    }
    let count = invalid.len();
    invalid.push(valid);
    let docs = daemon(&workspace.0, &invalid);
    for (doc, request) in docs[..count].iter().zip(&invalid) {
        assert_eq!(doc["command"], "diff", "request {request}: {doc}");
        assert_eq!(doc["success"], false);
        assert_eq!(
            doc["error"]["kind"], "invalid_comparison_request",
            "request {request}: {doc}"
        );
        assert!(doc["result"].is_null(), "request {request}: {doc}");
    }
    comparison(&docs[count], "unchanged");
}

#[test]
fn mismatched_roots_reuse_the_comparison_unavailable_result() {
    let workspace = Workspace::new();
    let docs = daemon(
        &workspace.0,
        &[request(
            side("left", &[("left", "fn main() {}")]),
            side("right", &[("right", "fn main() {}")]),
        )],
    );
    comparison(&docs[0], "unavailable");
    for name in ["before", "after"] {
        assert_eq!(docs[0]["result"]["inputs"][name]["status"], "root_mismatch");
    }
}

#[test]
fn missing_modules_cannot_fall_back_to_disk_the_session_or_the_other_side() {
    let workspace = Workspace::new();
    let dependency = "fn value() -> i32 { return 42; }";
    for path in ["dep.ixa", "before/dep.ixa", "after/dep.ixa"] {
        workspace.write(path, dependency);
    }
    let missing = side(
        "main",
        &[("main", "use dep; fn main() -> i32 { return dep::value(); }")],
    );
    let mut complete = missing.clone();
    complete["sources"]
        .as_array_mut()
        .unwrap()
        .push(json!({"module": "dep", "text": dependency}));
    let docs = daemon(
        &workspace.0,
        &[
            json!({"op": "set", "path": "dep.ixa", "text": dependency}),
            request(complete.clone(), complete.clone()),
            request(missing.clone(), complete.clone()),
            request(complete, missing),
        ],
    );
    comparison(&docs[1], "unchanged");
    for (doc, name) in [(&docs[2], "before"), (&docs[3], "after")] {
        comparison(doc, "unavailable");
        assert_eq!(doc["result"]["inputs"][name]["status"], "source_errors");
        assert!(
            doc["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["comparison_side"] == name && d["code"] == "E_UNKNOWN_MODULE"),
            "{doc}"
        );
    }
}

#[test]
fn comparisons_leave_session_sources_queries_and_explain_results_untouched() {
    let workspace = Workspace::new();
    let source = "data P { x: i32; } fn read(p: P) -> i32 { return p.x; }";
    let disk = source.replace("return p.x;", "return 0;");
    let path = workspace.write("main.ixa", &disk);
    let mut invalid = single(source, &disk);
    invalid["semantic"] = json!(false);
    let docs = daemon(
        &workspace.0,
        &[
            json!({"op": "set", "path": path, "text": source}),
            json!({"op": "check", "path": path}),
            json!({"op": "explain", "path": path, "symbol": "read"}),
            json!({"op": "stats"}),
            single(source, &disk),
            json!({"op": "stats"}),
            single("fn broken( {", source),
            json!({"op": "stats"}),
            invalid,
            json!({"op": "stats"}),
            json!({"op": "check", "path": path}),
            json!({"op": "explain", "path": path, "symbol": "read"}),
        ],
    );
    assert_eq!(docs[1]["success"], true);
    comparison(&docs[4], "changed");
    comparison(&docs[6], "unavailable");
    assert_eq!(docs[8]["error"]["kind"], "invalid_comparison_request");
    for index in [5, 7, 9] {
        assert_eq!(
            docs[index]["result"], docs[3]["result"],
            "comparison changed session stats"
        );
    }
    assert_eq!(docs[10]["success"], true);
    assert_eq!(docs[10]["result"]["evaluated"], json!([]));
    assert_eq!(docs[11]["result"]["symbol"], docs[2]["result"]["symbol"]);
    assert_eq!(
        docs[11]["result"]["symbol"]["params"][0]["behavior"],
        "borrow"
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), disk);
}

#[test]
fn malformed_json_does_not_prevent_the_next_comparison() {
    let workspace = Workspace::new();
    let valid = single("fn main() {}", "fn main() {}");
    let docs = daemon_lines(&workspace.0, format!("not json\n{valid}\n"), 2);
    assert_eq!(docs[0]["error"]["kind"], "io");
    comparison(&docs[1], "unchanged");
}
