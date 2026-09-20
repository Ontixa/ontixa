//! Transaction-integrity regressions: baseline policy, plan
//! provenance (session/workspace/fingerprint binding), replay and
//! stale guards, and no-mutation guarantees on every rejection.

use ontixa_db::{Db, RenameError};

/// Registers `sources` as `(module, text)` pairs; returns the Db.
/// File 0 is the workspace root by convention.
fn ws(sources: &[(&str, &str)]) -> Db {
    let mut db = Db::new();
    for (module, text) in sources {
        db.add_source_named(*module, *text);
    }
    db
}

const DEP: &str = "data Vec2 { x: i32; y: i32; }\n\
                   fn double(x: i32) -> i32 { return x * 2; }\n\
                   fn norm(v: Vec2) -> i32 { return v.x * v.x + v.y * v.y; }\n";

const MAIN: &str = "use dep;\n\
                    use dep::Vec2;\n\
                    fn main() -> i32 {\n\
                    \x20   let v = Vec2 { x: 3, y: 4 };\n\
                    \x20   return dep::double(norm2(v));\n\
                    }\n\
                    fn norm2(v: Vec2) -> i32 { return dep::norm(v); }\n";

fn sources(db: &Db, n: usize) -> Vec<String> {
    (0..n).map(|f| db.source(f).to_string()).collect()
}

// ---------- baseline policy ----------

#[test]
fn dirty_baseline_rejected_before_planning() {
    // Pre-existing error in the root: `missing_fn` never resolves.
    // The rename must refuse before scanning — validation compares
    // the candidate against "zero errors", so a dirty baseline
    // makes the check meaningless.
    let mut db = ws(&[
        ("main", "use dep; fn main() -> i32 { return missing_fn(); }"),
        ("dep", DEP),
    ]);
    assert!(db.check(0).diags.has_errors(), "fixture has an error");
    let rev = db.revision();
    let before = sources(&db, 2);
    match db.plan_rename(0, "dep::double", "twice") {
        Err(RenameError::BaselineErrors(d)) => {
            assert_eq!(d.code, ontixa_diagnostics::Code::BaselineErrors);
        }
        other => panic!("expected BaselineErrors, got {other:?}"),
    }
    // No validated plan, no mutation.
    assert_eq!(db.revision(), rev);
    assert_eq!(sources(&db, 2), before);
}

#[test]
fn dirty_baseline_rejected_even_for_valid_rename() {
    // Even an unknown-symbol request reports the baseline policy —
    // the workspace must be clean before rename semantics apply.
    let mut db = ws(&[
        ("main", "fn main() -> i32 { return missing_fn(); }"),
        ("dep", DEP),
    ]);
    assert!(matches!(
        db.plan_rename(0, "nope", "x"),
        Err(RenameError::BaselineErrors(_))
    ));
}

#[test]
fn clean_baseline_still_shadow_checks() {
    // Baseline clean → the shadow-compile catches a rename that
    // would introduce a *new* error: renaming `double` to `norm`
    // collides with `fn norm` — rejected without mutation.
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    assert!(!db.check(0).diags.has_errors());
    let before = sources(&db, 2);
    let rev = db.revision();
    assert!(matches!(
        db.plan_rename(0, "dep::double", "norm"),
        Err(RenameError::Conflict(_))
    ));
    assert_eq!(db.revision(), rev);
    assert_eq!(sources(&db, 2), before);
}

// ---------- plan provenance ----------

#[test]
fn plan_cannot_cross_database_at_same_revision() {
    // Two Dbs, identical sources, identical revision. The plan
    // still cannot cross: it is bound to the producing session.
    let mut a = ws(&[("main", MAIN), ("dep", DEP)]);
    let mut b = ws(&[("main", MAIN), ("dep", DEP)]);
    assert_eq!(a.revision(), b.revision());
    let plan = a.plan_rename(0, "dep::double", "twice").unwrap();
    match b.apply_rename(&plan) {
        Err(RenameError::PlanMismatch(d)) => {
            assert_eq!(d.code, ontixa_diagnostics::Code::PlanMismatch);
        }
        other => panic!("expected PlanMismatch, got {other:?}"),
    }
    // Db B untouched — still holds the old name.
    assert!(b.source(1).contains("fn double"));
    // Db A applies its own plan fine.
    a.apply_rename(&plan).unwrap();
    assert!(a.source(1).contains("fn twice"));
}

