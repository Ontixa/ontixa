//! The Semantic Program Graph (SPG).
//!
//! The SPG is the compiler's structured self-description: every
//! definition, symbol, type, statement, and expression becomes a node;
//! every semantic relationship becomes a typed edge. It is the
//! primary interface for tooling and coding agents — they read the
//! graph instead of re-parsing text. Inferred parameter contracts are
//! first-class data here (`has_param` edges carry `behavior`).
//!
//! Milestone-1 scope: one module per file, no imports, no generics.

use ontixa_source::Span;
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as JsonValue};

/// A node identifier (index into [`SemanticGraph::nodes`]).
pub type NodeId = u32;

/// The graph's schema version — bump on breaking shape changes.
pub const SPG_SCHEMA_VERSION: u32 = 1;

/// What kind of entity a node represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// The compilation unit root.
    Module,
    /// A `fn` definition.
    Function,
    /// A `data` definition.
    Data,
    /// A parameter symbol.
    Param,
    /// A `data` field symbol.
    Field,
    /// A `let` binding symbol.
    Local,
    /// A type occurrence (deduplicated per semantic type).
    Type,
    /// An expression node; `expr_kind` in attrs gives the HIR kind.
    Expr,
    /// A statement node; `stmt_kind` in attrs gives the HIR kind.
    Stmt,
}

/// What relationship an edge represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Module → definition.
    Declares,
    /// Function → parameter symbol. `behavior` attr = inferred contract.
    HasParam,
    /// Function → its return type node.
    Returns,
    /// Data → field symbol.
    HasField,
    /// Function → local binding symbol.
    HasLocal,
    /// Symbol/expr → its type node.
    TypedAs,
    /// Parent → child (block→stmt, block→tail, expr→subexpr, stmt→expr).
    Contains,
    /// Call expression → callee function.
    Calls,
    /// Var expression → referenced symbol.
    Reads,
    /// Field expression → accessed field symbol.
    AccessesField,
    /// Struct-literal expression → constructed data def.
    Constructs,
    /// `let` statement → bound local symbol.
    Binds,
    /// Assign statement → written local/param symbol.
    Writes,
}

/// One node in the semantic graph.
#[derive(Debug, Clone, Serialize)]
pub struct SpgNode {
    /// Node identifier.
    pub id: NodeId,
    /// Entity kind.
    pub kind: NodeKind,
    /// Human-readable label (name or short description).
    pub label: String,
    /// Source span when the node has one.
    pub span: Option<Span>,
    /// Stable structured attributes (e.g. `behavior`, `ty`, `expr_kind`).
    #[serde(skip_serializing_if = "JsonMap::is_empty")]
    pub attrs: JsonMap<String, JsonValue>,
}

/// One typed edge between nodes.
#[derive(Debug, Clone, Serialize)]
pub struct SpgEdge {
    /// Source node.
    pub from: NodeId,
    /// Target node.
    pub to: NodeId,
    /// Relationship kind.
    pub kind: EdgeKind,
    /// Optional structured attributes (e.g. `behavior`, `position`).
    #[serde(skip_serializing_if = "JsonMap::is_empty")]
    pub attrs: JsonMap<String, JsonValue>,
}

/// The semantic program graph for one module.
#[derive(Debug, Default, Serialize)]
pub struct SemanticGraph {
    /// Schema version for machine consumers.
    pub schema: u32,
    /// All nodes in allocation order.
    pub nodes: Vec<SpgNode>,
    /// All edges in allocation order.
    pub edges: Vec<SpgEdge>,
}

impl SemanticGraph {
    pub fn new() -> Self {
        Self {
            schema: SPG_SCHEMA_VERSION,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    pub fn add_node(
        &mut self,
        kind: NodeKind,
        label: impl Into<String>,
        span: Option<Span>,
    ) -> NodeId {
        let id = self.nodes.len() as NodeId;
        self.nodes.push(SpgNode {
            id,
            kind,
            label: label.into(),
            span,
            attrs: JsonMap::new(),
        });
        id
    }

    pub fn add_edge(&mut self, from: NodeId, to: NodeId, kind: EdgeKind) {
        self.edges.push(SpgEdge {
            from,
            to,
            kind,
            attrs: JsonMap::new(),
        });
    }

    /// Adds an edge carrying a single attribute.
    pub fn add_edge_attr(
        &mut self,
        from: NodeId,
        to: NodeId,
        kind: EdgeKind,
        key: &str,
        value: JsonValue,
    ) {
        self.edges.push(SpgEdge {
            from,
            to,
            kind,
            attrs: {
                let mut m = JsonMap::new();
                m.insert(key.to_string(), value);
                m
            },
        });
    }

    /// Sets an attribute on an existing node.
    pub fn set_attr(&mut self, id: NodeId, key: &str, value: JsonValue) {
        self.nodes[id as usize].attrs.insert(key.to_string(), value);
    }
}
