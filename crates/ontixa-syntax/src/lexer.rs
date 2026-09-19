//! The Ontixa lexer.
//!
//! Produces a flat token vector including trivia (whitespace and
//! comments) so the green tree stays lossless. Byte offsets are
//! `rowan::TextSize`-compatible ranges.
//!
//! Error policy: the lexer never aborts. Unrecognized bytes become
//! `ERROR_TOKEN`s and malformed literals/comments produce diagnostics
//! while scanning continues.

use crate::kinds::{SyntaxKind, keyword_kind};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_source::Span;

/// One lexed token: a kind plus its half-open byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    /// Token kind.
    pub kind: SyntaxKind,
    /// Byte offset range `[start, end)`.
    pub start: u32,
    /// End offset (exclusive).
    pub end: u32,
}

impl Token {
    /// The token's byte range as a [`Span`].
    pub const fn span(&self) -> Span {
        Span::new(self.start, self.end)
    }
}

/// Lexes `src` into tokens plus any lexer diagnostics.
pub fn lex(src: &str) -> (Vec<Token>, Diagnostics) {
    let mut lexer = Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        tokens: Vec::new(),
        diags: Diagnostics::new(),
    };
    lexer.run();
    (lexer.tokens, lexer.diags)
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    diags: Diagnostics,
}

