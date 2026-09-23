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

// ======================================================================
// Wider op vocabulary (ADR-0016 addendum): signature, `use`, and
// `data`-field ops on the same transaction pipeline.
// ======================================================================

const SIG: &str = "fn f(mut x: i32) -> i32 { x = x + 1; return x; }\n\
                   fn g(x: i32) -> i32 { return x * 2; }\n\
                   fn main() -> i32 { return f(1); }\n";

const DATA_MAIN: &str = "data P { x: i32; y: i64; }\n\
     fn main() -> i32 { let mut p = P { x: 1, y: 2 }; p.y = 3; return p.x; }\n";

// ---------- signature ops ----------

#[test]
fn rename_param_rewrites_decl_and_body_sites() {
    let mut db = ws(&[("main", SIG)]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::RenameParam {
                symbol: "f".into(),
                param: "x".into(),
                to: "acc".into(),
            }],
        )
        .unwrap();
    assert_eq!(plan.ops(), &["rename_param f x → acc"]);
    let text = new_text(&plan, 0);
    assert!(text.contains("fn f(mut acc: i32)"), "{text}");
    assert!(text.contains("acc = acc + 1;"), "{text}");
    assert!(text.contains("return acc;"), "{text}");
    // Decl token, assign base, the Var in `x + 1`, the Var in
    // `return x` — the param's four binding sites.
    assert_eq!(plan.edits().len(), 4);
    // The fn's name, other params, and callers are untouched.
    assert!(text.contains("fn f("), "{text}");
    assert!(text.contains("return f(1);"), "{text}");
    // Preview never mutates.
    assert!(!db.source(0).contains("acc"));
}

