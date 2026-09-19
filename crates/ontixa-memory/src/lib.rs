//! Ownership, borrow, move, and escape inference — with no syntax.
//!
//! Pipeline position:
//!
//! ```text
//! HirModule + TypeTables ──▶ infer_ownership ──▶ OwnershipTables
//! ```
//!
//! Every parameter of every function gets an inferred
//! [`ParamBehavior`] contract (`copy`, `borrow`, `borrow_mut`,
//! `move`, `escape`, `unknown`) that call sites enforce: a `borrow`
//! argument stays usable, a `move`/`escape`/`unknown` argument is
//! consumed. Use-after-move and uninitialized reads are diagnosed in
//! the same pass.

mod analyze;
mod behavior;

pub use analyze::{FactStamps, OwnershipOracle, OwnershipTables, ParamFacts, infer_ownership};
pub use behavior::ParamBehavior;

/// Full pipeline convenience: parse → HIR → type check → ownership
/// inference. Returns everything downstream passes need.
pub fn analyze_src(
    src: &str,
) -> (
    ontixa_hir::HirModule,
    ontixa_types::ModuleTypes,
    OwnershipTables,
    ontixa_source::Interner,
    ontixa_diagnostics::Diagnostics,
) {
    let (module, tables, interner, mut diags) = ontixa_types::check_src(src);
    let ownership = infer_ownership(
        &module,
        &tables,
        &interner,
        &mut diags,
        None,
        &mut OwnershipOracle::default(),
    );
    (module, tables, ownership, interner, diags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ontixa_source::DefId;

    /// Runs the full pipeline; asserts no diagnostics; returns the
    /// inferred contract of function `f`'s parameter `idx`.
    fn behavior(src: &str, f: &str, idx: usize) -> ParamBehavior {
        let (m, _, own, mut interner, diags) = analyze_src(src);
        assert!(diags.is_empty(), "{diags:?}");
        let def: DefId = m.scope.fns[&interner.intern(f)];
        own.contract(def)[idx]
    }

    fn codes(src: &str) -> Vec<ontixa_diagnostics::Code> {
        let (_, _, _, _, diags) = analyze_src(src);
        diags.iter().map(|d| d.code).collect()
    }

    // ---------- the acceptance gate ----------

    #[test]
    fn infers_read_only_borrow() {
        let b = behavior(
            "data P { x: i32; } fn read(p: P) -> i32 { return p.x; } fn main() -> i32 { return 0; }",
            "read",
            0,
        );
        assert_eq!(b, ParamBehavior::Borrow);
    }

    #[test]
    fn infers_mutable_borrow() {
        let b = behavior(
            "data P { x: i32; } fn bump(mut p: P) -> i32 { p.x = p.x + 1; return p.x; } fn main() -> i32 { return 0; }",
            "bump",
            0,
        );
        assert_eq!(b, ParamBehavior::BorrowMut);
    }

    #[test]
    fn infers_move_for_consumed_param() {
        let b = behavior(
            "data P { x: i32; } fn eat(p: P) -> i32 { return 0; } fn main() -> i32 { return 0; }",
            "eat",
            0,
        );
        assert_eq!(b, ParamBehavior::Move);
    }

    #[test]
    fn infers_escape_for_returned_param() {
        let b = behavior(
            "data P { x: i32; } fn id(p: P) -> P { return p; } fn main() -> i32 { return 0; }",
            "id",
            0,
        );
        assert_eq!(b, ParamBehavior::Escape);
    }

    #[test]
    fn infers_escape_through_struct_construction() {
        let b = behavior(
            "data P { x: i32; } data W { p: P; } fn wrap(p: P) -> W { return W { p: p }; } fn main() -> i32 { return 0; }",
            "wrap",
            0,
        );
        assert_eq!(b, ParamBehavior::Escape);
    }

    #[test]
    fn infers_copy_for_primitives() {
        let b = behavior(
            "fn add(x: i32, y: i32) -> i32 { return x + y; } fn main() -> i32 { return 0; }",
            "add",
            0,
        );
        assert_eq!(b, ParamBehavior::Copy);
        let b = behavior(
            "fn add(x: i32, y: i32) -> i32 { return x + y; } fn main() -> i32 { return 0; }",
            "add",
            1,
        );
        assert_eq!(b, ParamBehavior::Copy);
    }

    #[test]
    fn infers_move_when_passed_to_moving_callee() {
        let b = behavior(
            "data P { x: i32; } fn eat(p: P) -> i32 { return 0; } fn relay(p: P) -> i32 { return eat(p); } fn main() -> i32 { return 0; }",
            "relay",
            0,
        );
        assert_eq!(b, ParamBehavior::Move);
    }

    #[test]
    fn infers_borrow_when_passed_to_borrowing_callee() {
        let b = behavior(
            "data P { x: i32; } fn read(p: P) -> i32 { return p.x; } fn relay(p: P) -> i32 { return read(p); } fn main() -> i32 { return 0; }",
            "relay",
            0,
        );
        assert_eq!(b, ParamBehavior::Borrow);
    }

    #[test]
    fn infers_escape_through_escaping_callee() {
        let b = behavior(
            "data P { x: i32; } fn id(p: P) -> P { return p; } fn twice(p: P) -> P { return id(p); } fn main() -> i32 { return 0; }",
            "twice",
            0,
        );
        assert_eq!(b, ParamBehavior::Escape);
    }

    #[test]
    fn borrow_arg_stays_usable() {
        let c = codes(
            "data P { x: i32; } fn read(p: P) -> i32 { return p.x; } fn main() -> i32 { let q = P { x: 1 }; let a = read(q); let b = read(q); return a + b; }",
        );
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn moved_arg_is_consumed() {
        let c = codes(
            "data P { x: i32; } fn eat(p: P) -> i32 { return 0; } fn main() -> i32 { let q = P { x: 1 }; let a = eat(q); let b = eat(q); return a + b; }",
        );
        assert!(c.contains(&ontixa_diagnostics::Code::UseAfterMove), "{c:?}");
    }

    #[test]
    fn escaping_arg_is_consumed() {
        let c = codes(
            "data P { x: i32; } fn id(p: P) -> P { return p; } fn main() -> i32 { let q = P { x: 1 }; let a = id(q); let b = id(q); return b.x; }",
        );
        assert!(c.contains(&ontixa_diagnostics::Code::UseAfterMove), "{c:?}");
    }

    #[test]
    fn use_after_move_let() {
        let c = codes(
            "data P { x: i32; } fn main() -> i32 { let q = P { x: 1 }; let r = q; return q.x + r.x; }",
        );
        assert!(c.contains(&ontixa_diagnostics::Code::UseAfterMove), "{c:?}");
    }

    #[test]
    fn moved_local_reinitializes_on_assign() {
        let c = codes(
            "data P { x: i32; } fn main() -> i32 { let mut q = P { x: 1 }; let r = q; q = P { x: 2 }; return q.x + r.x; }",
        );
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn conditional_move_is_flagged() {
        let c = codes(
            "data P { x: i32; } fn eat(p: P) -> i32 { return 0; } fn main() -> i32 { let q = P { x: 1 }; if q.x > 0 { let a = eat(q); } return q.x; }",
        );
        assert!(c.contains(&ontixa_diagnostics::Code::UseAfterMove), "{c:?}");
    }

    #[test]
    fn unconditional_move_in_both_branches_is_definite() {
        let c = codes(
            "data P { x: i32; } fn eat(p: P) -> i32 { return 0; } fn main() -> i32 { let q = P { x: 1 }; if q.x > 0 { let a = eat(q); } else { let b = eat(q); } return q.x; }",
        );
        assert!(c.contains(&ontixa_diagnostics::Code::UseAfterMove), "{c:?}");
    }

    #[test]
    fn uninitialized_read_is_flagged() {
        let c = codes("fn main() -> i32 { let x: i32; return x; }");
        assert!(
            c.contains(&ontixa_diagnostics::Code::Uninitialized),
            "{c:?}"
        );
    }

    #[test]
    fn borrow_mut_arg_stays_usable() {
        let c = codes(
            "data P { x: i32; } fn bump(mut p: P) -> i32 { p.x = p.x + 1; return p.x; } fn main() -> i32 { let mut q = P { x: 1 }; let a = bump(q); return a + q.x; }",
        );
        assert!(c.is_empty(), "{c:?}");
    }

    // ---------- immutability by default ----------

    #[test]
    fn immutable_local_reassignment_is_rejected() {
        let c = codes(
            "data P { x: i32; } fn main() -> i32 { let q = P { x: 1 }; q = P { x: 2 }; return q.x; }",
        );
        assert!(
            c.contains(&ontixa_diagnostics::Code::ImmutableAssignment),
            "{c:?}"
        );
    }

    #[test]
    fn mutable_local_reassignment_is_allowed() {
        let c = codes(
            "data P { x: i32; } fn main() -> i32 { let mut q = P { x: 1 }; q = P { x: 2 }; return q.x; }",
        );
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn immutable_field_mutation_is_rejected() {
        let c = codes(
            "data P { x: i32; } fn main() -> i32 { let p = P { x: 1 }; p.x = 2; return p.x; }",
        );
        assert!(
            c.contains(&ontixa_diagnostics::Code::ImmutableAssignment),
            "{c:?}"
        );
    }

    #[test]
    fn mutable_field_mutation_is_allowed() {
        let c = codes(
            "data P { x: i32; } fn main() -> i32 { let mut p = P { x: 1 }; p.x = 2; return p.x; }",
        );
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn immutable_param_mutation_is_rejected() {
        let c = codes(
            "data P { x: i32; } fn bump(p: P) -> i32 { p.x = 2; return p.x; } fn main() -> i32 { return 0; }",
        );
        assert!(
            c.contains(&ontixa_diagnostics::Code::ImmutableAssignment),
            "{c:?}"
        );
    }

    #[test]
    fn mutable_param_mutation_is_allowed() {
        let c = codes(
            "data P { x: i32; } fn bump(mut p: P) -> i32 { p.x = 2; return p.x; } fn main() -> i32 { return 0; }",
        );
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn borrow_mut_of_immutable_binding_is_rejected() {
        let c = codes(
            "data P { x: i32; } fn bump(mut p: P) -> i32 { p.x = 2; return p.x; } fn main() -> i32 { let q = P { x: 1 }; return bump(q); }",
        );
        assert!(
            c.contains(&ontixa_diagnostics::Code::MutableBorrowOfImmutable),
            "{c:?}"
        );
    }

    #[test]
    fn borrow_mut_of_mutable_binding_is_allowed() {
        let c = codes(
            "data P { x: i32; } fn bump(mut p: P) -> i32 { p.x = 2; return p.x; } fn main() -> i32 { let mut q = P { x: 1 }; return bump(q); }",
        );
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn deferred_init_of_immutable_binding_is_allowed() {
        // `let x: T; x = v;` writes once — initialization, not mutation.
        let c = codes("fn main() -> i32 { let x: i32; x = 7; return x; }");
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn second_assign_to_immutable_binding_is_rejected() {
        let c = codes("fn main() -> i32 { let x: i32; x = 7; x = 8; return x; }");
        assert!(
            c.contains(&ontixa_diagnostics::Code::ImmutableAssignment),
            "{c:?}"
        );
    }

    #[test]
    fn borrow_mut_of_temporary_is_allowed() {
        // A fresh value passed to a mutating callee is contained —
        // no caller authority needed.
        let c = codes(
            "data P { x: i32; } fn bump(mut p: P) -> i32 { p.x = 2; return p.x; } fn main() -> i32 { return bump(P { x: 1 }); }",
        );
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn escape_dominates_move() {
        // p is both moved into `id` and returned — escape wins.
        let b = behavior(
            "data P { x: i32; } fn f(p: P) -> P { let q = p; return q; } fn main() -> i32 { return 0; }",
            "f",
            0,
        );
        assert_eq!(b, ParamBehavior::Escape);
    }

    // ---------- fixpoint convergence (no round cap) ----------

    /// Builds `data P` + a call chain `f0 -> f1 -> ... -> f_{n-1}`.
    /// `tail_body` is the body of the last function; every earlier
    /// function is `fn fK(p: P) -> P { return f_{K+1}(p); }`.
    fn chain(n: usize, tail_body: &str) -> String {
        let mut src = String::from("data P { x: i32; } ");
        for i in 0..n - 1 {
            src.push_str(&format!("fn f{i}(p: P) -> P {{ return f{}(p); }} ", i + 1));
        }
        src.push_str(&format!("fn f{}(p: P) -> P {{ {tail_body} }}", n - 1));
        src.push_str(" fn main() -> i32 { return 0; }");
        src
    }

    #[test]
    fn deep_chain_32_propagates_escape() {
        let src = chain(32, "return p;");
        assert_eq!(behavior(&src, "f0", 0), ParamBehavior::Escape);
    }

    #[test]
    fn deep_chain_100_propagates_escape() {
        // Would have collapsed to `Unknown` under the old 16-round cap.
        let src = chain(100, "return p;");
        assert_eq!(behavior(&src, "f0", 0), ParamBehavior::Escape);
        assert_eq!(behavior(&src, "f42", 0), ParamBehavior::Escape);
    }

    #[test]
    fn deep_chain_100_propagates_move() {
        // The tail drops `p` and returns a fresh P — so p is consumed
        // (Move) at every level, not escaped.
        let src = chain(100, "return P { x: 0 };");
        assert_eq!(behavior(&src, "f0", 0), ParamBehavior::Move);
        assert_eq!(behavior(&src, "f50", 0), ParamBehavior::Move);
        assert_eq!(behavior(&src, "f99", 0), ParamBehavior::Move);
    }

    #[test]
    fn direct_recursion_converges() {
        let b = behavior(
            "data P { x: i32; } fn f(p: P) -> i32 { return f(p); } fn main() -> i32 { return 0; }",
            "f",
            0,
        );
        // f only forwards p to itself — never reads/mutates/moves it.
        assert_eq!(b, ParamBehavior::Borrow);
    }

    #[test]
    fn mutual_recursion_converges() {
        let src = "data P { x: i32; } fn f(p: P) -> i32 { return g(p); } fn g(p: P) -> i32 { return f(p); } fn main() -> i32 { return 0; }";
        assert_eq!(behavior(src, "f", 0), ParamBehavior::Borrow);
        assert_eq!(behavior(src, "g", 0), ParamBehavior::Borrow);
    }

    #[test]
    fn recursion_with_escape_converges() {
        // The base case returns p — so p escapes through the cycle.
        let src = "data P { x: i32; } fn f(p: P, c: bool) -> P { if c { p } else { f(p, false) } } fn main() -> i32 { return 0; }";
        assert_eq!(behavior(src, "f", 0), ParamBehavior::Escape);
    }

    #[test]
    fn multiple_sccs_converge() {
        // Two independent recursion clusters plus a chain into them.
        let src = "data P { x: i32; } \
                   fn a1(p: P) -> i32 { return a2(p); } \
                   fn a2(p: P) -> i32 { return a1(p); } \
                   fn b1(p: P) -> i32 { return b2(p); } \
                   fn b2(p: P) -> i32 { return b1(p); } \
                   fn entry(p: P) -> i32 { return a1(p) + b1(p); } \
                   fn main() -> i32 { return 0; }";
        for f in ["a1", "a2", "b1", "b2", "entry"] {
            assert_eq!(behavior(src, f, 0), ParamBehavior::Borrow, "{f}");
        }
    }

    #[test]
    fn fixpoint_is_deterministic() {
        let src = chain(48, "return p;");
        let b0 = behavior(&src, "f0", 0);
        let b1 = behavior(&src, "f0", 0);
        assert_eq!(b0, b1);
        assert_eq!(b0, ParamBehavior::Escape);
    }
}
