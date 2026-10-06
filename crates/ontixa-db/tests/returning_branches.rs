//! Returning branches must compose with incremental queries and span rebasing.
use ontixa_db::{Artifacts, Db, QueryKey};
use ontixa_diagnostics::Code;
use ontixa_interpreter::Interp;

fn run(a: &Artifacts) -> String {
    assert!(a.is_valid(), "{:?}", a.diags);
    let interp = Interp::new(&a.mir, &a.module, &a.interner);
    interp.show(&interp.run("main").unwrap())
}

fn assert_fresh(db: &mut Db, file: usize, source: &str, expected: &str) {
    let mut fresh = Db::new();
    let fresh_file = fresh.add_source(source);
    let actual = db.compile(file);
    let clean = fresh.compile(fresh_file);
    assert_eq!(run(actual), expected);
    assert_eq!(run(clean), expected);
    assert_eq!(actual.graph, clean.graph);
    assert_eq!(
        serde_json::to_value(&actual.mir).unwrap(),
        serde_json::to_value(&clean.mir).unwrap()
    );
}

#[test]
fn mixed_branch_edits_reuse_other_bodies_and_match_fresh_artifacts() {
    let original = "fn choose(c: bool) -> i32 { if c { return 42; } else { 7 } }\n\
                    fn untouched() -> i32 { 5 }\n\
                    fn main() -> i32 { choose(false) + untouched() }";
    let mut db = Db::new();
    let file = db.add_source(original);
    assert_eq!(run(db.compile(file)), "12");
    assert!(db.compile(file).is_valid());
    assert!(db.last_evaluated().is_empty());

    for (body, expected) in [
        ("if c { 42 } else { return 7; }", "12"),
        ("if c { return 42; } else { 8 }", "13"),
        ("if c { return 42; } else { 7 }", "12"),
    ] {
        let edited = original.replace("if c { return 42; } else { 7 }", body);
        db.set_source(file, edited.clone());
        assert_eq!(run(db.compile(file)), expected);
        let lowered: Vec<_> = db
            .last_evaluated()
            .iter()
            .filter_map(|key| match key {
                QueryKey::HirBody(def) => Some(db.interner().resolve(def.name).to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(lowered, ["choose"]);
        assert_fresh(&mut db, file, &edited, expected);
    }
}

#[test]
fn ownership_guard_edits_do_not_leave_stale_move_diagnostics() {
    let original = "data P { x: i32; }\n\
                    fn keep(p: P, c: bool) -> P { if c { return p; }; p }\n\
                    fn main() -> i32 { keep(P { x: 42 }, false).x }";
    let mut db = Db::new();
    let file = db.add_source(original);
    assert_fresh(&mut db, file, original, "42");

    // Removing the return makes the moved branch reach the later use of p.
    let moved = original.replace("return p;", "let q = p;");
    db.set_source(file, moved.clone());
    let actual = db.check(file).diags.clone().into_vec();
    assert_eq!(
        actual.iter().map(|d| d.code).collect::<Vec<_>>(),
        [Code::UseAfterMove]
    );
    let mut fresh = Db::new();
    let fresh_file = fresh.add_source(moved);
    assert_eq!(actual, fresh.check(fresh_file).diags.clone().into_vec());

    for source in [
        original.replace("if c { return p; }", "if c {} else { return p; }"),
        original.to_string(),
    ] {
        db.set_source(file, source.clone());
        assert!(db.check(file).is_valid());
        assert_fresh(&mut db, file, &source, "42");
    }
}

#[test]
fn mixed_branch_errors_rebase_and_clear_after_repair() {
    for (bad_body, good_body) in [
        (
            "if c { return false; } else { 42 }",
            "if c { return 7; } else { 42 }",
        ),
        (
            "if c { return 7; } else { false }",
            "if c { return 7; } else { 42 }",
        ),
    ] {
        let source = format!("fn helper() {{}}\nfn choose(c: bool) -> i32 {{ {bad_body} }}");
        let mut db = Db::new();
        let file = db.add_source(source.clone());
        let before = db.check(file).diags.clone().into_vec();
        assert!(before.iter().any(|d| d.code == Code::TypeMismatch));

        let prefix = "// moved source\n\n";
        let shifted = format!("{prefix}{source}");
        db.set_source(file, shifted.clone());
        let after = db.check(file).diags.clone().into_vec();
        assert_eq!(
            after,
            before
                .iter()
                .map(|d| d.rebased(prefix.len() as u32))
                .collect::<Vec<_>>()
        );
        assert!(
            !db.last_evaluated()
                .iter()
                .any(|key| matches!(key, QueryKey::HirBody(_) | QueryKey::BodyTypes(_)))
        );
        let mut fresh = Db::new();
        let fresh_file = fresh.add_source(shifted.clone());
        assert_eq!(after, fresh.check(fresh_file).diags.clone().into_vec());

        db.set_source(file, shifted.replace(bad_body, good_body));
        assert!(db.compile(file).is_valid());
        db.set_source(file, shifted);
        assert_eq!(after, db.compile(file).diags.clone().into_vec());
    }
}