#[test]
fn rename_param_rejects_conflicts_unknowns_and_bad_shapes() {
    let mut db = ws(&[(
        "main",
        "fn f(x: i32) -> i32 { let y = x + 1; return y; }\ndata D { a: i32; }\n",
    )]);
    // `to` already bound as a `let` — rewriting the param's sites
    // would silently capture them.
    match db.plan_patch(
        0,
        &[PatchOp::RenameParam {
            symbol: "f".into(),
            param: "x".into(),
            to: "y".into(),
        }],
    ) {
        Err(PatchError::Conflict(d)) => assert_eq!(d.code, Code::NameConflict),
        other => panic!("expected Conflict, got {other:?}"),
    }
    // No such parameter.
    match db.plan_patch(
        0,
        &[PatchOp::RenameParam {
            symbol: "f".into(),
            param: "nope".into(),
            to: "n".into(),
        }],
    ) {
        Err(PatchError::UnknownSymbol(d)) => assert_eq!(d.code, Code::UnknownSymbol),
        other => panic!("expected UnknownSymbol, got {other:?}"),
    }
    // A `data` def is the wrong kind of target.
    match db.plan_patch(
        0,
        &[PatchOp::RenameParam {
            symbol: "D".into(),
            param: "a".into(),
            to: "b".into(),
        }],
    ) {
        Err(PatchError::Unsupported(d)) => assert_eq!(d.code, Code::UnsupportedTarget),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    // `to` must be a bare identifier.
    match db.plan_patch(
        0,
        &[PatchOp::RenameParam {
            symbol: "f".into(),
            param: "x".into(),
            to: "not a name".into(),
        }],
    ) {
        Err(PatchError::Malformed(d)) => assert_eq!(d.code, Code::MalformedPatch),
        other => panic!("expected Malformed, got {other:?}"),
    }
}

#[test]
fn signature_retypes_compose_in_one_patch() {
    let mut db = ws(&[("main", SIG)]);
    // `g`: `x` and the `->` annotation both go i32 → i64 — the body
    // (`x * 2` adopts i64 through the param) still checks; `g` is
    // never called so no caller breaks.
    let plan = db
        .plan_patch(
            0,
            &[
                PatchOp::SetParamType {
                    symbol: "g".into(),
                    param: "x".into(),
                    ty: "i64".into(),
                },
                PatchOp::SetRetType {
                    symbol: "g".into(),
                    ty: Some("i64".into()),
                },
            ],
        )
        .unwrap();
    let text = new_text(&plan, 0);
    assert!(text.contains("fn g(x: i64) -> i64"), "{text}");
    assert!(text.contains("fn f(mut x: i32) -> i32"), "{text}");
    assert!(text.contains("return f(1);"), "{text}");
}

#[test]
fn a_half_retyped_signature_fails_the_shadow_compile() {
    let mut db = ws(&[("main", SIG)]);
    let before = sources(&db, 1);
    // Retype `x` without the return type: `x * 2` becomes `i64`
    // against a declared `i32` return — the shadow compile rejects.
    let err = db
        .plan_patch(
            0,
            &[PatchOp::SetParamType {
                symbol: "g".into(),
                param: "x".into(),
                ty: "i64".into(),
            }],
        )
        .unwrap_err();
    let codes: Vec<_> = err.diagnostics().iter().map(|d| d.code).collect();
    assert!(codes.contains(&Code::PatchRejected), "{codes:?}");
    assert_eq!(sources(&db, 1), before);
}

#[test]
fn set_ret_type_adds_replaces_and_removes() {
    let mut db = ws(&[(
        "main",
        "fn f() { }\nfn g() -> unit { }\nfn h() -> i32 { return 5; }\n",
    )]);
    let plan = db
        .plan_patch(
            0,
            &[
                // Annotate a previously unit-typed fn.
                PatchOp::SetRetType {
                    symbol: "f".into(),
                    ty: Some("unit".into()),
                },
                // Drop `g`'s annotation — `unit` written or absent
                // is the same TypeRef.
                PatchOp::SetRetType {
                    symbol: "g".into(),
                    ty: None,
                },
                // Replace: the literal `5` adopts `i64` under the
                // new annotation (expected types flow inward).
                PatchOp::SetRetType {
                    symbol: "h".into(),
                    ty: Some("i64".into()),
                },
            ],
        )
        .unwrap();
    let text = new_text(&plan, 0);
    assert!(text.contains("fn f() -> unit { }"), "{text}");
    assert!(text.contains("fn g() { }"), "{text}");
    assert!(text.contains("fn h() -> i64"), "{text}");
}

#[test]
fn set_ret_type_rejections() {
    // The body no longer matches.
    let mut db = ws(&[("main", "fn h() -> str { return \"s\"; }\n")]);
    match db.plan_patch(
        0,
        &[PatchOp::SetRetType {
            symbol: "h".into(),
            ty: Some("i32".into()),
        }],
    ) {
        Err(PatchError::Rejected(d, _)) => assert_eq!(d.code, Code::PatchRejected),
        other => panic!("expected Rejected, got {other:?}"),
    }
    // Removing the annotation leaves `return 1` checking against
    // `unit`.
    let mut db = ws(&[("main", "fn h() -> i32 { return 1; }\n")]);
    match db.plan_patch(
        0,
        &[PatchOp::SetRetType {
            symbol: "h".into(),
            ty: None,
        }],
    ) {
        Err(PatchError::Rejected(d, _)) => assert_eq!(d.code, Code::PatchRejected),
        other => panic!("expected Rejected, got {other:?}"),
    }
    // Nothing to remove.
    let mut db = ws(&[("main", "fn h() { }\n")]);
    match db.plan_patch(
        0,
        &[PatchOp::SetRetType {
            symbol: "h".into(),
            ty: None,
        }],
    ) {
        Err(PatchError::Unsupported(d)) => assert_eq!(d.code, Code::UnsupportedTarget),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    // A `ty` that is not a type never reaches the workspace.
    let mut db = ws(&[("main", "fn h() { }\n")]);
    match db.plan_patch(
        0,
        &[PatchOp::SetRetType {
            symbol: "h".into(),
            ty: Some("i32 junk".into()),
        }],
    ) {
        Err(PatchError::Malformed(d)) => assert_eq!(d.code, Code::MalformedPatch),
        other => panic!("expected Malformed, got {other:?}"),
    }
}

// ---------- `use` ops ----------

#[test]
fn add_use_lands_after_the_use_block_and_reaches_new_modules() {
    // `other` is registered but unreachable — the added `use` pulls
    // it into the workspace's scope, so the body can call through it.
    let mut db = ws(&[
        ("main", MAIN),
        ("dep", DEP),
        ("other", "fn zero() -> i32 { return 0; }\n"),
    ]);
    let plan = db
        .plan_patch(
            0,
            &[
                PatchOp::AddUse {
                    module: None,
                    path: "other".into(),
                    alias: None,
                },
                PatchOp::ReplaceBody {
                    symbol: "main".into(),
                    body: "{ return dep::double(21) + other::zero(); }".into(),
                },
            ],
        )
        .unwrap();
    let text = new_text(&plan, 0);
    // After the last `use`, on its own line.
    assert!(text.starts_with("use dep;\nuse other;\n"), "{text}");
    assert!(text.contains("other::zero()"), "{text}");
}

#[test]
fn add_use_without_a_use_block_stays_below_the_comment_banner() {
    let mut db = ws(&[
        ("main", "// banner\nfn main() -> i32 { return 0; }\n"),
        ("dep", DEP),
    ]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::AddUse {
                module: None,
                path: "dep::double".into(),
                alias: Some("twice".into()),
            }],
        )
        .unwrap();
    assert_eq!(
        new_text(&plan, 0),
        "// banner\nuse dep::double as twice;\nfn main() -> i32 { return 0; }\n"
    );
}

#[test]
fn remove_use_drops_the_decl_and_its_newline() {
    let mut db = ws(&[
        (
            "main",
            "use dep;\nuse other;\nfn main() -> i32 { return dep::double(1); }\n",
        ),
        ("dep", DEP),
        ("other", "fn zero() -> i32 { return 0; }\n"),
    ]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::RemoveUse {
                module: None,
                path: "other".into(),
                alias: None,
            }],
        )
        .unwrap();
    assert_eq!(
        new_text(&plan, 0),
        "use dep;\nfn main() -> i32 { return dep::double(1); }\n"
    );
}

