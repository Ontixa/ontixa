//! Canonical formatting over the lossless CST.
//!
//! `ontixa fmt` is a tree transform, not a text rewrite: the source is
//! parsed into the rowan green tree, every non-whitespace token is
//! re-emitted in source order, and the separator between each adjacent
//! pair is recomputed from token kinds and each token's parent node —
//! never from the original spacing. Token text is emitted verbatim, so
//! a formatted file re-lexes to an identical token stream and the
//! formatter is idempotent: `format_file(format_file(x))` is stable.
//!
//! Canonical rules:
//!
//! * four spaces per nesting level — `{` of a block/`data`/struct
//!   literal and `(`/`[` open a level, their closers end it;
//! * every item, statement, and field starts on its own line —
//!   one-line `if` bodies and `data` field lists always expand;
//! * `} else {` stays joined; empty brace pairs glue to `{}`;
//! * exactly one blank line between top-level items, except `use`
//!   declarations which group without blank lines;
//! * no space before `(` `[` `)` `]` `,` `;` `:` `::` `.` `..` —
//!   except `(` after a keyword (`if (x)` keeps its space) and `[`
//!   opening an array literal or `[T]` type, which space normally
//!   (indexing stays glued: `s[i]`); no
//!   space after `(` `[` `::` `.` `..` or a prefix `-`/`!`; `:` spaces
//!   after;
//!   everything else is separated by a single space;
//! * comments are preserved verbatim: a comment sharing a line with
//!   code stays trailing (one space), an own-line comment keeps its
//!   own line at canonical indent, and a `//` comment forces a line
//!   break after itself (trailing whitespace is stripped);
//! * line endings normalize to `\n` and the file ends with exactly
//!   one; runs of blank lines collapse.
//!
//! Deliberate limits: blocks never stay inline and struct literals
//! never break — the canonical form is structural, with no
//! width-based line breaking. Files with parse diagnostics are
//! refused rather than reformatted: tokens inside `ERROR` nodes have
//! no trustworthy layout rules.

use crate::kinds::SyntaxKind;
use crate::{SyntaxNode, SyntaxToken, parse_file};
use ontixa_diagnostics::Diagnostics;

/// One indentation level — four spaces, matching the shipped examples.
const INDENT: &str = "    ";

/// Formats `src` into the canonical Ontixa layout.
///
/// Returns the parse's own diagnostics when the file does not parse
/// cleanly — formatting tokens inside `ERROR` nodes would guess at
/// structure the parser itself rejected. Otherwise returns the
/// canonical text, which always ends with a single `\n` (or is empty
/// when the input holds no tokens).
pub fn format_file(src: &str) -> Result<String, Diagnostics> {
    let (root, diags) = parse_file(src);
    if diags.has_errors() {
        return Err(diags);
    }
    Ok(render(&root, src))
}

/// The whitespace emitted between two adjacent tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sep {
    /// Tokens glue directly: `a.x`, `f(`, `{}`.
    None,
    /// One space: `x: i32`, `a + b`, `} else {`.
    Space,
    /// Line break plus canonical indentation.
    Newline,
    /// Blank line plus indentation — top-level item boundary.
    BlankLine,
}

/// Whether `tok` begins a statement, item, or field on a fresh line —
/// and if so whether that node sits at file top level.
enum LinePos {
    /// Inside a block/`data` body — newline at the current indent.
    Nested,
    /// Direct child of `SOURCE_FILE` — blank-line boundary, unless the
    /// item groups with its predecessor (`use` runs).
    Top(SyntaxKind),
}

/// Node kinds whose leading token starts a fresh line. `FIELD` is the
/// `name: Type;` member of `data`; `STRUCT_LIT_FIELD` is deliberately
/// absent so struct literals stay inline.
const LINE_KINDS: &[SyntaxKind] = &[
    SyntaxKind::USE_DECL,
    SyntaxKind::DATA_DECL,
    SyntaxKind::FN_DECL,
    SyntaxKind::FIELD,
    SyntaxKind::LET_STMT,
    SyntaxKind::ASSIGN_STMT,
    SyntaxKind::RETURN_STMT,
    SyntaxKind::EXPR_STMT,
];

