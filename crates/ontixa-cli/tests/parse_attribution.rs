//! Source-file provenance of parser diagnostics in the ordinary CLI and daemon.

use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

#[test]
fn cli_and_daemon_preserve_all_parse_error_modules_and_source_bytes() {
    let directory = loop {
        let path = std::env::temp_dir().join(format!(
            "ontixa_parse_attribution_{}_{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => break path,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("create fixture: {e}"),
        }
    };
    let sources = [
        ("main", "use a; use b; fn f( {"),
        ("a", "fn f( {"),
        ("b", "fn f( {"),
    ];
    for (name, text) in sources {
        std::fs::write(directory.join(format!("{name}.ixa")), text).unwrap();
    }
    let main = directory.join("main.ixa");
    let output = Command::new(env!("CARGO_BIN_EXE_ontixa"))
        .args(["check", main.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let cli: Value = serde_json::from_slice(&output.stdout).unwrap();
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_ontixad"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(
        daemon.stdin.take().unwrap(),
        "{}",
        json!({"op": "check", "path": main})
    )
    .unwrap();
    let output = daemon.wait_with_output().unwrap();
    assert!(output.status.success());
    let daemon: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(cli["diagnostics"], daemon["diagnostics"]);
    let diagnostics = cli["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 9);
    for (name, text) in sources {
        let file = directory.join(format!("{name}.ixa"));
        let spans: Vec<_> = diagnostics
            .iter()
            .filter(|d| d["primary"]["file"] == file.to_str().unwrap())
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
        assert_eq!(std::fs::read_to_string(file).unwrap(), text);
    }
    std::fs::remove_dir_all(directory).unwrap();
}
