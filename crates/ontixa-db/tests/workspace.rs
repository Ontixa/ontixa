//! Multi-module workspace tests: `use` declarations, qualified
//! paths, per-file environments, cross-file invalidation, and
//! file-tagged diagnostics — all through the public `Db` API.
//!
//! A workspace is "one root file plus the registered modules its
//! `use`s reach". Each file is registered with `add_source_named`,
//! where the name is the module the file provides (the CLI/daemon
//! pass the file stem).

use ontixa_db::{Db, QueryKey};
use ontixa_diagnostics::Code;
use ontixa_interpreter::{Interp, Value};
use ontixa_source::FileId;

/// Builds a workspace: `root_src` provides module `main`; each dep
/// `(name, src)` registers a module. Returns the `Db` and the root
/// file id.
fn ws(root_src: &str, deps: &[(&str, &str)]) -> (Db, usize) {
    let mut db = Db::new();
    let root = db.add_source_named("main", root_src);
    for (name, src) in deps {
        db.add_source_named(*name, *src);
    }
    (db, root)
}

/// The display string of running `entry` — panics on compile errors
/// or a runtime trap, with the diagnostics in the message.
fn run(a: &ontixa_db::Artifacts, entry: &str) -> String {
    let interp = Interp::new(&a.mir, &a.module, &a.interner);
    match interp.run(entry) {
        Ok(v) => interp.show(&v),
        Err(e) => panic!("run `{entry}` failed: {e}"),
    }
}

const MATH: &str = "\
fn double(x: i32) -> i32 { return x * 2; }
fn triple(x: i32) -> i32 { return x * 3; }
fn answer() -> i32 { return 7; }";

// ---- resolution & execution ----------------------------------------

#[test]
fn qualified_call_compiles_and_runs() {
    let (mut db, root) = ws(
        "use math; fn main() -> i32 { return math::double(21); }",
        &[("math", MATH)],
    );
    let a = db.compile(root);
    assert!(a.is_valid(), "{:?}", a.diags);
    assert_eq!(run(a, "main"), "42");
}

#[test]
fn module_qualified_entry_runs() {
    let (mut db, root) = ws(
        "use math; fn main() -> i32 { return 0; }",
        &[("math", MATH)],
    );
    let a = db.compile(root);
    assert!(a.is_valid(), "{:?}", a.diags);
    assert_eq!(run(a, "math::answer"), "7");
    // `run_in` is the programmatic form.
    let interp = Interp::new(&a.mir, &a.module, &a.interner);
    assert!(matches!(interp.run_in("math", "answer"), Ok(Value::Int(7))));
}

#[test]
fn imported_alias_binds_member() {
    let (mut db, root) = ws(
        "use math::double as dbl; fn main() -> i32 { return dbl(21); }",
        &[("math", MATH)],
    );
    let a = db.compile(root);
    assert!(a.is_valid(), "{:?}", a.diags);
    assert_eq!(run(a, "main"), "42");
}

#[test]
fn imported_member_needs_no_alias() {
    let (mut db, root) = ws(
        "use math::double; fn main() -> i32 { return double(21); }",
        &[("math", MATH)],
    );
    let a = db.compile(root);
    assert!(a.is_valid(), "{:?}", a.diags);
    assert_eq!(run(a, "main"), "42");
}

#[test]
fn qualified_data_type_and_literal() {
    let geo = "data Point { x: i32; y: i32; }";
    let (mut db, root) = ws(
        "use geo; fn main() -> i32 { let p = geo::Point { x: 3, y: 4 }; return p.x + p.y; }",
        &[("geo", geo)],
    );
    let a = db.compile(root);
    assert!(a.is_valid(), "{:?}", a.diags);
    assert_eq!(run(a, "main"), "7");
}

#[test]
fn qualified_type_annotation() {
    let geo = "data Point { x: i32; }";
    let (mut db, root) = ws(
        "use geo; fn take(p: geo::Point) -> i32 { return p.x; } \
         fn main() -> i32 { return take(geo::Point { x: 9 }); }",
        &[("geo", geo)],
    );
    let a = db.compile(root);
    assert!(a.is_valid(), "{:?}", a.diags);
    assert_eq!(run(a, "main"), "9");
}

