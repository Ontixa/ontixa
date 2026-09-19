//! Lowering from the lossless CST to the canonical [`AstModule`].
//!
//! The parser has already emitted diagnostics for malformed regions;
//! lowering's job is structural interpretation. It skips `ERROR` nodes
//! (their diagnostics exist) and reports only structural problems the
//! parser could not know — e.g. an expression that is not a valid
//! assignment target.

use crate::ast::{
    AstModule, BinOp, Block, DataDecl, Expr, Field, FieldInit, FnDecl, Ident, Item, Literal, Param,
    Place, Stmt, TypeExpr, UnOp,
};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_source::Span;
use ontixa_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

/// Lowers a `SOURCE_FILE` root into an [`AstModule`].
///
/// `diags` receives structural diagnostics discovered during lowering.
pub fn lower_module(root: &SyntaxNode, diags: &mut Diagnostics) -> AstModule {
    let mut l = Lowerer { diags };
    let items = root.children().filter_map(|child| l.item(&child)).collect();
    AstModule {
        items,
        span: root.text_range().into(),
    }
}

struct Lowerer<'a> {
    diags: &'a mut Diagnostics,
}

impl Lowerer<'_> {
    fn error(&mut self, message: impl Into<String>, span: Span) {
        self.diags
            .push(Diagnostic::error(Code::Parse, message).primary(span));
    }

    fn item(&mut self, node: &SyntaxNode) -> Option<Item> {
        match node.kind() {
            SyntaxKind::DATA_DECL => Some(Item::Data(self.data_decl(node))),
            SyntaxKind::FN_DECL => Some(Item::Fn(self.fn_decl(node))),
            SyntaxKind::ERROR => None,
            _ => None,
        }
    }

    /// Text of the single non-trivia token inside a `NAME`/`NAME_REF`
    /// node. Keyword tokens are accepted (a diagnostic already exists).
    fn name_text(&self, node: &SyntaxNode) -> Option<(String, Span)> {
        node.children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .find(|t| !t.kind().is_trivia())
            .map(|t| (t.text().to_string(), t.text_range().into()))
    }

    fn name(&self, node: &SyntaxNode) -> Ident {
        match self.name_text(node) {
            Some((text, span)) => Ident::new(text, span),
            None => Ident::new("<missing>", node.text_range().into()),
        }
    }

    fn first_name(&self, node: &SyntaxNode) -> Ident {
        node.children()
            .find(|n| n.kind() == SyntaxKind::NAME || n.kind() == SyntaxKind::NAME_REF)
            .map(|n| self.name(&n))
            .unwrap_or_else(|| Ident::new("<missing>", node.text_range().into()))
    }

    fn type_ref(&self, node: &SyntaxNode) -> Option<TypeExpr> {
        node.children()
            .find(|n| n.kind() == SyntaxKind::TYPE_REF)
            .map(|n| TypeExpr {
                name: self.first_name(&n),
            })
    }

    fn data_decl(&mut self, node: &SyntaxNode) -> DataDecl {
        let name = self.first_name(node);
        let fields = node
            .children()
            .filter(|n| n.kind() == SyntaxKind::FIELD)
            .filter_map(|f| self.field(&f))
            .collect();
        DataDecl {
            name,
            fields,
            span: node.text_range().into(),
        }
    }

    fn field(&mut self, node: &SyntaxNode) -> Option<Field> {
        let name = self.first_name(node);
        let ty = self.type_ref(node)?;
        Some(Field {
            name,
            ty,
            span: node.text_range().into(),
        })
    }

    fn fn_decl(&mut self, node: &SyntaxNode) -> FnDecl {
        let name = self.first_name(node);
        let params = node
            .children()
            .find(|n| n.kind() == SyntaxKind::PARAM_LIST)
            .map(|list| {
                list.children()
                    .filter(|n| n.kind() == SyntaxKind::PARAM)
                    .filter_map(|p| self.param(&p))
                    .collect()
            })
            .unwrap_or_default();
        let ret = node
            .children()
            .find(|n| n.kind() == SyntaxKind::RET_TYPE)
            .and_then(|n| self.type_ref(&n));
        let body = node
            .children()
            .find(|n| n.kind() == SyntaxKind::BLOCK)
            .map(|b| self.block(&b))
            .unwrap_or_else(|| Block {
                stmts: Vec::new(),
                tail: None,
                span: node.text_range().into(),
            });
        FnDecl {
            name,
            params,
            ret,
            body,
            span: node.text_range().into(),
        }
    }

    fn param(&mut self, node: &SyntaxNode) -> Option<Param> {
        let name = self.first_name(node);
        let ty = self.type_ref(node)?;
        Some(Param {
            name,
            ty,
            span: node.text_range().into(),
        })
    }

    fn block(&mut self, node: &SyntaxNode) -> Block {
        let mut stmts: Vec<Stmt> = Vec::new();
        let mut tail: Option<Box<Expr>> = None;
        let children: Vec<SyntaxNode> = node.children().collect();
        let last = children.len().saturating_sub(1);
        for (i, child) in children.iter().enumerate() {
            match child.kind() {
                SyntaxKind::LET_STMT => {
                    if let Some(s) = self.let_stmt(child) {
                        stmts.push(s);
                    }
                }
                SyntaxKind::ASSIGN_STMT => {
                    if let Some(s) = self.assign_stmt(child) {
                        stmts.push(s);
                    }
                }
                SyntaxKind::RETURN_STMT => {
                    stmts.push(self.return_stmt(child));
                }
                SyntaxKind::EXPR_STMT => {
                    let has_semi = child
                        .children_with_tokens()
                        .filter_map(SyntaxElement::into_token)
                        .any(|t| t.kind() == SyntaxKind::SEMICOLON);
                    if let Some(e) = self.expr_from_stmt(child) {
                        // Only the final expression child, written without
                        // a semicolon, is the block's tail value. A
                        // semicolon-free expression in any earlier
                        // position is a statement whose value is
                        // discarded — e.g. `if c { } return x` must not
                        // promote the `if` past the `return`.
                        if !has_semi && i == last {
                            tail = Some(Box::new(e));
                        } else {
                            let span = child.text_range().into();
                            stmts.push(Stmt::Expr {
                                expr: e,
                                has_semi,
                                span,
                            });
                        }
                    }
                }
                SyntaxKind::ERROR => {}
                _ => {}
            }
        }
        Block {
            stmts,
            tail,
            span: node.text_range().into(),
        }
    }

    fn let_stmt(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        let name = node
            .children()
            .find(|n| n.kind() == SyntaxKind::NAME)
            .map(|n| self.name(&n))?;
        let ty = self.type_ref(node);
        // The initializer is the expression after the `=` token.
        let has_eq = node
            .children_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .any(|t| t.kind() == SyntaxKind::EQ);
        let init = if has_eq {
            node.children()
                .find(|n| is_value_node(n.kind()))
                .and_then(|e| self.expr(&e))
        } else {
            None
        };
        Some(Stmt::Let {
            name,
            ty,
            init,
            span: node.text_range().into(),
        })
    }

    fn assign_stmt(&mut self, node: &SyntaxNode) -> Option<Stmt> {
        // Children: <place expr> `=` <value expr> `;`
        let mut exprs = node.children().filter(|n| is_value_node(n.kind()));
        let target_expr = exprs.next()?;
        let value = exprs.next().and_then(|e| self.expr(&e));
        let target = self.expr_to_place(&target_expr)?;
        Some(Stmt::Assign {
            target,
            value: value?,
            span: node.text_range().into(),
        })
    }

    /// Validates that an expression is a valid assignment target
    /// (`x` or `x.f.g`). Anything else is a structural error.
    fn expr_to_place(&mut self, node: &SyntaxNode) -> Option<Place> {
        match node.kind() {
            SyntaxKind::NAME_REF => Some(Place {
                base: self.name(node),
                fields: Vec::new(),
            }),
            SyntaxKind::FIELD_EXPR => {
                // FIELD_EXPR children: <base expr> `.` NAME_REF
                let mut names = Vec::new();
                let mut current = node.clone();
                loop {
                    let (base, field) = split_field_expr(&current)?;
                    names.push(self.name(&field));
                    match base.kind() {
                        SyntaxKind::FIELD_EXPR => current = base,
                        SyntaxKind::NAME_REF => {
                            names.push(self.name(&base));
                            break;
                        }
                        _ => {
                            self.error("invalid assignment target", current.text_range().into());
                            return None;
                        }
                    }
                }
                names.reverse();
                let base = names.remove(0);
                Some(Place {
                    base,
                    fields: names,
                })
            }
            _ => {
                self.error("invalid assignment target", node.text_range().into());
                None
            }
        }
    }

    fn return_stmt(&mut self, node: &SyntaxNode) -> Stmt {
        let value = node
            .children()
            .find(|n| is_value_node(n.kind()))
            .and_then(|e| self.expr(&e));
        Stmt::Return {
            value,
            span: node.text_range().into(),
        }
    }

    fn expr_from_stmt(&mut self, node: &SyntaxNode) -> Option<Expr> {
        node.children()
            .find(|n| is_value_node(n.kind()))
            .and_then(|e| self.expr(&e))
    }

    fn expr(&mut self, node: &SyntaxNode) -> Option<Expr> {
        let span: Span = node.text_range().into();
        Some(match node.kind() {
            SyntaxKind::LITERAL => {
                let tok = node
                    .children_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .find(|t| !t.kind().is_trivia())?;
                Expr::Literal {
                    value: self.literal(&tok),
                    span,
                }
            }
            SyntaxKind::NAME_REF => Expr::Var {
                name: self.name(node),
            },
            SyntaxKind::CALL_EXPR => self.call_expr(node)?,
            SyntaxKind::FIELD_EXPR => {
                let (base, field) = split_field_expr(node)?;
                Expr::Field {
                    base: Box::new(self.expr(&base)?),
                    name: self.name(&field),
                    span,
                }
            }
            SyntaxKind::BIN_EXPR => self.bin_expr(node)?,
            SyntaxKind::PREFIX_EXPR => {
                let mut elems = node.children_with_tokens();
                let op_tok = elems.find_map(SyntaxElement::into_token)?;
                let operand = elems
                    .filter_map(SyntaxElement::into_node)
                    .find(|n| is_value_node(n.kind()))?;
                let op = match op_tok.kind() {
                    SyntaxKind::MINUS => UnOp::Neg,
                    SyntaxKind::NOT => UnOp::Not,
                    _ => return Some(Expr::Error { span }),
                };
                Expr::Unary {
                    op,
                    expr: Box::new(self.expr(&operand)?),
                    span,
                }
            }
            SyntaxKind::IF_EXPR => self.if_expr(node)?,
            SyntaxKind::PAREN_EXPR => {
                let inner = node.children().find(|n| is_value_node(n.kind()))?;
                return self.expr(&inner); // canonical: parens removed
            }
            SyntaxKind::BLOCK => Expr::Block {
                block: self.block(node),
                span,
            },
            SyntaxKind::STRUCT_LIT => self.struct_lit(node)?,
            SyntaxKind::ERROR => Expr::Error { span },
            _ => return None,
        })
    }

    fn call_expr(&mut self, node: &SyntaxNode) -> Option<Expr> {
        // CALL_EXPR children: <callee expr> ARG_LIST
        let mut callee_node: Option<SyntaxNode> = None;
        let mut args: Vec<Expr> = Vec::new();
        for child in node.children() {
            if child.kind() == SyntaxKind::ARG_LIST {
                for arg in child.children() {
                    if is_value_node(arg.kind()) {
                        if let Some(e) = self.expr(&arg) {
                            args.push(e);
                        }
                    }
                }
            } else if callee_node.is_none() {
                callee_node = Some(child);
            }
        }
        let callee_node = callee_node?;
        if callee_node.kind() != SyntaxKind::NAME_REF {
            self.error(
                "only direct function calls are supported",
                callee_node.text_range().into(),
            );
            return None;
        }
        Some(Expr::Call {
            callee: self.name(&callee_node),
            args,
            span: node.text_range().into(),
        })
    }

    fn bin_expr(&mut self, node: &SyntaxNode) -> Option<Expr> {
        // BIN_EXPR children: <lhs> <op token> <rhs>
        let mut op: Option<BinOp> = None;
        let mut parts: Vec<Expr> = Vec::new();
        for elem in node.children_with_tokens() {
            match elem {
                SyntaxElement::Node(n) => {
                    if is_value_node(n.kind()) {
                        if let Some(e) = self.expr(&n) {
                            parts.push(e);
                        }
                    }
                }
                SyntaxElement::Token(t) => {
                    if !t.kind().is_trivia() {
                        op = Some(token_to_binop(t.kind())?);
                    }
                }
            }
        }
        if parts.len() != 2 {
            return Some(Expr::Error {
                span: node.text_range().into(),
            });
        }
        let rhs = parts.pop()?;
        let lhs = parts.pop()?;
        Some(Expr::Binary {
            op: op?,
            lhs: Box::new(lhs),
            rhs: Box::new(rhs),
            span: node.text_range().into(),
        })
    }

    fn if_expr(&mut self, node: &SyntaxNode) -> Option<Expr> {
        // IF_EXPR children: `if` <cond> BLOCK [`else` (BLOCK | IF_EXPR)]
        let mut cond: Option<SyntaxNode> = None;
        let mut blocks: Vec<SyntaxNode> = Vec::new();
        let mut else_if: Option<SyntaxNode> = None;
        let mut seen_block = false;
        for child in node.children() {
            match child.kind() {
                SyntaxKind::BLOCK => {
                    // A block in condition position (`if { c } { }`)
                    // precedes the then-block.
                    if cond.is_none() {
                        cond = Some(child);
                    } else {
                        blocks.push(child);
                        seen_block = true;
                    }
                }
                // An `else if` is an IF_EXPR appearing *after* the then-block.
                SyntaxKind::IF_EXPR if seen_block => else_if = Some(child),
                k if is_value_node(k) && cond.is_none() => cond = Some(child),
                _ => {}
            }
        }
        let cond_node = cond?;
        let then_node = blocks.first().cloned()?;
        // Else branch: a chained `else if` or a second block.
        let else_ = if let Some(n) = else_if {
            self.expr(&n).map(Box::new)
        } else {
            blocks.get(1).cloned().map(|b| {
                Box::new(Expr::Block {
                    block: self.block(&b),
                    span: b.text_range().into(),
                })
            })
        };
        Some(Expr::If {
            cond: Box::new(self.expr(&cond_node)?),
            then: self.block(&then_node),
            else_,
            span: node.text_range().into(),
        })
    }

    fn struct_lit(&mut self, node: &SyntaxNode) -> Option<Expr> {
        let name = self.first_name(node);
        let fields = node
            .children()
            .filter(|n| n.kind() == SyntaxKind::STRUCT_LIT_FIELD)
            .filter_map(|f| {
                // Children: <field NAME_REF> `:` <value expr>. The field
                // name is itself a NAME_REF, so the value is the first
                // value node *after* it — otherwise `x: 1` would treat
                // `x` as the value.
                let mut values = f.children().filter(|n| is_value_node(n.kind()));
                let name_node = values.next()?;
                if name_node.kind() != SyntaxKind::NAME_REF {
                    return None;
                }
                let fname = self.name(&name_node);
                let value = values.next().and_then(|v| self.expr(&v))?;
                Some(FieldInit {
                    name: fname,
                    value,
                    span: f.text_range().into(),
                })
            })
            .collect();
        Some(Expr::StructLit {
            name,
            fields,
            span: node.text_range().into(),
        })
    }

    fn literal(&mut self, tok: &SyntaxToken) -> Literal {
        match tok.kind() {
            SyntaxKind::TRUE_KW => Literal::Bool(true),
            SyntaxKind::FALSE_KW => Literal::Bool(false),
            SyntaxKind::INT_NUMBER => {
                let cleaned: String = tok.text().chars().filter(|c| *c != '_').collect();
                match cleaned.parse::<i128>() {
                    Ok(v) => Literal::Int(v),
                    Err(_) => {
                        self.error(
                            format!("integer literal `{}` is too large", tok.text()),
                            tok.text_range().into(),
                        );
                        Literal::Int(0)
                    }
                }
            }
            SyntaxKind::FLOAT_NUMBER => {
                let cleaned: String = tok.text().chars().filter(|c| *c != '_').collect();
                match cleaned.parse::<f64>() {
                    Ok(v) => Literal::Float(v),
                    Err(_) => {
                        self.error(
                            format!("invalid float literal `{}`", tok.text()),
                            tok.text_range().into(),
                        );
                        Literal::Float(0.0)
                    }
                }
            }
            SyntaxKind::STRING => Literal::Str(decode_string(tok.text())),
            _ => Literal::Int(0),
        }
    }
}

