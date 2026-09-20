//! `persist()` and the staged transaction engine: disk stale-guard,
//! staging, journaling, commit swap, rollback, and crash recovery —
//! with injected failures at every phase.

use ontixa_cli::persist::{
    FailPoint, Phase, RecoveryOutcome, TxFile, persist_tx, persist_tx_hooks, recover,
};
use ontixa_cli::rename::persist;
use ontixa_db::Db;
use ontixa_source::{FileId, SourceFile};
use std::io::Write;
use std::path::{Path, PathBuf};

const DEP: &str = "fn double(x: i32) -> i32 { return x * 2; }\n";

const MAIN: &str = "use dep;\n\
                    fn main() -> i32 { return dep::double(21); }\n";

/// A uniquely named workspace dir under the OS temp root.
fn ws_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ontixa_persist_{tag}_{}_{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("create temp workspace");
    dir
}

/// Writes `path` with `text`, returning the `SourceFile` view the
/// CLI layer would hand `persist`.
fn write(path: &Path, name: &str, id: u32, text: &str) -> SourceFile {
    let mut f = std::fs::File::create(path).expect("create source");
    f.write_all(text.as_bytes()).expect("write source");
    SourceFile::new(
        FileId::new(id),
        name.to_string(),
        Some(path.to_path_buf()),
        text.to_string(),
    )
}

/// A two-file transaction over `dir`: a.ixa + b.ixa, each getting
/// `after` bytes appended.
fn tx(dir: &Path) -> (PathBuf, PathBuf, Vec<TxFile>) {
    let a = dir.join("a.ixa");
    let b = dir.join("b.ixa");
    std::fs::write(&a, "fn a() -> i32 { return 1; }\n").unwrap();
    std::fs::write(&b, "fn b() -> i32 { return 2; }\n").unwrap();
    let files = vec![
        TxFile {
            path: a.clone(),
            before: b"fn a() -> i32 { return 1; }\n".to_vec(),
            after: b"fn aa() -> i32 { return 1; }\n".to_vec(),
        },
        TxFile {
            path: b.clone(),
            before: b"fn b() -> i32 { return 2; }\n".to_vec(),
            after: b"fn bb() -> i32 { return 2; }\n".to_vec(),
        },
    ];
    (a, b, files)
}

fn bytes(p: &Path) -> Vec<u8> {
    std::fs::read(p).expect("read file")
}

/// No transaction artifacts may survive a finished run.
fn assert_clean(dir: &Path) {
    let leftovers: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("ontixa-tx"))
        .collect();
    assert!(leftovers.is_empty(), "leftover artifacts: {leftovers:?}");
}

// ---------- high-level persist() ----------

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
    // stand exactly as they were — and no artifacts leaked.
    assert_eq!(
        std::fs::read_to_string(&dep_p).unwrap(),
        "fn double(x: i32) -> i32 { return x * 2; } // touched\n"
    );
    assert_eq!(std::fs::read_to_string(&main_p).unwrap(), MAIN);
    assert_clean(&dir);
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
    // Journal, stages and backups are all gone.
    assert_clean(&dir);
}

// ---------- staged engine: prepare-phase rejections ----------

#[test]
fn external_edit_between_plan_and_persist_rejected() {
    let dir = ws_dir("race");
    let (a, b, files) = tx(&dir);
    // Writer races in after validation but before the transaction.
    std::fs::write(&b, "fn b() -> i32 { return 2; } // racy\n").unwrap();
    let err = persist_tx(&dir, &files).unwrap_err();
    assert_eq!(err.phase, Phase::Prepare);
    assert!(err.message.contains("changed on disk"));
    // Both files exactly as the external writer left them.
    assert_eq!(bytes(&b), b"fn b() -> i32 { return 2; } // racy\n");
    assert_eq!(bytes(&a), b"fn a() -> i32 { return 1; }\n");
    assert_clean(&dir);
}

