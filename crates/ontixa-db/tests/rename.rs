//! Rename-transaction tests: plan/preview, alias semantics,
//! validation rejections, stale guard, atomic apply, and the
//! incremental evidence that unchanged defs stay memoized.

use ontixa_db::{Db, QueryKey, RenameError};
use ontixa_source::{DefKey, FileId};

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
fn new_text(plan: &ontixa_db::RenamePlan, file: usize) -> &str {
    &plan
        .new_sources()
        .iter()
        .find(|(f, _)| *f == file)
        .expect("file has new source")
        .1
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

// ---------- preview / scan coverage ----------

#[test]
fn preview_covers_decl_and_qualified_sites() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let dep_src = db.source(1).to_string();
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    assert_eq!(plan.old_name(), "double");
    // Two sites: the decl `fn double` in dep, the `double` segment
    // of `dep::double(...)` in main. `norm`/`Vec2` sites untouched.
    assert_eq!(plan.edits().len(), 2);
    let dep_file = FileId::new(1);
    let decl = plan.edits().iter().find(|e| e.file == dep_file).unwrap();
    assert_eq!(
        &dep_src[decl.span.start as usize..decl.span.end as usize],
        "double"
    );
    assert!(new_text(&plan, 1).contains("fn twice(x: i32)"));
    assert!(new_text(&plan, 1).contains("fn norm(v: Vec2)"));
    assert!(new_text(&plan, 0).contains("dep::twice(norm2(v))"));
    assert!(new_text(&plan, 0).contains("dep::norm(v)"));
}