/// The `{`/`}`-style parents that open an indentation level.
/// `STRUCT_LIT` counts so a comment forcing a newline inside a literal
/// still lands at a sensible depth; its braces never break the line
/// on their own.
fn braced(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::BLOCK | SyntaxKind::DATA_DECL | SyntaxKind::STRUCT_LIT
    )
}

/// `tok` opens a nesting level — `{` of a braced node, `(` or `[`.
fn opens_indent(tok: &SyntaxToken) -> bool {
    match tok.kind() {
        SyntaxKind::L_PAREN | SyntaxKind::L_BRACKET => true,
        SyntaxKind::L_BRACE => tok.parent().is_some_and(|p| braced(p.kind())),
        _ => false,
    }
}

/// `tok` closes a nesting level opened by [`opens_indent`]. The
/// dedent is applied *before* the token emits so a `}` landing on its
/// own line sits at the outer indent.
fn closes_indent(tok: &SyntaxToken) -> bool {
    match tok.kind() {
        SyntaxKind::R_PAREN | SyntaxKind::R_BRACKET => true,
        SyntaxKind::R_BRACE => tok.parent().is_some_and(|p| braced(p.kind())),
        _ => false,
    }
}

/// `}` of a block or `data` body gets its own dedented line.
/// Struct-literal braces stay inline (`P { x: 1 }`).
fn is_block_close(tok: &SyntaxToken) -> bool {
    tok.kind() == SyntaxKind::R_BRACE
        && tok
            .parent()
            .is_some_and(|p| matches!(p.kind(), SyntaxKind::BLOCK | SyntaxKind::DATA_DECL))
}

/// `-`/`!` used as a prefix operator glues to its operand (`-x`);
/// the same tokens in infix position space normally (`a - b`). The
/// parent node disambiguates.
fn is_prefix_op(tok: &SyntaxToken) -> bool {
    matches!(tok.kind(), SyntaxKind::MINUS | SyntaxKind::NOT)
        && tok
            .parent()
            .is_some_and(|p| p.kind() == SyntaxKind::PREFIX_EXPR)
}

/// If `tok` begins a line-kind node — everything before it inside the
/// nearest `LINE_KINDS` ancestor is trivia — returns its position
/// class. For comments the rule is stricter: only *whitespace* may
/// precede, so `// a` / `// b` comment runs stay glued while the
/// first comment still carries the item's line break.
fn line_pos(tok: &SyntaxToken) -> Option<LinePos> {
    let node = tok
        .parent_ancestors()
        .find(|n| LINE_KINDS.contains(&n.kind()))?;
    let starts = node
        .descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .take_while(|t| t.text_range() != tok.text_range())
        .all(|t| match t.kind() {
            SyntaxKind::WHITESPACE => true,
            SyntaxKind::COMMENT => tok.kind() != SyntaxKind::COMMENT,
            _ => false,
        });
    if !starts {
        return None;
    }
    if node
        .parent()
        .is_some_and(|p| p.kind() == SyntaxKind::SOURCE_FILE)
    {
        Some(LinePos::Top(node.kind()))
    } else {
        Some(LinePos::Nested)
    }
}

/// The kind of the top-level item containing `tok` — used to keep
/// consecutive `use` declarations grouped without blank lines.
fn item_kind_of(tok: &SyntaxToken) -> Option<SyntaxKind> {
    tok.parent_ancestors()
        .find(|n| {
            n.parent()
                .is_some_and(|p| p.kind() == SyntaxKind::SOURCE_FILE)
        })
        .map(|n| n.kind())
}

/// Whether the source between `a` and `b` contained a line break —
/// only whitespace can sit there, so this is a substring check. It
/// decides whether a comment was trailing or on its own line.
fn newline_between(a: &SyntaxToken, b: &SyntaxToken, src: &str) -> bool {
    let lo = usize::from(a.text_range().end());
    let hi = usize::from(b.text_range().start());
    src[lo..hi].contains('\n')
}

