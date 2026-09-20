//! `persist()` regressions: the disk stale-guard must refuse to
//! overwrite a file that changed after the plan was validated, and
//! a mid-write failure must restore every already-written file.

use ontixa_cli::rename::persist;
use ontixa_db::Db;
use ontixa_source::{FileId, SourceFile};
use std::io::Write;
use std::path::PathBuf;

const DEP: &str = "fn double(x: i32) -> i32 { return x * 2; }\n";

const MAIN: &str = "use dep;\n\
                    fn main() -> i32 { return dep::double(21); }\n";

/// A uniquely named workspace dir under the OS temp root.
fn ws_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ontixa_persist_{tag}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp workspace");
    dir
}

/// Writes `path` with `text`, returning the `SourceFile` view the
/// CLI layer would hand `persist`.
fn write(path: &PathBuf, name: &str, id: u32, text: &str) -> SourceFile {
    let mut f = std::fs::File::create(path).expect("create source");
    f.write_all(text.as_bytes()).expect("write source");
    SourceFile::new(
        FileId::new(id),
        name.to_string(),
        Some(path.clone()),
        text.to_string(),
    )
}

#[test]
fn stale_disk_is_rejected_not_overwritten() {
    let dir = ws_dir("stale");
    let main_p = dir.join("main.ixa");
    let dep_p = dir.join("dep.ixa");
    let sfs = vec![
        write(&main_p, "main", 0, MAIN),
        write(&dep_p, "dep", 1, DEP),
    ];
    let mut db = Db::new();
    db.add_source_named("main", MAIN);
    db.add_source_named("dep", DEP);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();

    // An external writer edits dep.ixa between plan and persist.
    std::fs::write(
        &dep_p,
        "fn double(x: i32) -> i32 { return x * 2; } // touched\n",
    )
    .expect("external edit");

    let err = persist(&plan, &sfs).expect_err("stale disk must reject");
    assert!(err.contains("changed on disk"), "error: {err}");
    // Nothing was overwritten: the external edit and main both
    // stand exactly as they were.
    assert_eq!(
        std::fs::read_to_string(&dep_p).unwrap(),
        "fn double(x: i32) -> i32 { return x * 2; } // touched\n"
    );
    assert_eq!(std::fs::read_to_string(&main_p).unwrap(), MAIN);
}

#[test]
fn happy_path_writes_every_file() {
    let dir = ws_dir("happy");
    let main_p = dir.join("main.ixa");
    let dep_p = dir.join("dep.ixa");
    let sfs = vec![
        write(&main_p, "main", 0, MAIN),
        write(&dep_p, "dep", 1, DEP),
    ];
    let mut db = Db::new();
    db.add_source_named("main", MAIN);
    db.add_source_named("dep", DEP);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();

    persist(&plan, &sfs).expect("persist");
    assert!(
        std::fs::read_to_string(&dep_p)
            .unwrap()
            .contains("fn twice")
    );
    assert!(
        std::fs::read_to_string(&main_p)
            .unwrap()
            .contains("dep::twice")
    );
}

#[test]
fn mid_write_failure_rolls_back() {
    // Second file unwritable → the first file must be restored to
    // its original bytes, not left half-renamed.
    let dir = ws_dir("rollback");
    let main_p = dir.join("main.ixa");
    let dep_p = dir.join("dep.ixa");
    let sfs = vec![
        write(&main_p, "main", 0, MAIN),
        write(&dep_p, "dep", 1, DEP),
    ];
    let mut db = Db::new();
    db.add_source_named("main", MAIN);
    db.add_source_named("dep", DEP);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();

    // dep.ixa (file 1 — second in write order) read-only.
    let mut perm = std::fs::metadata(&dep_p).unwrap().permissions();
    perm.set_readonly(true);
    std::fs::set_permissions(&dep_p, perm.clone()).expect("chmod readonly");

    let err = persist(&plan, &sfs).expect_err("unwritable file rejects");
    assert!(err.contains("cannot write"), "error: {err}");
    // main.ixa was written first — rollback must restore it.
    assert_eq!(std::fs::read_to_string(&main_p).unwrap(), MAIN);

    // Restore writability so the temp dir cleans up on Windows.
    #[allow(clippy::permissions_set_readonly_false)]
    {
        perm.set_readonly(false);
        std::fs::set_permissions(&dep_p, perm).expect("restore writability");
    }
}