#[test]
fn unaliased_import_rebinds_references() {
    let mut db = ws(&[
        (
            "main",
            "use dep::double; fn main() -> i32 { return double(3); }",
        ),
        ("dep", DEP),
    ]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    // Three sites: decl, use-path segment, the bare `double` call —
    // `use dep::x` rebinds under the new name.
    assert_eq!(plan.edits().len(), 3);
    assert!(new_text(&plan, 0).contains("use dep::twice;"));
    assert!(new_text(&plan, 0).contains("return twice(3);"));
}

#[test]
fn aliased_import_keeps_local_name() {
    let mut db = ws(&[
        (
            "main",
            "use dep::double as dbl; fn main() -> i32 { return dbl(3); }",
        ),
        ("dep", DEP),
    ]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    // Two sites only: decl + use-path member. The `dbl` alias and
    // every reference through it stay exactly as written.
    assert_eq!(plan.edits().len(), 2);
    assert!(new_text(&plan, 0).contains("use dep::twice as dbl;"));
    assert!(new_text(&plan, 0).contains("return dbl(3);"));
    assert!(!new_text(&plan, 0).contains("twice(3)"));
}

#[test]
fn module_alias_is_transparent() {
    let mut db = ws(&[
        (
            "main",
            "use dep as m; fn main() -> i32 { return m::double(3); }",
        ),
        ("dep", DEP),
    ]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    assert_eq!(plan.edits().len(), 2);
    assert!(new_text(&plan, 0).contains("m::twice(3)"));
}

#[test]
fn data_rename_covers_type_and_literal_sites() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db.plan_rename(0, "dep::Vec2", "Point").unwrap();
    // `Vec2` sites: decl, `use dep::Vec2`, the struct literal,
    // `v: Vec2` param annotation, `-> ...` via `dep::Vec2` — every
    // one resolves to the data def.
    assert!(plan.edits().len() >= 4, "edits: {:?}", plan.edits());
    assert!(new_text(&plan, 1).contains("data Point {"));
    assert!(new_text(&plan, 0).contains("use dep::Point;"));
    assert!(new_text(&plan, 0).contains("Point { x: 3, y: 4 }"));
    assert!(new_text(&plan, 0).contains("fn norm2(v: Point)"));
    assert!(new_text(&plan, 0).contains("-> i32 { return dep::norm(v); }"));
}

#[test]
fn data_rename_covers_array_element_types() {
    // `[Vec2]` — the element path inside a bracketed array type is a
    // rename site exactly like a bare type path.
    let mut db = ws(&[
        (
            "main",
            "use dep::Vec2;\n\
             fn sum(v: [Vec2]) -> i32 { return v.len; }\n\
             fn main() -> i32 { let a: [Vec2] = []; return sum(a); }\n",
        ),
        ("dep", DEP),
    ]);
    let plan = db.plan_rename(0, "dep::Vec2", "Point").unwrap();
    assert!(new_text(&plan, 0).contains("fn sum(v: [Point])"));
    assert!(new_text(&plan, 0).contains("let a: [Point] = []"));
}

// ---------- apply ----------

#[test]
fn apply_is_atomic_and_runs() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    let rev = db.revision();
    let report = db.apply_rename(&plan).unwrap();
    // One revision bump covers every file — no partial mutation.
    assert_eq!(report.revision, rev + 1);
    assert_eq!(report.files.len(), 2);
    assert!(report.diags.is_empty() || !report.diags.has_errors());
    assert!(db.source(0).contains("dep::twice"));
    assert!(db.source(1).contains("fn twice"));
    // The renamed program still runs: twice(norm(3,4)) = 50.
    let a = db.compile_owned(0);
    let interp = ontixa_interpreter::Interp::new(&a.mir, &a.module, &a.interner);
    assert_eq!(interp.show(&interp.run("main").unwrap()), "50");
}

#[test]
fn apply_then_per_def_values_survive_rename() {
    // A rename preserves semantic identity for defs whose item text
    // is byte-identical: calls key on `DefId`, not name — `dep::twice`
    // resolves to the same def `dep::double` did. `norm` (in the
    // edited dep file) and `norm2` (caller-adjacent but untouched)
    // keep every per-def value's `computed_at` stamp. `main`'s own
    // body legitimately differs — `double`→`twice` shifted its
    // item-relative spans.
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    db.compile(0); // warm the whole per-def pipeline
    let key = |db: &Db, file: u32, name: &str| {
        DefKey::new(
            FileId::new(0),
            FileId::new(file),
            db.interner().get(name).unwrap(),
        )
    };
    let stamps: Vec<(QueryKey, u64)> = [("norm2", 0u32), ("norm", 1)]
        .into_iter()
        .flat_map(|(n, f)| {
            [
                QueryKey::HirBody(key(&db, f, n)),
                QueryKey::BodyTypes(key(&db, f, n)),
                QueryKey::MirBody(key(&db, f, n)),
            ]
        })
        .map(|q| (q, db.stamp(q)))
        .collect();
    let main_hir = db.stamp(QueryKey::HirBody(key(&db, 0, "main")));
    assert!(stamps.iter().all(|(_, s)| *s > 0), "queries were warm");

    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    db.apply_rename(&plan).unwrap();
    db.compile(0);

    for (q, before) in stamps {
        assert_eq!(
            db.stamp(q),
            before,
            "{q:?} produced a different value across the rename"
        );
    }
    // `main`'s spans moved — its body recomputed a different value.
    assert_ne!(db.stamp(QueryKey::HirBody(key(&db, 0, "main"))), main_hir);
    // The renamed def is a *new* key — its body was computed fresh.
    let twice = key(&db, 1, "twice");
    assert!(db.stamp(QueryKey::HirBody(twice)) > 0);
}

// ---------- rejections ----------

#[test]
fn conflict_rejected_without_mutation() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let before: Vec<String> = (0..2).map(|f| db.source(f).to_string()).collect();
    let rev = db.revision();
    let err = db.plan_rename(0, "dep::double", "norm").unwrap_err();
    match err {
        RenameError::Conflict(d) => {
            assert_eq!(d.code, ontixa_diagnostics::Code::NameConflict);
            // The clash site is `fn norm` in dep — tagged to its file.
            assert_eq!(d.file, Some(FileId::new(1)));
        }
        other => panic!("expected conflict, got {other:?}"),
    }
    // Nothing changed: sources and revision untouched.
    for (f, text) in before.iter().enumerate() {
        assert_eq!(db.source(f), text);
    }
    assert_eq!(db.revision(), rev);
}

#[test]
fn invalid_names_rejected() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    for bad in ["123abc", "fn", "not a name", "x-y", ""] {
        assert!(
            matches!(
                db.plan_rename(0, "dep::double", bad),
                Err(RenameError::InvalidName(_))
            ),
            "expected InvalidName for {bad:?}"
        );
    }
}

