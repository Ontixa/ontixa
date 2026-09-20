//! Local and parameter rename tests: positional selection through
//! `plan_rename_at`, shadowing semantics, capture rejection via
//! binding correspondence, and the no-mutation guarantee.

use ontixa_db::{Db, RenameError};

fn db(src: &str) -> Db {
    let mut db = Db::new();
    db.add_source_named("main", src);
    db
}

/// Byte offset of the `n`-th occurrence of `needle` (0-based).
fn at(src: &str, needle: &str, n: usize) -> u32 {
    let mut pos = 0;
    for i in 0..=n {
        let found = src[pos..]
            .find(needle)
            .map(|x| pos + x)
            .unwrap_or_else(|| panic!("{needle:?} occurrence {i} not found"));
        if i == n {
            return found as u32;
        }
        pos = found + 1;
    }
    unreachable!()
}

fn src_of(db: &Db) -> &str {
    db.source(0)
}

// ---------- happy paths ----------

#[test]
fn param_rename_covers_decl_and_refs() {
    let src = "fn f(x: i32) -> i32 { return x * x; }\n";
    let mut db = db(src);
    let plan = db.plan_rename_at(0, 0, at(src, "x", 0), "n").unwrap();
    assert_eq!(plan.old_name(), "x");
    assert_eq!(plan.edits().len(), 3, "edits: {:?}", plan.edits());
    db.apply_rename(&plan).unwrap();
    assert_eq!(src_of(&db), "fn f(n: i32) -> i32 { return n * n; }\n");
}

#[test]
fn let_binding_rename() {
    let src = "fn f() -> i32 { let a = 1; let b = a + 1; return b; }\n";
    let mut db = db(src);
    // Select via the *use* site, not the decl — positional either way.
    let plan = db
        .plan_rename_at(0, 0, at(src, "a + 1", 0), "base")
        .unwrap();
    assert_eq!(plan.edits().len(), 2);
    db.apply_rename(&plan).unwrap();
    assert_eq!(
        src_of(&db),
        "fn f() -> i32 { let base = 1; let b = base + 1; return b; }\n"
    );
}

#[test]
fn nested_block_refs_rewrite() {
    let src = "fn f() -> i32 { let x = 1; { let y = x + 1; } return x; }\n";
    let mut db = db(src);
    let plan = db.plan_rename_at(0, 0, at(src, "x", 0), "k").unwrap();
    // decl + `x + 1` in the inner block + `return x`.
    assert_eq!(plan.edits().len(), 3);
    db.apply_rename(&plan).unwrap();
    assert_eq!(
        src_of(&db),
        "fn f() -> i32 { let k = 1; { let y = k + 1; } return k; }\n"
    );
}

#[test]
fn same_name_in_two_fns_stays_separate() {
    let src = "fn f() -> i32 { let v = 1; return v; }\n\
               fn g() -> i32 { let v = 2; return v; }\n";
    let mut db = db(src);
    let plan = db.plan_rename_at(0, 0, at(src, "v", 2), "w").unwrap();
    // Only g's local — f's `v` is a different symbol.
    assert_eq!(plan.edits().len(), 2);
    db.apply_rename(&plan).unwrap();
    assert_eq!(
        src_of(&db),
        "fn f() -> i32 { let v = 1; return v; }\n\
         fn g() -> i32 { let w = 2; return w; }\n"
    );
}

#[test]
fn inner_shadow_selects_inner_only() {
    // `if` supplies the nested scope: `x > 0` and `return x` bind
    // the outer decl, `let x`/`let u = x` inside the branch bind
    // the inner.
    let src = "fn f() -> i32 { let x = 1; if x > 0 { let x = 2; let u = x; } return x; }\n";
    let mut db = db(src);
    // Inner decl is `x` occurrence #2.
    let plan = db.plan_rename_at(0, 0, at(src, "x", 2), "y").unwrap();
    assert_eq!(plan.edits().len(), 2);
    db.apply_rename(&plan).unwrap();
    assert_eq!(
        src_of(&db),
        "fn f() -> i32 { let x = 1; if x > 0 { let y = 2; let u = y; } return x; }\n"
    );
}

