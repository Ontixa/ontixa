//! HIR data structures.
//!
//! HIR is the first *semantic* representation: names are resolved to
//! [`SymbolId`]s, definitions to [`DefId`]s, and every expression
//! lives in its owning [`HirBody`]'s arena indexed by [`ExprId`].
//! It is deliberately free of runtime concerns — no evaluation
//! state, no codegen details.
//!
//! # Arena ownership (milestone 2)
//!
//! Expression nodes and binding symbols are **body-local**: editing
//! one function can never renumber another function's `ExprId`s or
//! `SymbolId`s, which is what makes per-definition incremental
//! caching (`hir_body(DefKey)`) meaningful. Local symbol ids carry
//! [`SymbolId::LOCAL_BIT`]; resolve them through [`HirBody::symbol`],
//! which dispatches between the body arena and the module
//! `SymbolTable`.

use ontixa_source::{DefId, ExprId, FileId, InternId, ModuleId, Span, SymbolId};
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
    /// `[T]` — a homogeneous array. `elem` is flat by construction:
    /// nested arrays are rejected at resolution.
    Array {
        /// The resolved element type.
        elem: ElemRef,
    },
    /// Resolution failed — a diagnostic already exists. Poison values
    /// keep downstream passes running without inventing semantics.
    Poison,
}

/// A resolved array element type — every [`TypeRef`] leaf except
/// `Unit` (arrays of nothing carry nothing), `Array` (nested arrays
/// are not supported yet), and `Poison`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind")]
pub enum ElemRef {
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
    /// A user `data` type.
    Struct(DefId),
}

impl ElemRef {
    /// The element type for a resolved leaf [`TypeRef`], or `None`
    /// when `ty` cannot be an element (`unit`, `[_]`, `Poison`).
    pub fn of(ty: TypeRef) -> Option<ElemRef> {
        Some(match ty {
            TypeRef::Bool => ElemRef::Bool,
            TypeRef::I32 => ElemRef::I32,
            TypeRef::I64 => ElemRef::I64,
            TypeRef::U32 => ElemRef::U32,
            TypeRef::U64 => ElemRef::U64,
            TypeRef::F32 => ElemRef::F32,
            TypeRef::F64 => ElemRef::F64,
            TypeRef::Str => ElemRef::Str,
            TypeRef::Struct(d) => ElemRef::Struct(d),
            TypeRef::Unit | TypeRef::Array { .. } | TypeRef::Poison => return None,
        })
    }
}

/// The kind of a symbol in the module symbol table or a body arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    /// A `fn` definition.
    Function,
    /// A `data` definition.
    Data,
    /// A field of a `data` definition.
    Field,
    /// A variant of an enum `data` definition.
    Variant,
    /// A function parameter.
    Param,
    /// A `let` binding.
    Local,
}

/// One entry in a symbol table (module-level or body-local).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Symbol {
    /// This symbol's own ID (redundant with its table index; carries
    /// [`SymbolId::LOCAL_BIT`] inside a body arena).
    pub id: SymbolId,
    /// Interned name.
    pub name: InternId,
    /// What kind of entity this is.
    pub kind: SymbolKind,
    /// Whether the binding was declared `mut` (locals/params only —
    /// `false` for defs and fields).
    pub mutable: bool,
    /// Owning definition for params, locals, and fields.
    pub owner: Option<DefId>,
    /// Declaration site.
    pub span: Span,
}

/// The module-wide symbol table, indexed by [`SymbolId`]. Holds only
/// *module-level* symbols: defs and `data` fields. Params and locals
/// live in their owning [`HirBody`].
#[derive(Debug, Default, Clone, PartialEq)]
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

    /// Looks up a module-level symbol by ID. Panics on body-local
    /// ids — use [`HirBody::symbol`] for those.
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
#[derive(Debug, Clone, PartialEq)]
pub struct Def {
    /// This def's ID — dense over the whole workspace, so a def in a
    /// dependency file can be referenced directly by `Call`/`StructLit`.
    pub id: DefId,
    /// The file that declares this def.
    pub file: FileId,
    /// Index of the def's item within `file`'s `AstModule.items` —
    /// locates the item's absolute start (the rebase base) without a
    /// name lookup.
    pub item: u32,
    /// Symbol carrying the def's name.
    pub name: SymbolId,
    /// Kind-specific payload.
    pub kind: DefKind,
    /// Declaration span.
    pub span: Span,
}

