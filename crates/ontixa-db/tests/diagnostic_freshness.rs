//! Diagnostics follow the current source independently of semantic early cutoff.

use ontixa_db::{Db, QueryKey};
use ontixa_diagnostics::Diagnostic;

fn workspace(files: &[(&str, String)]) -> Db {
    let mut db = Db::new();
    for (module, text) in files {
        db.add_source_named(*module, text.clone());
    }
    db
}

fn diagnostics(db: &mut Db, root: usize, compile: bool) -> Vec<Diagnostic> {
    if compile {
        db.compile(root).diags.clone().into_vec()
    } else {
        db.check(root).diags.into_vec()
    }
}

fn assert_edits(
    initial: &[(&str, &str)],
    edits: &[(usize, &str)],
    compile: bool,
    semantic_reuse: bool,
) -> Vec<Vec<QueryKey>> {
    let mut files: Vec<_> = initial.iter().map(|(n, s)| (*n, s.to_string())).collect();
    let mut db = workspace(&files);
    let _ = diagnostics(&mut db, 0, compile);
    let semantic_keys: Vec<_> = db
        .last_evaluated()
        .into_iter()
        .filter(|k| {
            matches!(
                k,
                QueryKey::HirBody(_)
                    | QueryKey::BodyTypes(_)
                    | QueryKey::MirBody(_)
                    | QueryKey::Ownership(_)
            )
        })
        .collect();
    let mut runs = Vec::new();
    for &(file, text) in edits {
        let stats = db.stats().executed.clone();
        let semantic_stamps: Vec<_> = semantic_keys.iter().map(|&k| (k, db.stamp(k))).collect();
        db.set_source(file, text);
        files[file].1 = text.to_string();
        let actual = diagnostics(&mut db, 0, compile);
        runs.push(db.last_evaluated());
        let expected = diagnostics(&mut workspace(&files), 0, compile);
        assert_eq!(actual, expected, "file {file} edited to {text:?}");
        if semantic_reuse {
            for stage in ["hir", "types", "mir", "ownership"] {
                assert_eq!(
                    db.stats().executed.get(stage),
                    stats.get(stage),
                    "recomputed {stage}"
                );
            }
            for (key, stamp) in semantic_stamps {
                assert_eq!(db.stamp(key), stamp, "changed {key:?}");
            }
        }
        assert_eq!(diagnostics(&mut db, 0, compile), actual);
        assert!(
            db.last_evaluated().is_empty(),
            "unchanged demand recomputed"
        );
    }
    runs
}

const BROKEN_DEP: &[(&str, &str)] = &[("main", "use dep; fn main() {}"), ("dep", "fn f( {")];
const EMPTY_AST_ERROR: &[(&str, &str)] = &[("main", "use dep; fn main() {}"), ("dep", "@")];
const EMPTY_DEP: &[(&str, &str)] = &[("main", "use dep; fn main() {}"), ("dep", "")];

#[test]
fn malformed_dependency_offsets_match_cold() {
    assert_edits(
        BROKEN_DEP,
        &[(1, "\n\nfn f( {"), (1, "\n\n\nfn f( {")],
        false,
        true,
    );
}

#[test]
fn compile_diagnostics_match_cold() {
    assert_edits(BROKEN_DEP, &[(1, "\n\nfn f( {")], true, true);
}

#[test]
fn fixed_dependency_clears_errors() {
    assert_edits(BROKEN_DEP, &[(1, "fn f() {}")], false, false);
}

#[test]
fn ast_equal_edit_repositions_errors() {
    assert_edits(EMPTY_AST_ERROR, &[(1, "\n\n@")], false, true);
}

#[test]
fn ast_equal_edit_clears_errors() {
    assert_edits(EMPTY_AST_ERROR, &[(1, "")], false, true);
}

#[test]
fn ast_equal_edit_introduces_errors() {
    assert_edits(EMPTY_DEP, &[(1, "@")], false, true);
}

#[test]
fn ast_equal_compile_introduces_errors() {
    assert_edits(EMPTY_DEP, &[(1, "@")], true, true);
}

#[test]
fn ast_equal_compile_clears_errors() {
    assert_edits(EMPTY_AST_ERROR, &[(1, "")], true, true);
}

#[test]
fn same_offset_error_message_updates() {
    assert_edits(EMPTY_AST_ERROR, &[(1, "#")], false, true);
}

#[test]
fn multiple_modules_match_cold() {
    assert_edits(
        &[
            ("main", "use left; use right; fn main() {}"),
            ("left", "@"),
            ("right", "fn f( {"),
        ],
        &[(1, ""), (2, "\n\nfn f( {"), (2, "fn f() {}")],
        false,
        false,
    );
}

#[test]
fn transitive_module_matches_cold() {
    assert_edits(
        &[
            ("main", "use middle; fn main() {}"),
            ("middle", "use dep; fn g() {}"),
            ("dep", "fn f( {"),
        ],
        &[(2, "\n\nfn f( {")],
        false,
        true,
    );
}

#[test]
fn root_offsets_match_cold() {
    assert_edits(
        &[("main", "fn f( {")],
        &[(0, "\n\nfn f( {"), (0, "fn f() {}")],
        false,
        false,
    );
}

#[test]
fn unchanged_source_runs_nothing() {
    let runs = assert_edits(BROKEN_DEP, &[(1, "fn f( {")], false, true);
    assert!(runs[0].is_empty());
}

#[test]
fn unreachable_module_runs_nothing() {
    let runs = assert_edits(
        &[("main", "fn main() {}"), ("unused", "@")],
        &[(1, "\n\n@")],
        false,
        true,
    );
    assert!(runs[0].is_empty());
}

#[test]
fn valid_offset_shift_reuses_semantic_queries() {
    assert_edits(
        &[
            ("main", "use dep; fn main() -> i32 { return dep::f(); }"),
            ("dep", "fn f() -> i32 { return 1; }"),
        ],
        &[(1, "\n\nfn f() -> i32 { return 1; }")],
        true,
        true,
    );
}

#[test]
fn item_relative_errors_preserve_reuse_and_offsets() {
    assert_edits(
        &[
            ("main", "use dep; fn main() {}"),
            ("dep", "fn f() -> i32 { return nope; }"),
        ],
        &[(1, "\n\nfn f() -> i32 { return nope; }")],
        false,
        true,
    );
}

#[test]
fn removed_import_excludes_cached_dependency_errors() {
    assert_edits(
        BROKEN_DEP,
        &[
            (0, "fn main() {}"),
            (1, "\n\nfn f( {"),
            (0, "use dep; fn main() {}"),
        ],
        false,
        false,
    );
}

#[test]
fn alternating_roots_collect_only_their_current_workspace() {
    let mut files = vec![
        ("left", "use dep; fn left() {}".to_string()),
        ("right", "use dep; fn right() {}".to_string()),
        ("dep", "fn f( {".to_string()),
        ("unused", "@".to_string()),
    ];
    let mut db = workspace(&files);
    for text in ["fn f( {", "\n\nfn f( {", "fn f() {}", "@", ""] {
        db.set_source(2, text);
        files[2].1 = text.to_string();
        for root in [0, 1, 2, 1, 0] {
            for compile in [false, true] {
                let actual = diagnostics(&mut db, root, compile);
                let expected = diagnostics(&mut workspace(&files), root, compile);
                assert_eq!(
                    actual, expected,
                    "root {root}, dep {text:?}, compile {compile}"
                );
                assert_eq!(diagnostics(&mut db, root, compile), actual);
                assert!(db.last_evaluated().is_empty());
            }
        }
    }
}
