//! Context edits invalidate arithmetic types and preserve diagnostic rebasing.
use ontixa_db::Db;
use ontixa_diagnostics::Code;
use ontixa_interpreter::Interp;

#[test]
fn edited_numeric_contexts_match_fresh_compilation() {
    let narrow = "fn take(x: i8) -> i8 { x }\nfn main() -> i8 { take(128 + 0) }";
    let wide = narrow.replace("i8", "i16");
    let shifted = format!("// shift diagnostic spans\n\n{narrow}");
    let repaired = narrow.replace("128", "127");
    let mut db = Db::new();
    let file = db.add_source(narrow);
    let before = db.check(file).diags.clone().into_vec();
    assert_eq!(
        before.iter().map(|d| d.code).collect::<Vec<_>>(),
        [Code::LiteralOverflow]
    );

    for (source, value) in [
        (wide.as_str(), Some("128")),
        (shifted.as_str(), None),
        (repaired.as_str(), Some("127")),
        (narrow, None),
    ] {
        db.set_source(file, source);
        let mut fresh = Db::new();
        let fresh_file = fresh.add_source(source);
        let actual = db.compile(file);
        let clean = fresh.compile(fresh_file);
        assert_eq!(
            actual.diags.clone().into_vec(),
            clean.diags.clone().into_vec()
        );
        if let Some(value) = value {
            assert!(actual.is_valid(), "{:?}", actual.diags);
            assert_eq!(actual.graph, clean.graph);
            assert_eq!(
                serde_json::to_value(&actual.mir).unwrap(),
                serde_json::to_value(&clean.mir).unwrap()
            );
            let interp = Interp::new(&actual.mir, &actual.module, &actual.interner);
            assert_eq!(interp.show(&interp.run("main").unwrap()), value);
        } else {
            let diagnostics = actual.diags.clone().into_vec();
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].code, Code::LiteralOverflow);
            let shift = if source == shifted {
                (shifted.len() - narrow.len()) as u32
            } else {
                0
            };
            assert_eq!(
                diagnostics[0].primary,
                before[0].primary.map(|s| s.abs(shift))
            );
        }
    }
}

#[test]
fn edited_default_integer_minima_match_fresh_compilation() {
    let minimum = "fn main() -> i32 { let x = -2147483648; x }";
    let overflow = minimum.replace("2147483648", "2147483649");
    let shifted = format!("// shift the signed diagnostic\n\n{overflow}");
    let annotated = minimum.replace("let x =", "let x: i32 =");
    let mut db = Db::new();
    let file = db.add_source(minimum);
    for (source, valid) in [
        (minimum, true),
        (overflow.as_str(), false),
        (shifted.as_str(), false),
        (annotated.as_str(), true),
        (minimum, true),
    ] {
        db.set_source(file, source);
        let mut fresh = Db::new();
        let fresh_file = fresh.add_source(source);
        let actual = db.compile(file);
        let clean = fresh.compile(fresh_file);
        assert_eq!(actual.is_valid(), valid, "{source}: {:?}", actual.diags);
        assert_eq!(
            actual.diags.clone().into_vec(),
            clean.diags.clone().into_vec()
        );
        assert_eq!(actual.graph, clean.graph);
        assert_eq!(
            serde_json::to_value(&actual.mir).unwrap(),
            serde_json::to_value(&clean.mir).unwrap()
        );
        if valid {
            let interp = Interp::new(&actual.mir, &actual.module, &actual.interner);
            assert_eq!(interp.show(&interp.run("main").unwrap()), "-2147483648");
        } else {
            let diagnostics = actual.diags.clone().into_vec();
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(diagnostics[0].code, Code::LiteralOverflow);
        }
    }
}
