//! Reference interpreter — executes typed Ontixa MIR.
//!
//! Pipeline position:
//!
//! ```text
//! MirModule + HirModule ──▶ Interp::run("main") ──▶ Value
//! ```
//!
//! Storage is cell-based ([`Cell`]): every local and every struct field
//! is an `Rc<RefCell<Value>>`. Call arguments whose inferred contract
//! is `borrow`/`borrow_mut` pass the caller's cell, so a callee's
//! writes through a mutable borrow are real mutations of caller state.
//! All other positions receive a fresh cell — move/copy semantics were
//! already enforced by `ontixa-memory`, so a shallow copy is faithful.
//!
//! This interpreter is the milestone-1 semantics oracle: the same MIR
//! will later feed code generation, and this executor defines what the
//! generated code must compute.

mod interp;
mod value;

pub use interp::{Interp, RuntimeError};
pub use value::{Cell, Value};

/// Full pipeline: parse → … → MIR → execute `main`. Returns the
/// produced artifacts alongside the result so callers can introspect.
pub fn run_src(
    src: &str,
    entry: &str,
) -> Result<
    (
        Value,
        ontixa_mir::MirModule,
        ontixa_hir::HirModule,
        ontixa_source::Interner,
        ontixa_diagnostics::Diagnostics,
    ),
    RuntimeError,
