//! Diagnostic guides must teach corrections accepted by the actual checker.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_ontixa");
const CODES: &[&str] = &[
    "E_USE_AFTER_MOVE",
    "E_UNINITIALIZED",
    "E_IMMUTABLE_ASSIGNMENT",
    "E_MUTABLE_BORROW_OF_IMMUTABLE",
];

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("ontixa_diagnostic_{name}_{}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(args)
            .current_dir(&self.0)
            .output()
            .expect("run ontixa")
    }

    fn source(&self, source: &str) {
        std::fs::write(self.0.join("example.ixa"), source).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn envelope(out: &Output) -> Value {
    assert!(out.stderr.is_empty(), "unexpected stderr: {out:?}");
    // A single parse rejects trailing documents or progress logs.
    let doc: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
    assert_eq!(doc["schema"], 1);
    assert_eq!(doc["timings"], json!([]));
    doc
}

#[test]
fn all_bundled_examples_fail_specifically_then_pass_the_checker() {
    for code in CODES {
        let scratch = Scratch::new(code);
        // No source tree, source argument, or repository-relative file is needed.
        let lookup = scratch.run(&["diagnostic", code, "--json"]);
        assert!(lookup.status.success());
        let doc = envelope(&lookup);
        assert_eq!(doc["command"], "diagnostic");
        assert_eq!(doc["success"], true);
        assert_eq!(doc["diagnostics"], json!([]));
        assert!(doc["error"].is_null());
        let guide = &doc["result"]["guide"];
        assert_eq!(guide["code"], *code);
        for field in ["title", "meaning", "cause", "correction"] {
            assert!(!guide[field].as_str().unwrap().is_empty());
        }
        assert!(!guide["see_also"].as_array().unwrap().is_empty());

        let failing = guide["failing_example"].as_str().unwrap();
        scratch.source(failing);
        let checked = scratch.run(&["check", "example.ixa", "--json"]);
        assert_eq!(checked.status.code(), Some(1));
        let failure = envelope(&checked);
        assert_eq!(failure["success"], false);
        assert_eq!(failure["result"], Value::Null);
        assert_eq!(failure["diagnostics"].as_array().unwrap().len(), 1);
        assert_eq!(failure["diagnostics"][0]["code"], *code);
        assert_eq!(failure["diagnostics"][0]["severity"], "error");

        let corrected = guide["corrected_example"].as_str().unwrap();
        scratch.source(corrected);
        let checked = scratch.run(&["check", "example.ixa", "--json"]);
        assert!(checked.status.success(), "{code}: {checked:?}");
        assert_eq!(envelope(&checked)["diagnostics"], json!([]));

        let again = scratch.run(&["diagnostic", code, "--json"]);
        assert_eq!(lookup.stdout, again.stdout, "guide must be byte-stable");
        let human = scratch.run(&["diagnostic", code]);
        assert!(human.status.success());
        assert!(human.stderr.is_empty());
        let text = String::from_utf8(human.stdout).unwrap();
        assert!(text.contains(failing));
        assert!(text.contains(corrected));
        assert!(text.contains(guide["correction"].as_str().unwrap()));
    }
}

#[test]
fn lookup_distinguishes_uncovered_codes_from_unknown_codes() {
    let scratch = Scratch::new("lookup_failures");
    for (code, kind) in [
        ("E_PARSE", "diagnostic_guide_unavailable"),
        ("W_UNREACHABLE_ARM", "diagnostic_guide_unavailable"),
        ("I_INTERNAL", "diagnostic_guide_unavailable"),
        ("E_NOT_REAL", "unknown_diagnostic_code"),
        ("e_use_after_move", "unknown_diagnostic_code"),
    ] {
        let output = scratch.run(&["diagnostic", code, "--json"]);
        assert_eq!(output.status.code(), Some(2));
        let doc = envelope(&output);
        assert_eq!(doc["command"], "diagnostic");
        assert_eq!(doc["success"], false);
        assert_eq!(doc["diagnostics"], json!([]));
        assert_eq!(doc["error"]["kind"], kind);
        assert_eq!(doc["result"]["code"], code);
        assert_eq!(doc["result"]["available_codes"], json!(CODES));
        assert_eq!(
            output.stdout,
            scratch.run(&["diagnostic", code, "--json"]).stdout
        );

        let human = scratch.run(&["diagnostic", code]);
        assert_eq!(human.status.code(), Some(2));
        assert!(human.stdout.is_empty());
        let text = String::from_utf8(human.stderr).unwrap();
        assert!(text.contains(doc["error"]["message"].as_str().unwrap()));
        for available in CODES {
            assert!(text.contains(available));
        }
    }
}

#[test]
fn human_check_hints_are_covered_deduplicated_and_do_not_change_json() {
    let scratch = Scratch::new("check_hints");
    scratch.source("fn main() -> i32 { let x: i32; let y: i32; return x + y; }");
    let checked = scratch.run(&["check", "example.ixa"]);
    assert_eq!(checked.status.code(), Some(1));
    assert!(checked.stdout.is_empty());
    let text = String::from_utf8(checked.stderr).unwrap();
    assert_eq!(text.matches("ontixa diagnostic E_UNINITIALIZED").count(), 1);
    let machine = envelope(&scratch.run(&["check", "example.ixa", "--json"]));
    for diagnostic in machine["diagnostics"].as_array().unwrap() {
        assert_eq!(diagnostic["code"], "E_UNINITIALIZED");
        assert_eq!(diagnostic["help"], json!([]));
        assert_eq!(diagnostic["notes"], json!([]));
    }
    // Uncovered source errors and successful checks do not advertise a guide.
    for source in [
        "fn main() -> i32 { return missing; }",
        "fn main() -> i32 { return 0; }",
    ] {
        scratch.source(source);
        let checked = scratch.run(&["check", "example.ixa"]);
        assert!(!String::from_utf8_lossy(&checked.stderr).contains("ontixa diagnostic"));
    }
}

#[test]
fn command_help_discovers_guides_and_explain_keeps_its_symbol_contract() {
    let scratch = Scratch::new("help_and_explain");
    let help = scratch.run(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("diagnostic"));
    let help = scratch.run(&["diagnostic", "--help"]);
    assert!(help.status.success());
    let text = String::from_utf8(help.stdout).unwrap();
    for code in CODES {
        assert!(text.contains(code));
    }

    scratch.source("data Point { x: i32; }\nfn read(p: Point) -> i32 { return p.x; }");
    let explained = scratch.run(&["explain", "example.ixa", "read", "--json"]);
    assert!(explained.status.success());
    let doc = envelope(&explained);
    assert_eq!(doc["command"], "explain");
    assert_eq!(doc["result"]["symbol"]["name"], "read");
    assert_eq!(doc["result"]["symbol"]["params"][0]["behavior"], "borrow");
    let explained = scratch.run(&["explain", "example.ixa", "--json"]);
    assert!(explained.status.success());
    assert_eq!(
        envelope(&explained)["result"]["defs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn recognized_codes_match_the_public_catalog() {
    use ontixa_diagnostics::Code;
    use std::collections::BTreeSet;
    let documented: BTreeSet<&str> = include_str!("../../../docs/diagnostics.md")
        .lines()
        .filter_map(|line| line.strip_prefix("| `").and_then(|s| s.split_once('`')))
        .map(|(code, _)| code)
        .filter(|code| code.starts_with("E_") || code.starts_with("W_") || code.starts_with("I_"))
        .collect();
    let registered: BTreeSet<&str> = Code::ALL.iter().map(|code| code.as_str()).collect();
    assert_eq!(documented, registered);
    assert_eq!(
        registered.len(),
        Code::ALL.len(),
        "duplicate code registration"
    );
    for code in Code::ALL {
        assert_eq!(Code::from_code(code.as_str()), Some(*code));
        assert_eq!(serde_json::to_value(code).unwrap(), code.as_str());
    }
    assert_eq!(Code::from_code("E_NOT_REAL"), None);
}