#[test]
fn unknown_and_ambiguous_rejected() {
    // `other` must be reachable for its `double` to compete —
    // unreachable files are not part of the workspace.
    let mut db = ws(&[
        ("main", "use dep; use other; fn main() -> i32 { return 0; }"),
        ("dep", DEP),
        ("other", "fn double(x: i32) -> i32 { return x; }"),
    ]);
    assert!(matches!(
        db.plan_rename(0, "dep::nope", "x"),
        Err(RenameError::UnknownSymbol(_))
    ));
    assert!(matches!(
        db.plan_rename(0, "nope", "x"),
        Err(RenameError::UnknownSymbol(_))
    ));
    // `double` exists in dep and other — bare name is ambiguous.
    assert!(matches!(
        db.plan_rename(0, "double", "twice"),
        Err(RenameError::AmbiguousSymbol(_))
    ));
    // Qualified resolves despite the ambiguity.
    assert!(db.plan_rename(0, "other::double", "single").is_ok());
}

#[test]
fn stale_plan_rejected_without_mutation() {
    let mut db = ws(&[("main", MAIN), ("dep", DEP)]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    // An unrelated edit lands between plan and apply.
    db.set_source(0, "use dep; fn main() -> i32 { return 0; }");
    let before: Vec<String> = (0..2).map(|f| db.source(f).to_string()).collect();
    let rev = db.revision();
    match db.apply_rename(&plan) {
        Err(RenameError::Stale { planned, current }) => {
            assert_eq!(planned, plan.revision());
            assert_eq!(current, rev);
        }
        _ => panic!("expected stale rejection"),
    }
    // No partial mutation: the edit stands, nothing renamed.
    for (f, text) in before.iter().enumerate() {
        assert_eq!(db.source(f), text);
    }
    assert_eq!(db.revision(), rev);
}

// ---------- resolution semantics ----------

#[test]
fn bare_name_resolves_through_root_import() {
    // Root binds `double` via `use dep::double` — renaming bare
    // `double` targets the dep def (the same symbol the root sees).
    let mut db = ws(&[
        (
            "main",
            "use dep::double; fn main() -> i32 { return double(3); }",
        ),
        ("dep", DEP),
    ]);
    let plan = db.plan_rename(0, "double", "twice").unwrap();
    assert!(new_text(&plan, 1).contains("fn twice"));
    assert!(new_text(&plan, 0).contains("use dep::twice;"));
}

#[test]
fn bare_name_finds_unimported_def() {
    // Root doesn't import `helper` — the workspace-wide fallback
    // still resolves the unique match.
    let mut db = ws(&[
        ("main", "fn main() -> i32 { return 0; }"),
        ("dep", "fn helper() -> i32 { return 1; }"),
    ]);
    // `dep` isn't reachable from root (no `use`) — the workspace
    // scope only spans reachable files, so this stays unknown.
    assert!(matches!(
        db.plan_rename(0, "helper", "h"),
        Err(RenameError::UnknownSymbol(_))
    ));
    // Reach it and the bare name resolves.
    let mut db = ws(&[
        (
            "main",
            "use dep; fn main() -> i32 { return dep::helper(); }",
        ),
        ("dep", "fn helper() -> i32 { return 1; }"),
    ]);
    let plan = db.plan_rename(0, "helper", "h").unwrap();
    assert!(new_text(&plan, 1).contains("fn h()"));
    assert!(new_text(&plan, 0).contains("dep::h()"));
}

// ---------- enum variants and `match` ----------

/// Renaming an enum `data` rewrites the *data* segment of every
/// `T::V` constructor and pattern path — the variant names stay.
#[test]
fn data_rename_covers_variant_constructor_and_pattern_sites() {
    let mut db = ws(&[
        (
            "main",
            "use dep::Opt;\n\
             fn main() -> i32 {\n\
             \x20   let o = Opt::Some(1);\n\
             \x20   return match o { Opt::Some(v) => v, Opt::None => 0 };\n\
             }\n",
        ),
        ("dep", "data Opt { Some(i32); None; }\n"),
    ]);
    let plan = db.plan_rename(0, "dep::Opt", "Maybe").unwrap();
    // decl + `use dep::Opt` + `Opt::Some(1)` + two pattern paths.
    assert!(new_text(&plan, 1).contains("data Maybe {"));
    assert!(new_text(&plan, 0).contains("use dep::Maybe;"));
    assert!(new_text(&plan, 0).contains("let o = Maybe::Some(1);"));
    assert!(new_text(&plan, 0).contains("Maybe::Some(v) => v, Maybe::None => 0"));
    // The variant names themselves are never sites.
    assert!(!new_text(&plan, 0).contains("Maybe::Some(val)"));
}

/// `m::T::V` — the data segment rewrites inside a module-qualified
/// variant path.
#[test]
fn data_rename_covers_module_qualified_variant_sites() {
    let mut db = ws(&[
        (
            "main",
            "use dep;\n\
             fn main() -> i32 {\n\
             \x20   let o = dep::Opt::Some(1);\n\
             \x20   return match o { dep::Opt::Some(v) => v, dep::Opt::None => 0 };\n\
             }\n",
        ),
        ("dep", "data Opt { Some(i32); None; }\n"),
    ]);
    let plan = db.plan_rename(0, "dep::Opt", "Maybe").unwrap();
    assert!(new_text(&plan, 0).contains("dep::Maybe::Some(1)"));
    assert!(new_text(&plan, 0).contains("dep::Maybe::Some(v) => v, dep::Maybe::None => 0"));
}

/// A variant's payload type paths are rename sites like any other
/// type position in a `data` declaration.
#[test]
fn data_rename_covers_variant_payload_types() {
    let mut db = ws(&[
        (
            "main",
            "use dep;\n\
             fn main() -> i32 {\n\
             \x20   let o = dep::Opt::Some(dep::P { x: 1 });\n\
             \x20   return match o { dep::Opt::Some(p) => p.x, dep::Opt::None => 0 };\n\
             }\n",
        ),
        ("dep", "data P { x: i32; }\ndata Opt { Some(P); None; }\n"),
    ]);
    let plan = db.plan_rename(0, "dep::P", "Point").unwrap();
    assert!(new_text(&plan, 1).contains("data Point {"));
    assert!(
        new_text(&plan, 1).contains("Some(Point)"),
        "payload type should rewrite: {}",
        new_text(&plan, 1)
    );
    assert!(new_text(&plan, 0).contains("dep::Point { x: 1 }"));
    // `Opt` sites untouched.
    assert!(new_text(&plan, 0).contains("dep::Opt::Some"));
}

/// Renaming a data def keeps `use m::T as U` aliases stable — the
/// variant path through the alias does not rewrite.
#[test]
fn aliased_data_import_keeps_variant_paths() {
    let mut db = ws(&[
        (
            "main",
            "use dep::Opt as O;\n\
             fn main() -> i32 {\n\
             \x20   let o = O::Some(1);\n\
             \x20   return match o { O::Some(v) => v, O::None => 0 };\n\
             }\n",
        ),
        ("dep", "data Opt { Some(i32); None; }\n"),
    ]);
    let plan = db.plan_rename(0, "dep::Opt", "Maybe").unwrap();
    // decl + `use` member segment — the alias `O` and `O::V` paths
    // stay exactly as written.
    assert!(new_text(&plan, 0).contains("use dep::Maybe as O;"));
    assert!(new_text(&plan, 0).contains("let o = O::Some(1);"));
    assert!(new_text(&plan, 0).contains("O::Some(v) => v, O::None => 0"));
}

/// `dep::Opt::Some` is a variant path and `dep::double` a call —
/// renaming `double` edits only the call member, never the variant
/// path's `Opt`/`Some` segments.
#[test]
fn variant_paths_do_not_confuse_fn_rename() {
    let mut db = ws(&[
        (
            "main",
            "use dep;\n\
             fn main() -> i32 {\n\
             \x20   let o = dep::Opt::Some(1);\n\
             \x20   return match o { dep::Opt::Some(v) => v, dep::Opt::None => dep::double(0) };\n\
             }\n",
        ),
        (
            "dep",
            "data Opt { Some(i32); None; }\n\
             fn double(x: i32) -> i32 { return x * 2; }\n",
        ),
    ]);
    let plan = db.plan_rename(0, "dep::double", "twice").unwrap();
    assert!(new_text(&plan, 0).contains("dep::twice(0)"));
    assert!(new_text(&plan, 0).contains("dep::Opt::Some(1)"));
    assert!(new_text(&plan, 0).contains("dep::Opt::Some(v) => v, dep::Opt::None"));
}
