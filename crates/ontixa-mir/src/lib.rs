//! Typed MIR — the execution-facing IR.
//!
//! Pipeline position:
//!
//! ```text
//! HirModule + TypeTables + OwnershipTables ──▶ lower_mir ──▶ MirModule
//! ```
//!
//! MIR is a small CFG per function: basic blocks, temporaries,
//! resolved field projections, and `Return`/`Branch`/`Goto`
//! terminators. Inferred parameter contracts travel on `Call`
//! rvalues so the executor can share caller storage for
//! `borrow`/`borrow_mut` arguments.

mod lower;
mod mir;

pub use lower::{lower_fn, lower_mir};
pub use mir::{
    BasicBlock, BlockId, Const, Local, LocalDecl, MirBody, MirModule, MirStmt, Operand, Place,
    Rvalue, Terminator,
};

/// Full pipeline convenience: parse → … → ownership → MIR.
pub fn mir_src(
    src: &str,
) -> (
    MirModule,
    ontixa_hir::HirModule,
    ontixa_types::ModuleTypes,
    ontixa_memory::OwnershipTables,
    ontixa_source::Interner,
    ontixa_diagnostics::Diagnostics,
) {
    let (module, tables, ownership, interner, diags) = ontixa_memory::analyze_src(src);
    let mir = lower_mir(&module, &tables, &ownership);
    (mir, module, tables, ownership, interner, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mir(src: &str) -> (MirModule, ontixa_hir::HirModule, ontixa_source::Interner) {
        let (mir, m, _, _, interner, diags) = mir_src(src);
        assert!(diags.is_empty(), "{diags:?}");
        (mir, m, interner)
    }

    #[test]
    fn lowers_straight_line_fn() {
        let (mir, m, mut interner) =
            mir("fn main() -> i32 { let x = 1; let y = x + 1; return y; }");
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = mir.body(main).expect("main body");
        assert_eq!(body.blocks.len(), 1);
        assert!(matches!(body.blocks[0].term, Terminator::Return { .. }));
        // Params 0 + locals x,y + temp for the binary.
        assert!(body.locals.len() >= 3);
    }

    #[test]
    fn lowers_if_to_branch_diamond() {
        let (mir, m, mut interner) =
            mir("fn main() -> i32 { let x = if true { 1 } else { 2 }; return x; }");
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = mir.body(main).expect("body");
        assert!(
            body.blocks
                .iter()
                .any(|b| matches!(b.term, Terminator::Branch { .. }))
        );
    }

    #[test]
    fn call_carries_contract() {
        let (mir, m, mut interner) = mir(
            "data P { x: i32; } fn read(p: P) -> i32 { return p.x; } fn main() -> i32 { let q = P { x: 1 }; return read(q); }",
        );
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = mir.body(main).expect("body");
        let call = body
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .find_map(|s| match s {
                MirStmt::Assign {
                    val: Rvalue::Call { contract, .. },
                    ..
                } => Some(contract.clone()),
                _ => None,
            })
            .expect("call");
        assert_eq!(call[0], ontixa_memory::ParamBehavior::Borrow);
    }

    #[test]
    fn lowers_string_ops_to_typed_rvalues() {
        let (mir, m, mut interner) = mir(
            "fn main() -> i32 { let s = \"abc\"; let c = s[0]; let t = s[..s.len]; return c.len + t.len; }",
        );
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = mir.body(main).expect("body");
        let rvalues: Vec<&Rvalue> = body
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .map(|s| match s {
                MirStmt::Assign { val, .. } | MirStmt::Eval { val } => val,
            })
            .collect();
        assert!(
            rvalues.iter().any(|r| matches!(r, Rvalue::Index { .. })),
            "no Index rvalue in {rvalues:?}"
        );
        // `s[..s.len]`: a Slice with no lo and a Len-computed hi.
        assert!(
            rvalues.iter().any(|r| matches!(
                r,
                Rvalue::Slice {
                    lo: None,
                    hi: Some(_),
                    ..
                }
            )),
            "no open-lo Slice rvalue in {rvalues:?}"
        );
        assert_eq!(
            rvalues
                .iter()
                .filter(|r| matches!(r, Rvalue::Len { .. }))
                .count(),
            3,
            "expected three Len rvalues in {rvalues:?}"
        );
    }

    #[test]
    fn locals_carry_source_mutability() {
        let (mir, m, mut interner) = mir(
            "fn f(mut a: i32, b: i32) -> i32 { let mut x = a; let y = b; x = x + y; return x; }",
        );
        let f = m.scope.root_env().fns[&interner.intern("f")];
        let body = mir.body(f).expect("body");
        // Params come first: `mut a` writable, `b` not.
        assert!(body.locals[0].mutable);
        assert!(!body.locals[1].mutable);
        // Bindings follow in declaration order: `mut x`, then `y`.
        assert!(body.locals[2].mutable, "let mut x");
        assert!(!body.locals[3].mutable, "let y");
        // Compiler temporaries are always writable internally.
        assert!(
            body.locals
                .iter()
                .filter(|l| l.sym.is_none())
                .all(|l| l.mutable)
        );
    }

    // ---- arrays and `for` -------------------------------------------

    #[test]
    fn lowers_array_ops_to_typed_rvalues() {
        let (mir, m, mut interner) = mir(
            // `a[..1]` consumes `a` (slice produces a fresh array),
            // so `a.len` is read before the slice.
            "fn main() -> i32 { let a = [1, 2]; let x = a[0]; let n = a.len; let s = a[..1]; return x + n + s.len; }",
        );
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = mir.body(main).expect("body");
        let rvalues: Vec<&Rvalue> = body
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .map(|s| match s {
                MirStmt::Assign { val, .. } | MirStmt::Eval { val } => val,
            })
            .collect();
        assert!(
            rvalues.iter().any(|r| matches!(
                r,
                Rvalue::ArrayLit { elems } if elems.len() == 2
            )),
            "no ArrayLit rvalue in {rvalues:?}"
        );
        assert!(
            rvalues.iter().any(|r| matches!(r, Rvalue::Index { .. })),
            "no Index rvalue in {rvalues:?}"
        );
        assert!(
            rvalues.iter().any(|r| matches!(r, Rvalue::Slice { .. })),
            "no Slice rvalue in {rvalues:?}"
        );
        // `a.len` twice (one via `a.len`, one via `s.len`).
        assert_eq!(
            rvalues
                .iter()
                .filter(|r| matches!(r, Rvalue::Len { .. }))
                .count(),
            2,
            "expected two Len rvalues in {rvalues:?}"
        );
    }

    /// `for` compiles to a counted loop: a head block that branches on
    /// `counter < limit`, a body that ends in `counter += 1; Goto(head)`,
    /// and an exit block that continues after the loop.
    #[test]
    fn for_range_lowers_to_counted_loop() {
        let (mir, m, mut interner) =
            mir("fn main() -> i32 { let mut t = 0; for i in 0..3 { t = t + i; } return t; }");
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = mir.body(main).expect("body");
        // entry + head + body + exit = at least 4 blocks.
        assert!(body.blocks.len() >= 4, "{:?}", body.blocks);
        // Exactly one Branch — the loop head.
        let head = body
            .blocks
            .iter()
            .find(|b| matches!(b.term, Terminator::Branch { .. }))
            .expect("loop head branch");
        let Terminator::Branch { then, else_, .. } = head.term else {
            unreachable!()
        };
        // The body block ends with `+= 1` then `Goto(head)`.
        let body_bb = &body.blocks[then.0 as usize];
        assert!(
            body_bb.stmts.iter().any(|s| matches!(
                s,
                MirStmt::Assign {
                    val: Rvalue::Binary {
                        op: ontixa_hir::BinOp::Add,
                        rhs: Operand::Const(Const::Int(1)),
                        ..
                    },
                    ..
                }
            )),
            "no counter increment in {body_bb:?}"
        );
        assert!(
            matches!(body_bb.term, Terminator::Goto { target } if target == head.id),
            "body must jump back to the head, got {:?}",
            body_bb.term
        );
        // The exit block falls through to the `return`.
        assert!(
            matches!(
                body.blocks[else_.0 as usize].term,
                Terminator::Return { .. }
            ),
            "exit block should return, got {:?}",
            body.blocks[else_.0 as usize].term
        );
    }

    /// Array iteration snapshots the array into a temp, then counts an
    /// index against a `Len` computed once before the loop.
    #[test]
    fn for_array_snapshots_iterable() {
        let (mir, m, mut interner) = mir(
            "fn main() -> i32 { let a = [1, 2]; let mut t = 0; for x in a { t = t + x; } return t; }",
        );
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = mir.body(main).expect("body");
        let head = body
            .blocks
            .iter()
            .position(|b| matches!(b.term, Terminator::Branch { .. }))
            .expect("loop head");
        // The snapshot copy and the `len` must be assigned *before*
        // the head block.
        let pre: Vec<&Rvalue> = body.blocks[..head]
            .iter()
            .flat_map(|b| &b.stmts)
            .map(|s| match s {
                MirStmt::Assign { val, .. } | MirStmt::Eval { val } => val,
            })
            .collect();
        assert!(
            pre.iter().any(|r| matches!(r, Rvalue::Len { .. })),
            "no snapshot Len before the loop in {pre:?}"
        );
        // The loop variable binds `arr[idx]` inside the body block.
        let Terminator::Branch { then, .. } = body.blocks[head].term else {
            unreachable!()
        };
        let body_bb = &body.blocks[then.0 as usize];
        assert!(
            body_bb.stmts.iter().any(|s| matches!(
                s,
                MirStmt::Assign {
                    val: Rvalue::Index { .. },
                    ..
                }
            )),
            "no element binding in body {body_bb:?}"
        );
    }

    /// A `return` inside a loop body still terminates that block — the
    /// increment/back-edge is emitted only when the body is open.
    #[test]
    fn for_body_with_return_skips_backedge() {
        let (mir, m, mut interner) =
            mir("fn main() -> i32 { for i in 0..3 { return i; } return 0; }");
        let main = m.scope.root_env().fns[&interner.intern("main")];
        let body = mir.body(main).expect("body");
        let head = body
            .blocks
            .iter()
            .find(|b| matches!(b.term, Terminator::Branch { .. }))
            .expect("loop head");
        let Terminator::Branch { then, .. } = head.term else {
            unreachable!()
        };
        assert!(
            matches!(body.blocks[then.0 as usize].term, Terminator::Return { .. }),
            "body should return, got {:?}",
            body.blocks[then.0 as usize].term
        );
    }
}
