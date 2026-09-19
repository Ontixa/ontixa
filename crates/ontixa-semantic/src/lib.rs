//! The Semantic Program Graph — the compiler's structured
//! self-description.
//!
//! Pipeline position:
//!
//! ```text
//! HirModule + TypeTables + OwnershipTables ──▶ build_graph ──▶ SemanticGraph
//! ```
//!
//! Tooling and coding agents consume the graph (JSON) instead of
//! re-parsing text. Inferred parameter contracts appear on
//! `has_param` edges as `behavior`.

mod build;
mod graph;

pub use build::build_graph;
pub use graph::{EdgeKind, NodeId, NodeKind, SPG_SCHEMA_VERSION, SemanticGraph, SpgEdge, SpgNode};

/// Full pipeline convenience: parse → HIR → types → ownership → graph.
pub fn graph_src(
    src: &str,
) -> (
    SemanticGraph,
    ontixa_hir::HirModule,
    ontixa_types::ModuleTypes,
    ontixa_memory::OwnershipTables,
    ontixa_source::Interner,
    ontixa_diagnostics::Diagnostics,
) {
    let (module, tables, ownership, interner, diags) = ontixa_memory::analyze_src(src);
    let graph = build_graph(&module, &tables, &ownership, &interner);
    (graph, module, tables, ownership, interner, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_contains_param_behaviors() {
        let (g, _, _, _, _, diags) = graph_src(
            "data P { x: i32; } fn read(p: P) -> i32 { return p.x; } fn main() -> i32 { let q = P { x: 1 }; return read(q); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let read = g
            .nodes
            .iter()
            .find(|n| n.label == "read")
            .expect("read fn node");
        let param_edge = g
            .edges
            .iter()
            .find(|e| e.from == read.id && e.kind == EdgeKind::HasParam)
            .expect("has_param edge");
        assert_eq!(
            param_edge.attrs.get("behavior").and_then(|v| v.as_str()),
            Some("borrow")
        );
    }

    #[test]
    fn graph_has_call_edges() {
        let (g, _, _, _, _, diags) =
            graph_src("fn g() -> i32 { return 1; } fn main() -> i32 { return g(); }");
        assert!(diags.is_empty(), "{diags:?}");
        let gnode = g.nodes.iter().find(|n| n.label == "g").expect("g node");
        assert!(
            g.edges
                .iter()
                .any(|e| e.kind == EdgeKind::Calls && e.to == gnode.id)
        );
    }

    #[test]
    fn graph_serializes_to_json() {
        let (g, _, _, _, _, _) = graph_src("fn main() -> i32 { return 42; }");
        let v = serde_json::to_value(&g).expect("serialize");
        assert!(v.get("nodes").is_some());
        assert!(v.get("edges").is_some());
        assert_eq!(v.get("schema").and_then(|s| s.as_u64()), Some(1));
    }

    #[test]
    fn type_nodes_are_deduplicated() {
        let (g, _, _, _, _, diags) = graph_src(
            "fn f(a: i32, b: i32) -> i32 { return a + b; } fn main() -> i32 { return f(1, 2); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let i32_nodes = g
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Type && n.label == "i32")
            .count();
        assert_eq!(i32_nodes, 1);
    }
}
