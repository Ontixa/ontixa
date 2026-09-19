//! Typed MIR: a compact control-flow graph per function.
//!
//! MIR is the boundary between *analysis* and *execution*. HIR keeps
//! lexical structure; MIR flattens it into basic blocks with explicit
//! temporaries, resolved field projections, and terminators. Every
//! value's [`Ty`] is known — nothing in MIR is untyped.
//!
//! Borrowed call arguments stay *places* (`Operand::Place`), so the
//! interpreter can share caller storage for `borrow`/`borrow_mut`
//! contracts instead of copying.

use ontixa_hir::{BinOp, UnOp};
use ontixa_memory::ParamBehavior;
use ontixa_source::{DefId, SymbolId};
use ontixa_types::Ty;
use serde::Serialize;

/// A basic-block identifier within one function body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct BlockId(pub u32);

/// A local slot identifier (`locals[i]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Local(pub u32);

/// A storage location: a local plus resolved field projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Place {
    /// Local slot.
    pub local: Local,
    /// Declared field indices, applied in order.
    pub proj: Vec<u32>,
}

impl Place {
    /// The bare local `l`.
    pub fn local(l: Local) -> Self {
        Place {
            local: l,
            proj: Vec::new(),
        }
    }
}

/// A compile-time constant.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", content = "value")]
pub enum Const {
    /// Integer (fits its [`Ty`]). Serialized as i64/u64 when it fits —
    /// `serde_json` cannot represent bare `i128`.
    Int(#[serde(serialize_with = "ser_i128")] i128),
    /// Float.
    Float(f64),
    /// String.
    Str(String),
    /// Boolean.
    Bool(bool),
    /// Unit value.
    Unit,
}

/// Serializes an `i128` as a JSON number when it fits in 64 bits,
/// else as a string (defensive — today's literals never exceed `u64`).
fn ser_i128<S: serde::Serializer>(v: &i128, s: S) -> Result<S::Ok, S::Error> {
    if let Ok(i) = i64::try_from(*v) {
        s.serialize_i64(i)
    } else if let Ok(u) = u64::try_from(*v) {
        s.serialize_u64(u)
    } else {
        s.serialize_str(&v.to_string())
    }
}

/// An operand: either a constant or a place.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum Operand {
    /// A constant.
    Const(Const),
    /// A storage location read (or shared, for borrowed call args).
    Place(Place),
}

/// A computed value (right-hand side of an assignment).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum Rvalue {
    /// Read/copy a place or use a constant.
    Use(Operand),
    /// `lhs op rhs`.
    Binary {
        /// Operator.
        op: BinOp,
        /// Left operand.
        lhs: Operand,
        /// Right operand.
        rhs: Operand,
    },
    /// `op operand`.
    Unary {
        /// Operator.
        op: UnOp,
        /// Operand.
        operand: Operand,
    },
    /// Direct call; callee contract comes from `param_behaviors`.
    Call {
        /// Callee definition.
        def: DefId,
        /// Argument operands.
        args: Vec<Operand>,
        /// Inferred callee param contracts (copied for the executor).
        contract: Vec<ParamBehavior>,
    },
    /// `Name { ... }` construction with declared-order field indices.
    StructLit {
        /// The `data` definition.
        def: DefId,
        /// `(declared field index, value)` pairs.
        fields: Vec<(u32, Operand)>,
    },
}

/// A statement inside a basic block.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum MirStmt {
    /// `dst = rvalue`.
    Assign {
        /// Destination place.
        dst: Place,
        /// Computed value.
        val: Rvalue,
    },
    /// Evaluate and discard (expression statements).
    Eval {
        /// Computed value.
        val: Rvalue,
    },
}

/// How a basic block ends.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum Terminator {
    /// Return a value to the caller.
    Return(Operand),
    /// Conditional branch.
    Branch {
        /// Condition (`bool` operand).
        cond: Operand,
        /// Taken when true.
        then: BlockId,
        /// Taken when false.
        else_: BlockId,
    },
    /// Unconditional jump.
    Goto(BlockId),
}

/// A basic block: straight-line statements plus a terminator.
#[derive(Debug, Clone, Serialize)]
pub struct BasicBlock {
    /// Block identifier.
    pub id: BlockId,
    /// Statements in order.
    pub stmts: Vec<MirStmt>,
    /// How control leaves the block.
    pub term: Terminator,
}

/// One local slot: a user binding or a compiler temporary.
#[derive(Debug, Clone, Serialize)]
pub struct LocalDecl {
    /// The source symbol, when this slot is a user binding.
    pub sym: Option<SymbolId>,
    /// The slot's type.
    pub ty: Ty,
}

/// A function's MIR body.
#[derive(Debug, Clone, Serialize)]
pub struct MirBody {
    /// The function definition this body belongs to.
    pub def: DefId,
    /// Parameter slots (locals `0..params.len()`), symbols + types.
    pub params: Vec<(SymbolId, Ty)>,
    /// Inferred contract per parameter.
    pub param_behaviors: Vec<ParamBehavior>,
    /// Return type.
    pub ret: Ty,
    /// All locals (params first, then bindings, then temporaries).
    pub locals: Vec<LocalDecl>,
    /// Basic blocks; `blocks[0]` is the entry block.
    pub blocks: Vec<BasicBlock>,
}

/// The module's MIR: one optional body per definition.
#[derive(Debug, Default, Serialize)]
pub struct MirModule {
    /// `fns[i]` is the MIR body of `DefId(i)`, or `None` for `data`.
    pub fns: Vec<Option<MirBody>>,
}

impl MirModule {
    /// The MIR body of a function def, when present.
    pub fn body(&self, def: DefId) -> Option<&MirBody> {
        self.fns.get(def.index()).and_then(|b| b.as_ref())
    }
}