/// Definition payload.
#[derive(Debug, Clone, PartialEq)]
pub enum DefKind {
    /// A function with signature and (post-lowering) body.
    Function(FnSig),
    /// A `data` shape.
    Data(DataShape),
}

/// A function signature: parameter symbols and resolved types.
#[derive(Debug, Clone, PartialEq)]
pub struct FnSig {
    /// Parameter symbols in order. `params[i].symbol` is the
    /// body-local id `SymbolId::local(i)` — the param's `Symbol`
    /// record lives in `body.local_symbols[i]` once the body is
    /// lowered.
    pub params: Vec<ParamDef>,
    /// Resolved return type (`Unit` when no `->` was written).
    pub ret: TypeRef,
}

/// A parameter: symbol plus resolved type.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamDef {
    /// The parameter's body-local symbol id (`SymbolId::local(i)`).
    pub symbol: SymbolId,
    /// Interned parameter name (kept here so signatures — and tools
    /// explaining them — don't need the body to be lowered).
    pub name: InternId,
    /// Whether the parameter was declared `mut`.
    pub mutable: bool,
    /// Resolved parameter type.
    pub ty: TypeRef,
    /// Source span of the parameter.
    pub span: Span,
}

/// A `data` definition's shape — either a record (`fields`) or an
/// enum (`variants`); the parser reports mixing, so at most one is
/// populated for valid source.
#[derive(Debug, Clone, PartialEq)]
pub struct DataShape {
    /// Fields in declaration order (index = field position).
    pub fields: Vec<FieldDef>,
    /// Field name → position index.
    pub field_index: FxHashMap<InternId, u32>,
    /// Variants in declaration order (index = discriminant).
    pub variants: Vec<VariantDef>,
    /// Variant name → discriminant index.
    pub variant_index: FxHashMap<InternId, u32>,
}

impl DataShape {
    /// Whether this `data` is an enum (has variants) rather than a
    /// record (has fields).
    pub fn is_enum(&self) -> bool {
        !self.variants.is_empty()
    }

    /// `"enum"` or `"record"` — the shape's stable name for
    /// diagnostics and graph attrs.
    pub fn shape_name(&self) -> &'static str {
        if self.is_enum() { "enum" } else { "record" }
    }
}

/// A variant of an enum `data` definition.
#[derive(Debug, Clone, PartialEq)]
pub struct VariantDef {
    /// The variant's symbol (module-level, owned by the data def).
    pub symbol: SymbolId,
    /// Resolved payload element types in order.
    pub payload: Vec<TypeRef>,
    /// Discriminant index (declaration order).
    pub index: u32,
}

/// A field of a `data` definition.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    /// The field's symbol (module-level).
    pub symbol: SymbolId,
    /// Resolved field type.
    pub ty: TypeRef,
    /// Field position index.
    pub index: u32,
}

/// One file's name environment within the workspace: what its
/// top-level names and `use` declarations bring into scope.
///
/// `fns`/`datas` hold only *this file's own* defs (one namespace — a
/// name may be defined once per file). `modules` binds module names
/// (`use m;`) and `imports` binds member aliases (`use m::x as y;`).
/// Bare names resolve through `fns`/`datas` then `imports`;
/// `m::x` paths resolve through `modules` then the target's env.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileEnv {
    /// Function name → `DefId`.
    pub fns: FxHashMap<InternId, DefId>,
    /// Data name → `DefId`.
    pub datas: FxHashMap<InternId, DefId>,
    /// Module name → the file that provides it (`use m;`).
    pub modules: FxHashMap<InternId, FileId>,
    /// Bound name → the imported def (`use m::x [as y];`).
    pub imports: FxHashMap<InternId, DefId>,
}