> {
    let (mir, module, _tables, _ownership, interner, diags) = ontixa_mir::mir_src(src);
    let interp = Interp::new(&mir, &module, &interner);
    let v = interp.run(entry)?;
    Ok((v, mir, module, interner, diags))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `main` and extracts an integer result.
    fn eval_int(src: &str) -> i128 {
        let (v, _m, _h, _i, diags) = run_src(src, "main").expect("run");
        assert!(diags.is_empty(), "{diags:?}");
        match v {
            Value::Int(i) => i,
            other => panic!("expected int, got {other:?}"),
        }
    }

    /// Runs `main` and extracts a string result.
    fn eval_str(src: &str) -> String {
        let (v, _m, _h, _i, diags) = run_src(src, "main").expect("run");
        assert!(diags.is_empty(), "{diags:?}");
        match v {
            Value::Str(s) => s.to_string(),
            other => panic!("expected str, got {other:?}"),
        }
    }

    #[test]
    fn runs_literal_return() {
        assert_eq!(eval_int("fn main() -> i32 { return 42; }"), 42);
    }

    #[test]
    fn runs_locals_and_arithmetic() {
        assert_eq!(
            eval_int("fn main() -> i32 { let x = 6; let y = x * 7 + 1; return y; }"),
            43
        );
    }

    #[test]
    fn runs_calls() {
        assert_eq!(
            eval_int(
                "fn double(x: i32) -> i32 { return x * 2; } fn main() -> i32 { return double(21); }"
            ),
            42
        );
    }

    #[test]
    fn runs_branches() {
        let src = "fn pick(c: bool) -> i32 { return if c { 1 } else { 2 }; }
                   fn main() -> i32 { return pick(3 > 2); }";
        assert_eq!(eval_int(src), 1);
    }

    #[test]
    fn runs_structs_and_fields() {
        let src = "data P { x: i32; y: i32; }
                   fn main() -> i32 { let p = P { x: 3, y: 4 }; return p.x + p.y; }";
        assert_eq!(eval_int(src), 7);
    }

    #[test]
    fn field_assign_writes_through() {
        let src = "data P { x: i32; }
                   fn main() -> i32 { let mut p = P { x: 1 }; p.x = 9; return p.x; }";
        assert_eq!(eval_int(src), 9);
    }

    #[test]
    fn borrow_mut_mutates_caller_storage() {
        // `bump` writes through its parameter — the inference layer
        // must classify `p` as `borrow_mut`, and the interpreter must
        // share the caller's cell so the write is visible.
        let src = "data P { x: i32; }
                   fn bump(mut p: P) { p.x = p.x + 1; }
                   fn main() -> i32 { let mut q = P { x: 41 }; bump(q); return q.x; }";
        let (v, _m, _h, _i, diags) = run_src(src, "main").expect("run");
        assert!(diags.is_empty(), "{diags:?}");
        match v {
            Value::Int(i) => assert_eq!(i, 42),
            other => panic!("expected int, got {other:?}"),
        }
    }

    #[test]
    fn move_contract_does_not_alias() {
        // `keep` moves `p` and returns it — `q` is dead after the
        // call (compile-time enforced). The callee's param is a fresh
        // cell, so returning it does not expose caller storage.
        let src = "data P { x: i32; }
                   fn keep(p: P) -> P { return p; }
                   fn main() -> i32 { let q = P { x: 5 }; let r = keep(q); return r.x; }";
        let (v, _m, _h, _i, diags) = run_src(src, "main").expect("run");
        assert!(diags.is_empty(), "{diags:?}");
        match v {
            Value::Int(i) => assert_eq!(i, 5),
            other => panic!("expected int, got {other:?}"),
        }
    }

    #[test]
    fn copied_ints_stay_usable() {
        let src = "fn add(a: i32, b: i32) -> i32 { return a + b; }
                   fn main() -> i32 { let x = 1; let y = add(x, x); return y + x; }";
        assert_eq!(eval_int(src), 3);
    }

    #[test]
    fn missing_main_is_runtime_error() {
        let err = run_src("fn f() -> i32 { return 0; }", "main").unwrap_err();
        assert!(matches!(err, RuntimeError::MissingEntry(_)));
    }

    #[test]
    fn div_by_zero_traps() {
        let err = run_src("fn main() -> i32 { return 1 / 0; }", "main").unwrap_err();
        match err {
            RuntimeError::Trap(m) => assert!(m.contains("division")),
            other => panic!("expected trap, got {other:?}"),
        }
    }

    // ---- strings -----------------------------------------------------

    #[test]
    fn runs_string_concat_len_and_order() {
        assert_eq!(
            eval_str("fn main() -> str { return \"ab\" + \"cd\"; }"),
            "abcd"
        );
        assert_eq!(eval_int("fn main() -> i32 { return \"héllo\".len; }"), 5);
        // Lexicographic order on Unicode scalars.
        assert_eq!(
            eval_int("fn main() -> i32 { return if \"abc\" < \"abd\" { 1 } else { 0 }; }"),
            1
        );
        assert_eq!(
            eval_int("fn main() -> i32 { return if \"b\" >= \"a\" { 1 } else { 0 }; }"),
            1
        );
    }

    #[test]
    fn runs_string_index_and_slice() {
        assert_eq!(eval_str("fn main() -> str { return \"héllo\"[1]; }"), "é");
        assert_eq!(
            eval_str("fn main() -> str { return \"abcdef\"[1..4]; }"),
            "bcd"
        );
        assert_eq!(
            eval_str("fn main() -> str { return \"abcdef\"[3..]; }"),
            "def"
        );
        assert_eq!(
            eval_str("fn main() -> str { return \"abcdef\"[..2]; }"),
            "ab"
        );
        assert_eq!(eval_str("fn main() -> str { return \"abc\"[..]; }"), "abc");
        // Empty ranges are valid.
        assert_eq!(eval_str("fn main() -> str { return \"abc\"[1..1]; }"), "");
    }

    #[test]
    fn string_index_out_of_bounds_traps() {
        let err = run_src("fn main() -> str { return \"ab\"[2]; }", "main").unwrap_err();
        match err {
            RuntimeError::Trap(m) => assert!(m.contains("out of bounds"), "{m}"),
            other => panic!("expected trap, got {other:?}"),
        }
        let err = run_src("fn main() -> str { return \"ab\"[0 - 1]; }", "main").unwrap_err();
        match err {
            RuntimeError::Trap(m) => assert!(m.contains("out of bounds"), "{m}"),
            other => panic!("expected trap, got {other:?}"),
        }
    }

    #[test]
    fn string_slice_out_of_bounds_traps() {
        for slice in ["1..9", "2..1", "0 - 1..2"] {
            let src = format!("fn main() -> str {{ return \"abc\"[{slice}]; }}");
            let err = run_src(&src, "main").unwrap_err();
            match err {
                RuntimeError::Trap(m) => assert!(m.contains("out of bounds"), "{m}"),
                other => panic!("expected trap, got {other:?}"),
            }
        }
    }

    // ---- contract oracle --------------------------------------------
    //
    // The oracle's traps are unreachable through valid source — the
    // static pass rejects violations first. They exist so a *wrong*
    // contract (compiler bug, hand-built MIR) fails loudly at runtime
    // instead of silently corrupting memory. Tests therefore forge the
    // bad contract in MIR directly, or run rejected source.

    /// Rewrites every `Call` contract in `def`'s body with `f`.
    fn tamper_contracts(
        mir: &mut ontixa_mir::MirModule,
        def: ontixa_source::DefId,
        f: impl Fn(&mut Vec<ontixa_memory::ParamBehavior>),
    ) {
        use ontixa_mir::{MirStmt, Rvalue};
        let body = mir.fns[def.index()].as_mut().expect("body");
        for b in &mut body.blocks {
            for s in &mut b.stmts {
                let rv = match s {
                    MirStmt::Assign { val, .. } | MirStmt::Eval { val } => val,
                };
                if let Rvalue::Call { contract, .. } = rv {
                    f(contract);
                }
            }
        }
    }

    #[test]
    fn oracle_traps_write_through_shared_borrow() {
        // `bump` writes through `p` — inference says `borrow_mut`.
        // Lie about it (`borrow`) and the callee's write must trap.
        let src = "data P { x: i32; }
                   fn bump(mut p: P) { p.x = p.x + 1; }
                   fn main() -> i32 { let mut q = P { x: 1 }; bump(q); return q.x; }";
        let (mut mir, module, _t, _o, interner, diags) = ontixa_mir::mir_src(src);
        assert!(diags.is_empty(), "{diags:?}");
        let main = module.scope.root_env().fns[&interner.get("main").unwrap()];
        tamper_contracts(&mut mir, main, |c| {
            c[0] = ontixa_memory::ParamBehavior::Borrow
        });
        let err = Interp::new(&mir, &module, &interner)
            .run("main")
            .unwrap_err();
        match err {
            RuntimeError::Trap(m) => assert!(m.contains("borrow contract"), "{m}"),
            other => panic!("expected trap, got {other:?}"),
        }
    }

    #[test]
    fn oracle_detects_borrow_write_into_nested_field() {
        // The oracle's structural equality must see writes nested
        // inside struct fields, not just whole-cell swaps.
        let src = "data P { x: i32; }
                   data Q { p: P; }
                   fn bump(mut q: Q) { q.p.x = q.p.x + 1; }
                   fn main() -> i32 { let mut q = Q { p: P { x: 1 } }; bump(q); return q.p.x; }";
        let (mut mir, module, _t, _o, interner, diags) = ontixa_mir::mir_src(src);
        assert!(diags.is_empty(), "{diags:?}");
        let main = module.scope.root_env().fns[&interner.get("main").unwrap()];
        tamper_contracts(&mut mir, main, |c| {
            c[0] = ontixa_memory::ParamBehavior::Borrow
        });
        let err = Interp::new(&mir, &module, &interner)
            .run("main")
            .unwrap_err();
        match err {
            RuntimeError::Trap(m) => assert!(m.contains("borrow contract"), "{m}"),
            other => panic!("expected trap, got {other:?}"),
        }
    }

    #[test]
    fn oracle_traps_second_move_from_consumed_local() {
        // `keep` escapes `p` (returns it) — the second call on `q` is
        // E_USE_AFTER_MOVE statically. The runtime agrees: the cell is
        // poisoned after the first call and the second traps.
        let src = "data P { x: i32; }
                   fn keep(p: P) -> P { return p; }
                   fn main() -> i32 { let q = P { x: 1 }; let a = keep(q); let b = keep(q); return a.x + b.x; }";
        let (mir, module, _t, _o, interner, diags) = ontixa_mir::mir_src(src);
        assert!(!diags.is_empty(), "expected use-after-move diagnostic");
        let err = Interp::new(&mir, &module, &interner)
            .run("main")
            .unwrap_err();
        match err {
            RuntimeError::Trap(m) => assert!(m.contains("already-consumed"), "{m}"),
            other => panic!("expected trap, got {other:?}"),
        }
    }

    #[test]
    fn oracle_traps_read_of_moved_local() {
        // After a consuming call, *any* later read of the caller's
        // cell — not just another call — must trap on the hole.
        let src = "data P { x: i32; }
                   fn keep(p: P) -> P { return p; }
                   fn main() -> i32 { let q = P { x: 1 }; let a = keep(q); return a.x + q.x; }";
        let (mir, module, _t, _o, interner, diags) = ontixa_mir::mir_src(src);
        assert!(!diags.is_empty(), "expected use-after-move diagnostic");
        let err = Interp::new(&mir, &module, &interner)
            .run("main")
            .unwrap_err();
        match err {
            RuntimeError::Trap(m) => assert!(m.contains("moved-out"), "{m}"),
            other => panic!("expected trap, got {other:?}"),
        }
    }
}
