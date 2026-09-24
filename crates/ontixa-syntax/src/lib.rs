//! The Ontixa syntax layer.
//!
//! Pipeline position:
//!
//! ```text
//! source text ──▶ lexer::lex ──▶ Vec<Token> ──▶ parser::parse ──▶ GreenNode (lossless CST)
//! ```
//!
//! The green tree is *lossless*: concatenating every token's text —
//! whitespace and comments included — reproduces the input exactly.
//! Comments and precise byte ranges therefore survive parsing, which is
//! what `ontixa fmt`, future IDE support, and semantic patches require.

mod fmt;
mod kinds;
mod lexer;
mod parser;

pub use fmt::format_file;
pub use kinds::{OntixaLanguage, SyntaxKind, keyword_kind};
pub use lexer::{Token, lex, token_text};
pub use parser::{Parse, parse};

/// Typed alias for syntax nodes of this language.
pub type SyntaxNode = rowan::SyntaxNode<OntixaLanguage>;
/// Typed alias for syntax tokens of this language.
pub type SyntaxToken = rowan::SyntaxToken<OntixaLanguage>;
/// Typed alias for node-or-token children.
pub type SyntaxElement = rowan::SyntaxElement<OntixaLanguage>;
/// Typed alias for the concrete syntax tree root.
pub type SyntaxTree = SyntaxNode;

