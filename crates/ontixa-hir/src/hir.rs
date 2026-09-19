//! HIR data structures.
//!
//! HIR is the first *semantic* representation: names are resolved to
//! [`SymbolId`]s, definitions to [`DefId`]s, and every expression lives in
//! a module-wide arena indexed by [`ExprId`]. It is deliberately free of
//! runtime concerns — no evaluation state, no codegen details.

use ontixa_source::{DefId, ExprId, InternId, ModuleId, Span, SymbolId};
use rustc_hash::FxHashMap;
use serde::Serialize;

pub use ontixa_ast::{BinOp, Literal, UnOp};

/// An interned name with its source location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Name {
    /// Interned text.
    pub id: InternId,
    /// Source location.
    pub span: Span,
}

/// A resolved type reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind")]
pub enum TypeRef {
    /// `bool`
    Bool,
    /// `i32`
    I32,
    /// `i64`
    I64,
    /// `u32`
    U32,
    /// `u64`
    U64,
    /// `f32`
    F32,
    /// `f64`
    F64,
    /// `str`
    Str,
    /// `unit` (also the type of a function without `->`).
    Unit,
    /// A user `data` type.
    Struct(DefId),
    /// Resolution failed — a diagnostic already exists. Poison values
    /// keep downstream passes running without inventing semantics.
    Poison,
}

/// The kind of a symbol in the module symbol table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// A `fn` definition.
    Function,
    /// A `data` definition.
    Data,
    /// A field of a `data` definition.
    Field,
    /// A function parameter.
    Param,
    /// A `let` binding.
    Local,
}

/// One entry in the module symbol table.
#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    /// This symbol's own ID (redundant with its table index).
    pub id: SymbolId,
    /// Interned name.
    pub name: InternId,
    /// What kind of entity this is.
    pub kind: SymbolKind,
    /// Owning definition for params, locals, and fields.
    pub owner: Option<DefId>,
    /// Declaration site.
    pub span: Span,
}

/// The module-wide symbol table, indexed by [`SymbolId`].
#[derive(Debug, Default)]
pub struct SymbolTable {
    symbols: Vec<Symbol>,
}

impl SymbolTable {
    /// Allocates a new symbol entry.
    pub fn push(&mut self, mut symbol: Symbol) -> SymbolId {
        let id = SymbolId::new(self.symbols.len() as u32);
        symbol.id = id;
        self.symbols.push(symbol);
        id
    }

    /// Looks up a symbol by ID.
    pub fn get(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.index()]
    }

    /// All symbols in allocation order.
    pub fn iter(&self) -> impl Iterator<Item = &Symbol> {
        self.symbols.iter()
    }

    /// Number of symbols.
    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    /// Whether the table is empty.
    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }
}

/// A top-level definition (function or data).
#[derive(Debug, Clone)]
pub struct Def {
    /// This def's ID.
    pub id: DefId,
    /// Symbol carrying the def's name.
    pub name: SymbolId,
    /// Kind-specific payload.
    pub kind: DefKind,
    /// Declaration span.
    pub span: Span,
}

/// Definition payload.
#[derive(Debug, Clone)]
pub enum DefKind {
    /// A function with signature and (post-lowering) body.
    Function(FnSig),
    /// A `data` shape.
    Data(DataShape),
}

/// A function signature: parameter symbols and resolved types.
#[derive(Debug, Clone)]
pub struct FnSig {
    /// Parameter symbols in order (each is `SymbolKind::Param`).
    pub params: Vec<ParamDef>,
    /// Resolved return type (`Unit` when no `->` was written).
    pub ret: TypeRef,
}

/// A parameter: symbol plus resolved type.
#[derive(Debug, Clone)]
pub struct ParamDef {
    /// The parameter's symbol.
    pub symbol: SymbolId,
    /// Resolved parameter type.
    pub ty: TypeRef,
    /// Source span of the parameter.
    pub span: Span,
}

/// A `data` definition's shape.
#[derive(Debug, Clone)]
pub struct DataShape {
    /// Fields in declaration order (index = field position).
    pub fields: Vec<FieldDef>,
    /// Field name → position index.
    pub field_index: FxHashMap<InternId, u32>,
}

/// A field of a `data` definition.
#[derive(Debug, Clone)]
pub struct FieldDef {
    /// The field's symbol.
    pub symbol: SymbolId,
    /// Resolved field type.
    pub ty: TypeRef,
    /// Field position index.
    pub index: u32,
}

/// The module scope produced by name resolution: everything knowable
/// without looking inside function bodies.
#[derive(Debug)]
pub struct ModuleScope {
    /// Module identity.
    pub module: ModuleId,
    /// Top-level definitions, indexed by `DefId`.
    pub defs: Vec<Def>,
    /// Module-wide symbol table (defs, fields, params so far; body
    /// lowering appends locals).
    pub symbols: SymbolTable,
    /// Function name → `DefId`.
    pub fns: FxHashMap<InternId, DefId>,
    /// Data name → `DefId`.
    pub datas: FxHashMap<InternId, DefId>,
}

