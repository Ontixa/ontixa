//! Structured-patch transaction tests: op resolution, spec
//! validation, shadow-compile rejection, stale/provenance guards,
//! and the atomic-apply contract — the rename pipeline generalized
//! to a bounded edit set (`replace_body`, `remove_def`, `add_def`).

use ontixa_db::{Db, PatchError, PatchOp, PatchPlan};
use ontixa_diagnostics::Code;
use ontixa_source::FileId;

/// Registers `sources` as `(module, text)` pairs; returns the Db.
/// File 0 is the workspace root by convention.
fn ws(sources: &[(&str, &str)]) -> Db {
    let mut db = Db::new();
    for (module, text) in sources {
        db.add_source_named(*module, *text);
    }
    db
}

/// Splice the plan's `new_sources` — the text apply would install.
fn new_text(plan: &PatchPlan, file: usize) -> &str {
    &plan
        .new_sources()
        .iter()
        .find(|(f, _)| *f == file)
        .expect("file has new source")
        .1
}

/// Every source text, for the nothing-mutated assertions.
fn sources(db: &Db, n: usize) -> Vec<String> {
    (0..n).map(|f| db.source(f).to_string()).collect()
}

const DEP: &str = "fn double(x: i32) -> i32 { return x * 2; }\n\
                   fn norm(x: i32) -> i32 { return x; }\n";

const MAIN: &str = "use dep;\n\
                    fn main() -> i32 { return dep::double(21); }\n\
                    fn local() -> i32 { return 1; }\n";

// ---------- preview ----------

#[test]
fn replace_body_preview_splices_only_the_block() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::ReplaceBody {
                symbol: "main::local".into(),
                body: "{ return 7; }".into(),
            }],
        )
        .unwrap();
    assert_eq!(plan.ops(), &["replace_body main::local"]);
    assert_eq!(plan.edits().len(), 1);
    let e = &plan.edits()[0];
    assert_eq!(e.file, FileId::new(0));
    // The edit covers exactly the `{ ... }` block — the signature
    // and `main`'s call site are untouched.
    assert_eq!(
        &db.source(0)[e.span.start as usize..e.span.end as usize],
        "{ return 1; }"
    );
    assert_eq!(e.replace, "{ return 7; }");
    assert!(new_text(&plan, 0).contains("fn local() -> i32 { return 7; }"));
    assert!(new_text(&plan, 0).contains("dep::double(21)"));
    // Preview never mutates.
    assert!(!db.source(0).contains("return 7"));
}

#[test]
fn remove_def_swallows_the_item_and_its_newline() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::RemoveDef {
                symbol: "dep::norm".into(),
            }],
        )
        .unwrap();
    assert_eq!(plan.edits().len(), 1);
    let text = new_text(&plan, 1);
    assert!(!text.contains("norm"), "{text}");
    // No stray blank line left behind.
    assert_eq!(text, "fn double(x: i32) -> i32 { return x * 2; }\n");
}

#[test]
fn add_def_appends_to_the_module_file() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::AddDef {
                module: Some("dep".into()),
                text: "fn triple(x: i32) -> i32 { return x * 3; }".into(),
            }],
        )
        .unwrap();
    assert_eq!(plan.ops(), &["add_def triple → dep"]);
    let e = &plan.edits()[0];
    // Insertion at the module file's EOF.
    assert_eq!(e.file, FileId::new(1));
    assert_eq!(e.span.start as usize, DEP.len());
    assert_eq!(e.span.start, e.span.end);
    let text = new_text(&plan, 1);
    assert!(text.ends_with("fn triple(x: i32) -> i32 { return x * 3; }\n"));
    assert!(text.starts_with("fn double"));
}

#[test]
fn add_def_without_module_targets_the_root() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::AddDef {
                module: None,
                text: "fn added() -> i32 { return 9; }".into(),
            }],
        )
        .unwrap();
    assert_eq!(plan.edits()[0].file, FileId::new(0));
    assert!(new_text(&plan, 0).ends_with("fn added() -> i32 { return 9; }\n"));
}

#[test]
fn multi_op_patch_orders_same_point_inserts_by_spec() {
    let mut db = ws(&[("main", "fn main() -> i32 { return 0; }\n")]);
    let plan = db
        .plan_patch(
            0,
            &[
                PatchOp::AddDef {
                    module: None,
                    text: "fn first() -> i32 { return 1; }".into(),
                },
                PatchOp::AddDef {
                    module: None,
                    text: "fn second() -> i32 { return 2; }".into(),
                },
            ],
        )
        .unwrap();
    let text = new_text(&plan, 0);
    // Spec order is file order.
    let first = text.find("fn first").unwrap();
    let second = text.find("fn second").unwrap();
    assert!(first < second, "{text}");
}

// ---------- apply ----------

