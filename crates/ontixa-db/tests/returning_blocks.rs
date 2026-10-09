//! Returning block context edits keep cached diagnostics and artifacts current.
use ontixa_db::{Db, QueryKey};
use ontixa_diagnostics::Code;
use ontixa_interpreter::Interp;

fn assert_fresh(db: &mut Db, file: usize, source: &str, value: Option<&str>) {
    let checked = db.check(file).diags.clone().into_vec();
    let mut fresh = Db::new();
    let fresh_file = fresh.add_source(source);
    assert_eq!(checked, fresh.check(fresh_file).diags.clone().into_vec());
    let actual = db.compile(file);
    let clean = fresh.compile(fresh_file);
    assert_eq!(actual.diags.clone().into_vec(), checked);
    assert_eq!(checked, clean.diags.clone().into_vec());
    assert_eq!(actual.types, clean.types);
    assert_eq!(actual.graph, clean.graph);
    assert_eq!(
        serde_json::to_value(&actual.mir).unwrap(),
        serde_json::to_value(&clean.mir).unwrap()
    );
    assert_eq!(actual.is_valid(), value.is_some(), "{source}: {checked:?}");
    if let Some(value) = value {
        for a in [actual, clean] {
            let interp = Interp::new(&a.mir, &a.module, &a.interner);
            assert_eq!(interp.show(&interp.run("main").unwrap()), value);
        }
    }
    db.compile(file);
    assert!(db.last_evaluated().is_empty());
}

#[test]
fn returning_block_repairs_and_context_changes_match_fresh_artifacts() {
    let original = "fn id(x: i32) -> i32 { x }\nfn untouched() -> i32 { 5 }\nfn main() -> i32 { id({ return 42; }) }";
    let mut db = Db::new();
    let file = db.add_source(original);
    assert_fresh(&mut db, file, original, Some("42"));
    for (block, value) in [
        ("{ 42; }", None),
        ("{ return false; }", None),
        ("{ return 42; let dead: bool = 1; }", None),
        ("{ return 42; }", Some("42")),
        ("{ if true { return 7; } else { return 42; }; }", Some("7")),
        ("{ true && { return 42; }; }", None),
        ("{ return 42; }", Some("42")),
    ] {
        let source = original.replace("{ return 42; }", block);
        db.set_source(file, source.clone());
        db.check(file);
        let lowered: Vec<_> = db
            .last_evaluated()
            .iter()
            .filter_map(|key| match key {
                QueryKey::HirBody(def) => Some(db.interner().resolve(def.name).to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(lowered, ["main"]);
        assert_fresh(&mut db, file, &source, value);
    }
    // A different parameter context changes the block annotation, not its return.
    let changed_context =
        original.replace("fn id(x: i32) -> i32 { x }", "fn id(x: bool) -> i32 { 0 }");
    db.set_source(file, changed_context.clone());
    assert_fresh(&mut db, file, &changed_context, Some("42"));
}

#[test]
fn returning_block_diagnostics_rebase_without_rechecking_bodies() {
    for block in ["{ return false; }", "{ return 42; let dead: bool = 1; }"] {
        let original = format!("fn id(x: i32) -> i32 {{ x }}\nfn main() -> i32 {{ id({block}) }}");
        let mut db = Db::new();
        let file = db.add_source(original.clone());
        let before = db.check(file).diags.clone().into_vec();
        assert_eq!(
            before.iter().map(|d| d.code).collect::<Vec<_>>(),
            [Code::TypeMismatch]
        );
        let prefix = "// shift all item spans\n\n";
        let shifted = format!("{prefix}{original}");
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
        assert_fresh(&mut db, file, &shifted, None);
        let repaired = shifted.replace(block, "{ return 42; }");
        db.set_source(file, repaired.clone());
        assert_fresh(&mut db, file, &repaired, Some("42"));
        db.set_source(file, shifted.clone());
        assert_fresh(&mut db, file, &shifted, None);
    }
}

#[test]
fn owned_return_edits_keep_caller_consumption_diagnostics_current() {
    let original = "data P { x: i32; } fn id(p: P) -> P { p } fn keep(p: P, c: bool) -> P { c && { return p; }; p } fn main() -> i32 { let p = P { x: 42 }; let q = keep(p, false); q.x }";
    let mut db = Db::new();
    let file = db.add_source(original);
    assert_fresh(&mut db, file, original, Some("42"));
    for source in [
        original.replace("c && { return p; }; p", "id({ return p; })"),
        original.replace("return p;", "let q = p; true"),
        original.replace("q.x }", "p.x }"),
        original.to_string(),
    ] {
        let valid = !source.contains("let q = p;") && !source.ends_with("p.x }");
        db.set_source(file, source.clone());
        if !valid {
            let diagnostics = db.check(file).diags.clone().into_vec();
            assert_eq!(
                diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
                [Code::UseAfterMove]
            );
        }
        assert_fresh(&mut db, file, &source, valid.then_some("42"));
    }
}
