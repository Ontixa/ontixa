//! Persistent daemon checks must agree with a fresh session after every edit.

use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

fn daemon(requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ontixad"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for request in requests {
        writeln!(stdin, "{request}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(responses.len(), requests.len());
    responses
}

#[test]
fn dependency_errors_appear_move_and_clear_without_stale_copies() {
    let main = "diagnostic-freshness/main.ixa";
    let dep = "diagnostic-freshness/dep.ixa";
    let set_main = json!({"op":"set", "path":main, "text":"use dep; fn main() {}"});
    let set_dep = |text: &str| json!({"op":"set", "path":dep, "text":text});
    let check = json!({"op":"check", "path":main});
    for sources in [
        vec!["", "@", "\n\n@", "#", ""],
        vec!["fn f( {", "\n\nfn f( {", "fn f() {}"],
    ] {
        let mut requests = vec![set_main.clone(), set_dep(sources[0]), check.clone()];
        for text in &sources[1..] {
            requests.extend([set_dep(text), check.clone(), check.clone()]);
        }
        let responses = daemon(&requests);
        for (i, text) in sources[1..].iter().enumerate() {
            let warm = &responses[4 + 3 * i];
            let repeated = &responses[5 + 3 * i];
            let cold = daemon(&[set_main.clone(), set_dep(text), check.clone()]);
            assert_eq!(warm["success"], cold[2]["success"], "dep {text:?}");
            assert_eq!(warm["diagnostics"], cold[2]["diagnostics"], "dep {text:?}");
            assert_eq!(repeated["success"], warm["success"]);
            assert_eq!(repeated["diagnostics"], warm["diagnostics"]);
            assert_eq!(repeated["result"]["evaluated"], json!([]));
            for diagnostic in warm["diagnostics"].as_array().unwrap() {
                assert_eq!(diagnostic["code"], "E_PARSE");
                assert_eq!(diagnostic["primary"]["file"], dep);
            }
            assert_eq!(warm["success"], text.is_empty() || *text == "fn f() {}");
        }
    }
}
