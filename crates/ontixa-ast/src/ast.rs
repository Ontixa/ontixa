//! Canonical AST node types.
//!
//! The AST is *owned* (not a view over the CST) and *canonical*: parens
//! are removed, statement/expression boundaries are normalized, and every
//! node carries its source [`Span`]. It is the boundary where trivia is
//! left behind — the CST remains available underneath for tooling that
//! needs comments and formatting fidelity.

use ontixa_source::Span;
use serde::Serialize;

/// A source identifier: its text and where it was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ident {
    /// Identifier text.
    pub name: String,
    /// Source location of the identifier token.
    pub span: Span,
}

impl Ident {
    /// Creates an identifier.
    pub fn new(name: impl Into<String>, span: Span) -> Self {
        Self {
            name: name.into(),
            span,
        }
    }
}

/// A `::`-separated name path: `m` or `m::x`. A single segment is a
/// plain (unqualified) name; two segments name a member of a module.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Path {
    /// Segments in written order (`m`, then `x`).
    pub segs: Vec<Ident>,
    /// Span covering the whole path.
    pub span: Span,
}

impl Path {
    /// A one-segment path from a bare identifier.
    pub fn single(name: Ident) -> Self {
        Self {
            span: name.span,
            segs: vec![name],
        }
    }

    /// The display form (`m::x`).
    pub fn display(&self) -> String {
        self.segs
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::")
    }
}

/// A `use` declaration: `use m;` binds a module name; `use m::x as y;`
/// binds one member of a module under `y` (or `x` without `as`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UseDecl {
    /// `m` (module import) or `m::x` (member import).
    pub path: Path,
    /// The `as` alias, when written.
    pub alias: Option<Ident>,
    /// Span of the whole declaration.
    pub span: Span,
}

/// Root of a compilation unit.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AstModule {
    /// `use` declarations in source order.
    pub uses: Vec<UseDecl>,
    /// Top-level items in source order.
    pub items: Vec<Item>,
    /// Whole-file span.
    pub span: Span,
}

/// A top-level item.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub enum Item {
    /// `data Name { ... }`.
    Data(DataDecl),
    /// `fn name(...) { ... }`.
    Fn(FnDecl),
}

impl Item {
    /// The item's whole-declaration span.
    pub fn span(&self) -> Span {
        match self {
            Item::Data(d) => d.span,
            Item::Fn(f) => f.span,
        }
    }
}

/// A `data` declaration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DataDecl {
    /// Declared name.
    pub name: Ident,
    /// Fields in declaration order.
    pub fields: Vec<Field>,
    /// Span of the whole declaration.
    pub span: Span,
}

/// One field inside a `data` declaration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Field {
    /// Field name.
    pub name: Ident,
    /// Declared type.
    pub ty: TypeExpr,
    /// Span of the field entry.
    pub span: Span,
}

/// A function declaration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FnDecl {
    /// Declared name.
    pub name: Ident,
    /// Parameters in order.
    pub params: Vec<Param>,
    /// Declared return type (`None` = unit).
    pub ret: Option<TypeExpr>,
    /// Body block.
    pub body: Block,
    /// Span of the whole declaration.
    pub span: Span,
}

/// A function parameter.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Param {
    /// Parameter name.
    pub name: Ident,
    /// Whether the binding was declared `mut`.
    pub mutable: bool,
    /// Declared type.
    pub ty: TypeExpr,
    /// Span of the parameter.
    pub span: Span,
}

/// A type position: a named type, optionally module-qualified
/// (`m::Name`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TypeExpr {
    /// The type path as written.
    pub path: Path,
}

/// A block: statements plus an optional trailing expression whose value
/// is the block's value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Block {
    /// Statements in order.
    pub stmts: Vec<Stmt>,
    /// Tail expression (the last semicolon-free expression).
    pub tail: Option<Box<Expr>>,
    /// Span of the `{ ... }` range.
    pub span: Span,
}

/// An assignment target: a binding plus field projections (`x`, `x.f`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Place {
    /// Root binding.
    pub base: Ident,
    /// Field projections applied in order.
    pub fields: Vec<Ident>,
}