#[test]
fn missing_destination_rejected() {
    let dir = ws_dir("missing");
    let (a, _b, mut files) = tx(&dir);
    std::fs::remove_file(&a).unwrap();
    let err = persist_tx(&dir, &files).unwrap_err();
    assert_eq!(err.phase, Phase::Prepare);
    files.clear();
    assert_clean(&dir);
}

#[test]
fn directory_in_place_of_file_rejected() {
    let dir = ws_dir("dirswap");
    let (a, _b, files) = tx(&dir);
    std::fs::remove_file(&a).unwrap();
    std::fs::create_dir(&a).unwrap();
    let err = persist_tx(&dir, &files).unwrap_err();
    assert_eq!(err.phase, Phase::Prepare);
    assert!(
        err.message.contains("not a regular file"),
        "{}",
        err.message
    );
}

#[test]
fn path_outside_root_rejected() {
    let dir = ws_dir("escape");
    let outside = ws_dir("outside").join("x.ixa");
    std::fs::write(&outside, "x").unwrap();
    let files = vec![TxFile {
        path: outside.clone(),
        before: b"x".to_vec(),
        after: b"y".to_vec(),
    }];
    let err = persist_tx(&dir, &files).unwrap_err();
    assert_eq!(err.phase, Phase::Prepare);
    assert!(err.message.contains("outside the workspace root"));
    assert_eq!(bytes(&outside), b"x");
}

#[cfg(unix)]
#[test]
fn symlink_destination_rejected() {
    let dir = ws_dir("symlink");
    let real = dir.join("real.ixa");
    let link = dir.join("link.ixa");
    std::fs::write(&real, "fn r() -> i32 { return 1; }\n").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let files = vec![TxFile {
        path: link,
        before: b"fn r() -> i32 { return 1; }\n".to_vec(),
        after: b"fn rr() -> i32 { return 1; }\n".to_vec(),
    }];
    let err = persist_tx(&dir, &files).unwrap_err();
    assert_eq!(err.phase, Phase::Prepare);
    assert!(err.message.contains("symlink"), "{}", err.message);
    assert_eq!(bytes(&real), b"fn r() -> i32 { return 1; }\n");
}

// ---------- staging-phase failures ----------

#[test]
fn journal_write_failure_touches_nothing() {
    let dir = ws_dir("jfail");
    let (a, b, files) = tx(&dir);
    let err = persist_tx_hooks(&dir, &files, Some(FailPoint::JournalWrite)).unwrap_err();
    assert_eq!(err.phase, Phase::Staging);
    assert_eq!(bytes(&a), b"fn a() -> i32 { return 1; }\n");
    assert_eq!(bytes(&b), b"fn b() -> i32 { return 2; }\n");
    assert_clean(&dir);
}

#[test]
fn stage_write_failure_leaves_no_artifacts() {
    let dir = ws_dir("swfail");
    let (a, b, files) = tx(&dir);
    // File 1's stage fails: file 0's stage must be cleaned, the
    // journal removed, live bytes untouched.
    let err = persist_tx_hooks(&dir, &files, Some(FailPoint::StageWrite(1))).unwrap_err();
    assert_eq!(err.phase, Phase::Staging);
    assert_eq!(bytes(&a), b"fn a() -> i32 { return 1; }\n");
    assert_eq!(bytes(&b), b"fn b() -> i32 { return 2; }\n");
    assert_clean(&dir);
}

#[test]
fn stage_sync_failure_leaves_no_artifacts() {
    let dir = ws_dir("ssfail");
    let (a, _b, files) = tx(&dir);
    let err = persist_tx_hooks(&dir, &files, Some(FailPoint::StageSync(0))).unwrap_err();
    assert_eq!(err.phase, Phase::Staging);
    assert_eq!(bytes(&a), b"fn a() -> i32 { return 1; }\n");
    assert_clean(&dir);
}