impl Lexer<'_> {
    fn run(&mut self) {
        while self.pos < self.bytes.len() {
            let start = self.pos;
            let b = self.bytes[self.pos];
            match b {
                b' ' | b'\t' | b'\n' | b'\r' => self.whitespace(),
                b'/' if self.peek(1) == Some(b'/') => self.line_comment(),
                b'/' if self.peek(1) == Some(b'*') => self.block_comment(),
                b'0'..=b'9' => self.number(),
                b'"' => self.string(),
                _ if is_ident_start(b) => self.ident_or_keyword(),
                _ => self.punctuation(),
            }
            debug_assert!(
                self.pos > start || self.tokens.last().is_some_and(|t| t.end as usize > start),
                "lexer made no progress at byte {start}"
            );
            // Absolute guard against zero-progress loops on corrupt input.
            if self.pos == start {
                self.pos += 1;
            }
        }
    }

    fn push(&mut self, kind: SyntaxKind, start: usize) {
        self.tokens.push(Token {
            kind,
            start: start as u32,
            end: self.pos as u32,
        });
    }

    fn peek(&self, ahead: usize) -> Option<u8> {
        self.bytes.get(self.pos + ahead).copied()
    }

    fn whitespace(&mut self) {
        let start = self.pos;
        while matches!(self.peek(0), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
        self.push(SyntaxKind::WHITESPACE, start);
    }

    fn line_comment(&mut self) {
        let start = self.pos;
        self.pos += 2;
        while let Some(b) = self.peek(0) {
            if b == b'\n' {
                break;
            }
            self.pos += 1;
        }
        self.push(SyntaxKind::COMMENT, start);
    }

    fn block_comment(&mut self) {
        let start = self.pos;
        self.pos += 2;
        loop {
            match self.peek(0) {
                None => {
                    self.diags.push(
                        Diagnostic::error(Code::UnterminatedComment, "unterminated block comment")
                            .primary(Span::new(start as u32, self.pos as u32)),
                    );
                    break;
                }
                Some(b'*') if self.peek(1) == Some(b'/') => {
                    self.pos += 2;
                    break;
                }
                Some(_) => self.pos += 1,
            }
        }
        self.push(SyntaxKind::COMMENT, start);
    }

    fn number(&mut self) {
        let start = self.pos;
        self.pos += 1;
        while matches!(self.peek(0), Some(b'0'..=b'9' | b'_')) {
            self.pos += 1;
        }
        let mut kind = SyntaxKind::INT_NUMBER;
        // Fraction: `.` followed by a digit (`1.` stays INT + DOT).
        if self.peek(0) == Some(b'.') && matches!(self.peek(1), Some(b'0'..=b'9')) {
            kind = SyntaxKind::FLOAT_NUMBER;
            self.pos += 1;
            while matches!(self.peek(0), Some(b'0'..=b'9' | b'_')) {
                self.pos += 1;
            }
        }
        // Exponent.
        if matches!(self.peek(0), Some(b'e' | b'E'))
            && (matches!(self.peek(1), Some(b'0'..=b'9'))
                || (matches!(self.peek(1), Some(b'+' | b'-'))
                    && matches!(self.peek(2), Some(b'0'..=b'9'))))
        {
            kind = SyntaxKind::FLOAT_NUMBER;
            self.pos += 1;
            if matches!(self.peek(0), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(0), Some(b'0'..=b'9' | b'_')) {
                self.pos += 1;
            }
        }
        self.push(kind, start);
    }

    fn string(&mut self) {
        let start = self.pos;
        self.pos += 1;
        loop {
            match self.peek(0) {
                None | Some(b'\n') => {
                    self.diags.push(
                        Diagnostic::error(Code::UnterminatedString, "unterminated string literal")
                            .primary(Span::new(start as u32, self.pos as u32)),
                    );
                    break;
                }
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek(0) {
                        Some(b'n' | b't' | b'r' | b'0' | b'\\' | b'"' | b'\'') => {
                            self.pos += 1;
                        }
                        Some(other) => {
                            let esc_start = self.pos - 1;
                            self.pos += 1;
                            self.diags.push(
                                Diagnostic::error(
                                    Code::Parse,
                                    format!("invalid escape sequence `\\{}`", other as char),
                                )
                                .primary(Span::new(esc_start as u32, self.pos as u32)),
                            );
                        }
                        None => {
                            self.diags.push(
                                Diagnostic::error(
                                    Code::UnterminatedString,
                                    "unterminated string literal",
                                )
                                .primary(Span::new(start as u32, self.pos as u32)),
                            );
                            break;
                        }
                    }
                }
                Some(_) => self.pos += 1,
            }
        }
        self.push(SyntaxKind::STRING, start);
    }

    fn ident_or_keyword(&mut self) {
        let start = self.pos;
        while self.peek(0).is_some_and(is_ident_continue) {
            self.pos += 1;
        }
        let text = &self.src[start..self.pos];
        let kind = keyword_kind(text).unwrap_or(SyntaxKind::IDENT);
        self.push(kind, start);
    }

    fn punctuation(&mut self) {
        use SyntaxKind as K;
        let start = self.pos;
        let b = self.bytes[self.pos];
        let at = |offset: usize, byte: u8| self.peek(offset) == Some(byte);
        let (kind, len): (SyntaxKind, usize) = match b {
            b'(' => (K::L_PAREN, 1),
            b')' => (K::R_PAREN, 1),
            b'{' => (K::L_BRACE, 1),
            b'}' => (K::R_BRACE, 1),
            b'[' => (K::L_BRACKET, 1),
            b']' => (K::R_BRACKET, 1),
            b',' => (K::COMMA, 1),
            b';' => (K::SEMICOLON, 1),
            b'?' => (K::QUESTION, 1),
            b'@' => (K::AT, 1),
            b'#' => (K::POUND, 1),
            b'~' => (K::TILDE, 1),
            b'^' => (K::CARET, 1),
            b':' => {
                if at(1, b':') {
                    (K::COLON2, 2)
                } else {
                    (K::COLON, 1)
                }
            }
            b'.' => {
                if at(1, b'.') {
                    (K::DOT2, 2)
                } else {
                    (K::DOT, 1)
                }
            }
            b'-' => {
                if at(1, b'>') {
                    (K::ARROW, 2)
                } else if at(1, b'=') {
                    (K::MINUS_EQ, 2)
                } else {
                    (K::MINUS, 1)
                }
            }
            b'=' => {
                if at(1, b'=') {
                    (K::EQ2, 2)
                } else if at(1, b'>') {
                    (K::FAT_ARROW, 2)
                } else {
                    (K::EQ, 1)
                }
            }
            b'!' => {
                if at(1, b'=') {
                    (K::NEQ, 2)
                } else {
                    (K::NOT, 1)
                }
            }
            b'<' => {
                if at(1, b'=') {
                    (K::LE, 2)
                } else if at(1, b'<') {
                    (K::SHL, 2)
                } else {
                    (K::LT, 1)
                }
            }
            b'>' => {
                if at(1, b'=') {
                    (K::GE, 2)
                } else if at(1, b'>') {
                    (K::SHR, 2)
                } else {
                    (K::GT, 1)
                }
            }
            b'+' => {
                if at(1, b'=') {
                    (K::PLUS_EQ, 2)
                } else {
                    (K::PLUS, 1)
                }
            }
            b'*' => {
                if at(1, b'=') {
                    (K::STAR_EQ, 2)
                } else {
                    (K::STAR, 1)
                }
            }
            // `//` and `/*` are claimed by the comment dispatch before
            // `punctuation` runs; a lone `/` or `/=` lands here.
            b'/' => {
                if at(1, b'=') {
                    (K::SLASH_EQ, 2)
                } else {
                    (K::SLASH, 1)
                }
            }
            b'%' => {
                if at(1, b'=') {
                    (K::PERCENT_EQ, 2)
                } else {
                    (K::PERCENT, 1)
                }
            }
            b'&' => {
                if at(1, b'&') {
                    (K::AND2, 2)
                } else {
                    (K::AMP, 1)
                }
            }
            b'|' => {
                if at(1, b'|') {
                    (K::OR2, 2)
                } else {
                    (K::PIPE, 1)
                }
            }
            _ => {
                // One full UTF-8 character is consumed so the error range
                // covers the offending character.
                let len = utf8_len(b).max(1);
                self.diags.push(
                    Diagnostic::error(
                        Code::UnexpectedCharacter,
                        format!("unexpected character {}", describe_byte(b)),
                    )
                    .primary(Span::new(start as u32, (start + len) as u32)),
                );
                (K::ERROR_TOKEN, len)
            }
        };
        self.pos += len;
        self.push(kind, start);
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Length of the UTF-8 sequence starting with `b`; 1 for ASCII/invalid.
const fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

fn describe_byte(b: u8) -> String {
    if b.is_ascii_graphic() || b == b' ' {
        format!("`{}`", b as char)
    } else {
        format!("byte 0x{b:02X}")
    }
}

/// Returns the text of `token` within `src`.
pub fn token_text<'a>(src: &'a str, token: &Token) -> &'a str {
    &src[token.start as usize..token.end as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<SyntaxKind> {
        lex(src).0.iter().map(|t| t.kind).collect()
    }

    fn nontrivia(src: &str) -> Vec<SyntaxKind> {
        lex(src)
            .0
            .iter()
            .filter(|t| !t.kind.is_trivia())
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn lexes_hello_program() {
        let src = "fn add(a: i32, b: i32) -> i32 {\n    return a + b;\n}\n";
        use SyntaxKind as K;
        assert_eq!(
            nontrivia(src),
            vec![
                K::FN_KW,
                K::IDENT,
                K::L_PAREN,
                K::IDENT,
                K::COLON,
                K::IDENT,
                K::COMMA,
                K::IDENT,
                K::COLON,
                K::IDENT,
                K::R_PAREN,
                K::ARROW,
                K::IDENT,
                K::L_BRACE,
                K::RETURN_KW,
                K::IDENT,
                K::PLUS,
                K::IDENT,
                K::SEMICOLON,
                K::R_BRACE,
            ]
        );
    }

    #[test]
    fn trivia_is_lossless() {
        let src = "// hi\nfn  f() { }\n";
        let (tokens, diags) = lex(src);
        assert!(diags.is_empty());
        let reconstructed: String = tokens.iter().map(|t| token_text(src, t)).collect();
        assert_eq!(reconstructed, src);
        assert!(kinds(src).contains(&SyntaxKind::COMMENT));
    }

    #[test]
    fn float_vs_int_and_dot() {
        use SyntaxKind as K;
        assert_eq!(nontrivia("1.5"), vec![K::FLOAT_NUMBER]);
        assert_eq!(nontrivia("1."), vec![K::INT_NUMBER, K::DOT]);
        assert_eq!(nontrivia("1e3"), vec![K::FLOAT_NUMBER]);
        assert_eq!(nontrivia("x.y"), vec![K::IDENT, K::DOT, K::IDENT]);
    }

    #[test]
    fn string_escapes() {
        let (tokens, diags) = lex("\"a\\n\\t\\\\\\\"b\"");
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(tokens[0].kind, SyntaxKind::STRING);
    }

    #[test]
    fn unterminated_string_reports() {
        let (_, diags) = lex("\"abc");
        assert!(diags.has_errors());
    }

    #[test]
    fn unknown_char_reports() {
        let (tokens, diags) = lex("fn $ f()");
        assert!(diags.has_errors());
        assert!(tokens.iter().any(|t| t.kind == SyntaxKind::ERROR_TOKEN));
    }

    #[test]
    fn keywords_recognized() {
        use SyntaxKind as K;
        assert_eq!(
            nontrivia("data fn let if else"),
            vec![K::DATA_KW, K::FN_KW, K::LET_KW, K::IF_KW, K::ELSE_KW]
        );
    }
}
