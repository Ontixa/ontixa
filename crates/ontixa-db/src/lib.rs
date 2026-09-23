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
mod patch;
mod query;
mod rename;

pub use db::{Artifacts, CheckReport, Db, StageTiming};
pub use patch::{PatchEdit, PatchError, PatchOp, PatchPlan, PatchReport};
pub use query::{CheckedBody, QueryKey, QueryStats};
pub use rename::{RenameEdit, RenameError, RenamePlan, RenameReport, RenameTarget};

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
        assert!(evals.contains(&QueryKey::Ownership(FileId::new(0))));
        assert_eq!(evald_defs(&db, hir_key), ["sink"]);
        // Ownership's value changed → mir bodies that read contracts
        // re-lower: sink's body changed, main's contract input did.
        assert_eq!(evald_defs(&db, mir_key), ["main", "sink"]);
    }

    /// Check-vs-build split: `check` demands `Diagnostics` only —
    /// no graph, no MIR, no artifact assembly. This is the evidence
    /// that validation never pays for codegen-facing work.
    #[test]
    fn check_does_not_build_graph_or_mir() {
        let mut db = Db::new();
        let f = db.add_source(THREE_FN);
        let report = db.check(f);
        assert!(report.is_valid());
        let evals = db.last_evaluated();
        assert!(!evals.is_empty());
        for key in &evals {
            assert!(
                !matches!(
                    key,
                    QueryKey::Graph(_) | QueryKey::MirBody(_) | QueryKey::Compile(_)
                ),
                "check evaluated {key:?}"
            );
        }
        assert!(!db.stats().executed.contains_key("graph"));
        assert!(!db.stats().executed.contains_key("mir"));
        assert!(!db.stats().executed.contains_key("assemble"));
    }

    /// `check` is not a weaker pass — it reports exactly the
    /// diagnostics `compile` collects, in the same sorted order.
    #[test]
    fn check_reports_same_diagnostics_as_compile() {
        let src = "fn main() -> i32 { return nope; }";
        let mut db = Db::new();
        let f = db.add_source(src);
        let report = db.check(f);
        assert!(report.diags.has_errors());
        let codes: Vec<_> = report.diags.iter().map(|d| d.code).collect();
        let mut db2 = Db::new();
        let f2 = db2.add_source(src);
        let a = db2.compile(f2);
        let codes2: Vec<_> = a.diags.iter().map(|d| d.code).collect();
        assert_eq!(codes, codes2);
    }

    /// After `check`, `compile` evaluates only the missing tail —
    /// graph + mir + assemble. Frontend work stays memoized.
    #[test]
    fn check_then_compile_evaluates_only_backend() {
        let mut db = Db::new();
        let f = db.add_source(THREE_FN);
        assert!(db.check(f).is_valid());
        assert!(db.compile(f).is_valid());
        let evals = db.last_evaluated();
        assert!(!evals.is_empty(), "compile must build graph+mir");
        for key in &evals {
            assert!(
                matches!(
                    key,
                    QueryKey::Graph(_) | QueryKey::MirBody(_) | QueryKey::Compile(_)
                ),
                "compile re-ran frontend query {key:?}"
            );
        }
    }

    // ---- item-relative spans ---------------------------------------

    fn ast_item_key(k: &QueryKey) -> Option<DefKey> {
        match k {
            QueryKey::AstItem(k) => Some(*k),
            _ => None,
        }
    }

    /// An edit that only *shifts* an item's absolute offset — here a
    /// comment inserted before `g` — recomputes its `AstItem` to an
    /// equal (item-relative) value, so the whole per-definition chain
    /// (HIR → types → MIR) and the ownership fixpoint stay memoized.
    /// With file-absolute spans this edit rebuilt `g` and `main`.
    #[test]
    fn offset_shift_reuses_shifted_bodies() {
        let mut db = Db::new();
        let f = db.add_source(THREE_FN);
        assert!(db.compile(f).is_valid());
        db.set_source(f, THREE_FN.replace("fn g(q", "// shifted\nfn g(q"));
        assert!(db.compile(f).is_valid());
        // AstItems re-verified (their Ast dep changed) but recomputed
        // equal — that's exactly the early-cutoff boundary.
        assert_eq!(evald_defs(&db, ast_item_key), ["f", "g", "main"]);
        assert!(evald_defs(&db, hir_key).is_empty());
        assert!(evald_defs(&db, types_key).is_empty());
        assert!(evald_defs(&db, mir_key).is_empty());
        assert!(
            !db.last_evaluated()
                .contains(&QueryKey::Ownership(FileId::new(0)))
        );
    }

    /// Tagged item-relative diagnostics are rebased with the
    /// *current* item bases at collection time — a shifted function's
    /// diagnostic lands on its new absolute offset, identical to what
    /// a fresh compile of the shifted source produces.
    #[test]
    fn diagnostics_track_shifted_offsets() {
        let bad = "fn ok() -> i32 { return 0; }\nfn tail() -> i32 { return nope; }";
        let mut db = Db::new();
        let f = db.add_source(bad);
        let report = db.check(f);
        assert!(report.diags.iter().all(|d| d.origin.is_none()));
        let before: Vec<_> = report.diags.iter().map(|d| (d.code, d.primary)).collect();
        assert!(!before.is_empty());
        let prefix = "// shifted\n";
        let shifted = format!("{prefix}{bad}");
        db.set_source(f, shifted.clone());
        let after: Vec<_> = db
            .check(f)
            .diags
            .iter()
            .map(|d| (d.code, d.primary))
            .collect();
        let bump = prefix.len() as u32;
        let expected: Vec<_> = before
            .iter()
            .map(|&(c, p)| (c, p.map(|s| s.abs(bump))))
            .collect();
        assert_eq!(expected, after);
        // Incremental result equals a from-scratch compile of the
        // shifted source — rebasing can't drift.
        let mut db2 = Db::new();
        let f2 = db2.add_source(shifted);
        let fresh: Vec<_> = db2
            .check(f2)
            .diags
            .iter()
            .map(|d| (d.code, d.primary))
            .collect();
        assert_eq!(after, fresh);
    }

    /// Secondary labels rebase with the same base: the "value moved
    /// here" label must land on the shifted move site.
    #[test]
    fn diagnostic_labels_track_shifted_offsets() {
        let src = "data P { x: i32; } fn main() -> i32 { let q = P { x: 1 }; let r = q; return q.x + r.x; }";
        let mut db = Db::new();
        let f = db.add_source(src);
        let before: Vec<Span> = db
            .check(f)
            .diags
            .iter()
            .flat_map(|d| d.labels.iter().map(|l| l.span))
            .collect();
        assert!(!before.is_empty());
        let prefix = "// pad pad\n";
        db.set_source(f, format!("{prefix}{src}"));
        let after: Vec<Span> = db
            .check(f)
            .diags
            .iter()
            .flat_map(|d| d.labels.iter().map(|l| l.span))
            .collect();
        let bump = prefix.len() as u32;
        let expected: Vec<Span> = before.iter().map(|s| s.abs(bump)).collect();
        assert_eq!(expected, after);
    }

    /// The graph's node spans are file-absolute for consumers —
    /// they must track a shift too.
    #[test]
    fn graph_spans_track_shifted_offsets() {
        let prefix = "// shifted\n";
        let mut db = Db::new();
        let f = db.add_source(THREE_FN);
        let before: Vec<Option<Span>> = db.compile(f).graph.nodes.iter().map(|n| n.span).collect();
        db.set_source(f, format!("{prefix}{THREE_FN}"));
        let after: Vec<Option<Span>> = db.compile(f).graph.nodes.iter().map(|n| n.span).collect();
        assert_eq!(before.len(), after.len());
        let bump = prefix.len() as u32;
        for (b, a) in before.iter().zip(&after) {
            assert_eq!(*a, b.map(|s| s.abs(bump)), "node span");
        }
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

    // ---- determinism -------------------------------------------------

    const DIVERSE: &str = "\
data P { x: i32; y: i32; }
fn read(p: P) -> i32 { return p.x + p.y; }
fn bump(mut p: P) { p.x = p.x + 1; }
fn keep(p: P) -> P { return p; }
fn main() -> i32 { let mut q = P { x: 1, y: 2 }; bump(q); let r = keep(q); return read(r); }";

    /// Two independent `Db` sessions compiling the same source must
    /// produce byte-identical serialized artifacts — no HashMap
    /// iteration order, pointer, or timing leakage into the output.
    #[test]
    fn artifacts_are_byte_deterministic_across_sessions() {
        let compile_once = || {
            let mut db = Db::new();
            let f = db.add_source(DIVERSE);
            db.compile_owned(f)
        };
        let a = compile_once();
        let b = compile_once();
        assert_eq!(
            serde_json::to_string(&a.ast).unwrap(),
            serde_json::to_string(&b.ast).unwrap(),
            "AST"
        );
        assert!(a.module == b.module, "HIR");
        assert_eq!(
            serde_json::to_string(&a.graph).unwrap(),
            serde_json::to_string(&b.graph).unwrap(),
            "SPG"
        );
        assert_eq!(
            serde_json::to_string(&a.mir).unwrap(),
            serde_json::to_string(&b.mir).unwrap(),
            "MIR"
        );
        let diags = |d: &ontixa_diagnostics::Diagnostics| {
            d.iter()
                .map(|x| (x.code, x.message.clone(), x.primary, x.labels.len()))
                .collect::<Vec<_>>()
        };
        assert_eq!(diags(&a.diags), diags(&b.diags), "diagnostics");
    }

    /// An edit followed by a revert to byte-identical text re-runs
    /// exactly the edited body's chain — the memo holds the edited
    /// state, so the revert is a genuine change, but a bounded one.
    /// And the result must equal a from-scratch compile: incremental
    /// recomputation can never produce different artifacts than a
    /// clean build would.
    #[test]
    fn revert_produces_from_scratch_artifacts() {
        let mut db = Db::new();
        let f = db.add_source(DIVERSE);
        let original = serde_json::to_string(&db.compile(f).graph).unwrap();
        db.set_source(f, DIVERSE.replace("x + p.y", "x * p.y"));
        assert!(db.compile(f).is_valid());
        db.set_source(f, DIVERSE);
        assert!(db.compile(f).is_valid());
        // Bounded invalidation: only the edited def's chain re-ran.
        assert_eq!(evald_defs(&db, hir_key), ["read"]);
        assert_eq!(evald_defs(&db, mir_key), ["read"]);
        // And the reverted graph is byte-identical to the original's.
        assert_eq!(
            serde_json::to_string(&db.compile(f).graph).unwrap(),
            original
        );
    }

    /// Diagnostic order is part of the deterministic contract:
    /// repeated compiles of the same invalid source emit the same
    /// sequence, and `--json` consumers get a stable `code` stream.
    #[test]
    fn diagnostics_order_is_deterministic() {
        let src = "\
fn main() -> i32 { let x = 1; x = 2; return nope; let q = y; }";
        let mut db = Db::new();
        let f = db.add_source(src);
        let first: Vec<_> = db
            .compile(f)
            .diags
            .iter()
            .map(|d| (d.code, d.message.clone()))
            .collect();
        assert!(!first.is_empty());
        let mut db2 = Db::new();
        let f2 = db2.add_source(src);
        let second: Vec<_> = db2
            .compile(f2)
            .diags
            .iter()
            .map(|d| (d.code, d.message.clone()))
            .collect();
        assert_eq!(first, second);
    }

    use ontixa_source::{DefKey, FileId, Span};
}