#[test]
fn remove_use_rejections() {
    // A reference through the removed decl dangles → shadow reject.
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let before = sources(&db, 2);
    match db.plan_patch(
        0,
        &[PatchOp::RemoveUse {
            module: None,
            path: "dep".into(),
            alias: None,
        }],
    ) {
        Err(PatchError::Rejected(d, _)) => assert_eq!(d.code, Code::PatchRejected),
        other => panic!("expected Rejected, got {other:?}"),
    }
    assert_eq!(sources(&db, 2), before);

    // `alias` must match exactly — an unaliased `remove_use` does
    // not hit `use dep::double as d2;`.
    let mut db = ws(&[
        (
            "main",
            "use dep::double as d2;\nfn main() -> i32 { return d2(1); }\n",
        ),
        ("dep", DEP),
    ]);
    match db.plan_patch(
        0,
        &[PatchOp::RemoveUse {
            module: None,
            path: "dep::double".into(),
            alias: None,
        }],
    ) {
        Err(PatchError::UnknownSymbol(d)) => assert_eq!(d.code, Code::UnknownSymbol),
        other => panic!("expected UnknownSymbol, got {other:?}"),
    }
    // With the alias it resolves — and removing it breaks `d2(1)`.
    match db.plan_patch(
        0,
        &[PatchOp::RemoveUse {
            module: None,
            path: "dep::double".into(),
            alias: Some("d2".into()),
        }],
    ) {
        Err(PatchError::Rejected(d, _)) => assert_eq!(d.code, Code::PatchRejected),
        other => panic!("expected Rejected, got {other:?}"),
    }

    // An unreachable module is not a patch target.
    let mut db = ws(&[
        ("main", "fn main() -> i32 { return 0; }\n"),
        ("dep", DEP),
        ("other", "fn x() -> i32 { return 0; }\n"),
    ]);
    match db.plan_patch(
        0,
        &[PatchOp::AddUse {
            module: Some("other".into()),
            path: "dep".into(),
            alias: None,
        }],
    ) {
        Err(PatchError::UnknownModule(d)) => assert_eq!(d.code, Code::UnknownModule),
        other => panic!("expected UnknownModule, got {other:?}"),
    }
    // `path` never carries an `as` clause.
    match db.plan_patch(
        0,
        &[PatchOp::AddUse {
            module: None,
            path: "dep as d".into(),
            alias: None,
        }],
    ) {
        Err(PatchError::Malformed(d)) => assert_eq!(d.code, Code::MalformedPatch),
        other => panic!("expected Malformed, got {other:?}"),
    }
}

