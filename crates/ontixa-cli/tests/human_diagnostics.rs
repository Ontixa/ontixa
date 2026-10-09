//! Human source annotations and the unchanged JSON diagnostics agree.

use serde_json::{Value, json};
use std::process::Command;

#[test]
fn use_after_move_retains_primary_and_move_site_on_same_or_separate_lines() {
    let directory = loop {
        let path = std::env::temp_dir().join(format!(
            "ontixa_human_diagnostics_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => break path,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => panic!("create fixture: {e}"),
        }
    };
    let path = directory.join("main.ixa");
    for separator in [" ", "\n    "] {
        let source = format!(
            "data Point {{ x: i32; }}\n\
             fn keep(p: Point) -> Point {{ return p; }}\n\
             fn main() -> i32 {{\n    let p = Point {{ x: 1 }};\n    \
             let q = keep(p);{separator}return p.x;\n}}\n"
        );
        std::fs::write(&path, &source).unwrap();
        let human = Command::new(env!("CARGO_BIN_EXE_ontixa"))
            .arg("check")
            .arg(&path)
            .output()
            .unwrap();
        assert_eq!(human.status.code(), Some(1));
        assert!(human.stdout.is_empty());
        let stderr = String::from_utf8(human.stderr).unwrap();
        assert_eq!(stderr.matches("- value moved here").count(), 1);
        assert_eq!(stderr.matches('^').count(), 1, "{stderr}");
        assert_eq!(stderr.matches("let q = keep(p);").count(), 1);
        assert_eq!(stderr.matches("return p.x;").count(), 1);

        let machine = Command::new(env!("CARGO_BIN_EXE_ontixa"))
            .arg("check")
            .arg(&path)
            .arg("--json")
            .output()
            .unwrap();
        assert_eq!(machine.status.code(), Some(1));
        assert!(machine.stderr.is_empty());
        let envelope: Value = serde_json::from_slice(&machine.stdout).unwrap();
        let primary = source.find("return p.x").unwrap() + "return ".len();
        let moved = source.find("keep(p);").unwrap() + "keep(".len();
        assert_eq!(
            envelope,
            json!({
                "schema": 1, "command": "check", "success": false,
                "diagnostics": [{
                    "code": "E_USE_AFTER_MOVE", "severity": "error",
                    "message": "`p` is used after its value moved",
                    "primary": {"file": path.to_str().unwrap(), "start": primary, "end": primary + 1},
                    "labels": [{"start": moved, "end": moved + 1, "message": "value moved here"}],
                    "notes": [], "help": [], "subject": "p"
                }],
                "result": null, "timings": [], "error": null
            })
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    }
    std::fs::remove_dir_all(directory).unwrap();
}
