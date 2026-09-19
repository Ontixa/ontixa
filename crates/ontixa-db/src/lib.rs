//! The Ontixa compiler database — a memoized incremental query engine.
//!
//! Pipeline position:
//!
//! ```text
//! Db::add_source ──▶ Db::compile ──▶ Artifacts (ast → mir + diags + timings)
//! ```
//!
//! `Db` memoizes every pipeline stage as a [`QueryKey`]-keyed entry
//! with explicit dependency edges. `set_source` rewrites the `Source`
//! input and bumps the global revision; the next demand verifies each
//! cached entry's dependencies and re-evaluates only what drifted.
//! Per-definition queries are keyed by the stable `DefKey`, so editing
//! one function reuses every other function's HIR, type tables, MIR,
//! and collected ownership facts. [`QueryStats`] reports
//! executed-vs-reused counters — the measurable evidence of
//! incrementality. See `docs/adr/0008-incremental-engine.md`.

mod db;
mod eval;
mod query;

pub use db::{Artifacts, Db, StageTiming};
pub use query::{CheckedBody, QueryKey, QueryStats};

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "fn main() -> i32 { let x = 1; return x + 1; }";

    #[test]
    fn compiles_and_caches() {
        let mut db = Db::new();
        let f = db.add_source(SRC);
        assert!(db.compile(f).is_valid());
        let rev = db.compile(f).built_revision;
        // Same revision → memoized (same artifacts object).
        let p1 = db.compile(f) as *const Artifacts;
        let p2 = db.compile(f) as *const Artifacts;
        assert_eq!(p1, p2);
        assert_eq!(db.compile(f).built_revision, rev);
    }

    #[test]
    fn edit_invalidates() {
        let mut db = Db::new();
        let f = db.add_source(SRC);
        let r0 = db.compile(f).built_revision;
        db.set_source(f, "fn main() -> i32 { return 7; }");
        let a = db.compile(f);
        assert!(a.built_revision > r0);
        assert!(a.is_valid());
    }

    #[test]
    fn reports_stage_timings() {
        let mut db = Db::new();
        let f = db.add_source(SRC);
        let stages: Vec<&str> = db.compile(f).timings.iter().map(|t| t.stage).collect();
        for stage in [
            "lex+parse",
            "ast",
            "resolve",
            "hir",
            "types",
            "ownership",
            "graph",
            "mir",
        ] {
            assert!(stages.contains(&stage), "missing stage {stage}");
        }
    }

    #[test]
    fn errors_surface_in_artifacts() {
        let mut db = Db::new();
        let f = db.add_source("fn main() -> i32 { return nope; }");
        let a = db.compile(f);
        assert!(!a.is_valid());
        assert!(a.diags.has_errors());
    }

    // ---- incremental acceptance (ADR-0008) --------------------------

    /// Names of the per-def queries evaluated in the last demand.
    fn evald_defs(db: &Db, kind: fn(&QueryKey) -> Option<DefKey>) -> Vec<String> {
        db.last_evaluated()
            .iter()
            .filter_map(kind)
            .map(|k| db.interner().resolve(k.name).to_string())
            .collect()
    }

    fn hir_key(k: &QueryKey) -> Option<DefKey> {
        match k {
            QueryKey::HirBody(k) => Some(*k),
            _ => None,
        }
    }
    fn types_key(k: &QueryKey) -> Option<DefKey> {
        match k {
            QueryKey::BodyTypes(k) => Some(*k),
            _ => None,
        }
    }
    fn mir_key(k: &QueryKey) -> Option<DefKey> {
        match k {
            QueryKey::MirBody(k) => Some(*k),
            _ => None,
        }
    }

    const THREE_FN: &str = "\
data P { x: i32; }
fn f(p: i32) -> i32 { return p + 1; }
fn g(q: i32) -> i32 { return q * 2; }
fn main() -> i32 { return f(1) + g(2); }";

    /// Acceptance A: a second compile with no edit evaluates nothing.
    #[test]
    fn unchanged_recompile_evaluates_nothing() {
        let mut db = Db::new();
        let f = db.add_source(THREE_FN);
        assert!(db.compile(f).is_valid());
        assert!(!db.last_evaluated().is_empty());
        assert!(db.compile(f).is_valid());
        assert_eq!(db.last_evaluated(), Vec::new());
        assert!(db.stats().totals().1 > 0, "second compile all reuse");
    }

    /// Acceptance B: editing inside one body re-evaluates only that
    /// body's per-def queries; unrelated bodies stay memoized.
    #[test]
    fn body_edit_invalidates_only_that_body() {
        let mut db = Db::new();
        let f = db.add_source(THREE_FN);
        assert!(db.compile(f).is_valid());
        db.set_source(f, THREE_FN.replace("p + 1", "p + 7"));
        assert!(db.compile(f).is_valid());
        assert_eq!(evald_defs(&db, hir_key), ["f"]);
        assert_eq!(evald_defs(&db, types_key), ["f"]);
        // f's contract is unchanged (still borrow) → ownership value
        // equal → downstream mir of *other* fns never re-ran.
        assert_eq!(evald_defs(&db, mir_key), ["f"]);
        // Inside the ownership fixpoint, only f's facts were
        // re-walked; g and main served from the oracle memo.
        assert_eq!(db.oracle().last_collected, 1);
        assert_eq!(db.oracle().last_reused, 2);
    }

    /// Acceptance C: a comment-only edit cuts off at the AST —
    /// no per-definition work re-runs at all. (Note: spans are
    /// file-absolute, so edits *inside* a body still rebuild that
    /// body — body-relative spans are future work.)
    #[test]
    fn whitespace_edit_cuts_off_at_ast() {
        let mut db = Db::new();
        let f = db.add_source(THREE_FN);
        assert!(db.compile(f).is_valid());
        db.set_source(f, format!("{THREE_FN}\n// trailing comment\n"));
        assert!(db.compile(f).is_valid());
        assert!(evald_defs(&db, hir_key).is_empty());
        assert!(evald_defs(&db, types_key).is_empty());
        assert!(evald_defs(&db, mir_key).is_empty());
    }

    /// Acceptance D: changing a callee's *contract* propagates — the
    /// ownership fixpoint re-runs and callers' MIR re-lowers even
    /// though the caller's own body never changed.
    #[test]
    fn contract_change_propagates_to_caller() {
        // `sink` is declared last so editing its body cannot shift
        // `main`'s item spans — main's HIR/types must stay memoized.
        let src = "\
data P { x: i32; }
fn main() -> i32 { let q = P { x: 1 }; return sink(q); }
fn sink(p: P) -> i32 { return p.x; }";
        let mut db = Db::new();
        let f = db.add_source(src);
        assert!(db.compile(f).is_valid());
        // `let r = p` moves the param → sink's contract borrow → move.
        db.set_source(f, src.replace("return p.x;", "let r = p; return r.x;"));
        assert!(db.compile(f).is_valid());
        let evals = db.last_evaluated();
        for (k, e) in &db.memo {
            eprintln!(
                "entry {:?}: computed_at={} verified_at={} deps={:?}",
                k, e.computed_at, e.verified_at, e.deps
            );
        }
        assert!(evals.contains(&QueryKey::Ownership(FileId::new(0))));
        assert_eq!(evald_defs(&db, hir_key), ["sink"]);
        // Ownership's value changed → mir bodies that read contracts
        // re-lower: sink's body changed, main's contract input did.
        assert_eq!(evald_defs(&db, mir_key), ["main", "sink"]);
    }

    /// Multi-file: editing file B touches nothing in file A.
    #[test]
    fn files_are_independent() {
        let mut db = Db::new();
        let a = db.add_source(THREE_FN);
        let b = db.add_source("fn other() -> i32 { return 0; }");
        assert!(db.compile(a).is_valid());
        assert!(db.compile(b).is_valid());
        db.set_source(b, "fn other() -> i32 { return 1; }");
        assert!(db.compile(b).is_valid());
        for key in db.last_evaluated() {
            assert_eq!(key.file(), FileId::new(1));
        }
        assert!(db.compile(a).is_valid());
        assert_eq!(db.last_evaluated(), Vec::new());
    }

    use ontixa_source::{DefKey, FileId};
}
