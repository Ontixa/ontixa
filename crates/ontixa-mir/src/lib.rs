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
        let main = m.scope.fns[&interner.intern("main")];
        let body = mir.body(main).expect("main body");
        assert_eq!(body.blocks.len(), 1);
        assert!(matches!(body.blocks[0].term, Terminator::Return(_)));
        // Params 0 + locals x,y + temp for the binary.
        assert!(body.locals.len() >= 3);
    }

    #[test]
    fn lowers_if_to_branch_diamond() {
        let (mir, m, mut interner) =
            mir("fn main() -> i32 { let x = if true { 1 } else { 2 }; return x; }");
        let main = m.scope.fns[&interner.intern("main")];
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
        let main = m.scope.fns[&interner.intern("main")];
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
}
