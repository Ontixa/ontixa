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
                   fn main() -> i32 { let p = P { x: 1 }; p.x = 9; return p.x; }";
        assert_eq!(eval_int(src), 9);
    }

    #[test]
    fn borrow_mut_mutates_caller_storage() {
        // `bump` writes through its parameter — the inference layer
        // must classify `p` as `borrow_mut`, and the interpreter must
        // share the caller's cell so the write is visible.
        let src = "data P { x: i32; }
                   fn bump(p: P) { p.x = p.x + 1; }
                   fn main() -> i32 { let q = P { x: 41 }; bump(q); return q.x; }";
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
}