/// Parses `src` and returns the typed root node plus diagnostics.
pub fn parse_file(src: &str) -> (SyntaxNode, ontixa_diagnostics::Diagnostics) {
    let parse = parse(src);
    (SyntaxNode::new_root(parse.green), parse.diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> SyntaxNode {
        let (root, diags) = parse_file(src);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        root
    }

    fn find_kind(root: &SyntaxNode, kind: SyntaxKind) -> bool {
        root.descendants().any(|n| n.kind() == kind)
    }

    #[test]
    fn tree_is_lossless() {
        let src = "// lead\ndata P {\n    x: i32; // trail\n}\nfn f() -> i32 { return 1; }\n";
        let root = parse_ok(src);
        assert_eq!(root.text().to_string(), src);
    }

    #[test]
    fn parses_data_and_fn() {
        let root = parse_ok("data P { x: i32; } fn f() -> i32 { return 1; }");
        assert!(find_kind(&root, SyntaxKind::DATA_DECL));
        assert!(find_kind(&root, SyntaxKind::FN_DECL));
        assert!(find_kind(&root, SyntaxKind::FIELD));
        assert!(find_kind(&root, SyntaxKind::PARAM_LIST));
    }

    #[test]
    fn parses_struct_literal_and_field_access() {
        let root =
            parse_ok("data P { x: i32; } fn f(p: P) -> i32 { let q = P { x: 1 }; return q.x; }");
        assert!(find_kind(&root, SyntaxKind::STRUCT_LIT));
        assert!(find_kind(&root, SyntaxKind::FIELD_EXPR));
    }

    #[test]
    fn parses_calls_and_binary_ops() {
        let root = parse_ok("fn f() -> i32 { return g(1, 2) + 3 * 4; }");
        assert!(find_kind(&root, SyntaxKind::CALL_EXPR));
        assert!(find_kind(&root, SyntaxKind::BIN_EXPR));
    }

    #[test]
    fn parses_if_else() {
        let root = parse_ok("fn f() -> i32 { if true { return 1; } else { return 2; } }");
        assert!(find_kind(&root, SyntaxKind::IF_EXPR));
    }

    #[test]
    fn parses_string_index_and_slices() {
        let root =
            parse_ok("fn f(s: str) -> str { return s[0] + s[1..3] + s[2..] + s[..4] + s[..]; }");
        // One index + four range forms share the INDEX_EXPR node;
        // each `..` range is a RANGE child.
        assert_eq!(
            root.descendants()
                .filter(|n| n.kind() == SyntaxKind::INDEX_EXPR)
                .count(),
            5
        );
        assert_eq!(
            root.descendants()
                .filter(|n| n.kind() == SyntaxKind::RANGE)
                .count(),
            4
        );
    }

    #[test]
    fn index_binds_tighter_than_infix() {
        // `s[0] == "a"` — the bracket is a postfix op on `s`, then
        // `==` compares the result.
        let root = parse_ok("fn f(s: str) -> bool { return s[0] == \"a\"; }");
        let bin = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::BIN_EXPR)
            .expect("binary expr");
        assert!(bin.children().any(|n| n.kind() == SyntaxKind::INDEX_EXPR));
    }

    #[test]
    fn empty_brackets_report() {
        let (_, diags) = parse_file("fn f(s: str) -> str { return s[]; }");
        assert!(diags.has_errors());
    }

    #[test]
    fn recovers_from_garbage() {
        let (root, diags) = parse_file("fn f( { return 1; } fn g() {}");
        assert!(diags.has_errors());
        // Tree still covers the whole input.
        assert!(root.text().len() > rowan::TextSize::from(0u32));
    }

    #[test]
    fn parses_array_literals() {
        let root = parse_ok("fn f() -> i32 { let a = [1, 2, 3]; let b = []; return a[0]; }");
        let arrays: Vec<_> = root
            .descendants()
            .filter(|n| n.kind() == SyntaxKind::ARRAY_EXPR)
            .collect();
        assert_eq!(arrays.len(), 2);
        // `a[0]` is still an index, not an array literal.
        assert!(find_kind(&root, SyntaxKind::INDEX_EXPR));
    }

    #[test]
    fn parses_for_over_range_and_array() {
        let root =
            parse_ok("fn f(a: [i32]) -> i32 { for i in 0..a.len { } for x in a { } return 0; }");
        assert_eq!(
            root.descendants()
                .filter(|n| n.kind() == SyntaxKind::FOR_EXPR)
                .count(),
            2
        );
        assert!(find_kind(&root, SyntaxKind::RANGE));
    }

    #[test]
    fn parses_all_range_forms() {
        // `..` alone is an unbounded range (rejected by the checker).
        let root =
            parse_ok("fn f() -> i32 { let s = \"a\"; return s[0] + s[1..] + s[..2] + s[..]; }");
        assert_eq!(
            root.descendants()
                .filter(|n| n.kind() == SyntaxKind::RANGE)
                .count(),
            3
        );
    }

    /// `for i in 0.. { }`: the `{` opens the loop *body*, never the
    /// range's upper bound — the same ambiguity rule as `if` and
    /// struct literals. (Without it, the body would parse as a bound.)
    #[test]
    fn range_never_takes_brace_bound() {
        let (root, diags) = parse_file("fn f() -> i32 { for i in 0.. { return i; } return 0; }");
        assert!(diags.is_empty(), "{diags:?}");
        let range = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::RANGE)
            .expect("range");
        assert_eq!(
            range
                .children()
                .filter(|n| n.kind() == SyntaxKind::BLOCK)
                .count(),
            0
        );
        let for_expr = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::FOR_EXPR)
            .expect("for");
        assert!(for_expr.children().any(|n| n.kind() == SyntaxKind::BLOCK));
    }

    /// `-7` must produce a `PREFIX_EXPR` whose operator token is the
    /// minus — leading trivia lives inside the node and used to hide
    /// the operator from the AST lowerer (it emitted `Expr::Error`,
    /// silently evaluating to `unit`).
    #[test]
    fn unary_minus_keeps_operator_visible() {
        let (root, diags) = parse_file("fn f() -> i32 { let x = -7; return x; }");
        assert!(diags.is_empty(), "{diags:?}");
        let pre = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::PREFIX_EXPR)
            .expect("prefix expr");
        let first = pre
            .children_with_tokens()
            .filter_map(|e| e.into_token())
            .find(|t| !t.kind().is_trivia())
            .expect("operator token");
        assert_eq!(first.kind(), SyntaxKind::MINUS);
    }

    #[test]
    fn parses_data_variants() {
        let root = parse_ok(
            "data Option { Some(i32); None; }\ndata Shape { Circle(i32); Rect(i32, i32); Origin; }",
        );
        assert_eq!(
            root.descendants()
                .filter(|n| n.kind() == SyntaxKind::VARIANT)
                .count(),
            5
        );
    }

    #[test]
    fn parses_match_and_patterns() {
        let root = parse_ok(
            "data Option { Some(i32); None; }\nfn f(o: Option) -> i32 { return match o { Option::Some(v) => v, Option::None => 0, _ => 1 }; }",
        );
        assert!(find_kind(&root, SyntaxKind::MATCH_EXPR));
        assert_eq!(
            root.descendants()
                .filter(|n| n.kind() == SyntaxKind::MATCH_ARM)
                .count(),
            3
        );
        assert_eq!(
            root.descendants()
                .filter(|n| n.kind() == SyntaxKind::PAT_VARIANT)
                .count(),
            2
        );
        assert!(find_kind(&root, SyntaxKind::PAT_BIND));
    }

    #[test]
    fn match_scrutinee_never_opens_struct_lit() {
        // `match s { ... }` — the `{` is the arm list even though `s`
        // could otherwise read as a record literal's name.
        let (root, diags) = parse_file(
            "data E { A; B; }\nfn f(e: E) -> i32 { match e { E::A => 1, E::B => 2 } return 0; }",
        );
        assert!(diags.is_empty(), "{diags:?}");
        let m = root
            .descendants()
            .find(|n| n.kind() == SyntaxKind::MATCH_EXPR)
            .expect("match expr");
        assert!(m.children().any(|n| n.kind() == SyntaxKind::MATCH_ARM_LIST));
        assert!(!m.children().any(|n| n.kind() == SyntaxKind::STRUCT_LIT));
    }

    #[test]
    fn mixed_data_members_error() {
        let (_, diags) = parse_file("data Bad { x: i32; V(i32); }");
        assert!(diags.has_errors());
        let (_, diags) = parse_file("data Bad { V(i32); x: i32; }");
        assert!(diags.has_errors());
    }

    #[test]
    fn malformed_input_does_not_panic() {
        for src in [
            "",
            "}",
            "fn",
            "fn fn fn",
            "data { }",
            "fn f(",
            "let",
            "return",
            "if",
            "fn f() { let = ; }",
            "fn f() { (((((",
            "fn f() -> ",
            "data D { x }",
            "\u{0}\u{0}\u{0}",
            "fn f() { x = = = ; }",
            "fn f(s: str) { s[]; }",
            "fn f(s: str) { s[; }",
            "fn f(s: str) { s[.. ..]; }",
            "fn f() { let a = [1, ; }",
            "fn f() { let a = [,]; }",
            "fn f() { for in { } }",
            "fn f() { for i { } }",
            "fn f() { for i in }",
            "fn f() { [",
            "fn f() { for i in 0.. [1] }",
            "fn f() { match }",
            "fn f() { match x }",
            "fn f() { match x { ",
            "fn f() { match x { => }",
            "fn f() { match x { T::V( => 1 } }",
            "data D { V( }",
            "data D { V(i32) }",
            "data D { x i32; }",
            "data D { ; }",
        ] {
            let _ = parse_file(src); // must not panic
        }
    }
}