#[test]
fn journal_update_failure_leaves_no_artifacts() {
    let dir = ws_dir("jufail");
    let (a, _b, files) = tx(&dir);
    let err = persist_tx_hooks(&dir, &files, Some(FailPoint::JournalUpdate)).unwrap_err();
    assert_eq!(err.phase, Phase::Staging);
    assert_eq!(bytes(&a), b"fn a() -> i32 { return 1; }\n");
    assert_clean(&dir);
}

// ---------- commit-phase failures ----------

#[test]
fn swap_failure_rolls_back_swapped_files() {
    let dir = ws_dir("swapfail");
    let (a, b, files) = tx(&dir);
    // File 0 swaps fine, file 1's dest→bak rename fails → file 0
    // must be restored to its exact original bytes.
    let err = persist_tx_hooks(&dir, &files, Some(FailPoint::Swap(1))).unwrap_err();
    assert_eq!(err.phase, Phase::Commit);
    assert!(err.rollback_errors.is_empty(), "{:?}", err.rollback_errors);
    assert_eq!(bytes(&a), b"fn a() -> i32 { return 1; }\n");
    assert_eq!(bytes(&b), b"fn b() -> i32 { return 2; }\n");
    assert_clean(&dir);
}

#[test]
fn promote_failure_restores_the_current_file() {
    let dir = ws_dir("promfail");
    let (a, b, files) = tx(&dir);
    // File 0: dest→bak succeeded, stage→dest failed — the file
    // sitting only as a .bak must still come back.
    let err = persist_tx_hooks(&dir, &files, Some(FailPoint::Promote(0))).unwrap_err();
    assert_eq!(err.phase, Phase::Commit);
    assert_eq!(bytes(&a), b"fn a() -> i32 { return 1; }\n");
    assert_eq!(bytes(&b), b"fn b() -> i32 { return 2; }\n");
    assert_clean(&dir);
}

// ---------- crash recovery ----------

/// Dying after staging: journal + stages survive, no live file
/// moved. Recovery rolls back cleanly.
#[test]
fn staged_but_never_swapped_recovers_as_rollback() {
    let dir = ws_dir("stagedie");
    let (a, b, files) = tx(&dir);
    let err = persist_tx_hooks(&dir, &files, Some(FailPoint::StageDie(1))).unwrap_err();
    assert_eq!(err.phase, Phase::RecoveryRequired);
    let journal = err.journal.clone().expect("journal survives");
    assert!(journal.exists());

    // Live files untouched; artifacts present.
    assert_eq!(bytes(&a), b"fn a() -> i32 { return 1; }\n");
    assert_eq!(bytes(&b), b"fn b() -> i32 { return 2; }\n");

    let out = recover(&dir);
    assert_eq!(out.len(), 1);
    match &out[0] {
        RecoveryOutcome::RolledBack { files, .. } => assert_eq!(*files, 2),
        other => panic!("expected RolledBack, got {other:?}"),
    }
    assert_eq!(bytes(&a), b"fn a() -> i32 { return 1; }\n");
    assert_clean(&dir);
}

/// A real child process dies mid-commit; the parent recovers the
/// half-swapped transaction forward — both files hold `after`.
#[test]
fn killed_mid_commit_recovers_forward() {
    let dir = ws_dir("killfwd");
    let (a, b, _files) = tx(&dir);

    // Spawn a copy of this test binary that runs the die-persist
    // and exits mid-commit without rollback.
    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(exe)
        .args(["kill_child_entry", "--exact", "--nocapture"])
        .env("ONTIXA_TX_CHILD", "1")
        .env("ONTIXA_TX_DIR", &dir)
        .status()
        .expect("spawn child");
    assert!(!status.success(), "child must die");

    // After the kill: file 0 already swapped (after), file 1 still
    // before with its stage + journal on disk.
    assert_eq!(bytes(&a), b"fn aa() -> i32 { return 1; }\n");
    assert_eq!(bytes(&b), b"fn b() -> i32 { return 2; }\n");

    let out = recover(&dir);
    match out.as_slice() {
        [RecoveryOutcome::Committed { files, .. }] => assert_eq!(*files, 2),
        other => panic!("expected Committed, got {other:?}"),
    }
    assert_eq!(bytes(&a), b"fn aa() -> i32 { return 1; }\n");
    assert_eq!(bytes(&b), b"fn bb() -> i32 { return 2; }\n");
    assert_clean(&dir);

    // Recovery is idempotent — a second run finds nothing.
    assert_eq!(recover(&dir), vec![RecoveryOutcome::Clean]);
}

