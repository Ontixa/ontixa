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

/// A `data` declaration — either a record (`fields`) or an enum
/// (`variants`); the parser reports mixing, so at most one is
/// populated for valid source.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DataDecl {
    /// Declared name.
    pub name: Ident,
    /// Record fields in declaration order.
    pub fields: Vec<Field>,
    /// Enum variants in declaration order.
    pub variants: Vec<Variant>,
    /// Span of the whole declaration.
    pub span: Span,
}

/// One `name: Type;` field inside a record `data` declaration.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Field {
    /// Field name.
    pub name: Ident,
    /// Declared type.
    pub ty: TypeExpr,
    /// Span of the field entry.
    pub span: Span,
}

/// One `Name(T, ..)` / `Name` variant inside an enum `data`
/// declaration. A unit variant has an empty `payload`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Variant {
    /// Variant name.
    pub name: Ident,
    /// Payload element types in order.
    pub payload: Vec<TypeExpr>,
    /// Span of the variant entry.
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

/// A type position: a named type (`Name`, optionally module-qualified
/// `m::Name`) or an array type `[T]`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum TypeExpr {
    /// `Name` / `m::Name` — serializes as `{"path": ...}`.
    Named {
        /// The type path as written.
        path: Path,
    },
    /// `[T]` — a homogeneous array of `elem`. Element types are flat:
    /// `[[i32]]` parses, but type resolution rejects nested arrays.
    Array {
        /// The element type.
        elem: Box<TypeExpr>,
        /// Span covering the whole `[T]`.
        span: Span,
    },
}

impl TypeExpr {
    /// The type's full source span.
    pub fn span(&self) -> Span {
        match self {
            TypeExpr::Named { path } => path.span,
            TypeExpr::Array { span, .. } => *span,
        }
    }

    /// Every named path the type mentions — `[P]` contributes `P`'s
    /// path (element types are flat, so at most one level deep).
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            TypeExpr::Named { path } => vec![path],
            TypeExpr::Array { elem, .. } => elem.paths(),
        }
    }
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

/// One `pat => expr` arm of a `match`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MatchArm {
    /// The pattern guarding the arm.
    pub pat: Pattern,
    /// The arm's value expression.
    pub body: Expr,
    /// Span covering `pat => expr`.
    pub span: Span,
}

/// A match pattern.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub enum Pattern {
    /// `x` binds the whole scrutinee; `_` ignores it (wildcard).
    Bind {
        /// The bound name — `"_"` marks a wildcard.
        name: Ident,
    },
    /// `T::V`, `T::V(b, ..)`, `m::T::V(..)` — matches one variant of an
    /// enum `data` and binds payload elements positionally. A `binds`
    /// entry whose `name` is `"_"` skips that element.
    Variant {
        /// The variant path as written (`T::V` or `m::T::V`).
        path: Path,
        /// Payload bindings in order.
        binds: Vec<Ident>,
        /// Pattern span.
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
    /// `base[index]` — for `str`, `index` is a character position and
    /// the result is a one-character `str`; for `[T]` it is an
    /// element position and the result is a `T`.
    Index {
        /// The indexed expression.
        base: Box<Expr>,
        /// The index expression.
        index: Box<Expr>,
        /// Expression span.
        span: Span,
    },
    /// `base[lo..hi]` — a slice; either bound may be omitted. For
    /// `str` the result is a `str`; for `[T]` a fresh `[T]`.
    Slice {
        /// The sliced expression.
        base: Box<Expr>,
        /// Optional lower bound (`lo` in `base[lo..]`).
        lo: Option<Box<Expr>>,
        /// Optional upper bound (`hi` in `base[..hi]`).
        hi: Option<Box<Expr>>,
        /// Expression span.
        span: Span,
    },
    /// `[e, ...]` — a homogeneous array literal. Empty literals `[]`
    /// parse fine but need a type annotation to check.
    ArrayLit {
        /// Elements in order.
        elems: Vec<Expr>,
        /// Expression span.
        span: Span,
    },
    /// `lo..hi` — a half-open integer range; either bound may be
    /// omitted. Valid only as a `for` iterable (ranges inside `[]`
    /// brackets lower to `Slice` bounds, never to `Range`).
    Range {
        /// Optional lower bound (`lo` in `lo..hi`; `..hi` starts at 0).
        lo: Option<Box<Expr>>,
        /// Optional upper bound (`hi` in `lo..hi`; `lo..` is
        /// unbounded and rejected by the checker).
        hi: Option<Box<Expr>>,
        /// Expression span.
        span: Span,
    },
    /// `match e { pat => v, .. }` — selection over `data` variants.
    /// The scrutinee must be an enum `data` value; arms are tried in
    /// order and the whole match is the matched arm's value.
    Match {
        /// The matched value.
        scrutinee: Box<Expr>,
        /// Arms in written order.
        arms: Vec<MatchArm>,
        /// Expression span.
        span: Span,
    },
    /// `for x in e { .. }` — iterates `e` (an array or a `Range`),
    /// binding each element to a fresh `x`. Always `unit`-typed.
    For {
        /// Loop variable — a fresh body-local binding per iteration.
        var: Ident,
        /// Whether the binding was written `for mut x`.
        mutable: bool,
        /// The iterated expression.
        iter: Box<Expr>,
        /// Loop body.
        body: Block,
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
            | Expr::Index { span, .. }
            | Expr::Slice { span, .. }
            | Expr::ArrayLit { span, .. }
            | Expr::Range { span, .. }
            | Expr::Match { span, .. }
            | Expr::For { span, .. }
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
    /// Character literal (escapes decoded; exactly one scalar).
    Char(char),
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