/// Two files may define same-named fns — `DefId` identity keeps
/// them distinct, so `a::v()` and `b::v()` are different callees.
#[test]
fn same_name_defs_in_two_modules() {
    let (mut db, root) = ws(
        "use a; use b; fn main() -> i32 { return a::v() + b::v(); }",
        &[
            ("a", "fn v() -> i32 { return 1; }"),
            ("b", "fn v() -> i32 { return 2; }"),
        ],
    );
    let a = db.compile(root);
    assert!(a.is_valid(), "{:?}", a.diags);
    assert_eq!(run(a, "main"), "3");
}

/// A dep's own `use` pulls a further module in — transitive
/// reachability.
#[test]
fn transitive_uses_reach_inner_modules() {
    let (mut db, root) = ws(
        "use mid; fn main() -> i32 { return mid::go(); }",
        &[
            ("mid", "use leaf; fn go() -> i32 { return leaf::val(); }"),
            ("leaf", "fn val() -> i32 { return 5; }"),
        ],
    );
    let a = db.compile(root);
    assert!(a.is_valid(), "{:?}", a.diags);
    assert_eq!(run(a, "main"), "5");
}

/// Import cycles terminate — the BFS marks files visited.
#[test]
fn cyclic_uses_compile() {
    let (mut db, root) = ws(
        "use other; fn main() -> i32 { return other::v(); }",
        &[("other", "use main; fn v() -> i32 { return 1; }")],
    );
    let a = db.compile(root);
    assert!(a.is_valid(), "{:?}", a.diags);
    assert_eq!(run(a, "main"), "1");
}

// ---- rejections -----------------------------------------------------

#[test]
fn unknown_module_reports() {
    let (mut db, root) = ws("use nope; fn main() -> i32 { return 0; }", &[]);
    let report = db.check(root);
    assert!(report.diags.iter().any(|d| d.code == Code::UnknownModule));
}

#[test]
fn unknown_module_member_reports() {
    let (mut db, root) = ws(
        "use math; fn main() -> i32 { return math::nope(1); }",
        &[("math", MATH)],
    );
    let report = db.check(root);
    assert!(report.diags.has_errors());
    // Member misses are symbol errors, not module errors.
    assert!(report.diags.iter().all(|d| d.code != Code::UnknownModule));
}

/// A dep's defs are not in the root's bare-name scope — `use m;`
/// imports the module path only, not its members.
#[test]
fn dep_members_need_qualification() {
    let (mut db, root) = ws(
        "use math; fn main() -> i32 { return double(21); }",
        &[("math", MATH)],
    );
    let report = db.check(root);
    assert!(report.diags.has_errors());
}

/// A registered file that no `use` reaches stays outside the
/// workspace — its errors don't surface and its defs don't exist
/// for the root.
#[test]
fn unreached_file_stays_out_of_scope() {
    let (mut db, root) = ws(
        "fn main() -> i32 { return 0; }",
        &[("broken", "fn bad( -> i32 { return ; }")],
    );
    let report = db.check(root);
    assert!(
        !report.diags.has_errors(),
        "{:?}",
        report.diags.iter().map(|d| &d.message).collect::<Vec<_>>()
    );
}

/// Reach a broken file via `use` → its diagnostics surface, tagged
/// with *its* file id, and their spans index its own text.
#[test]
fn dep_diagnostics_carry_dep_file() {
    let (mut db, root) = ws(
        "use broken; fn main() -> i32 { return 0; }",
        &[("broken", "fn bad() -> i32 { return nope; }")],
    );
    let report = db.check(root);
    let dep_diags: Vec<_> = report
        .diags
        .iter()
        .filter(|d| d.file == Some(FileId::new(1)))
        .collect();
    assert!(!dep_diags.is_empty(), "{:?}", report.diags);
    // The span is an offset into `broken`'s text, not the root's.
    let broken_text = db.source(1);
    for d in dep_diags {
        let p = d.primary.expect("dep diag has a primary span");
        assert!(p.end as usize <= broken_text.len());
    }
}

// ---- incrementality --------------------------------------------------

/// Names of per-def HirBody keys evaluated in the last demand.
fn evald_hir(db: &Db) -> Vec<String> {
    db.last_evaluated()
        .iter()
        .filter_map(|k| match k {
            QueryKey::HirBody(key) => Some(*key),
            _ => None,
        })
        .map(|k| format!("{}:{}", k.file.index(), db.interner().resolve(k.name)))
        .collect()
}