/// Splits a `FIELD_EXPR` node into (base expr node, field NAME_REF node).
/// The base is the first node child; the field is the NAME_REF after it.
fn split_field_expr(node: &SyntaxNode) -> Option<(SyntaxNode, SyntaxNode)> {
    let mut children = node.children();
    let base = children.next()?;
    let field = children.find(|n| n.kind() == SyntaxKind::NAME_REF)?;
    Some((base, field))
}

/// Whether a node kind can hold a value expression. `NAME_REF` is an
/// expression in value position (a variable reference).
fn is_value_node(kind: SyntaxKind) -> bool {
    is_expr_kind(kind) || kind == SyntaxKind::NAME_REF
}

fn is_expr_kind(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::LITERAL
            | SyntaxKind::CALL_EXPR
            | SyntaxKind::FIELD_EXPR
            | SyntaxKind::BIN_EXPR
            | SyntaxKind::PREFIX_EXPR
            | SyntaxKind::IF_EXPR
            | SyntaxKind::PAREN_EXPR
            | SyntaxKind::BLOCK
            | SyntaxKind::STRUCT_LIT
    )
}

fn token_to_binop(kind: SyntaxKind) -> Option<BinOp> {
    Some(match kind {
        SyntaxKind::PLUS => BinOp::Add,
        SyntaxKind::MINUS => BinOp::Sub,
        SyntaxKind::STAR => BinOp::Mul,
        SyntaxKind::SLASH => BinOp::Div,
        SyntaxKind::PERCENT => BinOp::Rem,
        SyntaxKind::EQ2 => BinOp::Eq,
        SyntaxKind::NEQ => BinOp::Ne,
        SyntaxKind::LT => BinOp::Lt,
        SyntaxKind::LE => BinOp::Le,
        SyntaxKind::GT => BinOp::Gt,
        SyntaxKind::GE => BinOp::Ge,
        SyntaxKind::AND2 => BinOp::And,
        SyntaxKind::OR2 => BinOp::Or,
        _ => return None,
    })
}

/// Decodes a `"..."` token's escapes into the string value.
fn decode_string(text: &str) -> String {
    let inner = text.strip_prefix('"').unwrap_or(text);
    let inner = inner.strip_suffix('"').unwrap_or(inner);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('0') => out.push('\0'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('\'') => out.push('\''),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}
