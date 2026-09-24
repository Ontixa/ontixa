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
    let (ast, mut module, interner, mut diags) = ontixa_hir::parse_hir_ast(src);
    let tables = ontixa_types::check_module(&mut module, &interner, &mut diags);
    let ownership = ontixa_memory::infer_ownership(
        &module,
        &tables,
        &interner,
        &mut diags,
        None,
        &mut ontixa_memory::OwnershipOracle::default(),
    );
    // Rebase item-relative diagnostics to file-absolute, then derive
    // each def's item base for the graph's absolute node spans.
    diags.rebase_tagged(|d| ast.items[d.index()].span().start);
    let bases: Vec<u32> = ast.items.iter().map(|i| i.span().start).collect();
    let graph = build_graph(&module, &tables, &ownership, &interner, &bases);
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
    fn call_args_carry_passes_edges() {
        // The memory edge: `read(q)` passes `q`'s place to `read`'s
        // param under the inferred `borrow` contract.
        let (g, _, _, _, _, diags) = graph_src(
            "data P { x: i32; } fn read(p: P) -> i32 { return p.x; } fn main() -> i32 { let q = P { x: 1 }; return read(q); }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let passes: Vec<_> = g
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Passes)
            .collect();
        assert_eq!(passes.len(), 1);
        let e = passes[0];
        assert_eq!(
            e.attrs.get("behavior").and_then(|v| v.as_str()),
            Some("borrow")
        );
        assert_eq!(e.attrs.get("position").and_then(|v| v.as_u64()), Some(0));
        // Target is the callee's param node, carrying the same contract.
        let p = &g.nodes[e.to as usize];
        assert_eq!(p.kind, NodeKind::Param);
        assert_eq!(p.label, "p");
        assert_eq!(
            p.attrs.get("behavior").and_then(|v| v.as_str()),
            Some("borrow")
        );
        // Source is an expression node inside `main`.
        assert_eq!(g.nodes[e.from as usize].kind, NodeKind::Expr);
    }

    #[test]
    fn passes_edge_reflects_inferred_contract() {
        // A mutating callee produces a `borrow_mut` edge; an escaping
        // callee produces `escape` — the SPG exposes the distinction.
        let (g, _, _, _, _, diags) = graph_src(
            "data P { x: i32; }
             fn bump(mut p: P) { p.x = p.x + 1; }
             fn keep(p: P) -> P { return p; }
             fn main() -> i32 { let mut q = P { x: 1 }; bump(q); let r = keep(q); return r.x; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let behaviors: Vec<_> = g
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Passes)
            .map(|e| e.attrs["behavior"].as_str().unwrap())
            .collect();
        assert_eq!(behaviors, ["borrow_mut", "escape"]);
    }

    #[test]
    fn param_node_carries_escape_summary_and_evidence() {
        let (g, _, _, _, _, diags) = graph_src(
            "data P { x: i32; } fn keep(p: P) -> P { return p; } fn main() -> i32 { let q = P { x: 1 }; let r = keep(q); return r.x; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let p = g
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Param && n.label == "p")
            .expect("param node");
        let escapes: Vec<_> = p.attrs["escapes"]
            .as_array()
            .expect("escapes attr")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(escapes, ["return"]);
        // Evidence records the use site that proved the escape.
        let evidence = p.attrs["evidence"].as_array().expect("evidence attr");
        assert!(
            evidence
                .iter()
                .any(|e| e["kind"].as_str() == Some("escaped"))
        );
    }

    #[test]
    fn string_ops_have_graph_nodes() {
        let (g, _, _, _, _, diags) =
            graph_src("fn main() -> i32 { let s = \"abc\"; return s[0].len + s[1..].len; }");
        assert!(diags.is_empty(), "{diags:?}");
        fn kind(n: &SpgNode) -> &str {
            n.attrs
                .get("expr_kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        }
        assert!(g.nodes.iter().any(|n| kind(n) == "index"));
        assert!(g.nodes.iter().any(|n| kind(n) == "slice"));
        assert_eq!(g.nodes.iter().filter(|n| kind(n) == "len").count(), 2);
        // The slice's `lo` bound edge is role-tagged.
        let slice = g
            .nodes
            .iter()
            .find(|n| kind(n) == "slice")
            .expect("slice node");
        assert!(g.edges.iter().any(|e| {
            e.from == slice.id && e.attrs.get("role").and_then(|v| v.as_str()) == Some("lo")
        }));
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

    fn expr_kind(n: &SpgNode) -> &str {
        n.attrs
            .get("expr_kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
    }

    #[test]
    fn array_and_for_have_graph_nodes() {
        let (g, _, _, _, _, diags) = graph_src(
            "fn main() -> i32 { let a = [1, 2]; let mut t = 0; for x in a { t = t + x; } return t + a.len; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        assert!(g.nodes.iter().any(|n| expr_kind(n) == "array_lit"));
        assert!(g.nodes.iter().any(|n| expr_kind(n) == "for"));
        assert!(g.nodes.iter().any(|n| expr_kind(n) == "len"));
        // The array literal's elements attach by position.
        let lit = g
            .nodes
            .iter()
            .find(|n| expr_kind(n) == "array_lit")
            .expect("array_lit node");
        let positions: Vec<u64> = g
            .edges
            .iter()
            .filter(|e| e.from == lit.id && e.kind == EdgeKind::Contains)
            .filter_map(|e| e.attrs.get("position").and_then(|v| v.as_u64()))
            .collect();
        assert_eq!(positions.len(), 2);
        // `for` carries `iter` and `body` role edges.
        let f = g
            .nodes
            .iter()
            .find(|n| expr_kind(n) == "for")
            .expect("for node");
        for role in ["iter", "body"] {
            assert!(
                g.edges.iter().any(|e| {
                    e.from == f.id
                        && e.kind == EdgeKind::Contains
                        && e.attrs.get("role").and_then(|v| v.as_str()) == Some(role)
                }),
                "no `{role}` edge on for node"
            );
        }
    }

    #[test]
    fn range_for_has_bound_edges() {
        let (g, _, _, _, _, diags) =
            graph_src("fn main() -> i32 { for i in 0..3 { let z = i; } return 0; }");
        assert!(diags.is_empty(), "{diags:?}");
        let range = g
            .nodes
            .iter()
            .find(|n| expr_kind(n) == "range")
            .expect("range node");
        for role in ["lo", "hi"] {
            assert!(
                g.edges.iter().any(|e| {
                    e.from == range.id
                        && e.kind == EdgeKind::Contains
                        && e.attrs.get("role").and_then(|v| v.as_str()) == Some(role)
                }),
                "no `{role}` edge on range node"
            );
        }
    }

    #[test]
    fn array_type_nodes_render_bracketed() {
        let (g, _, _, _, _, diags) =
            graph_src("fn f(a: [i64]) -> [i64] { return a; } fn main() -> i32 { return 0; }");
        assert!(diags.is_empty(), "{diags:?}");
        assert!(
            g.nodes
                .iter()
                .any(|n| n.kind == NodeKind::Type && n.label == "[i64]")
        );
    }

    // ---- enum variants and `match` ----------------------------------

    #[test]
    fn enum_variants_have_graph_nodes() {
        let (g, _, _, _, _, diags) = graph_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(3); return match o { Opt::Some(v) => v, Opt::None => 0 }; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let opt = g
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Data && n.label == "Opt")
            .expect("Opt data node");
        assert_eq!(opt.attrs["shape"].as_str(), Some("enum"));
        // Two `has_variant` edges, discriminant-tagged in order.
        let mut edges: Vec<(u64, NodeId)> = g
            .edges
            .iter()
            .filter(|e| e.kind == EdgeKind::HasVariant && e.from == opt.id)
            .map(|e| (e.attrs["discriminant"].as_u64().unwrap(), e.to))
            .collect();
        edges.sort();
        assert_eq!(edges.len(), 2);
        let some = &g.nodes[edges[0].1 as usize];
        assert_eq!(some.kind, NodeKind::Variant);
        assert_eq!(some.label, "Some");
        // `Some(i32)`'s payload links position 0 to the i32 type node.
        assert!(g.edges.iter().any(|e| {
            e.from == some.id
                && e.kind == EdgeKind::TypedAs
                && e.attrs.get("position").and_then(|v| v.as_u64()) == Some(0)
                && g.nodes[e.to as usize].label == "i32"
        }));
    }

    #[test]
    fn variant_lit_constructs_variant_node() {
        let (g, _, _, _, _, diags) = graph_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(3); return match o { _ => 0 }; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let lit = g
            .nodes
            .iter()
            .find(|n| expr_kind(n) == "variant_lit")
            .expect("variant_lit node");
        assert_eq!(lit.attrs["variant"].as_u64(), Some(0));
        let edge = g
            .edges
            .iter()
            .find(|e| e.from == lit.id && e.kind == EdgeKind::Constructs)
            .expect("constructs edge");
        let target = &g.nodes[edge.to as usize];
        assert_eq!(target.kind, NodeKind::Variant);
        assert_eq!(target.label, "Some");
    }

    #[test]
    fn match_arms_link_patterns_binds_and_bodies() {
        let (g, _, _, _, _, diags) = graph_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::Some(3); return match o { Opt::Some(v) => v, other => 0 }; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let m = g
            .nodes
            .iter()
            .find(|n| expr_kind(n) == "match")
            .expect("match node");
        // Scrutinee edge.
        assert!(g.edges.iter().any(|e| {
            e.from == m.id
                && e.kind == EdgeKind::Contains
                && e.attrs.get("role").and_then(|v| v.as_str()) == Some("scrutinee")
        }));
        // Two arm nodes, position-tagged.
        let mut arms: Vec<(u64, NodeId)> = g
            .edges
            .iter()
            .filter(|e| {
                e.from == m.id && e.kind == EdgeKind::Contains && e.attrs.get("position").is_some()
            })
            .map(|e| (e.attrs["position"].as_u64().unwrap(), e.to))
            .collect();
        arms.sort();
        assert_eq!(arms.len(), 2);
        let arm0 = &g.nodes[arms[0].1 as usize];
        let arm1 = &g.nodes[arms[1].1 as usize];
        assert_eq!(arm0.kind, NodeKind::Arm);
        assert_eq!(arm0.attrs["pattern"].as_str(), Some("variant"));
        assert_eq!(arm0.attrs["variant"].as_u64(), Some(0));
        // Arm 0: `matches` → the `Some` variant, `binds` → `v`.
        let matched = g
            .edges
            .iter()
            .find(|e| e.from == arm0.id && e.kind == EdgeKind::Matches)
            .expect("matches edge");
        assert_eq!(g.nodes[matched.to as usize].label, "Some");
        let bound = g
            .edges
            .iter()
            .find(|e| e.from == arm0.id && e.kind == EdgeKind::Binds)
            .expect("binds edge");
        assert_eq!(g.nodes[bound.to as usize].label, "v");
        assert_eq!(g.nodes[bound.to as usize].kind, NodeKind::Local);
        // Both arms contain their body expression.
        for arm in [arm0, arm1] {
            assert!(g.edges.iter().any(|e| {
                e.from == arm.id
                    && e.kind == EdgeKind::Contains
                    && e.attrs.get("role").and_then(|v| v.as_str()) == Some("body")
            }));
        }
        // Arm 1 is a whole-scrutinee bind.
        assert_eq!(arm1.attrs["pattern"].as_str(), Some("bind"));
        let bound1 = g
            .edges
            .iter()
            .find(|e| e.from == arm1.id && e.kind == EdgeKind::Binds)
            .expect("binds edge");
        assert_eq!(g.nodes[bound1.to as usize].label, "other");
    }

    #[test]
    fn wildcard_arm_pattern_is_tagged() {
        let (g, _, _, _, _, diags) = graph_src(
            "data Opt { Some(i32); None; } fn main() -> i32 { let o = Opt::None; return match o { _ => 0 }; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let arm = g
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Arm)
            .expect("arm node");
        assert_eq!(arm.attrs["pattern"].as_str(), Some("wildcard"));
    }
}