#[test]
fn apply_is_atomic_and_the_patched_program_runs() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db
        .plan_patch(
            0,
            &[
                PatchOp::ReplaceBody {
                    symbol: "dep::double".into(),
                    body: "{ return x * 3; }".into(),
                },
                PatchOp::AddDef {
                    module: None,
                    text: "fn added() -> i32 { return 1; }".into(),
                },
            ],
        )
        .unwrap();
    let rev = db.revision();
    let report = db.apply_patch(&plan).unwrap();
    assert_eq!(report.revision, rev + 1);
    assert_eq!(report.ops, 2);
    assert_eq!(report.edits, 2);
    assert!(report.files.contains(&0) && report.files.contains(&1));
    assert!(!report.diags.has_errors());
    assert!(db.source(1).contains("return x * 3;"));
    assert!(db.source(0).contains("fn added()"));
    // The patched workspace still compiles and runs: 21 * 3 = 63.
    let a = db.compile_owned(0);
    let interp = ontixa_interpreter::Interp::new(&a.mir, &a.module, &a.interner);
    assert_eq!(interp.show(&interp.run("main").unwrap()), "63");
}

#[test]
fn remove_def_of_a_still_called_fn_is_rejected_atomically() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let before = sources(&db, 2);
    let rev = db.revision();
    let err = db
        .plan_patch(
            0,
            &[PatchOp::RemoveDef {
                symbol: "dep::double".into(),
            }],
        )
        .unwrap_err();
    let codes: Vec<_> = err.diagnostics().iter().map(|d| d.code).collect();
    assert!(codes.contains(&Code::PatchRejected), "{codes:?}");
    // The offending diagnostic rides along — the dangling call is
    // named, not just counted.
    assert!(
        err.diagnostics()
            .iter()
            .skip(1)
            .any(|d| d.severity == ontixa_diagnostics::Severity::Error),
        "{:?}",
        err.diagnostics()
    );
    // Nothing applied: byte-identical sources, same revision.
    assert_eq!(sources(&db, 2), before);
    assert_eq!(db.revision(), rev);
}

#[test]
fn replace_body_with_a_type_error_is_rejected_atomically() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let before = sources(&db, 2);
    let rev = db.revision();
    let err = db
        .plan_patch(
            0,
            &[PatchOp::ReplaceBody {
                symbol: "dep::double".into(),
                body: "{ return \"nope\"; }".into(),
            }],
        )
        .unwrap_err();
    assert!(matches!(err, PatchError::Rejected(..)));
    assert_eq!(sources(&db, 2), before);
    assert_eq!(db.revision(), rev);
}

// ---------- spec validation ----------

#[test]
fn malformed_specs_reject_before_any_workspace_work() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let cases: Vec<PatchOp> = vec![
        // `body` is not a `{ ... }` block.
        PatchOp::ReplaceBody {
            symbol: "dep::double".into(),
            body: "return x * 3;".into(),
        },
        // Empty payload.
        PatchOp::ReplaceBody {
            symbol: "dep::double".into(),
            body: "   ".into(),
        },
    ];
    for op in cases {
        match db.plan_patch(0, &[op]) {
            Err(PatchError::Malformed(d)) => assert_eq!(d.code, Code::MalformedPatch),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }
    // Empty op list.
    assert!(matches!(
        db.plan_patch(0, &[]),
        Err(PatchError::Malformed(_))
    ));
    // add_def payloads: not-parseable, two items, a `use` decl.
    for text in [
        "fn broken( {",
        "fn a() -> i32 { return 1; } fn b() -> i32 { return 2; }",
        "use dep;",
        "fn ok() -> i32 { return 1; } use dep;",
    ] {
        match db.plan_patch(
            0,
            &[PatchOp::AddDef {
                module: None,
                text: text.into(),
            }],
        ) {
            Err(PatchError::Malformed(d)) => assert_eq!(d.code, Code::MalformedPatch),
            other => panic!("expected Malformed for {text:?}, got {other:?}"),
        }
    }
}

#[test]
fn overlapping_ops_reject() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    // Two replace_body ops on one fn cover the same span.
    let err = db
        .plan_patch(
            0,
            &[
                PatchOp::ReplaceBody {
                    symbol: "dep::double".into(),
                    body: "{ return 1; }".into(),
                },
                PatchOp::ReplaceBody {
                    symbol: "dep::double".into(),
                    body: "{ return 2; }".into(),
                },
            ],
        )
        .unwrap_err();
    assert!(matches!(err, PatchError::Malformed(_)));
    // replace_body + remove_def on the same item overlap too.
    let err = db
        .plan_patch(
            0,
            &[
                PatchOp::RemoveDef {
                    symbol: "dep::norm".into(),
                },
                PatchOp::ReplaceBody {
                    symbol: "dep::norm".into(),
                    body: "{ return 0; }".into(),
                },
            ],
        )
        .unwrap_err();
    assert!(matches!(err, PatchError::Malformed(_)));
}

// ---------- semantic resolution rejections ----------

