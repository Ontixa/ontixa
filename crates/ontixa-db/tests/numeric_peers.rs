//! Independently typed peers invalidate callers across public module boundaries.
use ontixa_db::Db;
use ontixa_diagnostics::Code;
use ontixa_interpreter::Interp;

#[test]
fn cross_module_peer_edits_match_cold_types_mir_graph_and_diagnostics() {
    let root_src = "use peer; fn main() -> bool { let sum = 128 + peer::value(); 0 < sum }";
    let wide = "fn value() -> i16 { 1 }";
    let narrow = "fn value() -> i8 { 1 }";
    let float = "fn value() -> f32 { 1.0 }";
    let shifted = format!("// move caller spans\n\n{root_src}");
    let mut warm = Db::new();
    let root = warm.add_source_named("main", root_src);
    let peer = warm.add_source_named("peer", wide);

    for (main_source, peer_source, error) in [
        (root_src, wide, None),
        (root_src, narrow, Some(Code::LiteralOverflow)),
        (shifted.as_str(), narrow, Some(Code::LiteralOverflow)),
        (shifted.as_str(), float, Some(Code::TypeMismatch)),
        (shifted.as_str(), wide, None),
        (root_src, "fn value() -> i64 { 2 }", None),
        (root_src, wide, None),
    ] {
        warm.set_source(root, main_source);
        warm.set_source(peer, peer_source);
        let mut cold = Db::new();
        let cold_root = cold.add_source_named("main", main_source);
        cold.add_source_named("peer", peer_source);
        let actual = warm.compile(root);
        let fresh = cold.compile(cold_root);
        assert_eq!(
            actual.diags.clone().into_vec(),
            fresh.diags.clone().into_vec()
        );
        assert_eq!(actual.types, fresh.types);
        assert_eq!(actual.graph, fresh.graph);
        assert_eq!(
            serde_json::to_value(&actual.mir).unwrap(),
            serde_json::to_value(&fresh.mir).unwrap()
        );
        match error {
            Some(code) => {
                assert!(!actual.is_valid());
                assert!(actual.diags.iter().any(|d| d.code == code));
                assert!(actual.diags.iter().all(|d| d.code != Code::Internal));
            }
            None => {
                assert!(actual.is_valid(), "{:?}", actual.diags);
                let interp = Interp::new(&actual.mir, &actual.module, &actual.interner);
                assert_eq!(interp.show(&interp.run("main").unwrap()), "true");
            }
        }
    }
}
