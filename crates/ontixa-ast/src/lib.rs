//! The canonical AST.
//!
//! Pipeline position:
//!
//! ```text
//! GreenNode (lossless CST) ──▶ lower::lower_module ──▶ AstModule
//! ```
//!
//! The AST drops syntax-only noise (parens, separator tokens), normalizes
//! statements vs. tail expressions, and keeps a [`Span`] on every node.
//! It is serializable for `ontixa ast --json`.

mod ast;
mod lower;
mod rebase;

pub use ast::{
    AstModule, BinOp, Block, DataDecl, Expr, Field, FieldInit, FnDecl, Ident, Item, Literal,
    MatchArm, Param, Path, Pattern, Place, Stmt, TypeExpr, UnOp, UseDecl, Variant,
};
pub use lower::lower_module;
pub use rebase::rebase_item;

/// Parses `src` end-to-end (lex → parse → lower) and returns the AST
/// plus all diagnostics gathered along the way.
pub fn parse_ast(src: &str) -> (AstModule, ontixa_diagnostics::Diagnostics) {
    let (root, mut diags) = ontixa_syntax::parse_file(src);
    let module = lower_module(&root, &mut diags);
    (module, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(src: &str) -> AstModule {
        let (m, diags) = parse_ast(src);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        m
    }

    #[test]
    fn lowers_functions_and_data() {
        let m = module("data P { x: i32; y: i32; } fn f(a: P) -> i32 { return a.x; }");
        assert_eq!(m.items.len(), 2);
        match &m.items[0] {
            Item::Data(d) => {
                assert_eq!(d.name.name, "P");
                assert_eq!(d.fields.len(), 2);
            }
            _ => panic!("expected data"),
        }
        match &m.items[1] {
            Item::Fn(f) => {
                assert_eq!(f.name.name, "f");
                assert_eq!(f.params.len(), 1);
                assert!(f.ret.is_some());
            }
            _ => panic!("expected fn"),
        }
    }

    #[test]
    fn lowers_tail_expression_vs_statements() {
        let m = module("fn f() -> i32 { let x = 1; x + 1 }");
        match &m.items[0] {
            Item::Fn(f) => {
                assert_eq!(f.body.stmts.len(), 1);
                assert!(f.body.tail.is_some());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn lowers_struct_literal() {
        let m = module("data P { x: i32; } fn f() -> i32 { let p = P { x: 3 }; return p.x; }");
        match &m.items[1] {
            Item::Fn(f) => match &f.body.stmts[0] {
                Stmt::Let {
                    init: Some(Expr::StructLit { name, fields, .. }),
                    ..
                } => {
                    assert_eq!(name.display(), "P");
                    assert_eq!(fields[0].name.name, "x");
                }
                other => panic!("expected struct literal, got {other:?}"),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn lowers_assignment_to_field_place() {
        let m = module("data P { x: i32; } fn f(p: P) -> i32 { p.x = 5; return p.x; }");
        match &m.items[1] {
            Item::Fn(f) => match &f.body.stmts[0] {
                Stmt::Assign { target, .. } => {
                    assert_eq!(target.base.name, "p");
                    assert_eq!(target.fields[0].name, "x");
                }
                other => panic!("expected assign, got {other:?}"),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn binary_precedence_is_nested() {
        let m = module("fn f() -> i32 { return 1 + 2 * 3; }");
        match &m.items[0] {
            Item::Fn(f) => match &f.body.stmts[0] {
                Stmt::Return {
                    value:
                        Some(Expr::Binary {
                            op: BinOp::Add,
                            rhs,
                            ..
                        }),
                    ..
                } => assert!(matches!(**rhs, Expr::Binary { op: BinOp::Mul, .. })),
                other => panic!("expected add, got {other:?}"),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn if_else_chain() {
        let m = module(
            "fn f(x: i32) -> i32 { if x > 0 { return 1; } else if x == 0 { return 0; } else { return 2; } }",
        );
        match &m.items[0] {
            Item::Fn(f) => {
                // A semicolon-free `if` in tail position is the block's
                // value, matching Rust.
                match f.body.tail.as_deref() {
                    Some(Expr::If { else_, .. }) => {
                        assert!(matches!(**else_.as_ref().unwrap(), Expr::If { .. }));
                    }
                    other => panic!("expected if tail, got {other:?}"),
                }
            }
            _ => panic!(),
        }
    }

    #[test]
    fn lowers_index_and_slice() {
        let m = module("fn f(s: str) -> str { return s[0] + s[1..3] + s[2..] + s[..4] + s[..]; }");
        match &m.items[0] {
            Item::Fn(f) => match &f.body.stmts[0] {
                Stmt::Return {
                    value: Some(Expr::Binary { lhs, .. }),
                    ..
                } => {
                    // Leftmost operand of the `+` chain is `s[0]`.
                    let mut cur: &Expr = lhs;
                    loop {
                        match cur {
                            Expr::Index { base, index, .. } => {
                                assert!(matches!(**base, Expr::Var { .. }));
                                assert!(matches!(
                                    **index,
                                    Expr::Literal {
                                        value: Literal::Int(0),
                                        ..
                                    }
                                ));
                                break;
                            }
                            Expr::Binary { lhs: l, .. } => cur = l,
                            other => panic!("expected index, got {other:?}"),
                        }
                    }
                }
                other => panic!("expected return, got {other:?}"),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn lowers_slice_bounds() {
        // `a + b + c` is `(a + b) + c`.
        let m = module("fn f(s: str) -> str { return s[1..] + s[..2] + s[..]; }");
        match &m.items[0] {
            Item::Fn(f) => match &f.body.stmts[0] {
                Stmt::Return {
                    value: Some(Expr::Binary { lhs, rhs, .. }),
                    ..
                } => {
                    // rhs: `s[..]` — both bounds absent.
                    assert!(matches!(
                        **rhs,
                        Expr::Slice {
                            lo: None,
                            hi: None,
                            ..
                        }
                    ));
                    match &**lhs {
                        Expr::Binary {
                            lhs: l2, rhs: r2, ..
                        } => {
                            // `s[1..]`: lower only.
                            assert!(matches!(
                                **l2,
                                Expr::Slice {
                                    lo: Some(_),
                                    hi: None,
                                    ..
                                }
                            ));
                            // `s[..2]`: upper only.
                            assert!(matches!(
                                **r2,
                                Expr::Slice {
                                    lo: None,
                                    hi: Some(_),
                                    ..
                                }
                            ));
                        }
                        other => panic!("expected binary, got {other:?}"),
                    }
                }
                other => panic!("expected return, got {other:?}"),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn index_is_not_assignment_target() {
        let (_, diags) = parse_ast("fn f(s: str) { s[0] = \"x\"; }");
        assert!(diags.has_errors());
    }

    #[test]
    fn lowers_array_literal_and_index() {
        let m = module("fn f() -> i32 { let a = [1, 2, 3]; return a[0]; }");
        match &m.items[0] {
            Item::Fn(f) => {
                match &f.body.stmts[0] {
                    Stmt::Let {
                        init: Some(Expr::ArrayLit { elems, .. }),
                        ..
                    } => assert_eq!(elems.len(), 3),
                    other => panic!("expected array literal, got {other:?}"),
                }
                match &f.body.stmts[1] {
                    Stmt::Return {
                        value: Some(Expr::Index { .. }),
                        ..
                    } => {}
                    other => panic!("expected index, got {other:?}"),
                }
            }
            _ => panic!(),
        }
    }

    #[test]
    fn lowers_array_type_annotation() {
        let m = module("fn f(a: [i64]) -> i32 { let b: [i32] = a[0..]; return 0; }");
        match &m.items[0] {
            Item::Fn(f) => {
                assert!(matches!(f.params[0].ty, TypeExpr::Array { .. }));
                match &f.body.stmts[0] {
                    Stmt::Let {
                        ty: Some(TypeExpr::Array { elem, .. }),
                        init: Some(Expr::Slice { .. }),
                        ..
                    } => assert!(matches!(**elem, TypeExpr::Named { .. })),
                    other => panic!("expected typed let + slice, got {other:?}"),
                }
            }
            _ => panic!(),
        }
    }

    #[test]
    fn lowers_for_over_range() {
        let m = module("fn f() -> i32 { for i in 0..3 { let z = i; } return 0; }");
        match &m.items[0] {
            Item::Fn(f) => match &f.body.stmts[0] {
                Stmt::Expr {
                    expr:
                        Expr::For {
                            var,
                            iter,
                            body,
                            mutable,
                            ..
                        },
                    ..
                } => {
                    assert_eq!(var.name, "i");
                    assert!(!mutable);
                    assert!(matches!(
                        **iter,
                        Expr::Range {
                            lo: Some(_),
                            hi: Some(_),
                            ..
                        }
                    ));
                    assert_eq!(body.stmts.len(), 1);
                }
                other => panic!("expected for, got {other:?}"),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn for_mut_and_array_iterable() {
        let m = module("fn f(a: [i32]) -> i32 { for mut x in a { x = x + 1; } return 0; }");
        match &m.items[0] {
            Item::Fn(f) => match &f.body.stmts[0] {
                Stmt::Expr {
                    expr: Expr::For { mutable, iter, .. },
                    ..
                } => {
                    assert!(*mutable);
                    assert!(matches!(**iter, Expr::Var { .. }));
                }
                other => panic!("expected for, got {other:?}"),
            },
            _ => panic!(),
        }
    }

    /// `-7` used to lower to `Expr::Error` — the prefix operator was
    /// hidden behind leading trivia inside the `PREFIX_EXPR` node and
    /// the unary value silently became `unit`.
    #[test]
    fn unary_operators_lower_through_trivia() {
        let m = module("fn f() -> i32 { let x = -7; let y = !true; return x; }");
        match &m.items[0] {
            Item::Fn(f) => {
                match &f.body.stmts[0] {
                    Stmt::Let {
                        init: Some(Expr::Unary { op: UnOp::Neg, .. }),
                        ..
                    } => {}
                    other => panic!("expected unary neg, got {other:?}"),
                }
                match &f.body.stmts[1] {
                    Stmt::Let {
                        init: Some(Expr::Unary { op: UnOp::Not, .. }),
                        ..
                    } => {}
                    other => panic!("expected unary not, got {other:?}"),
                }
            }
            _ => panic!(),
        }
    }

    #[test]
    fn lowers_data_variants() {
        let m = module("data Option { Some(i32); None; } fn f() -> i32 { return 0; }");
        match &m.items[0] {
            Item::Data(d) => {
                assert!(d.fields.is_empty());
                assert_eq!(d.variants.len(), 2);
                assert_eq!(d.variants[0].name.name, "Some");
                assert_eq!(d.variants[0].payload.len(), 1);
                assert_eq!(d.variants[1].name.name, "None");
                assert!(d.variants[1].payload.is_empty());
            }
            _ => panic!("expected data"),
        }
    }

    #[test]
    fn lowers_match_arms_and_patterns() {
        let m = module(
            "data Option { Some(i32); None; }\nfn f(o: Option) -> i32 { return match o { Option::Some(v) => v, Option::None => 0, _ => 9, }; }",
        );
        match &m.items[1] {
            Item::Fn(f) => match &f.body.stmts[0] {
                Stmt::Return {
                    value:
                        Some(Expr::Match {
                            scrutinee, arms, ..
                        }),
                    ..
                } => {
                    assert!(matches!(**scrutinee, Expr::Var { .. }));
                    assert_eq!(arms.len(), 3);
                    match &arms[0].pat {
                        Pattern::Variant { path, binds, .. } => {
                            assert_eq!(path.display(), "Option::Some");
                            assert_eq!(binds.len(), 1);
                            assert_eq!(binds[0].name, "v");
                        }
                        other => panic!("expected variant pattern, got {other:?}"),
                    }
                    match &arms[2].pat {
                        Pattern::Bind { name } => assert_eq!(name.name, "_"),
                        other => panic!("expected wildcard, got {other:?}"),
                    }
                }
                other => panic!("expected match return, got {other:?}"),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn serializes_to_json() {
        let m = module("fn main() -> i32 { return 42; }");
        let json = serde_json::to_value(&m).expect("serialize");
        assert!(json.get("items").is_some());
    }
}