impl ModuleScope {
    /// Looks up a def by ID.
    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.index()]
    }

    /// Function signature of a def, when it is a function.
    pub fn fn_sig(&self, id: DefId) -> Option<&FnSig> {
        match &self.def(id).kind {
            DefKind::Function(sig) => Some(sig),
            _ => None,
        }
    }

    /// Data shape of a def, when it is a `data` definition.
    pub fn data_shape(&self, id: DefId) -> Option<&DataShape> {
        match &self.def(id).kind {
            DefKind::Data(shape) => Some(shape),
            _ => None,
        }
    }
}

/// A lowered function body.
#[derive(Debug, Clone)]
pub struct HirBody {
    /// The function this body belongs to.
    pub def: DefId,
    /// Root expression — always a `Block`.
    pub root: ExprId,
    /// Local binding symbols (params are in the signature instead).
    pub locals: Vec<SymbolId>,
}

/// An expression node in the module arena.
#[derive(Debug, Clone)]
pub struct HirExpr {
    /// This expression's arena ID.
    pub id: ExprId,
    /// Node payload.
    pub kind: HirExprKind,
    /// Source span.
    pub span: Span,
}

/// Expression payload.
#[derive(Debug, Clone)]
pub enum HirExprKind {
    /// A literal.
    Literal(LitValue),
    /// Reference to a binding (param or local).
    Var(SymbolId),
    /// Direct function call.
    Call {
        /// Callee definition.
        def: DefId,
        /// Arguments in order.
        args: Vec<ExprId>,
    },
    /// Field access (`base.name`). `field` is resolved by type checking.
    Field {
        /// Base expression.
        base: ExprId,
        /// Field name as written.
        name: Name,
        /// Field position index, filled by type checking.
        field: Option<u32>,
    },
    /// Infix operation.
    Binary {
        /// Operator.
        op: BinOp,
        /// Left operand.
        lhs: ExprId,
        /// Right operand.
        rhs: ExprId,
    },
    /// Prefix operation.
    Unary {
        /// Operator.
        op: UnOp,
        /// Operand.
        expr: ExprId,
    },
    /// `if` with optional else (else is a block or chained if).
    If {
        /// Condition.
        cond: ExprId,
        /// Then-branch (a `Block` expression).
        then: ExprId,
        /// Else branch expression, if any.
        else_: Option<ExprId>,
    },
    /// A block: statements plus optional tail expression.
    Block {
        /// Statements.
        stmts: Vec<HirStmt>,
        /// Tail expression.
        tail: Option<ExprId>,
    },
    /// `Name { f: v, ... }`.
    StructLit {
        /// The struct being constructed.
        def: DefId,
        /// `(field name, value)` pairs in written order.
        fields: Vec<(Name, ExprId)>,
    },
    /// Poisoned expression — an upstream diagnostic already exists.
    Poison,
}

/// A literal in HIR (same payload as AST).
pub type LitValue = ontixa_ast::Literal;

/// A statement inside a `Block`.
#[derive(Debug, Clone)]
pub enum HirStmt {
    /// `let x (: T)? (= init)? ;`
    Let {
        /// The new binding's symbol.
        symbol: SymbolId,
        /// Resolved annotation, when written.
        ty: Option<TypeRef>,
        /// Initializer expression.
        init: Option<ExprId>,
        /// Statement span.
        span: Span,
    },
    /// `place = value ;`
    Assign {
        /// Assignment target.
        target: HirPlace,
        /// Assigned expression.
        value: ExprId,
        /// Statement span.
        span: Span,
    },
    /// Expression statement.
    Expr {
        /// The expression.
        expr: ExprId,
        /// Whether a semicolon was written.
        has_semi: bool,
    },
    /// `return value? ;`
    Return {
        /// Returned expression.
        value: Option<ExprId>,
        /// Statement span.
        span: Span,
    },
}

/// An assignment target: binding plus field projections.
#[derive(Debug, Clone)]
pub struct HirPlace {
    /// Root binding symbol.
    pub base: SymbolId,
    /// Field projections in order.
    pub fields: Vec<Name>,
    /// Whole-place span.
    pub span: Span,
}

/// The lowered module: resolved scope plus bodies and the expression
/// arena.
#[derive(Debug)]
pub struct HirModule {
    /// Resolved module scope (defs, symbols, name indices).
    pub scope: ModuleScope,
    /// Module-wide expression arena, indexed by `ExprId`.
    pub exprs: Vec<HirExpr>,
    /// Function bodies by `DefId` (`None` for data defs).
    pub bodies: Vec<Option<HirBody>>,
}

impl HirModule {
    /// Looks up an expression by ID.
    pub fn expr(&self, id: ExprId) -> &HirExpr {
        &self.exprs[id.index()]
    }

    /// The body of a function def, when present.
    pub fn body(&self, def: DefId) -> Option<&HirBody> {
        self.bodies.get(def.index()).and_then(|b| b.as_ref())
    }
}
