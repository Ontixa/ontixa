//! Public acceptance gate for the bounded, compile-only contract comparison.

use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_ontixa");
static NEXT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn recursive_checker_contracts_are_known_without_executing_the_cycle() {
    let workspace = Workspace::new();
    let (before, after) = workspace.pair(
        "data P { x: i32; } fn recurse(p: P) -> i32 { recurse(p) }",
        "data P { x: i32; } fn recurse(p: P) -> i32 { p.x }",
    );
    let doc = comparison(&before, &after, "unchanged");
    for side in ["before", "after"] {
        assert_eq!(doc["result"]["inputs"][side]["uncertainties"], json!([]));
    }
    // The current finite fixpoint does not generate Unknown for recursion.
    // Confirm the actual checker result, without interpreting this program.
    let explained = Command::new(BIN)
        .arg("explain")
        .arg(&before)
        .args(["recurse", "--json"])
        .output()
        .unwrap();
    assert!(explained.status.success(), "{explained:?}");
    let explained: Value = serde_json::from_slice(&explained.stdout).unwrap();
    assert_eq!(
        explained["result"]["symbol"]["params"][0]["behavior"],
        "borrow"
    );
}

struct Workspace(PathBuf);

impl Workspace {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        loop {
            let path = std::env::temp_dir().join(format!(
                "ontixa_semantic_diff_{}_{}_{}",
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

    fn pair(&self, before: &str, after: &str) -> (PathBuf, PathBuf) {
        (
            self.write("before/main.ixa", before),
            self.write("after/main.ixa", after),
        )
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn invoke(before: &Path, after: &Path, json: bool) -> Output {
    let mut command = Command::new(BIN);
    command.args(["diff", "--semantic"]).arg(before).arg(after);
    if json {
        command.arg("--json");
    }
    // A regression that starts interpreting the large-loop fixture must fail
    // promptly instead of hanging the entire acceptance suite.
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain both pipes while waiting: even a small report can fill the
    // platform's pipe buffer, particularly on Windows.
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stdout = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let stderr = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if start.elapsed() > Duration::from_secs(20) {
            timed_out = true;
            let _ = child.kill();
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = Output {
        status,
        stdout: stdout.join().unwrap(),
        stderr: stderr.join().unwrap(),
    };
    assert!(
        !timed_out,
        "contract comparison did not finish without executing source: {output:?}"
    );
    output
}

fn document(output: &Output) -> Value {
    assert!(output.stderr.is_empty(), "unexpected stderr: {output:?}");
    let doc: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("expected one JSON envelope: {error}: {output:?}"));
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["command"], "diff");
    assert_eq!(doc["timings"], json!([]));
    assert!(doc["diagnostics"].is_array());
    assert_eq!(doc["result"]["comparison_schema"], 1);
    assert!(doc["result"]["coverage"].is_object());
    for side in ["before", "after"] {
        let input = &doc["result"]["inputs"][side];
        assert!(input["path"].is_string());
        assert!(input["module"].is_string());
        assert!(input["status"].is_string());
        assert!(input.as_object().unwrap().contains_key("error"));
    }
    doc
}

fn comparison(before: &Path, after: &Path, expected_status: &str) -> Value {
    let output = invoke(before, after, true);
    let doc = document(&output);
    assert_eq!(output.status.code(), Some(0), "{doc}");
    assert_eq!(doc["success"], true, "{doc}");
    assert!(doc["error"].is_null());
    assert_eq!(doc["result"]["status"], expected_status, "{doc}");
    for side in ["before", "after"] {
        assert_eq!(doc["result"]["inputs"][side]["status"], "ok");
        assert!(doc["result"]["inputs"][side]["error"].is_null());
    }
    let changes = doc["result"]["changes"].as_array().unwrap();
    assert!(
        changes.windows(2).all(|pair| {
            pair[0]["definition"].as_str().unwrap() < pair[1]["definition"].as_str().unwrap()
        }),
        "changes must be sorted by qualified definition: {doc}"
    );
    doc
}

fn change<'a>(doc: &'a Value, name: &str) -> &'a Value {
    doc["result"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|change| change["definition"] == name)
        .unwrap_or_else(|| panic!("missing change for {name}: {doc}"))
}

fn field(change: &Value, name: &str, before: Value, after: Value) {
    let actual = change["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["field"] == name)
        .unwrap_or_else(|| panic!("missing field {name}: {change}"));
    assert_eq!(
        actual,
        &json!({"field": name, "before": before, "after": after})
    );
}

fn unavailable(before: &Path, after: &Path, exit: i32) -> Value {
    let output = invoke(before, after, true);
    let doc = document(&output);
    assert_eq!(output.status.code(), Some(exit), "{doc}");
    assert_eq!(doc["success"], false);
    assert_eq!(doc["error"]["kind"], "comparison_unavailable");
    assert_eq!(doc["result"]["status"], "unavailable");
    assert!(
        doc["result"]["changes"].is_null(),
        "unavailable is not an empty diff: {doc}"
    );
    doc
}

#[test]
fn comments_whitespace_item_order_and_directory_prefixes_are_not_changes() {
    let workspace = Workspace::new();
    let before = workspace.write("old checkout/nested/main.ixa", "data P { x: i32; } data Q { y: bool; } fn read(p: P) -> i32 { return p.x; } fn keep(q: Q) -> Q { return q; }");
    let after = workspace.write("new checkout/main.ixa", "// Changed layout and allocation order\nfn keep(q: Q) -> Q { return q; }\n data Q { y: bool; }\nfn read( p:P )->i32 {return p.x;}\ndata P {x:i32;}\n");
    let doc = comparison(&before, &after, "unchanged");
    assert_eq!(doc["result"]["changes"], json!([]));
    assert_eq!(doc["result"]["inputs"]["before"]["module"], "main");
    assert_eq!(doc["result"]["inputs"]["after"]["module"], "main");
    let human = invoke(&before, &after, false);
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("No changes in compared fields"));
    for word in ["equivalent", "compatible"] {
        assert!(
            !String::from_utf8_lossy(&human.stdout)
                .to_lowercase()
                .contains(word)
        );
    }
}

#[test]
fn body_change_from_borrow_to_move_propagates_to_callers() {
    let workspace = Workspace::new();
    let source = "data P { x: i32; } fn read(p: P) -> i32 { return p.x; } fn relay(p: P) -> i32 { return read(p); }";
    let (before, after) = workspace.pair(source, &source.replace("return p.x;", "return 0;"));
    let doc = comparison(&before, &after, "changed");
    assert_eq!(doc["result"]["changes"].as_array().unwrap().len(), 2);
    for name in ["main::read", "main::relay"] {
        let changed = change(&doc, name);
        assert_eq!(changed["kind"], "modified");
        for (side, behavior) in [("before", "borrow"), ("after", "move")] {
            assert_eq!(
                changed[side],
                json!({"kind": "fn", "params": [{"name": "p", "type": "main::P", "mutable": false, "behavior": behavior, "escapes": []}], "returns": "i32"})
            );
        }
        assert_eq!(changed["fields"].as_array().unwrap().len(), 1);
        field(
            changed,
            "params[0].behavior",
            json!("borrow"),
            json!("move"),
        );
    }
    assert_eq!(std::fs::read_to_string(before).unwrap(), source);
}

#[test]
fn parameter_names_types_mutability_positions_and_returns_are_compared() {
    let workspace = Workspace::new();
    let (before, after) = workspace.pair(
        "fn f(value: i32, flag: bool) -> i32 { return 0; }",
        "fn f(mut flag: bool, value: i64) -> i64 { return 0; }",
    );
    let doc = comparison(&before, &after, "changed");
    let changed = change(&doc, "main::f");
    for (name, before, after) in [
        ("params[0].name", json!("value"), json!("flag")),
        ("params[0].type", json!("i32"), json!("bool")),
        ("params[0].mutable", json!(false), json!(true)),
        ("params[1].name", json!("flag"), json!("value")),
        ("params[1].type", json!("bool"), json!("i64")),
        ("returns", json!("i32"), json!("i64")),
    ] {
        field(changed, name, before, after);
    }
    assert_eq!(changed["fields"].as_array().unwrap().len(), 6);
    for (before_src, after_src) in [
        ("fn f() {}", "fn f(x: i32) {}"),
        ("fn f(x: i32) {}", "fn f() {}"),
    ] {
        let (before, after) = workspace.pair(before_src, after_src);
        let doc = comparison(&before, &after, "changed");
        let changed = change(&doc, "main::f");
        field(
            changed,
            "params",
            changed["before"]["params"].clone(),
            changed["after"]["params"].clone(),
        );
    }
}

#[test]
fn record_fields_enum_payloads_discriminants_and_shapes_preserve_order() {
    let workspace = Workspace::new();
    let (before, after) = workspace.pair(
        "data Record { x: i32; y: bool; } data Choice { A(i32, bool); B; } data Switch { x: i32; }",
        "data Record { y: bool; x: i32; } data Choice { B; A(bool, i64); } data Switch { X; }",
    );
    let doc = comparison(&before, &after, "changed");
    assert_eq!(doc["result"]["changes"].as_array().unwrap().len(), 3);
    let record = change(&doc, "main::Record");
    assert_eq!(
        record["before"],
        json!({"kind": "data", "shape": "record", "fields": [{"name": "x", "type": "i32"}, {"name": "y", "type": "bool"}], "variants": []})
    );
    assert_eq!(
        record["after"]["fields"],
        json!([{"name": "y", "type": "bool"}, {"name": "x", "type": "i32"}])
    );
    field(record, "fields[0].name", json!("x"), json!("y"));
    let choice = change(&doc, "main::Choice");
    assert_eq!(
        choice["before"],
        json!({"kind": "data", "shape": "enum", "fields": [], "variants": [{"name": "A", "discriminant": 0, "payload": ["i32", "bool"]}, {"name": "B", "discriminant": 1, "payload": []}]})
    );
    assert_eq!(
        choice["after"]["variants"],
        json!([{"name": "B", "discriminant": 0, "payload": []}, {"name": "A", "discriminant": 1, "payload": ["bool", "i64"]}])
    );
    field(
        change(&doc, "main::Switch"),
        "shape",
        json!("record"),
        json!("enum"),
    );
}

#[test]
fn nominal_and_array_types_use_qualified_module_identity() {
    let workspace = Workspace::new();
    let before_source = "use a; use b; fn read(p: a::P) -> i32 { return p.x; } fn keep(ps: [a::P]) -> [a::P] { return ps; } data Box { item: a::P; } data Choice { Some(a::P); }";
    let (before, after) = workspace.pair(before_source, &before_source.replace("a::P", "b::P"));
    for side in ["before", "after"] {
        for module in ["a", "b"] {
            workspace.write(&format!("{side}/{module}.ixa"), "data P { x: i32; }");
        }
    }
    let doc = comparison(&before, &after, "changed");
    assert_eq!(doc["result"]["changes"].as_array().unwrap().len(), 4);
    field(
        change(&doc, "main::read"),
        "params[0].type",
        json!("a::P"),
        json!("b::P"),
    );
    let keep = change(&doc, "main::keep");
    field(keep, "params[0].type", json!("[a::P]"), json!("[b::P]"));
    field(keep, "returns", json!("[a::P]"), json!("[b::P]"));
    field(
        change(&doc, "main::Box"),
        "fields[0].type",
        json!("a::P"),
        json!("b::P"),
    );
    field(
        change(&doc, "main::Choice"),
        "variants[0].payload[0]",
        json!("a::P"),
        json!("b::P"),
    );
}

#[test]
fn additions_removals_and_renames_are_sorted_by_qualified_name() {
    let workspace = Workspace::new();
    let (before, after) = workspace.pair(
        "fn old() -> i32 { return 1; } data Gone { x: i32; }",
        "data Added { x: bool; } fn renamed() -> i32 { return 1; }",
    );
    let doc = comparison(&before, &after, "changed");
    let changes = doc["result"]["changes"].as_array().unwrap();
    assert_eq!(
        changes
            .iter()
            .map(|c| c["definition"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["main::Added", "main::Gone", "main::old", "main::renamed"]
    );
    for (name, kind, missing, present) in [
        ("main::Added", "added", "before", "after"),
        ("main::Gone", "removed", "after", "before"),
        ("main::old", "removed", "after", "before"),
        ("main::renamed", "added", "before", "after"),
    ] {
        let changed = change(&doc, name);
        assert_eq!(changed["kind"], kind);
        assert!(changed[missing].is_null());
        assert!(changed[present].is_object());
        assert!(changed["fields"].is_array());
    }
    assert_eq!(
        change(&doc, "main::old")["before"],
        change(&doc, "main::renamed")["after"]
    );
}

#[test]
fn changing_definition_kind_is_a_modification_at_the_same_identity() {
    let workspace = Workspace::new();
    let (before, after) = workspace.pair("fn Item() {}", "data Item { x: i32; }");
    let doc = comparison(&before, &after, "changed");
    assert_eq!(doc["result"]["changes"].as_array().unwrap().len(), 1);
    let changed = change(&doc, "main::Item");
    assert_eq!(changed["kind"], "modified");
    field(changed, "kind", json!("fn"), json!("data"));
}

#[test]
fn reachable_siblings_are_compared_transitively_and_unused_files_are_ignored() {
    let workspace = Workspace::new();
    let (before, after) = workspace.pair("use bridge; fn main() {}", "use bridge; fn main() {}");
    for side in ["before", "after"] {
        workspace.write(&format!("{side}/bridge.ixa"), "use leaf;");
        workspace.write(&format!("{side}/unused.ixa"), "this is not valid source {");
    }
    workspace.write("before/leaf.ixa", "fn value(x: i32) -> i32 { return x; }");
    workspace.write("after/leaf.ixa", "fn value(x: i64) -> i64 { return x; }");
    let doc = comparison(&before, &after, "changed");
    assert_eq!(doc["result"]["changes"].as_array().unwrap().len(), 1);
    assert_eq!(change(&doc, "leaf::value")["kind"], "modified");
}

#[test]
fn escape_targets_are_qualified_deduplicated_and_independent_of_body_order() {
    let workspace = Workspace::new();
    let direct = "use types; use a; use z; fn route(p: types::P, c: bool, d: bool) -> types::P { return p; }";
    let routed = "use types; use a; use z; fn route(p: types::P, c: bool, d: bool) -> types::P { if c { return z::keep(p); } else { if d { return a::keep(p); } else { return z::keep(p); } } }";
    let (before, after) = workspace.pair(direct, routed);
    for side in ["before", "after"] {
        workspace.write(&format!("{side}/types.ixa"), "data P { x: i32; }");
        for module in ["a", "z"] {
            workspace.write(
                &format!("{side}/{module}.ixa"),
                "use types; fn keep(p: types::P) -> types::P { return p; }",
            );
        }
    }
    let doc = comparison(&before, &after, "changed");
    let changed = change(&doc, "main::route");
    assert_eq!(
        changed["before"]["params"][0]["escapes"],
        json!([{"kind": "return"}])
    );
    let escapes = changed["after"]["params"][0]["escapes"].as_array().unwrap();
    assert_eq!(escapes.len(), 3, "{changed}");
    for target in [
        json!({"kind": "return"}),
        json!({"kind": "call", "callee": "a::keep"}),
        json!({"kind": "call", "callee": "z::keep"}),
    ] {
        assert!(escapes.contains(&target), "missing {target}: {changed}");
    }
    workspace.write(
        "before/main.ixa",
        &routed
            .replace("z::keep", "TEMP::keep")
            .replace("a::keep", "z::keep")
            .replace("TEMP::keep", "a::keep"),
    );
    let unchanged = comparison(&before, &after, "unchanged");
    assert_eq!(unchanged["result"]["changes"], json!([]));
}

#[test]
fn uncovered_body_results_are_unchanged_and_programs_are_never_executed() {
    let workspace = Workspace::new();
    for after_source in [
        "fn main() -> i32 { return 42; }",
        "fn main() -> i32 { return 1 / 0; }",
        "fn main() -> i32 { for i in 0..2147483647 {} return 0; }",
    ] {
        let (before, after) = workspace.pair("fn main() -> i32 { return 0; }", after_source);
        let doc = comparison(&before, &after, "unchanged");
        assert_eq!(doc["result"]["changes"], json!([]));
    }
}

#[test]
fn source_errors_on_either_or_both_sides_make_comparison_unavailable() {
    let workspace = Workspace::new();
    let valid = "fn main() -> i32 { return 0; }";
    for (invalid, code) in [
        ("fn f() {} fn f() {}", "E_DUPLICATE_DEF"),
        ("data P { x: i32; x: i32; }", "E_DUPLICATE_FIELD"),
        ("fn broken( {", "E_PARSE"),
        ("fn main() -> i32 { return true; }", "E_TYPE_MISMATCH"),
        ("use absent; fn main() {}", "E_UNKNOWN_MODULE"),
    ] {
        for (before_src, after_src, bad_sides) in [
            (invalid, valid, vec!["before"]),
            (valid, invalid, vec!["after"]),
            (invalid, invalid, vec!["before", "after"]),
        ] {
            let (before, after) = workspace.pair(before_src, after_src);
            let doc = unavailable(&before, &after, 1);
            let diagnostics = doc["diagnostics"].as_array().unwrap();
            for side in ["before", "after"] {
                let expected = if bad_sides.contains(&side) {
                    "source_errors"
                } else {
                    "ok"
                };
                assert_eq!(doc["result"]["inputs"][side]["status"], expected, "{doc}");
                if bad_sides.contains(&side) {
                    assert!(
                        diagnostics
                            .iter()
                            .any(|d| d["comparison_side"] == side && d["code"] == code),
                        "missing {side} {code}: {doc}"
                    );
                }
            }
            assert!(
                diagnostics
                    .iter()
                    .all(|d| bad_sides.iter().any(|side| d["comparison_side"] == *side)),
                "every diagnostic must name its comparison side: {doc}"
            );
        }
    }
}

#[test]
fn dependency_diagnostics_keep_their_side_and_actual_source_file() {
    let workspace = Workspace::new();
    let (before, after) = workspace.pair("use dep; fn main() {}", "use dep; fn main() {}");
    for side in ["before", "after"] {
        workspace.write(
            &format!("{side}/dep.ixa"),
            "fn broken() -> i32 { return true; }",
        );
    }
    let doc = unavailable(&before, &after, 1);
    for side in ["before", "after"] {
        assert!(
            doc["diagnostics"].as_array().unwrap().iter().any(|d| {
                d["comparison_side"] == side
                    && d["primary"]["file"].as_str().is_some_and(|file| {
                        Path::new(file).ends_with(Path::new(side).join("dep.ixa"))
                    })
            }),
            "dependency diagnostic lost its source or side: {doc}"
        );
    }
}

#[test]
fn missing_roots_are_io_failures_and_the_other_side_is_still_checked() {
    let workspace = Workspace::new();
    let (before, after) = workspace.pair("fn main() {}", "fn main() {}");
    std::fs::remove_file(&before).unwrap();
    let doc = unavailable(&before, &after, 2);
    assert_eq!(doc["result"]["inputs"]["before"]["status"], "io");
    assert_eq!(doc["result"]["inputs"]["after"]["status"], "ok");
    workspace.write("after/main.ixa", "fn main() -> i32 { return true; }");
    let doc = unavailable(&before, &after, 2);
    assert_eq!(doc["result"]["inputs"]["after"]["status"], "source_errors");
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["comparison_side"] == "after")
    );
    std::fs::remove_file(&after).unwrap();
    let doc = unavailable(&before, &after, 2);
    assert_eq!(doc["result"]["inputs"]["after"]["status"], "io");
}

#[test]
fn different_root_names_and_invalid_module_identifiers_are_unavailable() {
    let workspace = Workspace::new();
    let before = workspace.write("before/main.ixa", "fn f() {}");
    let after = workspace.write("after/other.ixa", "fn f() {}");
    let doc = unavailable(&before, &after, 1);
    assert_eq!(doc["result"]["inputs"]["before"]["module"], "main");
    assert_eq!(doc["result"]["inputs"]["after"]["module"], "other");
    assert!(
        ["before", "after"]
            .iter()
            .any(|side| doc["result"]["inputs"][*side]["status"] == "root_mismatch")
    );
    let before = workspace.write("before/not-a-module.ixa", "fn f() {}");
    let after = workspace.write("after/not-a-module.ixa", "fn f() {}");
    unavailable(&before, &after, 1);
}

#[test]
fn json_is_byte_deterministic_for_success_and_source_errors() {
    let workspace = Workspace::new();
    for after_source in [
        "fn f(x: i64) -> i64 { return x; }",
        "fn f() -> i32 { return true; }",
    ] {
        let (before, after) = workspace.pair("fn f(x: i32) -> i32 { return x; }", after_source);
        let first = invoke(&before, &after, true);
        document(&first);
        let second = invoke(&before, &after, true);
        assert_eq!(first.status.code(), second.status.code());
        assert_eq!(first.stdout, second.stdout);
    }
}

#[test]
fn comparison_never_changes_sources_or_creates_transaction_artifacts() {
    fn snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> {
        fn visit(
            root: &Path,
            path: &Path,
            files: &mut std::collections::BTreeMap<PathBuf, Option<Vec<u8>>>,
        ) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                let key = path.strip_prefix(root).unwrap().to_path_buf();
                if path.is_dir() {
                    files.insert(key, None);
                    visit(root, &path, files);
                } else {
                    files.insert(key, Some(std::fs::read(path).unwrap()));
                }
            }
        }
        let mut files = std::collections::BTreeMap::new();
        visit(root, root, &mut files);
        files
    }