/// The canonical separator between adjacent tokens `prev` and `cur`.
fn separator(prev: &SyntaxToken, cur: &SyntaxToken, src: &str) -> Sep {
    let pk = prev.kind();
    let ck = cur.kind();
    // A `//` comment swallows the rest of its line — whatever follows
    // must start a fresh one.
    if pk == SyntaxKind::COMMENT && prev.text().starts_with("//") {
        return Sep::Newline;
    }
    // Adjacent `{`/`}` are an empty pair — `fn f() {}`, `data D {}`.
    if pk == SyntaxKind::L_BRACE && ck == SyntaxKind::R_BRACE {
        return Sep::None;
    }
    // A comment on the same line as code stays attached to it.
    if ck == SyntaxKind::COMMENT && !newline_between(prev, cur, src) {
        return Sep::Space;
    }
    match line_pos(cur) {
        Some(LinePos::Nested) => return Sep::Newline,
        Some(LinePos::Top(kind)) => {
            // A comment directly before the item's first real token
            // already carried the boundary — don't blank-line twice.
            if pk == SyntaxKind::COMMENT {
                return Sep::Newline;
            }
            // `use` declarations group; every other item boundary is
            // exactly one blank line.
            let grouped =
                kind == SyntaxKind::USE_DECL && item_kind_of(prev) == Some(SyntaxKind::USE_DECL);
            return if grouped {
                Sep::Newline
            } else {
                Sep::BlankLine
            };
        }
        None => {}
    }
    // An own-line comment mid-node takes the current indent.
    if ck == SyntaxKind::COMMENT {
        return Sep::Newline;
    }
    // A block/`data` `}` closes its body on a fresh dedented line.
    if is_block_close(cur) {
        return Sep::Newline;
    }
    // Tight pairs: no space before closers/separators/`::`/`.`/`:`;
    // `(` glues to a callee or group but not to a keyword (`if (x)`
    // keeps its space); `[` glues only for indexing (`s[i]`) — an
    // array literal or `[T]` type spaces normally (`= [1]`,
    // `a: [i32]`); nothing after `(`/`[`/`::`/`.` or a
    // prefix `-`/`!`. `..` glues only to a bound that lives inside
    // its own RANGE node — in `for i in 0.. {` the `{` is the loop
    // body and keeps its space.
    if matches!(
        ck,
        SyntaxKind::R_PAREN
            | SyntaxKind::R_BRACKET
            | SyntaxKind::COMMA
            | SyntaxKind::SEMICOLON
            | SyntaxKind::COLON
            | SyntaxKind::COLON2
            | SyntaxKind::DOT
            | SyntaxKind::DOT2
    ) || (ck == SyntaxKind::L_PAREN && !pk.is_keyword())
        || (ck == SyntaxKind::L_BRACKET
            && cur
                .parent()
                .is_some_and(|p| p.kind() == SyntaxKind::INDEX_EXPR))
        || matches!(
            pk,
            SyntaxKind::L_PAREN | SyntaxKind::L_BRACKET | SyntaxKind::DOT | SyntaxKind::COLON2
        )
        || (pk == SyntaxKind::DOT2
            && prev
                .parent()
                .is_some_and(|r| cur.parent_ancestors().any(|a| a == r)))
        || is_prefix_op(prev)
    {
        return Sep::None;
    }
    Sep::Space
}

