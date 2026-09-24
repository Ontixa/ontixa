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

    // ---- arrays and `for` --------------------------------------------

    /// Runs `main` and extracts an array of integers.
    fn eval_int_array(src: &str) -> Vec<i128> {
        let (v, _m, _h, _i, diags) = run_src(src, "main").expect("run");
        assert!(diags.is_empty(), "{diags:?}");
        match v {
            Value::Array(elems) => elems
                .iter()
                .map(|c| match *c.borrow() {
                    Value::Int(i) => i,
                    ref other => panic!("expected int element, got {other:?}"),
                })
                .collect(),
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn runs_array_literal_index_len() {
        assert_eq!(eval_int("fn main() -> i32 { return [1, 2, 3][1]; }"), 2);
        assert_eq!(
            eval_int("fn main() -> i32 { let a = [4, 5]; return a.len + a[0]; }"),
            6
        );
        assert_eq!(
            eval_int_array("fn main() -> [i32] { let a: [i32] = []; return a; }"),
            Vec::<i128>::new()
        );
        assert_eq!(
            eval_int_array("fn main() -> [i64] { let a: [i64] = [1, 2]; return a; }"),
            vec![1, 2]
        );
        // String elements.
        assert_eq!(
            eval_str("fn main() -> str { let a = [\"x\", \"yz\"]; return a[0] + a[1]; }"),
            "xyz"
        );
    }

    #[test]
    fn runs_array_slices() {
        assert_eq!(
            eval_int_array("fn main() -> [i32] { return [1, 2, 3, 4][1..3]; }"),
            vec![2, 3]
        );
        assert_eq!(
            eval_int_array("fn main() -> [i32] { let a = [1, 2, 3]; return a[1..]; }"),
            vec![2, 3]
        );
        assert_eq!(
            eval_int_array("fn main() -> [i32] { let a = [1, 2, 3]; return a[..2]; }"),
            vec![1, 2]
        );
        assert_eq!(
            eval_int_array("fn main() -> [i32] { let a = [1, 2, 3]; return a[..]; }"),
            vec![1, 2, 3]
        );
        assert_eq!(
            eval_int_array("fn main() -> [i32] { let a = [1, 2, 3]; return a[2..2]; }"),
            Vec::<i128>::new()
        );
    }

    #[test]
    fn array_index_out_of_bounds_traps() {
        for idx in ["2", "0 - 1"] {
            let src = format!("fn main() -> i32 {{ return [1, 2][{idx}]; }}");
            let err = run_src(&src, "main").unwrap_err();
            match err {
                RuntimeError::Trap(m) => assert!(m.contains("out of bounds"), "{m}"),
                other => panic!("expected trap, got {other:?}"),
            }
        }
    }

    #[test]
    fn array_slice_out_of_bounds_traps() {
        for slice in ["1..9", "2..1", "0 - 1..2"] {
            let src = format!("fn main() -> [i32] {{ return [1, 2, 3][{slice}]; }}");
            let err = run_src(&src, "main").unwrap_err();
            match err {
                RuntimeError::Trap(m) => assert!(m.contains("out of bounds"), "{m}"),
                other => panic!("expected trap, got {other:?}"),
            }
        }
    }

    #[test]
    fn runs_for_over_range() {
        assert_eq!(
            eval_int("fn main() -> i32 { let mut t = 0; for i in 0..5 { t = t + i; } return t; }"),
            10
        );
        // `..hi` starts at 0; `lo..hi` is half-open.
        assert_eq!(
            eval_int("fn main() -> i32 { let mut t = 0; for i in ..3 { t = t + 1; } return t; }"),
            3
        );
        assert_eq!(
            eval_int("fn main() -> i32 { let mut t = 0; for i in 2..5 { t = t + 1; } return t; }"),
            3
        );
        // A typed bound carries its integer type into the loop var;
        // a literal bound adopts it.
        assert_eq!(
            eval_int(
                "fn main() -> i64 { let lo: i64 = 1; let mut t: i64 = 0; for i in lo..4 { t = t + i; } return t; }"
            ),
            6
        );
    }

    #[test]
    fn for_empty_iterables_run_zero_times() {
        assert_eq!(
            eval_int(
                "fn main() -> i32 { for i in 3..3 { return i; } for i in 5..2 { return i; } return 7; }"
            ),
            7
        );
        assert_eq!(
            eval_int("fn main() -> i32 { let a: [i32] = []; for x in a { return x; } return 9; }"),
            9
        );
    }

    #[test]
    fn runs_for_over_array() {
        assert_eq!(
            eval_int(
                "fn main() -> i32 { let a = [3, 4, 5]; let mut t = 0; for x in a { t = t + x; } return t; }"
            ),
            12
        );
        // The var is a fresh element each iteration.
        assert_eq!(
            eval_int(
                "fn main() -> i32 { let mut last = 0; for x in [7, 8, 9] { last = x; } return last; }"
            ),
            9
        );
        // `for mut` allows rebinding the var inside the body.
        assert_eq!(
            eval_int(
                "fn main() -> i32 { let mut t = 0; for mut x in [1, 2] { x = x * 10; t = t + x; } return t; }"
            ),
            30
        );
    }

    /// The iterable is evaluated once and snapshotted: reassigning the
    /// array inside the body does not change what the loop visits.
    #[test]
    fn for_snapshots_the_iterable() {
        assert_eq!(
            eval_int(
                "fn main() -> i32 { let mut a = [1, 2, 3]; let mut n = 0; for x in a { n = n + 1; a = [9]; } return n * 10 + a[0]; }"
            ),
            39
        );
        // Range bounds are fixed before the first iteration too.
        assert_eq!(
            eval_int(
                "fn main() -> i32 { let mut hi = 3; let mut n = 0; for i in 0..hi { n = n + 1; hi = 0; } return n; }"
            ),
            3
        );
    }

    #[test]
    fn nested_loops_and_early_return() {
        assert_eq!(
            eval_int(
                "fn main() -> i32 { let mut t = 0; for i in 0..2 { for j in 0..2 { t = t + i * 10 + j; } } return t; }"
            ),
            // i=0: 0+1; i=1: 10+11 → 22
            22
        );
        assert_eq!(
            eval_int(
                "fn main() -> i32 { for x in [5, 6, 7] { if x == 6 { return x; } } return 0; }"
            ),
            6
        );
    }

    #[test]
    fn runs_arrays_of_structs() {
        let src = "data P { x: i32; }
                   fn main() -> i32 { let ps = [P { x: 1 }, P { x: 2 }]; let mut t = 0; for p in ps { t = t + p.x; } return t + ps.len; }";
        assert_eq!(eval_int(src), 5);
    }

    /// An array param written through `mut` is a `borrow_mut` — the
    /// callee's reassignment is visible to the caller.
    #[test]
    fn array_borrow_mut_writes_through() {
        let src = "fn reset(mut a: [i32]) { a = [9]; }
                   fn main() -> i32 { let mut ps = [1]; reset(ps); return ps[0]; }";
        assert_eq!(eval_int(src), 9);
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

    // ---- enum variants and `match` ----------------------------------

    #[test]
    fn runs_variant_construction_and_match() {
        let src = "data Opt { Some(i32); None; }
                   fn unwrap_or(o: Opt, fallback: i32) -> i32 {
                       return match o { Opt::Some(v) => v, Opt::None => fallback };
                   }
                   fn main() -> i32 {
                       return unwrap_or(Opt::Some(40), 0) + unwrap_or(Opt::None, 2);
                   }";
        assert_eq!(eval_int(src), 42);
    }

    #[test]
    fn match_dispatches_on_discriminant_and_binds_positionally() {
        let src = "data Shape { Circle(i32); Rect(i32, i32); Point; }
                   fn area(s: Shape) -> i32 {
                       return match s {
                           Shape::Circle(r) => 3 * r * r,
                           Shape::Rect(w, h) => w * h,
                           Shape::Point => 0,
                       };
                   }
                   fn main() -> i32 {
                       return area(Shape::Circle(2)) + area(Shape::Rect(3, 4)) + area(Shape::Point);
                   }";
        assert_eq!(eval_int(src), 24);
    }

    #[test]
    fn match_reads_but_does_not_move_scrutinee() {
        // Matching borrows the scrutinee: `o` stays live and can be
        // matched again — and passed on afterwards.
        let src = "data Opt { Some(i32); None; }
                   fn unwrap(o: Opt) -> i32 {
                       return match o { Opt::Some(v) => v, Opt::None => 0 };
                   }
                   fn main() -> i32 {
                       let o = Opt::Some(5);
                       let a = match o { Opt::Some(v) => v, Opt::None => 0 };
                       let b = match o { Opt::Some(v) => v + 1, Opt::None => 1 };
                       return a + b;
                   }";
        assert_eq!(eval_int(src), 11);
    }

    #[test]
    fn whole_scrutinee_binding_is_an_independent_copy() {
        // `other` binds a copy of the scrutinee — it is usable while
        // `o` remains live, and the two never alias.
        let src = "data Opt { Some(i32); None; }
                   fn main() -> i32 {
                       let o = Opt::Some(9);
                       let p = match o { other => other };
                       let a = match o { Opt::Some(v) => v, Opt::None => 0 };
                       let b = match p { Opt::Some(v) => v, Opt::None => 1 };
                       return a + b;
                   }";
        assert_eq!(eval_int(src), 18);
    }

    #[test]
    fn variant_display_renders_constructor_form() {
        let src = "data Opt { Some(i32); None; }
                   fn main() -> Opt { return Opt::Some(3); }";
        let (v, mir, module, interner, diags) = run_src(src, "main").expect("run");
        assert!(diags.is_empty(), "{diags:?}");
        let interp = Interp::new(&mir, &module, &interner);
        assert_eq!(interp.show(&v), "Opt::Some(3)");
    }

    #[test]
    fn unit_variant_display_has_no_parens() {
        let src = "data Opt { Some(i32); None; }
                   fn main() -> Opt { return Opt::None; }";
        let (v, mir, module, interner, diags) = run_src(src, "main").expect("run");
        assert!(diags.is_empty(), "{diags:?}");
        let interp = Interp::new(&mir, &module, &interner);
        assert_eq!(interp.show(&v), "Opt::None");
    }

    #[test]
    fn match_on_nested_variants() {
        // A payload can itself be an enum — matching is per-level.
        let src = "data Opt { Some(i32); None; }
                   data OptOpt { Outer(Opt); Empty; }
                   fn deep(o: OptOpt) -> i32 {
                       return match o {
                           OptOpt::Outer(inner) => match inner {
                               Opt::Some(v) => v,
                               Opt::None => 0,
                           },
                           OptOpt::Empty => 1,
                       };
                   }
                   fn main() -> i32 {
                       return deep(OptOpt::Outer(Opt::Some(41))) + deep(OptOpt::Empty);
                   }";
        assert_eq!(eval_int(src), 42);
    }

    #[test]
    fn match_arm_with_return_diverges() {
        // An arm that returns never reaches the join — the function's
        // result is the surviving arm's.
        let src = "data Opt { Some(i32); None; }
                   fn f(o: Opt) -> i32 {
                       match o { Opt::Some(v) => { return v; }, Opt::None => { return 7; } }
                   }
                   fn main() -> i32 { return f(Opt::None); }";
        assert_eq!(eval_int(src), 7);
    }
}