    let workspace = Workspace::new();
    for case in [
        "changed",
        "before source error",
        "after source error",
        "before missing",
        "after missing",
    ] {
        let (before, after) =
            workspace.pair("fn f() -> i32 { return 0; }", "fn f() -> i64 { return 1; }");
        match case {
            "before source error" => {
                workspace.write("before/main.ixa", "fn f() -> i32 { return true; }");
            }
            "after source error" => {
                workspace.write("after/main.ixa", "fn f() -> i32 { return true; }");
            }
            "before missing" => std::fs::remove_file(&before).unwrap(),
            "after missing" => std::fs::remove_file(&after).unwrap(),
            _ => {}
        }
        let original = snapshot(&workspace.0);
        if case == "changed" {
            comparison(&before, &after, "changed");
        } else {
            unavailable(
                &before,
                &after,
                if case.ends_with("missing") { 2 } else { 1 },
            );
        }
        assert_eq!(
            snapshot(&workspace.0),
            original,
            "comparison mutated workspace for {case}"
        );
    }
}

#[test]
fn semantic_mode_is_explicit_and_discoverable() {
    let workspace = Workspace::new();
    let (before, after) = workspace.pair("fn f() {}", "fn f() {}");
    let omitted = Command::new(BIN)
        .arg("diff")
        .arg(before)
        .arg(after)
        .output()
        .unwrap();
    assert_eq!(omitted.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&omitted.stderr).contains("--semantic"));
    let help = Command::new(BIN).args(["diff", "--help"]).output().unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--semantic"));
}