/// The workspace scope produced by name resolution: every definition
/// in every reachable file, plus each file's name environment.
/// Defs are globally indexed (`DefId` spans the whole workspace) so a
/// call in one module can reference a callee in another directly.
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleScope {
    /// Module identity (the workspace root's module).
    pub module: ModuleId,
    /// The workspace root file — the entry the scope was built around.
    pub root: FileId,
    /// Top-level definitions across all reachable files, indexed by
    /// `DefId`. Order is deterministic: files in discovery order
    /// (`files`), items in source order within each file.
    pub defs: Vec<Def>,
    /// Module-level symbol table (defs and `data` fields only;
    /// params and locals are body-local).
    pub symbols: SymbolTable,
    /// Per-file name environments.
    pub envs: FxHashMap<FileId, FileEnv>,
    /// Reachable files in discovery order — the root first.
    pub files: Vec<FileId>,
    /// Module name (file stem) per reachable file.
    pub file_names: FxHashMap<FileId, InternId>,
}

impl ModuleScope {
    /// Looks up a def by ID.
    pub fn def(&self, id: DefId) -> &Def {
        &self.defs[id.index()]
    }

    /// The name environment of `file` (`None` when the file is not
    /// part of this workspace).
    pub fn env(&self, file: FileId) -> Option<&FileEnv> {
        self.envs.get(&file)
    }

    /// The root file's name environment — where unqualified lookups
    /// in workspace-level entry points resolve.
    pub fn root_env(&self) -> &FileEnv {
        self.envs
            .get(&self.root)
            .expect("scope without its root env")
    }

    /// The module name a file provides (its stem), resolved through
    /// `interner`.
    pub fn file_name(&self, file: FileId) -> Option<InternId> {
        self.file_names.get(&file).copied()
    }

    /// The stable query key of a def in this workspace —
    /// `(root, file, name)`. Per-definition query results are only
    /// valid within the scope that produced them (`DefId`s are
    /// scope-relative), so the root belongs to the key.
    pub fn def_key(&self, id: DefId) -> ontixa_source::DefKey {
        let def = self.def(id);
        ontixa_source::DefKey::new(self.root, def.file, self.symbols.get(def.name).name)
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

/// A lowered function body — its own expression arena and its own
/// symbol arena (params followed by `let` bindings).
#[derive(Debug, Clone, PartialEq)]
pub struct HirBody {
    /// The function this body belongs to.
    pub def: DefId,
    /// Root expression — always a `Block`.
    pub root: ExprId,
    /// Body-local expression arena, indexed by `ExprId`.
    pub exprs: Vec<HirExpr>,
    /// Body-local symbol arena: `local_symbols[i]` has id
    /// `SymbolId::local(i)`. Params occupy `0..n_params`, in the same
    /// order as the signature; `let` bindings follow in source order.
    pub local_symbols: Vec<Symbol>,
}

impl HirBody {
    /// Looks up an expression in this body's arena.
    pub fn expr(&self, id: ExprId) -> &HirExpr {
        &self.exprs[id.index()]
    }

    /// Resolves any [`SymbolId`] reachable from this body: body-local
    /// ids (params, `let` bindings) index [`Self::local_symbols`];
    /// module-level ids index `scope.symbols`.
    pub fn symbol<'a>(&'a self, scope: &'a ModuleScope, id: SymbolId) -> &'a Symbol {
        if id.is_local() {
            &self.local_symbols[id.local_index()]
        } else {
            scope.symbols.get(id)
        }
    }

    /// Iterates the ids of `let` bindings (skips params).
    pub fn local_ids(&self) -> impl Iterator<Item = SymbolId> + '_ {
        self.local_symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::Local)
            .map(|s| s.id)
    }
}

/// An expression node in a body arena.
#[derive(Debug, Clone, PartialEq)]
pub struct HirExpr {
    /// This expression's arena ID.
    pub id: ExprId,
    /// Node payload.
    pub kind: HirExprKind,
    /// Source span.
    pub span: Span,
}