/// Editing a dep's function *body* re-lowers only that def: the
/// scope's signatures are unchanged (item-relative spans), so the
/// caller's HIR stays memoized end-to-end.
#[test]
fn dep_body_edit_invalidates_only_dep() {
    let (mut db, root) = ws(
        "use math; fn main() -> i32 { return math::double(21); }",
        &[("math", MATH)],
    );
    assert!(db.compile(root).is_valid());
    db.set_source(1, MATH.replace("x * 2", "x * 4"));
    assert!(db.compile(root).is_valid());
    assert_eq!(evald_hir(&db), ["1:double"]);
    assert_eq!(run(db.compile(root), "main"), "84");
}

/// Editing a dep's *signature* propagates: `double`'s contract or
/// arity change re-resolves the scope and re-lowers the caller.
#[test]
fn dep_signature_edit_reaches_callers() {
    let (mut db, root) = ws(
        "use math; fn main() -> i32 { return math::double(21); }",
        &[("math", MATH)],
    );
    assert!(db.compile(root).is_valid());
    db.set_source(1, MATH.replace("x * 2", "x * 2 + 0")); // body-only first
    assert!(db.compile(root).is_valid());
    // Now rename a param — sig value changes → scope re-resolves →
    // caller's body re-lowers (its call site is re-checked).
    db.set_source(
        1,
        MATH.replace("double(x: i32)", "double(y: i32)")
            .replace("x * 2", "y * 2"),
    );
    assert!(db.compile(root).is_valid());
    let evald = evald_hir(&db);
    assert!(evald.contains(&"1:double".to_string()), "{evald:?}");
    assert!(evald.contains(&"0:main".to_string()), "{evald:?}");
}

/// Removing a dep's `use` from the root shrinks the reachable set —
/// subsequent edits to the dep no longer touch the root's workspace.
#[test]
fn dropping_use_severs_dependency() {
    let (mut db, root) = ws(
        "use math; fn main() -> i32 { return math::double(21); }",
        &[("math", MATH)],
    );
    assert!(db.compile(root).is_valid());
    db.set_source(0, "fn main() -> i32 { return 0; }");
    assert!(db.compile(root).is_valid());
    // Editing math now: nothing in root's workspace re-runs.
    db.set_source(1, "fn double(x: i32) -> i32 { return x; }");
    assert!(db.compile(root).is_valid());
    assert_eq!(db.last_evaluated(), Vec::new());
}

/// `DefKey`s carry the file, so same-named defs in different files
/// are different query keys — no per-definition conflation.
#[test]
fn def_keys_are_file_scoped() {
    let (mut db, root) = ws(
        "use a; use b; fn main() -> i32 { return a::v() + b::v(); }",
        &[
            ("a", "fn v() -> i32 { return 1; }"),
            ("b", "fn v() -> i32 { return 2; }"),
        ],
    );
    assert!(db.compile(root).is_valid());
    let a = db.compile(root);
    let va = a
        .module
        .scope
        .env(FileId::new(1))
        .unwrap()
        .fns
        .values()
        .next()
        .copied()
        .unwrap();
    let vb = a
        .module
        .scope
        .env(FileId::new(2))
        .unwrap()
        .fns
        .values()
        .next()
        .copied()
        .unwrap();
    assert_ne!(va, vb);
    assert_eq!(a.module.scope.def(va).file, FileId::new(1));
    assert_eq!(a.module.scope.def(vb).file, FileId::new(2));
}

/// Two roots may share a dep in one `Db`. The same def has different
/// `DefId`s in each workspace (defs are indexed per scope), so
/// per-def query results must key on `(root, file, name)` — reusing
/// root A's `dep::v` body under root B would carry A's `DefId`s and
/// resolve its call to the wrong target (here: arity mismatch).
#[test]
fn shared_dep_under_two_roots_stays_correct() {
    let mut db = Db::new();
    let dep = db.add_source_named(
        "dep",
        "fn v() -> i32 { return helper(7); } fn helper(x: i32) -> i32 { return x; }",
    );
    let a = db.add_source_named("a", "use dep; fn main() -> i32 { return dep::v(); }");
    let b = db.add_source_named(
        "b",
        "fn extra() -> i32 { return 99; } use dep; fn main() -> i32 { return dep::v(); }",
    );
    assert_eq!(run(db.compile(a), "main"), "7");
    // `dep` checked directly roots its own single-file workspace —
    // the next demand on `b` must reclaim it as `b`'s dep.
    assert!(db.check(dep).is_valid());
    assert_eq!(run(db.compile(b), "main"), "7");
    // Round-trip: each root's entries stay its own.
    assert_eq!(run(db.compile(a), "main"), "7");
    assert_eq!(run(db.compile(b), "dep::v"), "7");
    assert_eq!(run(db.compile(dep), "v"), "7");
}