// ---------- `data` field ops ----------

#[test]
fn rename_field_rewrites_every_resolved_site() {
    // Decl in `dep`, literal + assign + access in `main` — all bound
    // to the same field index, all rewritten.
    let mut db = ws(&[
        (
            "main",
            "use dep;\n\
             fn mk() -> dep::P { return dep::P { x: 1, y: 2 }; }\n\
             fn main() -> i32 { let mut p = mk(); p.x = 5; return p.x + p.y; }\n",
        ),
        ("dep", "data P { x: i32; y: i32; }\n"),
    ]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::RenameField {
                symbol: "dep::P".into(),
                field: "x".into(),
                to: "w".into(),
            }],
        )
        .unwrap();
    // Decl site, literal field name, assign projection, access — 4.
    assert_eq!(plan.edits().len(), 4, "{:?}", plan.edits());
    assert!(new_text(&plan, 1).contains("data P { w: i32; y: i32; }"));
    let text = new_text(&plan, 0);
    assert!(text.contains("dep::P { w: 1, y: 2 }"), "{text}");
    assert!(text.contains("p.w = 5"), "{text}");
    assert!(text.contains("p.w + p.y"), "{text}");
    // The sibling field is untouched.
    assert!(text.contains("y: 2"));
}

#[test]
fn rename_field_rejections() {
    let mut db = ws(&[(
        "main",
        "data P { x: i32; }\nfn f(p: P) -> i32 { return p.x; }\n",
    )]);
    // Field ops refuse `fn` targets.
    match db.plan_patch(
        0,
        &[PatchOp::RenameField {
            symbol: "f".into(),
            field: "x".into(),
            to: "w".into(),
        }],
    ) {
        Err(PatchError::Unsupported(d)) => assert_eq!(d.code, Code::UnsupportedTarget),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    // Unknown field → E_UNKNOWN_FIELD.
    match db.plan_patch(
        0,
        &[PatchOp::RenameField {
            symbol: "P".into(),
            field: "nope".into(),
            to: "w".into(),
        }],
    ) {
        Err(PatchError::UnknownSymbol(d)) => assert_eq!(d.code, Code::UnknownField),
        other => panic!("expected UnknownSymbol(E_UNKNOWN_FIELD), got {other:?}"),
    }
}

#[test]
fn add_field_needs_the_literal_fixed_in_the_same_patch() {
    let mut db = ws(&[("main", DATA_MAIN)]);
    // Alone: `P { x: 1, y: 2 }` would be missing `z` → shadow reject.
    match db.plan_patch(
        0,
        &[PatchOp::AddField {
            symbol: "P".into(),
            field: "z".into(),
            ty: "i32".into(),
        }],
    ) {
        Err(PatchError::Rejected(d, _)) => assert_eq!(d.code, Code::PatchRejected),
        other => panic!("expected Rejected, got {other:?}"),
    }
    // Paired with a body that initializes `z`, it applies — the
    // separator copies the file's own `; ` convention.
    let plan = db
        .plan_patch(
            0,
            &[
                PatchOp::AddField {
                    symbol: "P".into(),
                    field: "z".into(),
                    ty: "i32".into(),
                },
                PatchOp::ReplaceBody {
                    symbol: "main".into(),
                    body: "{ let mut p = P { x: 1, y: 2, z: 0 }; p.y = 3; return p.x + p.z; }"
                        .into(),
                },
            ],
        )
        .unwrap();
    let text = new_text(&plan, 0);
    assert!(
        text.contains("data P { x: i32; y: i64; z: i32; }"),
        "{text}"
    );
    assert!(text.contains("z: 0"), "{text}");
}

#[test]
fn add_field_on_unconstructed_and_empty_data() {
    // No literal constructs `P` — the field just lands.
    let mut db = ws(&[(
        "main",
        "data P { x: i32; }\nfn main() -> i32 { return 0; }\n",
    )]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::AddField {
                symbol: "P".into(),
                field: "y".into(),
                ty: "str".into(),
            }],
        )
        .unwrap();
    assert!(new_text(&plan, 0).contains("data P { x: i32; y: str; }"));

    // An empty `data` takes its first field after the `{`.
    let mut db = ws(&[("main", "data E {}\nfn main() -> i32 { return 0; }\n")]);
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::AddField {
                symbol: "E".into(),
                field: "a".into(),
                ty: "i32".into(),
            }],
        )
        .unwrap();
    assert!(new_text(&plan, 0).contains("data E { a: i32;}"));
}

