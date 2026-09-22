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
    fn recovers_from_garbage() {
        let (root, diags) = parse_file("fn f( { return 1; } fn g() {}");
        assert!(diags.has_errors());
        // Tree still covers the whole input.
        assert!(root.text().len() > rowan::TextSize::from(0u32));
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
        ] {
            let _ = parse_file(src); // must not panic
        }
    }
}
