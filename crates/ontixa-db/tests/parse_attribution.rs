//! Parser diagnostics must retain the source file through workspace collection.

use ontixa_db::Db;
use ontixa_diagnostics::Code;
use ontixa_source::FileId;

fn parse_locations(db: &mut Db, root: usize) -> Vec<(FileId, u32, u32)> {
    db.check(root)
        .diags
        .iter()
        .filter(|d| d.code == Code::Parse)
        .map(|d| {
            let span = d.primary.expect("parse diagnostic has a span");
            (
                d.file.expect("parse diagnostic has a source file"),
                span.start,
                span.end,
            )
        })
        .collect()
}

#[test]
fn parse_errors_belong_to_the_root_even_when_it_is_not_file_zero() {
    let mut db = Db::new();
    db.add_source_named("unused", "fn unused() {}");
    let root = db.add_source_named("main", "fn f( {");
    assert_eq!(
        parse_locations(&mut db, root),
        vec![
            (FileId::new(root as u32), 6, 7),
            (FileId::new(root as u32), 7, 7),
            (FileId::new(root as u32), 7, 7)
        ]
    );
}

#[test]
fn identical_dependency_parse_errors_are_not_deduplicated_across_files() {
    let mut db = Db::new();
    let root = db.add_source_named("main", "use a; use b; fn main() {}");
    let a = db.add_source_named("a", "fn f( {");
    let b = db.add_source_named("b", "fn f( {");
    db.add_source_named("unused", "fn f( {");
    let locations = parse_locations(&mut db, root);
    for file in [a, b] {
        let spans: Vec<_> = locations
            .iter()
            .filter(|(f, _, _)| f.index() == file)
            .map(|(_, start, end)| (*start, *end))
            .collect();
        assert_eq!(spans, vec![(6, 7), (7, 7), (7, 7)]);
    }
    assert_eq!(locations.len(), 6);
    assert_eq!(parse_locations(&mut db, root), locations);
    assert!(db.last_evaluated().is_empty());
}

#[test]
fn newly_introduced_dependency_parse_errors_keep_their_file_after_edit() {
    let mut db = Db::new();
    let root = db.add_source_named("main", "use dep; fn main() {}");
    let dep = db.add_source_named("dep", "fn f() {}");
    assert!(parse_locations(&mut db, root).is_empty());
    db.set_source(dep, "\n\nfn f( {");
    let after = parse_locations(&mut db, root);
    let file = FileId::new(dep as u32);
    assert_eq!(after, vec![(file, 8, 9), (file, 9, 9), (file, 9, 9)]);
    assert_eq!(parse_locations(&mut db, root), after);
    assert!(db.last_evaluated().is_empty());
}

#[test]
fn lexer_errors_in_dependencies_keep_their_source_file() {
    let mut db = Db::new();
    let root = db.add_source_named("main", "use dep; fn main() {}");
    let text = "fn f() { ` }";
    let dep = db.add_source_named("dep", text);
    let report = db.check(root);
    let error = report
        .diags
        .iter()
        .find(|d| d.code == Code::UnexpectedCharacter)
        .unwrap();
    assert_eq!(error.file, Some(FileId::new(dep as u32)));
    let span = error.primary.unwrap();
    let start = text.find('`').unwrap() as u32;
    assert_eq!((span.start, span.end), (start, start + 1));
}
