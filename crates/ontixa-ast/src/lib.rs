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
    AstModule, BinOp, Block, DataDecl, Expr, Field, FieldInit, FnDecl, Ident, Item, Literal, Param,
    Place, Stmt, TypeExpr, UnOp,
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
                    assert_eq!(name.name, "P");
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
    fn serializes_to_json() {
        let m = module("fn main() -> i32 { return 42; }");
        let json = serde_json::to_value(&m).expect("serialize");
        assert!(json.get("items").is_some());
    }
}