#[test]
fn remove_field_takes_the_leading_seam_and_refuses_comments() {
    // `dead`'s span covers its leading whitespace — removal leaves
    // `P { x: 1, y: 2 }` literals dangling until the body is fixed
    // in the same patch.
    let mut db = ws(&[(
        "main",
        "data P { x: i32; dead: i64; y: i32; }\n\
         fn main() -> i32 { let p = P { x: 1, dead: 0, y: 2 }; return p.x + p.y; }\n",
    )]);
    match db.plan_patch(
        0,
        &[PatchOp::RemoveField {
            symbol: "P".into(),
            field: "dead".into(),
        }],
    ) {
        Err(PatchError::Rejected(d, _)) => assert_eq!(d.code, Code::PatchRejected),
        other => panic!("expected Rejected, got {other:?}"),
    }
    let plan = db
        .plan_patch(
            0,
            &[
                PatchOp::RemoveField {
                    symbol: "P".into(),
                    field: "dead".into(),
                },
                PatchOp::ReplaceBody {
                    symbol: "main".into(),
                    body: "{ let p = P { x: 1, y: 2 }; return p.x + p.y; }".into(),
                },
            ],
        )
        .unwrap();
    let text = new_text(&plan, 0);
    assert!(text.contains("data P { x: i32; y: i32; }"), "{text}");
    assert!(text.contains("P { x: 1, y: 2 }"), "{text}");

    // A comment in the removed field's leading seam refuses — its
    // attachment is ambiguous; the patch would silently drop it.
    let mut db = ws(&[(
        "main",
        "data P { x: i32; /* keep me */ dead: i64; }\nfn main() -> i32 { return 0; }\n",
    )]);
    match db.plan_patch(
        0,
        &[PatchOp::RemoveField {
            symbol: "P".into(),
            field: "dead".into(),
        }],
    ) {
        Err(PatchError::Unsupported(d)) => assert_eq!(d.code, Code::UnsupportedTarget),
        other => panic!("expected Unsupported, got {other:?}"),
    }
    assert!(db.source(0).contains("keep me"));
}

#[test]
fn set_field_type_retypes_through_the_shadow_compile() {
    // `w: str` never type-checks against `p.w = 3`.
    let mut db = ws(&[("main", DATA_MAIN)]);
    match db.plan_patch(
        0,
        &[PatchOp::SetFieldType {
            symbol: "P".into(),
            field: "y".into(),
            ty: "str".into(),
        }],
    ) {
        Err(PatchError::Rejected(d, _)) => assert_eq!(d.code, Code::PatchRejected),
        other => panic!("expected Rejected, got {other:?}"),
    }
    // `y: i64 → i32` — the literal `2` and assign `3` adopt `i32`.
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::SetFieldType {
                symbol: "P".into(),
                field: "y".into(),
                ty: "i32".into(),
            }],
        )
        .unwrap();
    assert!(new_text(&plan, 0).contains("data P { x: i32; y: i32; }"));
}

// ---------- cross-cutting guarantees ----------