/// A statement.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub enum Stmt {
    /// `let (mut)? name (: ty)? (= expr)? ;`
    Let {
        /// Binding name.
        name: Ident,
        /// Whether the binding was declared `mut`.
        mutable: bool,
        /// Optional type annotation.
        ty: Option<TypeExpr>,
        /// Optional initializer.
        init: Option<Expr>,
        /// Statement span.
        span: Span,
    },
    /// `place = expr ;`
    Assign {
        /// Target place.
        target: Place,
        /// Assigned value.
        value: Expr,
        /// Statement span.
        span: Span,
    },
    /// An expression evaluated for its value (discarded unless tail).
    Expr {
        /// The expression.
        expr: Expr,
        /// Whether a trailing semicolon was present.
        has_semi: bool,
        /// Statement span.
        span: Span,
    },
    /// `return expr? ;`
    Return {
        /// Optional return value.
        value: Option<Expr>,
        /// Statement span.
        span: Span,
    },
}

/// A `name: expr` pair inside a struct literal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldInit {
    /// Field name.
    pub name: Ident,
    /// Field value.
    pub value: Expr,
    /// Span of the entry.
    pub span: Span,
}

/// An expression.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub enum Expr {
    /// A literal.
    Literal {
        /// The literal value.
        value: Literal,
        /// Expression span.
        span: Span,
    },
    /// A variable reference.
    Var {
        /// The referenced name.
        name: Ident,
    },
    /// `callee(args)` — `callee` is `f` or `m::f`.
    Call {
        /// The called function path.
        callee: Path,
        /// Arguments.
        args: Vec<Expr>,
        /// Expression span.
        span: Span,
    },
    /// `base.field`.
    Field {
        /// The base expression.
        base: Box<Expr>,
        /// Field name.
        name: Ident,
        /// Expression span.
        span: Span,
    },
    /// `lhs op rhs`.
    Binary {
        /// Operator.
        op: BinOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
        /// Expression span.
        span: Span,
    },
    /// `op expr` (prefix).
    Unary {
        /// Operator.
        op: UnOp,
        /// Operand.
        expr: Box<Expr>,
        /// Expression span.
        span: Span,
    },
    /// `if cond { } (else ...)?`.
    If {
        /// Condition.
        cond: Box<Expr>,
        /// Then-branch block.
        then: Block,
        /// Else branch: a block or a chained `else if`.
        else_: Option<Box<Expr>>,
        /// Expression span.
        span: Span,
    },
    /// A block used as an expression.
    Block {
        /// The block.
        block: Block,
        /// Expression span.
        span: Span,
    },
    /// `Name { f: v, ... }` — `name` is `S` or `m::S`.
    StructLit {
        /// Struct path.
        name: Path,
        /// Field initializers.
        fields: Vec<FieldInit>,
        /// Expression span.
        span: Span,
    },
    /// A module-qualified name in value position (`m::x` written
    /// without a call). Always at least two segments — a bare name is
    /// a `Var`.
    Path {
        /// The path as written.
        path: Path,
    },
    /// Placeholder for input the parser could not interpret. Never
    /// produced for valid source.
    Error {
        /// Offending range.
        span: Span,
    },
}

impl Expr {
    /// The expression's source span.
    pub fn span(&self) -> Span {
        match self {
            Expr::Literal { span, .. }
            | Expr::Call { span, .. }
            | Expr::Field { span, .. }
            | Expr::Binary { span, .. }
            | Expr::Unary { span, .. }
            | Expr::If { span, .. }
            | Expr::Block { span, .. }
            | Expr::StructLit { span, .. }
            | Expr::Error { span } => *span,
            Expr::Var { name } => name.span,
            Expr::Path { path } => path.span,
        }
    }
}

/// A literal value.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value")]
pub enum Literal {
    /// Integer literal (already validated to fit `i128`).
    Int(i128),
    /// Float literal.
    Float(f64),
    /// String literal (escapes decoded).
    Str(String),
    /// Boolean literal.
    Bool(bool),
}

/// Infix operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum BinOp {
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `&&`
    And,
    /// `||`
    Or,
}

/// Prefix operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum UnOp {
    /// `-x`
    Neg,
    /// `!x`
    Not,
}
