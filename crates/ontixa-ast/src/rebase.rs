//! Rebasing an [`Item`]'s spans to item-relative coordinates.
//!
//! `AstModule` keeps file-absolute spans (it is the source of truth
//! for item bases). The incremental engine's per-definition queries
//! work on *item-relative* copies instead: subtracting the item's
//! absolute start makes a lowered body independent of where the item
//! sits in the file, so an edit that merely shifts an item's offset
//! leaves every per-def query result unchanged (early cutoff) and
//! item-local diagnostics stay valid until rebased at collection.

use crate::ast::{
    Block, DataDecl, Expr, Field, FieldInit, FnDecl, Ident, Item, Param, Path, Place, Stmt,
    TypeExpr,
};
use ontixa_source::Span;

/// Returns `item` with every span shifted by `-base`. `base` is
/// normally `item.span().start`, making all spans item-relative.
pub fn rebase_item(item: &Item, base: u32) -> Item {
    match item {
        Item::Data(d) => Item::Data(rebase_data(d, base)),
        Item::Fn(f) => Item::Fn(rebase_fn(f, base)),
    }
}

fn rel(span: Span, base: u32) -> Span {
    span.rel(base)
}

fn ident(i: &Ident, base: u32) -> Ident {
    Ident::new(i.name.clone(), rel(i.span, base))
}

fn path(p: &Path, base: u32) -> Path {
    Path {
        segs: p.segs.iter().map(|s| ident(s, base)).collect(),
        span: rel(p.span, base),
    }
}

fn type_expr(t: &TypeExpr, base: u32) -> TypeExpr {
    TypeExpr {
        path: path(&t.path, base),
    }
}

fn rebase_data(d: &DataDecl, base: u32) -> DataDecl {
    DataDecl {
        name: ident(&d.name, base),
        fields: d
            .fields
            .iter()
            .map(|f| Field {
                name: ident(&f.name, base),
                ty: type_expr(&f.ty, base),
                span: rel(f.span, base),
            })
            .collect(),
        span: rel(d.span, base),
    }
}

fn rebase_fn(f: &FnDecl, base: u32) -> FnDecl {
    FnDecl {
        name: ident(&f.name, base),
        params: f
            .params
            .iter()
            .map(|p| Param {
                name: ident(&p.name, base),
                mutable: p.mutable,
                ty: type_expr(&p.ty, base),
                span: rel(p.span, base),
            })
            .collect(),
        ret: f.ret.as_ref().map(|t| type_expr(t, base)),
        body: block(&f.body, base),
        span: rel(f.span, base),
    }
}

fn block(b: &Block, base: u32) -> Block {
    Block {
        stmts: b.stmts.iter().map(|s| stmt(s, base)).collect(),
        tail: b.tail.as_deref().map(|e| Box::new(expr(e, base))),
        span: rel(b.span, base),
    }
}

fn place(p: &Place, base: u32) -> Place {
    Place {
        base: ident(&p.base, base),
        fields: p.fields.iter().map(|f| ident(f, base)).collect(),
    }
}

fn stmt(s: &Stmt, base: u32) -> Stmt {
    match s {
        Stmt::Let {
            name,
            mutable,
            ty,
            init,
            span,
        } => Stmt::Let {
            name: ident(name, base),
            mutable: *mutable,
            ty: ty.as_ref().map(|t| type_expr(t, base)),
            init: init.as_ref().map(|e| expr(e, base)),
            span: rel(*span, base),
        },
        Stmt::Assign {
            target,
            value,
            span,
        } => Stmt::Assign {
            target: place(target, base),
            value: expr(value, base),
            span: rel(*span, base),
        },
        Stmt::Expr {
            expr: e,
            has_semi,
            span,
        } => Stmt::Expr {
            expr: expr(e, base),
            has_semi: *has_semi,
            span: rel(*span, base),
        },
        Stmt::Return { value, span } => Stmt::Return {
            value: value.as_ref().map(|e| expr(e, base)),
            span: rel(*span, base),
        },
    }
}

fn expr(e: &Expr, base: u32) -> Expr {
    match e {
        Expr::Literal { value, span } => Expr::Literal {
            value: value.clone(),
            span: rel(*span, base),
        },
        Expr::Var { name } => Expr::Var {
            name: ident(name, base),
        },
        Expr::Call { callee, args, span } => Expr::Call {
            callee: path(callee, base),
            args: args.iter().map(|a| expr(a, base)).collect(),
            span: rel(*span, base),
        },
        Expr::Field {
            base: b,
            name,
            span,
        } => Expr::Field {
            base: Box::new(expr(b, base)),
            name: ident(name, base),
            span: rel(*span, base),
        },
        Expr::Index {
            base: b,
            index,
            span,
        } => Expr::Index {
            base: Box::new(expr(b, base)),
            index: Box::new(expr(index, base)),
            span: rel(*span, base),
        },
        Expr::Slice {
            base: b,
            lo,
            hi,
            span,
        } => Expr::Slice {
            base: Box::new(expr(b, base)),
            lo: lo.as_deref().map(|e| Box::new(expr(e, base))),
            hi: hi.as_deref().map(|e| Box::new(expr(e, base))),
            span: rel(*span, base),
        },
        Expr::Binary { op, lhs, rhs, span } => Expr::Binary {
            op: *op,
            lhs: Box::new(expr(lhs, base)),
            rhs: Box::new(expr(rhs, base)),
            span: rel(*span, base),
        },
        Expr::Unary {
            op,
            expr: inner,
            span,
        } => Expr::Unary {
            op: *op,
            expr: Box::new(expr(inner, base)),
            span: rel(*span, base),
        },
        Expr::If {
            cond,
            then,
            else_,
            span,
        } => Expr::If {
            cond: Box::new(expr(cond, base)),
            then: block(then, base),
            else_: else_.as_deref().map(|e| Box::new(expr(e, base))),
            span: rel(*span, base),
        },
        Expr::Block { block: b, span } => Expr::Block {
            block: block(b, base),
            span: rel(*span, base),
        },
        Expr::StructLit { name, fields, span } => Expr::StructLit {
            name: path(name, base),
            fields: fields
                .iter()
                .map(|f| FieldInit {
                    name: ident(&f.name, base),
                    value: expr(&f.value, base),
                    span: rel(f.span, base),
                })
                .collect(),
            span: rel(*span, base),
        },
        Expr::Path { path: p } => Expr::Path {
            path: path(p, base),
        },
        Expr::Error { span } => Expr::Error {
            span: rel(*span, base),
        },
    }
}