#[test]
fn outer_shadow_leaves_inner_alone() {
    // Renaming the OUTER `x`: `let u = x` inside the branch binds
    // the inner decl — it must not rewrite.
    let src = "fn f() -> i32 { let x = 1; if x > 0 { let x = 2; let u = x; } return x; }\n";
    let mut db = db(src);
    let plan = db.plan_rename_at(0, 0, at(src, "x", 0), "y").unwrap();
    // Outer decl + `x > 0` condition + `return x`.
    assert_eq!(plan.edits().len(), 3, "edits: {:?}", plan.edits());
    db.apply_rename(&plan).unwrap();
    assert_eq!(
        src_of(&db),
        "fn f() -> i32 { let y = 1; if y > 0 { let x = 2; let u = x; } return y; }\n"
    );
}

#[test]
fn local_shadows_param_initializer_binds_param() {
    // `let x = x + 1` — the init's `x` resolves *before* the new
    // decl, so it binds the param. Renaming the param rewrites the
    // param name and the init ref, not the let decl or `return x`.
    let src = "fn f(x: i32) -> i32 { let x = x + 1; return x; }\n";
    let mut db = db(src);
    let plan = db.plan_rename_at(0, 0, at(src, "x", 0), "p").unwrap();
    assert_eq!(plan.edits().len(), 2, "edits: {:?}", plan.edits());
    db.apply_rename(&plan).unwrap();
    assert_eq!(
        src_of(&db),
        "fn f(p: i32) -> i32 { let x = p + 1; return x; }\n"
    );
}

#[test]
fn assign_target_and_mutations_rewrite() {
    let src = "fn f() -> i32 { let mut x = 0; x = x + 1; return x; }\n";
    let mut db = db(src);
    let plan = db.plan_rename_at(0, 0, at(src, "x", 0), "n").unwrap();
    // decl + place base + `x + 1` + `return x`.
    assert_eq!(plan.edits().len(), 4, "edits: {:?}", plan.edits());
    db.apply_rename(&plan).unwrap();
    assert_eq!(
        src_of(&db),
        "fn f() -> i32 { let mut n = 0; n = n + 1; return n; }\n"
    );
}

#[test]
fn length_change_edits_cover_tokens_exactly() {
    let src = "fn f() -> i32 { let xx = 1; return xx; }\n";
    let mut db = db(src);
    let plan = db
        .plan_rename_at(0, 0, at(src, "xx", 0), "long_name")
        .unwrap();
    db.apply_rename(&plan).unwrap();
    assert_eq!(
        src_of(&db),
        "fn f() -> i32 { let long_name = 1; return long_name; }\n"
    );
}

#[test]
fn comments_and_strings_never_rewrite() {
    let src = "// x is the count\n\
               fn f() -> i32 { let x = 1; return x; }\n";
    let mut db = db(src);
    let plan = db.plan_rename_at(0, 0, at(src, "x = 1", 0), "n").unwrap();
    db.apply_rename(&plan).unwrap();
    let out = src_of(&db);
    assert!(out.starts_with("// x is the count"), "{out}");
    assert!(out.contains("let n = 1; return n;"));
}

// ---------- capture / drift rejection ----------

#[test]
fn new_name_captured_by_inner_binding_rejected() {
    // Renaming outer `x` → `y` makes `x + y` in the inner scope
    // bind the inner `y` — a silent rebind; must reject.
    let src = "fn f() -> i32 { let x = 1; if 1 > 0 { let y = 2; let u = x + y; } return x; }\n";
    let mut db = db(src);
    let before = src_of(&db).to_string();
    let rev = db.revision();
    match db.plan_rename_at(0, 0, at(src, "x", 0), "y") {
        Err(RenameError::ValidationFailed(d, _)) => {
            assert_eq!(d.code, ontixa_diagnostics::Code::RenameRejected);
            assert!(d.message.contains("rebind"), "{}", d.message);
        }
        other => panic!("expected capture rejection, got {other:?}"),
    }
    assert_eq!(src_of(&db), before);
    assert_eq!(db.revision(), rev);
}