#[test]
fn plan_cannot_cross_workspaces() {
    // Same file count, same revision, different module names —
    // provenance binds to the workspace fingerprint too.
    let mut a = ws(&[("main", MAIN), ("dep", DEP)]);
    let mut b = ws(&[
        ("app", "fn main() -> i32 { return 0; }"),
        ("lib", "fn helper() -> i32 { return 1; }"),
    ]);
    assert_eq!(a.revision(), b.revision());
    let plan = a.plan_rename(0, "dep::double", "twice").unwrap();
    assert!(matches!(
        b.apply_rename(&plan),
        Err(RenameError::PlanMismatch(_))
    ));
}

#[test]
fn replayed_plan_rejected() {
    // A plan is single-shot: after apply the revision moved on —
    // replaying the same plan is stale, not a second apply.
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    db.apply_rename(&plan).unwrap();
    let after = sources(&db, 2);
    let rev = db.revision();
    assert!(matches!(
        db.apply_rename(&plan),
        Err(RenameError::Stale { .. })
    ));
    assert_eq!(db.revision(), rev);
    assert_eq!(sources(&db, 2), after);
}

#[test]
fn module_inventory_change_stales_plan() {
    // A file added after planning bumps the revision — the plan's
    // read set changed, so apply is rejected.
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    db.add_source_named("extra", "fn extra() -> i32 { return 0; }");
    assert!(matches!(
        db.apply_rename(&plan),
        Err(RenameError::Stale { .. })
    ));
    assert!(db.source(1).contains("fn double"));
}

#[test]
fn identical_text_edit_stales_plan() {
    // Revision is a write counter, not a content version: even a
    // byte-identical set_source makes the plan stale. Conservative,
    // never wrong — content-equal edits are rare in practice.
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    db.set_source(0, MAIN);
    assert!(matches!(
        db.apply_rename(&plan),
        Err(RenameError::Stale { .. })
    ));
}

// ---------- apply ordering / no partial mutation ----------

#[test]
fn stale_rejection_leaves_everything_untouched() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    db.set_source(0, "use dep; fn main() -> i32 { return 0; }");
    let before = sources(&db, 2);
    let rev = db.revision();
    assert!(matches!(
        db.apply_rename(&plan),
        Err(RenameError::Stale { .. })
    ));
    assert_eq!(sources(&db, 2), before);
    assert_eq!(db.revision(), rev);
    // And the workspace still checks cleanly — no half-state.
    assert!(!db.check(0).diags.has_errors());
}

#[test]
fn applied_plan_reports_post_rename_diagnostics() {
    // The report carries a fresh check of the renamed workspace —
    // consumers see the post-state, not the plan-time snapshot.
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    let report = db.apply_rename(&plan).unwrap();
    assert!(!report.diags.has_errors());
    assert_eq!(report.files.len(), 2);
    assert_eq!(report.revision, db.revision());
    assert!(db.source(0).contains("dep::twice"));
    assert!(db.source(1).contains("fn twice"));
    // Edit spans index the *pre-edit* text: the decl site's span
    // covers `double` in the old source, `twice` afterwards at the
    // same start.
    let decl = plan
        .edits()
        .iter()
        .find(|e| e.file.index() == 1)
        .expect("dep decl edit");
    assert_eq!(decl.span.end - decl.span.start, "double".len() as u32);
    assert_eq!(
        &db.source(1)[decl.span.start as usize..decl.span.start as usize + "twice".len()],
        "twice"
    );
}

#[test]
fn set_sources_rejects_bad_index_before_any_write() {
    // A forged out-of-range file index must not leave a
    // half-installed batch: indices validate before the first
    // write and before the revision bump.
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let rev = db.revision();
    let before = sources(&db, 2);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        db.set_sources(&[(0, "x".to_string()), (99, "y".to_string())]);
    }));
    assert!(r.is_err(), "out-of-range index panics");
    assert_eq!(db.revision(), rev, "revision bumped before panic");
    assert_eq!(sources(&db, 2), before, "file 0 was written before panic");
}
