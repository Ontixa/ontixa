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

pub use analyze::{OwnershipTables, infer_ownership};
pub use behavior::ParamBehavior;

/// Full pipeline convenience: parse → HIR → type check → ownership
/// inference. Returns everything downstream passes need.
pub fn analyze_src(
    src: &str,
) -> (
    ontixa_hir::HirModule,
    ontixa_types::TypeTables,
    OwnershipTables,
    ontixa_source::Interner,
    ontixa_diagnostics::Diagnostics,
) {
    let (module, tables, interner, mut diags) = ontixa_types::check_src(src);
    let ownership = infer_ownership(&module, &tables, &interner, &mut diags);
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
            "data P { x: i32; } fn bump(p: P) -> i32 { p.x = p.x + 1; return p.x; } fn main() -> i32 { return 0; }",
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
            "data P { x: i32; } fn main() -> i32 { let q = P { x: 1 }; let r = q; q = P { x: 2 }; return q.x + r.x; }",
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
            "data P { x: i32; } fn bump(p: P) -> i32 { p.x = p.x + 1; return p.x; } fn main() -> i32 { let q = P { x: 1 }; let a = bump(q); return a + q.x; }",
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
}