#[test]
fn unknown_ambiguous_and_wrong_kind_reject() {
    let mut db = ws(&[
        ("main", "use dep; use other; fn main() -> i32 { return 0; }"),
        ("dep", DEP),
        ("other", "fn double(x: i32) -> i32 { return x; }"),
    ]);
    assert!(matches!(
        db.plan_patch(
            0,
            &[PatchOp::RemoveDef {
                symbol: "dep::nope".into()
            }]
        ),
        Err(PatchError::UnknownSymbol(_))
    ));
    // `double` exists in both dep and other — bare name is ambiguous.
    assert!(matches!(
        db.plan_patch(
            0,
            &[PatchOp::RemoveDef {
                symbol: "double".into()
            }]
        ),
        Err(PatchError::AmbiguousSymbol(_))
    ));
    // replace_body on a `data` def is the wrong kind.
    let mut db = ws(&[
        ("main", "use dep; fn main() -> i32 { return 0; }"),
        ("dep", "data V { x: i32; }\nfn f() -> i32 { return 0; }\n"),
    ]);
    match db.plan_patch(
        0,
        &[PatchOp::ReplaceBody {
            symbol: "dep::V".into(),
            body: "{ }".into(),
        }],
    ) {
        Err(PatchError::Unsupported(d)) => assert_eq!(d.code, Code::UnsupportedTarget),
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn replace_body_with_a_comment_in_the_seam_is_refused() {
    // The comment between the signature and `{` rides inside the
    // block's syntax-node span — splicing there would drop it, so
    // the site is refused rather than silently losing text.
    let mut db = ws(&[("main", "fn main() -> i32 /* keep me */ { return 0; }\n")]);
    match db.plan_patch(
        0,
        &[PatchOp::ReplaceBody {
            symbol: "main".into(),
            body: "{ return 1; }".into(),
        }],
    ) {
        Err(PatchError::Unsupported(d)) => assert_eq!(d.code, Code::UnsupportedTarget),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    assert!(db.source(0).contains("keep me"));
}

#[test]
fn add_def_to_an_unreachable_module_rejects() {
    // `other` is registered but never `use`d — it is not in the
    // workspace's `use` graph.
    let mut db = ws(&[
        ("main", "use dep; fn main() -> i32 { return 0; }"),
        ("dep", DEP),
        ("other", "fn x() -> i32 { return 0; }"),
    ]);
    match db.plan_patch(
        0,
        &[PatchOp::AddDef {
            module: Some("other".into()),
            text: "fn y() -> i32 { return 0; }".into(),
        }],
    ) {
        Err(PatchError::UnknownModule(d)) => assert_eq!(d.code, Code::UnknownModule),
        other => panic!("expected UnknownModule, got {other:?}"),
    }
}

#[test]
fn dirty_baseline_rejects_the_whole_patch() {
    let mut db = ws(&[
        (
            "main",
            "use dep; fn main() -> i32 { return dep::double(21); }",
        ),
        ("dep", "fn double(x: i32) -> i32 { return nope; }"),
    ]);
    let before = sources(&db, 2);
    match db.plan_patch(
        0,
        &[PatchOp::AddDef {
            module: None,
            text: "fn a() -> i32 { return 1; }".into(),
        }],
    ) {
        Err(PatchError::BaselineErrors(d)) => assert_eq!(d.code, Code::BaselineErrors),
        other => panic!("expected BaselineErrors, got {other:?}"),
    }
    assert_eq!(sources(&db, 2), before);
}

// ---------- apply-time guards ----------

#[test]
fn stale_plan_rejected_without_mutation() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::RemoveDef {
                symbol: "dep::norm".into(),
            }],
        )
        .unwrap();
    // An unrelated edit lands between plan and apply.
    db.set_source(0, "use dep; fn main() -> i32 { return 0; }");
    let before = sources(&db, 2);
    let rev = db.revision();
    match db.apply_patch(&plan) {
        Err(PatchError::Stale { planned, current }) => {
            assert_eq!(planned, plan.revision());
            assert_eq!(current, rev);
        }
        other => panic!("expected Stale, got {other:?}"),
    }
    assert_eq!(sources(&db, 2), before);
    assert_eq!(db.revision(), rev);
}

#[test]
fn a_plan_can_never_cross_database_sessions() {
    let mut db1 = ws(&[("main", MAIN), ("dep", DEP)]);
    let mut db2 = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db1
        .plan_patch(
            0,
            &[PatchOp::RemoveDef {
                symbol: "dep::norm".into(),
            }],
        )
        .unwrap();
    // Same sources, same revision — still the wrong database.
    match db2.apply_patch(&plan) {
        Err(PatchError::PlanMismatch(d)) => assert_eq!(d.code, Code::PlanMismatch),
        other => panic!("expected PlanMismatch, got {other:?}"),
    }
    assert!(db2.source(1).contains("fn norm"));
}
