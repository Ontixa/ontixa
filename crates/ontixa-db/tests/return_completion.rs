//! Completion errors are ordinary memoized and rebased type diagnostics.
use ontixa_db::Db;
use ontixa_diagnostics::Code;

#[test]
fn missing_return_tracks_trivia_and_disappears_after_a_fallback_is_added() {
    let bad = "fn helper() {}\nfn main() -> i32 { { false && { return 7; true } }; }";
    let mut db = Db::new();
    let file = db.add_source(bad);
    let before: Vec<_> = db
        .check(file)
        .diags
        .iter()
        .map(|d| (d.code, d.primary))
        .collect();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].0, Code::MissingReturn);
    let prefix = "// shifted\n\n";
    let shifted = format!("{prefix}{bad}");
    db.set_source(file, shifted.clone());
    let after: Vec<_> = db
        .check(file)
        .diags
        .iter()
        .map(|d| (d.code, d.primary))
        .collect();
    assert_eq!(
        after,
        [(
            Code::MissingReturn,
            before[0].1.map(|s| s.abs(prefix.len() as u32))
        )]
    );
    let mut fresh = Db::new();
    let fresh_file = fresh.add_source(shifted.clone());
    assert_eq!(
        after,
        fresh
            .check(fresh_file)
            .diags
            .iter()
            .map(|d| (d.code, d.primary))
            .collect::<Vec<_>>()
    );
    db.set_source(file, shifted.replace("true } }; }", "true } }; 42 }"));
    assert!(db.compile(file).is_valid());
    db.set_source(file, shifted);
    assert_eq!(
        db.check(file)
            .diags
            .iter()
            .map(|d| d.code)
            .collect::<Vec<_>>(),
        [Code::MissingReturn]
    );
}