#[test]
fn member_ops_overlap_rules() {
    let mut db = ws(&[("main", SIG)]);
    // rename_param's body sites sit inside replace_body's block —
    // the pair overlaps.
    match db.plan_patch(
        0,
        &[
            PatchOp::ReplaceBody {
                symbol: "f".into(),
                body: "{ return x; }".into(),
            },
            PatchOp::RenameParam {
                symbol: "f".into(),
                param: "x".into(),
                to: "acc".into(),
            },
        ],
    ) {
        Err(PatchError::Malformed(d)) => assert_eq!(d.code, Code::MalformedPatch),
        other => panic!("expected Malformed(overlap), got {other:?}"),
    }
    // Disjoint member edits on the same item compose: the param's
    // name token and its type span do not overlap.
    let plan = db
        .plan_patch(
            0,
            &[
                PatchOp::RenameParam {
                    symbol: "g".into(),
                    param: "x".into(),
                    to: "v".into(),
                },
                PatchOp::SetParamType {
                    symbol: "g".into(),
                    param: "x".into(),
                    ty: "i64".into(),
                },
                PatchOp::SetRetType {
                    symbol: "g".into(),
                    ty: Some("i64".into()),
                },
            ],
        )
        .unwrap();
    assert!(new_text(&plan, 0).contains("fn g(v: i64) -> i64"));
}

#[test]
fn wider_ops_apply_atomically_and_run() {
    let mut db = ws(&[("main", SIG)]);
    let plan = db
        .plan_patch(
            0,
            &[
                PatchOp::RenameParam {
                    symbol: "f".into(),
                    param: "x".into(),
                    to: "acc".into(),
                },
                PatchOp::SetParamType {
                    symbol: "f".into(),
                    param: "x".into(),
                    ty: "i64".into(),
                },
                PatchOp::SetRetType {
                    symbol: "f".into(),
                    ty: Some("i64".into()),
                },
                PatchOp::SetRetType {
                    symbol: "main".into(),
                    ty: Some("i64".into()),
                },
            ],
        )
        .unwrap();
    let report = db.apply_patch(&plan).unwrap();
    assert_eq!(report.edits, plan.edits().len());
    assert!(!report.diags.has_errors());
    assert!(db.source(0).contains("fn f(mut acc: i64) -> i64"));
    assert!(db.source(0).contains("fn main() -> i64"));
    // The patched workspace runs: f(1) = 1 + 1 = 2.
    let a = db.compile_owned(0);
    let interp = ontixa_interpreter::Interp::new(&a.mir, &a.module, &a.interner);
    assert_eq!(interp.show(&interp.run("main").unwrap()), "2");
}

#[test]
fn wider_ops_carry_the_same_bounds_and_guards() {
    // The 64-op bound applies to every op kind.
    let mut db = ws(&[("main", SIG)]);
    let ops: Vec<PatchOp> = (0..65)
        .map(|_| PatchOp::RemoveDef {
            symbol: "nope".into(),
        })
        .collect();
    match db.plan_patch(0, &ops) {
        Err(PatchError::Malformed(d)) => assert_eq!(d.code, Code::MalformedPatch),
        other => panic!("expected Malformed(>64 ops), got {other:?}"),
    }
    // The 256 KiB payload bound covers `ty`/`body`-style fields too.
    let big = "x".repeat(256 * 1024 + 1);
    match db.plan_patch(
        0,
        &[PatchOp::SetFieldType {
            symbol: "P".into(),
            field: "x".into(),
            ty: big,
        }],
    ) {
        Err(PatchError::Malformed(d)) => assert_eq!(d.code, Code::MalformedPatch),
        other => panic!("expected Malformed(>256KiB), got {other:?}"),
    }
    // Stale-revision guard on a new-vocabulary plan.
    let plan = db
        .plan_patch(
            0,
            &[PatchOp::RenameParam {
                symbol: "f".into(),
                param: "x".into(),
                to: "acc".into(),
            }],
        )
        .unwrap();
    db.set_source(0, SIG);
    match db.apply_patch(&plan) {
        Err(PatchError::Stale { .. }) => {}
        other => panic!("expected Stale, got {other:?}"),
    }
}