#[test]
fn renamed_decl_captured_by_outer_binding_rejected() {
    // Inner `x` → `y` where `y` is already a parameter: the
    // inner refs would rebind to the parameter `y`.
    let src = "fn f(y: i32) -> i32 { if y > 0 { let x = 1; let u = x + y; } return y; }\n";
    let mut db = db(src);
    let before = src_of(&db).to_string();
    match db.plan_rename_at(0, 0, at(src, "x = 1", 0), "y") {
        Err(RenameError::ValidationFailed(d, _)) => {
            assert_eq!(d.code, ontixa_diagnostics::Code::RenameRejected);
        }
        other => panic!("expected capture rejection, got {other:?}"),
    }
    assert_eq!(src_of(&db), before);
}

#[test]
fn colliding_local_in_same_scope_rejected() {
    // `let x` → `y` while `let y` exists later in the same block:
    // `return x` would rebind to the *other* `y`.
    let src = "fn f() -> i32 { let x = 1; let y = 2; return x; }\n";
    let mut db = db(src);
    match db.plan_rename_at(0, 0, at(src, "x", 0), "y") {
        Err(RenameError::ValidationFailed(_, _)) => {}
        other => panic!("expected rejection, got {other:?}"),
    }
}

#[test]
fn shadowing_rename_that_still_binds_correctly_applies() {
    // Outer `x` → `z` where an inner `y` exists: refs to `x` still
    // bind it — valid, must not be confused with capture.
    let src = "fn f() -> i32 { let x = 1; { let y = 2; } return x; }\n";
    let mut db = db(src);
    let plan = db.plan_rename_at(0, 0, at(src, "x", 0), "z").unwrap();
    db.apply_rename(&plan).unwrap();
    assert_eq!(
        src_of(&db),
        "fn f() -> i32 { let z = 1; { let y = 2; } return z; }\n"
    );
}

// ---------- selection errors ----------

#[test]
fn offset_on_whitespace_is_unknown() {
    let src = "fn f() -> i32 { let x = 1; return x; }\n";
    let mut db = db(src);
    assert!(matches!(
        db.plan_rename_at(0, 0, at(src, ";", 0), "n"),
        Err(RenameError::UnknownSymbol(_))
    ));
}

#[test]
fn offset_on_fn_name_is_unsupported() {
    let src = "fn f() -> i32 { let x = 1; return x; }\n";
    let mut db = db(src);
    match db.plan_rename_at(0, 0, at(src, "f()", 0), "n") {
        Err(RenameError::Unsupported(d)) => {
            assert_eq!(d.code, ontixa_diagnostics::Code::UnsupportedTarget);
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn offset_outside_items_is_unknown() {
    let src = "fn f() -> i32 { return 0; }\n";
    let mut db = db(src);
    assert!(matches!(
        db.plan_rename_at(0, 0, src.len() as u32 + 10, "n"),
        Err(RenameError::UnknownSymbol(_))
    ));
}

#[test]
fn offset_inside_multibyte_char_is_clean_rejection() {
    // `é` is two bytes — an offset inside it must reject cleanly,
    // never panic or mis-select a neighboring token.
    let src = "// café x\nfn f() -> i32 { let x = 1; return x; }\n";
    let mut db = db(src);
    let e_start = src.find('é').unwrap();
    for off in [e_start, e_start + 1] {
        assert!(
            matches!(
                db.plan_rename_at(0, 0, off as u32, "n"),
                Err(RenameError::UnknownSymbol(_))
            ),
            "offset {off} must reject cleanly"
        );
    }
}

// ---------- the def path also verifies correspondence ----------

#[test]
fn def_rename_keeps_binding_correspondence() {
    // A rename whose candidate compiles cleanly AND preserves every
    // binding — the correspondence check passes, sites still resolve
    // to the same def identity.
    let src = "fn helper() -> i32 { return 1; }\n\
               fn main() -> i32 { let h = helper(); return h + helper(); }\n";
    let mut db = db(src);
    let plan = db.plan_rename(0, "helper", "util").unwrap();
    db.apply_rename(&plan).unwrap();
    assert_eq!(
        src_of(&db),
        "fn util() -> i32 { return 1; }\n\
         fn main() -> i32 { let h = util(); return h + util(); }\n"
    );
    // And the renamed program still runs: util+util = 2.
    let a = db.compile_owned(0);
    let interp = ontixa_interpreter::Interp::new(&a.mir, &a.module, &a.interner);
    assert_eq!(interp.show(&interp.run("main").unwrap()), "2");
}
