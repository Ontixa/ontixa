use ontixa_hir::BinOp;
use ontixa_mir::{Const, MirStmt, Operand, Rvalue, Terminator, mir_src};
use ontixa_types::Ty;

#[test]
fn short_circuit_places_rhs_call_only_on_required_branch() {
    for (op, skip) in [("&&", false), ("||", true)] {
        let src = format!(
            "fn rhs() -> bool {{ return true; }} fn main(a: bool) -> bool {{ return a {op} rhs(); }}"
        );
        let (mir, hir, _, _, mut interner, diags) = mir_src(&src);
        assert!(diags.is_empty(), "{diags:?}");
        let main = hir.scope.root_env().fns[&interner.intern("main")];
        let body = mir.body(main).unwrap();
        let Terminator::Branch { then, else_, .. } = &body.blocks[0].term else {
            panic!("logical operator must branch: {:?}", body.blocks);
        };
        let (skip_bb, rhs_bb) = if skip {
            (*then, *else_)
        } else {
            (*else_, *then)
        };
        let skipped = &body.blocks[skip_bb.0 as usize];
        let executed = &body.blocks[rhs_bb.0 as usize];
        assert!(executed.stmts.iter().any(|s| matches!(
            s,
            MirStmt::Assign {
                val: Rvalue::Call { .. },
                ..
            }
        )));
        assert!(!skipped.stmts.iter().any(|s| matches!(
            s,
            MirStmt::Assign {
                val: Rvalue::Call { .. },
                ..
            }
        )));
        let result = skipped
            .stmts
            .iter()
            .find_map(|s| match s {
                MirStmt::Assign {
                    dst,
                    val: Rvalue::Use(Operand::Const(Const::Bool(b))),
                } if *b == skip => Some(dst),
                _ => None,
            })
            .expect("skipped branch produces its bool result");
        assert_eq!(body.locals[result.local.0 as usize].ty, Ty::Bool);
        assert!(
            executed.stmts.iter().any(
                |s| matches!(s, MirStmt::Assign { dst, val: Rvalue::Use(_) } if dst == result)
            )
        );
        assert!(
            matches!((&skipped.term, &executed.term), (Terminator::Goto { target: a }, Terminator::Goto { target: b }) if a == b)
        );
        assert!(!body.blocks.iter().flat_map(|b| &b.stmts).any(|s| matches!(
            s,
            MirStmt::Assign {
                val: Rvalue::Binary {
                    op: BinOp::And | BinOp::Or,
                    ..
                },
                ..
            }
        )));
    }
}

#[test]
fn short_circuit_returning_rhs_keeps_return_terminator() {
    let src = "fn main(a: bool) -> bool { return a && { return false; true }; }";
    let (mir, hir, _, _, mut interner, diags) = mir_src(src);
    assert!(diags.is_empty(), "{diags:?}");
    let main = hir.scope.root_env().fns[&interner.intern("main")];
    let body = mir.body(main).unwrap();
    let Terminator::Branch { then, .. } = body.blocks[0].term else {
        panic!("branch")
    };
    assert!(matches!(
        body.blocks[then.0 as usize].term,
        Terminator::Return {
            value: Operand::Const(Const::Bool(false))
        }
    ));
}