/// Child-process entry: performs the die-persist then exits
/// non-zero, exactly like a killed worker would.
#[test]
fn kill_child_entry() {
    if std::env::var("ONTIXA_TX_CHILD").is_err() {
        // Parent run — the real assertions live in
        // `killed_mid_commit_recovers_forward`.
        return;
    }
    let dir = PathBuf::from(std::env::var("ONTIXA_TX_DIR").expect("tx dir"));
    let (_a, _b, files) = tx(&dir);
    let _ = persist_tx_hooks(&dir, &files, Some(FailPoint::Die(0)));
    std::process::exit(2);
}

/// Recovery must never guess: a file whose bytes match neither
/// `before` nor `after` is a conflict — journal and backups stay.
#[test]
fn recovery_conflicting_bytes_preserved() {
    let dir = ws_dir("conflict");
    let (a, b, files) = tx(&dir);
    let err = persist_tx_hooks(&dir, &files, Some(FailPoint::Die(0))).unwrap_err();
    let journal = err.journal.clone().unwrap();
    assert!(journal.exists());

    // Someone else edits the still-pending file.
    std::fs::write(&b, "fn b() -> i32 { return 2; } // foreign\n").unwrap();
    let out = recover(&dir);
    match out.as_slice() {
        [RecoveryOutcome::Conflict { path, .. }] => assert_eq!(*path, b),
        other => panic!("expected Conflict, got {other:?}"),
    }
    // Evidence stays: journal, the foreign bytes, and the backup
    // of the swapped file.
    assert!(journal.exists());
    assert_eq!(bytes(&b), b"fn b() -> i32 { return 2; } // foreign\n");
    assert_eq!(bytes(&a), b"fn aa() -> i32 { return 1; }\n");
}

// ---------- content fidelity ----------

#[test]
fn unicode_and_spaced_paths_roundtrip() {
    let dir = ws_dir("unicode");
    let sub = dir.join("thư mục có dấu");
    std::fs::create_dir_all(&sub).unwrap();
    let p = sub.join("tệp nguồn.ixa");
    std::fs::write(&p, "fn café() -> i32 { return 1; }\n").unwrap();
    let files = vec![TxFile {
        path: p.clone(),
        before: "fn café() -> i32 { return 1; }\n".as_bytes().to_vec(),
        after: "fn thé() -> i32 { return 1; }\n".as_bytes().to_vec(),
    }];
    persist_tx(&dir, &files).expect("persist");
    assert_eq!(bytes(&p), "fn thé() -> i32 { return 1; }\n".as_bytes());
    assert_clean(&dir);
}

#[test]
fn crlf_lf_and_multibyte_preserved_exactly() {
    let dir = ws_dir("eol");
    let p = dir.join("mixed.ixa");
    let before = b"fn a() -> i32 {\r\n  return 1; // caf\xc3\xa9\n}\r\n".to_vec();
    let after = b"fn ab() -> i32 {\r\n  return 1; // caf\xc3\xa9\n}\r\n".to_vec();
    std::fs::write(&p, &before).unwrap();
    let files = vec![TxFile {
        path: p.clone(),
        before: before.clone(),
        after: after.clone(),
    }];
    persist_tx(&dir, &files).expect("persist");
    assert_eq!(bytes(&p), after);
    assert_clean(&dir);
}
