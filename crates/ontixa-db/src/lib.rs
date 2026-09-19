//! The Ontixa compiler database.
//!
//! Pipeline position:
//!
//! ```text
//! Db::add_source ──▶ Db::compile ──▶ Artifacts (ast → mir + diags + timings)
//! ```
//!
//! `Db` memoizes each file's full artifact bundle keyed by a content
//! revision: `set_source` bumps the revision, the next `compile`
//! rebuilds. Every stage reports a [`StageTiming`], which powers
//! `ontixa check --timings` and the daemon's latency budget.
//!
//! Milestone-1 invalidation is file-granular; the salsa-style
//! fine-grained query design is ADR-0004.

mod db;

pub use db::{Artifacts, Db, StageTiming};

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
        // Same revision → memoized (no rebuild, same artifacts object).
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
        assert_eq!(
            stages,
            [
                "lex+parse",
                "ast",
                "hir",
                "types",
                "ownership",
                "graph",
                "mir"
            ]
        );
    }

    #[test]
    fn errors_surface_in_artifacts() {
        let mut db = Db::new();
        let f = db.add_source("fn main() -> i32 { return nope; }");
        let a = db.compile(f);
        assert!(!a.is_valid());
        assert!(a.diags.has_errors());
    }
}