/// Re-emits `root`'s token stream with canonical separators.
fn render(root: &SyntaxNode, src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut indent = 0usize;
    let mut prev: Option<SyntaxToken> = None;
    for el in root.descendants_with_tokens() {
        let Some(tok) = el.into_token() else { continue };
        if tok.kind() == SyntaxKind::WHITESPACE {
            continue;
        }
        if let Some(p) = &prev {
            // Closers dedent before emitting so `}` on its own line
            // sits at the outer indent.
            if closes_indent(&tok) {
                indent = indent.saturating_sub(1);
            }
            match separator(p, &tok, src) {
                Sep::None => {}
                Sep::Space => out.push(' '),
                Sep::Newline => {
                    out.push('\n');
                    for _ in 0..indent {
                        out.push_str(INDENT);
                    }
                }
                Sep::BlankLine => {
                    out.push_str("\n\n");
                    for _ in 0..indent {
                        out.push_str(INDENT);
                    }
                }
            }
        }
        // `//` comments emit end-trimmed (drops `\r` and stray
        // trailing spaces); block comments and code emit verbatim.
        if tok.kind() == SyntaxKind::COMMENT && tok.text().starts_with("//") {
            out.push_str(tok.text().trim_end());
        } else {
            out.push_str(tok.text());
        }
        if opens_indent(&tok) {
            indent += 1;
        }
        prev = Some(tok);
    }
    // Exactly one trailing newline on a non-empty file.
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(src: &str) -> String {
        format_file(src).expect("fixture must parse")
    }

    /// Golden: a collapsed program expands to the canonical layout —
    /// 4-space indent, one item boundary blank line, `x: i32` spacing.
    #[test]
    fn golden_layout() {
        let src =
            "data Point{   x:i32;y:i32;}\nfn distance2(p:Point)->i32{return p.x*p.x+p.y*p.y;}";
        let want = "data Point {\n    x: i32;\n    y: i32;\n}\n\nfn distance2(p: Point) -> i32 {\n    return p.x * p.x + p.y * p.y;\n}\n";
        assert_eq!(fmt(src), want);
    }

    /// Idempotent on everything: formatted output formats to itself.
    #[test]
    fn idempotent() {
        for src in [
            "fn main() -> i32 {\n    return 42;\n}\n",
            "data Point{   x:i32;y:i32;}\nfn f(p:Point)->i32{return p.x;}",
            "// lead\nfn f(){// inner\nlet x=if true{1}else{2};return x;// trail\n}\n// tail",
            "use m;use m::x as y;fn main()->i32{return m::f( 1 ,x );}",
            "fn f()->i32{let mut q=Point{x:1,y:2};q.x=q.x+1;return -q.x;}",
            "",
            "   \n\n",
            "// only a comment",
            "fn f() { let x = { 1 }; return x; }",
        ] {
            let once = fmt(src);
            let twice = fmt(&once);
            assert_eq!(once, twice, "not idempotent for {src:?}");
        }
    }

    /// Comments survive verbatim and keep their line position class:
    /// trailing stays trailing, own-line stays on its own line.
    #[test]
    fn comments_preserved() {
        let src = "// file header\nfn f() -> i32 { // inner\n    return 1; // trail\n}\n// tail";
        let got = fmt(src);
        assert_eq!(
            got,
            "// file header\nfn f() -> i32 { // inner\n    return 1; // trail\n}\n// tail\n"
        );
        // Block comments survive mid-expression.
        let src = "fn f() -> i32 { return /* note */ 1; }";
        assert_eq!(fmt(src), "fn f() -> i32 {\n    return /* note */ 1;\n}\n");
    }

    /// An already-canonical file is a byte-for-byte no-op.
    #[test]
    fn already_formatted_noop() {
        let src = "data Point {\n    x: i32;\n    y: i32;\n}\n\nfn distance2(p: Point) -> i32 {\n    return p.x * p.x + p.y * p.y;\n}\n";
        assert_eq!(fmt(src), src);
    }

    /// `use` declarations group without blank lines; other items are
    /// blank-separated. Blank runs collapse to one line.
    #[test]
    fn use_decls_group() {
        let src = "use a;\n\n\nuse a::x;\nuse b;\ndata D { f: i32; }";
        let want = "use a;\nuse a::x;\nuse b;\n\ndata D {\n    f: i32;\n}\n";
        assert_eq!(fmt(src), want);
    }

    /// `} else {` joins; single-statement blocks expand.
    #[test]
    fn if_else_canonical() {
        let src = "fn f(x:i32)->i32{return if x<0{0-x}else{x};}";
        let want = "fn f(x: i32) -> i32 {\n    return if x < 0 {\n        0 - x\n    } else {\n        x\n    };\n}\n";
        assert_eq!(fmt(src), want);
    }

    /// Empty brace pairs glue; empty files stay empty.
    #[test]
    fn empty_braces_and_empty_file() {
        assert_eq!(fmt("fn f() { }\ndata D {}\n"), "fn f() {}\n\ndata D {}\n");
        assert_eq!(fmt(""), "");
        assert_eq!(fmt("  \n\n "), "");
    }

    /// Prefix operators glue; infix operators space.
    #[test]
    fn prefix_vs_infix() {
        assert_eq!(
            fmt("fn f() -> i32 { let x = - 1; return ! true || x*-2; }"),
            "fn f() -> i32 {\n    let x = -1;\n    return !true || x * -2;\n}\n"
        );
    }

    /// Indexing and slicing glue: `s [ i ]` → `s[i]`, `a .. b` →
    /// `a..b`, and open bounds stay tight (`s[..]`, `s[i..]`).
    #[test]
    fn index_and_range_glue() {
        let src = "fn f(s: str) -> str { return s[ 0 ] + s[ 1 .. 3 ] + s[ 2 .. ] + s[ .. 4 ] + s[ .. ]; }";
        let want =
            "fn f(s: str) -> str {\n    return s[0] + s[1..3] + s[2..] + s[..4] + s[..];\n}\n";
        assert_eq!(fmt(src), want);
    }

    /// `[e, ...]` glues tight inside brackets; `for` spaces its
    /// keywords and keeps the `..` bound tight.
    #[test]
    fn array_and_for_canonical() {
        let src = "fn f(a:[i32])->i32{let b=[1,2,3];for i in 0..a.len{let z=b[i];}return b[0];}";
        let want = "fn f(a: [i32]) -> i32 {\n    let b = [1, 2, 3];\n    for i in 0..a.len {\n        let z = b[i];\n    }\n    return b[0];\n}\n";
        assert_eq!(fmt(src), want);
        // Empty array, open range, `for mut`.
        let src2 = "fn f()->i32{let e: [i32]=[ ];for mut x in 1..{x=x;}return 0;}";
        let want2 = "fn f() -> i32 {\n    let e: [i32] = [];\n    for mut x in 1.. {\n        x = x;\n    }\n    return 0;\n}\n";
        assert_eq!(fmt(src2), want2);
    }

    /// A file that doesn't parse is refused with the parser's own
    /// diagnostics — tokens inside ERROR nodes are never re-laid-out.
    #[test]
    fn parse_error_refuses() {
        assert!(format_file("fn f( { return 1; }").is_err());
        assert!(format_file("data { }").is_err());
    }

    /// CRLF input normalizes to LF and still parses to the canonical
    /// form; the trailing `\r` on a line comment is stripped.
    #[test]
    fn crlf_normalizes() {
        assert_eq!(
            fmt("fn f() -> i32 {\r\n    return 1; // note\r\n}\r\n"),
            "fn f() -> i32 {\n    return 1; // note\n}\n"
        );
    }

    /// Formatted output re-parses cleanly and to an identical token
    /// stream — formatting never merges or splits tokens.
    #[test]
    fn output_reparses_identically() {
        for src in [
            "data P{x:i32;}fn f(p:P)->i32{return p.x;}",
            "fn f()->i32{return if 1<2{3}else{4}+5;}",
            "fn f(){let mut q=P{x:1,y:2};q.x=q.x+1;}",
            "fn f(s: str) -> str { return s[1] + s[0..s.len]; }",
            "fn f(a: [i32]) -> i32 { let b = [1, a[0]]; for i in 0..b.len { let z = b[i]; } for mut x in a { x = x + 1; } return b[1]; }",
            "fn f() -> i32 { let e: [i32] = []; for i in 2.. { } return e.len; }",
        ] {
            let once = fmt(src);
            let toks = |s: &str| {
                crate::lex(s)
                    .0
                    .iter()
                    .filter(|t| !t.kind.is_trivia())
                    .map(|t| (t.kind, crate::token_text(s, t).to_string()))
                    .collect::<Vec<_>>()
            };
            assert_eq!(toks(src), toks(&once), "token stream changed for {src:?}");
        }
    }
}