/// Expression payload.
#[derive(Debug, Clone, PartialEq)]
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
    /// `base.len` on a `str` or `[T]` — a built-in read-only
    /// property. The type checker rewrites a `Field` node into this
    /// kind so downstream passes never mistake it for a data
    /// projection.
    Len {
        /// The string or array expression.
        base: ExprId,
    },
    /// Indexing (`base[index]`); for `str` the index is a character
    /// position and the result is a one-character `str`; for `[T]`
    /// it is an element position and the result is a `T`.
    Index {
        /// Indexed expression.
        base: ExprId,
        /// Index expression.
        index: ExprId,
    },
    /// Slicing (`base[lo..hi]`); either bound may be absent. For
    /// `str` the result is a `str`; for `[T]` a fresh `[T]`.
    Slice {
        /// Sliced expression.
        base: ExprId,
        /// Optional lower bound.
        lo: Option<ExprId>,
        /// Optional upper bound.
        hi: Option<ExprId>,
    },
    /// `[e, ...]` — a homogeneous array literal.
    ArrayLit {
        /// Elements in order.
        elems: Vec<ExprId>,
    },
    /// `lo..hi` — an integer range; either bound may be absent.
    /// Valid only as a `for` iterable — the checker diagnoses every
    /// other position.
    Range {
        /// Optional lower bound (`..hi` starts at 0).
        lo: Option<ExprId>,
        /// Optional upper bound (`lo..` is unbounded — rejected).
        hi: Option<ExprId>,
    },
    /// `for x in e { .. }` — iterates `e` (an array or a `Range`),
    /// binding each element to `var` fresh per iteration. `var` is a
    /// body-local `Local` symbol, assignable iff written `for mut x`.
    /// Always `unit`-typed.
    For {
        /// The loop variable's body-local symbol.
        var: SymbolId,
        /// The iterated expression — a `Range` node or an array value.
        iter: ExprId,
        /// The loop body (a `Block` expression).
        body: ExprId,
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
    /// `T::V(args)` — a variant constructor. `args` may be empty for a
    /// bare `T::V` path; arity is validated by type checking.
    VariantLit {
        /// The enum `data` being constructed.
        def: DefId,
        /// Discriminant index into the def's `variants`.
        variant: u32,
        /// Payload arguments in order.
        args: Vec<ExprId>,
    },
    /// `match e { pat => v, .. }` — selection over `data` variants.
    Match {
        /// The matched value.
        scrutinee: ExprId,
        /// Arms in written order.
        arms: Vec<HirArm>,
    },
    /// Poisoned expression — an upstream diagnostic already exists.
    Poison,
}

/// One `pat => expr` arm of a [`HirExprKind::Match`]. `span` covers
/// the whole arm.
#[derive(Debug, Clone, PartialEq)]
pub struct HirArm {
    /// The resolved pattern.
    pub pat: HirPat,
    /// The arm's value expression.
    pub body: ExprId,
    /// Arm span (`pat => expr`).
    pub span: Span,
}

/// A resolved match pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum HirPat {
    /// `T::V(b0, ..)` / `m::T::V(..)` — matches discriminant `variant`
    /// of `def`. `binds[i]` is the payload-`i` binding (`None` = `_`).
    Variant {
        /// The enum `data` the pattern selects.
        def: DefId,
        /// Discriminant index.
        variant: u32,
        /// Payload bindings, aligned with the variant's payload.
        binds: Vec<Option<SymbolId>>,
        /// Pattern span.
        span: Span,
    },
    /// `x` binds the whole scrutinee (`Some`); `_` ignores it (`None`).
    Bind {
        /// The bound body-local symbol, or `None` for `_`.
        sym: Option<SymbolId>,
        /// Pattern span.
        span: Span,
    },
    /// Resolution failed upstream — a diagnostic already exists.
    Poison,
}

/// A literal in HIR (same payload as AST).
pub type LitValue = ontixa_ast::Literal;

/// A statement inside a `Block`.
#[derive(Debug, Clone, PartialEq)]
pub enum HirStmt {
    /// `let x (: T)? (= init)? ;`
    Let {
        /// The new binding's body-local symbol.
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
#[derive(Debug, Clone, PartialEq)]
pub struct HirPlace {
    /// Root binding symbol (may be body-local).
    pub base: SymbolId,
    /// Field projections in order.
    pub fields: Vec<Name>,
    /// Whole-place span.
    pub span: Span,
}

/// The lowered module: resolved scope plus per-def bodies.
#[derive(Debug, Clone, PartialEq)]
pub struct HirModule {
    /// Resolved module scope (defs, symbols, name indices).
    pub scope: ModuleScope,
    /// Function bodies by `DefId` (`None` for data defs). Each body
    /// owns its expression and local-symbol arenas.
    pub bodies: Vec<Option<HirBody>>,
}

impl HirModule {
    /// The body of a function def, when present.
    pub fn body(&self, def: DefId) -> Option<&HirBody> {
        self.bodies.get(def.index()).and_then(|b| b.as_ref())
    }
}
