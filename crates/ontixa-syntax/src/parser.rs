//! Recursive-descent parser producing a lossless `rowan` green tree.
//!
//! The parser consumes the lexer's full token stream — trivia included —
//! so the resulting tree covers the entire source text byte-for-byte.
//!
//! Recovery strategy: on unexpected input the parser emits a diagnostic
//! and wraps the offending tokens in an `ERROR` node, resynchronizing at
//! statement/item boundaries (`;`, `}`, keywords that start items). The
//! parser must always make progress: every loop either consumes a token
//! or exits.

use crate::kinds::{OntixaLanguage, SyntaxKind};
use crate::lexer::{Token, lex};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_source::Span;
use rowan::{Checkpoint, GreenNode, GreenNodeBuilder, Language as _};

/// Result of parsing a source file.
pub struct Parse {
    /// The lossless green tree root (`SOURCE_FILE`).
    pub green: GreenNode,
    /// All diagnostics produced while lexing and parsing.
    pub diagnostics: Diagnostics,
}

/// Lexes and parses `src` into a green tree plus diagnostics.
pub fn parse(src: &str) -> Parse {
    let (tokens, lex_diags) = lex(src);
    let mut parser = Parser {
        src,
        toks: &tokens,
        pos: 0,
        builder: GreenNodeBuilder::new(),
        diags: lex_diags,
    };
    parser.source_file();
    Parse {
        green: parser.builder.finish(),
        diagnostics: parser.diags,
    }
}

struct Parser<'a> {
    src: &'a str,
    toks: &'a [Token],
    pos: usize,
    builder: GreenNodeBuilder<'static>,
    diags: Diagnostics,
}

// ---------- token cursor ----------

impl Parser<'_> {
    /// Kind of the `n`-th non-trivia token ahead (0 = current).
    fn nth(&self, n: usize) -> SyntaxKind {
        let mut seen = 0;
        let mut i = self.pos;
        while i < self.toks.len() {
            let kind = self.toks[i].kind;
            if !kind.is_trivia() {
                if seen == n {
                    return kind;
                }
                seen += 1;
            }
            i += 1;
        }
        SyntaxKind::EOF
    }

    fn current(&self) -> SyntaxKind {
        self.nth(0)
    }

    fn at(&self, kind: SyntaxKind) -> bool {
        self.current() == kind
    }

    fn at_any(&self, kinds: &[SyntaxKind]) -> bool {
        kinds.contains(&self.current())
    }

    fn at_end(&self) -> bool {
        self.current() == SyntaxKind::EOF
    }

    /// Pushes all pending trivia tokens into the current node.
    fn bump_trivia(&mut self) {
        while self.pos < self.toks.len() && self.toks[self.pos].kind.is_trivia() {
            let t = self.toks[self.pos];
            self.builder.token(
                OntixaLanguage::kind_to_raw(t.kind),
                &self.src[t.start as usize..t.end as usize],
            );
            self.pos += 1;
        }
    }

    /// Pushes trivia then the current non-trivia token.
    fn bump(&mut self) {
        self.bump_trivia();
        if self.pos < self.toks.len() {
            let t = self.toks[self.pos];
            self.builder.token(
                OntixaLanguage::kind_to_raw(t.kind),
                &self.src[t.start as usize..t.end as usize],
            );
            self.pos += 1;
        }
    }

    fn eat(&mut self, kind: SyntaxKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    /// Current non-trivia token's span, or an empty span at EOF.
    fn current_span(&self) -> Span {
        let mut i = self.pos;
        while i < self.toks.len() {
            if !self.toks[i].kind.is_trivia() {
                return self.toks[i].span();
            }
            i += 1;
        }
        let end = self.toks.last().map_or(0, |t| t.end);
        Span::empty(end)
    }

    fn error(&mut self, message: impl Into<String>) {
        let span = self.current_span();
        self.diags
            .push(Diagnostic::error(Code::Parse, message).primary(span));
    }

    fn expect(&mut self, kind: SyntaxKind, context: &str) -> bool {
        if self.eat(kind) {
            true
        } else {
            self.error(format!(
                "expected {} {context}, found {}",
                kind.describe(),
                self.current().describe(),
            ));
            false
        }
    }

    /// Emits `message`, then wraps tokens in an `ERROR` node until a
    /// recovery boundary. Recovery tokens are never consumed.
    fn err_recover(&mut self, message: impl Into<String>, recovery: &[SyntaxKind]) {
        self.error(message);
        if self.at_any(recovery) || self.at_end() {
            return;
        }
        self.builder
            .start_node(OntixaLanguage::kind_to_raw(SyntaxKind::ERROR));
        while !self.at_end() && !self.at_any(recovery) {
            self.bump();
        }
        self.builder.finish_node();
    }

    fn start(&mut self, kind: SyntaxKind) {
        self.builder.start_node(OntixaLanguage::kind_to_raw(kind));
    }

    fn finish(&mut self) {
        self.builder.finish_node();
    }

    fn checkpoint(&self) -> Checkpoint {
        self.builder.checkpoint()
    }

    fn start_at(&mut self, cp: Checkpoint, kind: SyntaxKind) {
        self.builder
            .start_node_at(cp, OntixaLanguage::kind_to_raw(kind));
    }
}

// ---------- grammar ----------

impl Parser<'_> {
    fn source_file(&mut self) {
        self.start(SyntaxKind::SOURCE_FILE);
        while !self.at_end() {
            match self.current() {
                SyntaxKind::DATA_KW => self.data_decl(),
                SyntaxKind::FN_KW => self.fn_decl(),
                _ => self.err_recover(
                    format!(
                        "expected `data` or `fn`, found {}",
                        self.current().describe()
                    ),
                    &[SyntaxKind::DATA_KW, SyntaxKind::FN_KW],
                ),
            }
        }
        // Attach trailing trivia (final newline, trailing comments) so the
        // tree covers the entire input byte-for-byte.
        self.bump_trivia();
        self.finish();
    }

    /// `name` in declaring position. Reserved keywords are consumed into
    /// the node so the tree stays shaped, but produce an error.
    fn name(&mut self) {
        self.start(SyntaxKind::NAME);
        match self.current() {
            SyntaxKind::IDENT => self.bump(),
            k if k.is_reserved_keyword() || k.is_keyword() => {
                self.error(format!(
                    "keyword {} cannot be used as an identifier",
                    k.describe()
                ));
                self.bump();
            }
            _ => {
                self.error(format!(
                    "expected identifier, found {}",
                    self.current().describe()
                ));
            }
        }
        self.finish();
    }

    /// `name` in referring position.
    fn name_ref(&mut self) {
        self.start(SyntaxKind::NAME_REF);
        match self.current() {
            SyntaxKind::IDENT => self.bump(),
            k if k.is_reserved_keyword() => {
                self.error(format!(
                    "keyword {} is reserved and cannot be used here",
                    k.describe()
                ));
                self.bump();
            }
            _ => {
                self.error(format!(
                    "expected identifier, found {}",
                    self.current().describe()
                ));
            }
        }
        self.finish();
    }

    fn type_ref(&mut self) {
        self.start(SyntaxKind::TYPE_REF);
        self.name();
        self.finish();
    }

    fn data_decl(&mut self) {
        self.start(SyntaxKind::DATA_DECL);
        self.bump(); // data
        self.name();
        if !self.expect(SyntaxKind::L_BRACE, "to start the field list") {
            // Leave recovery to the item loop, which resynchronizes on
            // `data` / `fn` / EOF.
            self.finish();
            return;
        }
        while !self.at_end() && !self.at(SyntaxKind::R_BRACE) {
            match self.current() {
                SyntaxKind::IDENT => self.field(),
                k if k.is_keyword() => {
                    self.start(SyntaxKind::FIELD);
                    self.error(format!(
                        "keyword {} cannot be used as a field name",
                        k.describe()
                    ));
                    self.bump();
                    self.expect(SyntaxKind::COLON, "after field name");
                    if !self.at_any(&[SyntaxKind::SEMICOLON, SyntaxKind::R_BRACE]) {
                        self.type_ref();
                    }
                    self.eat(SyntaxKind::SEMICOLON);
                    self.finish();
                }
                _ => self.err_recover(
                    format!(
                        "expected a field declaration, found {}",
                        self.current().describe()
                    ),
                    &[
                        SyntaxKind::SEMICOLON,
                        SyntaxKind::R_BRACE,
                        SyntaxKind::IDENT,
                    ],
                ),
            }
        }
        self.expect(SyntaxKind::R_BRACE, "to close the data declaration");
        self.finish();
    }

    fn field(&mut self) {
        self.start(SyntaxKind::FIELD);
        self.name();
        self.expect(SyntaxKind::COLON, "after field name");
        self.type_ref();
        self.expect(SyntaxKind::SEMICOLON, "after field type");
        self.finish();
    }

    fn fn_decl(&mut self) {
        self.start(SyntaxKind::FN_DECL);
        self.bump(); // fn
        self.name();
        self.param_list();
        if self.at(SyntaxKind::ARROW) {
            self.start(SyntaxKind::RET_TYPE);
            self.bump(); // ->
            self.type_ref();
            self.finish();
        }
        self.block();
        self.finish();
    }

    fn param_list(&mut self) {
        if !self.at(SyntaxKind::L_PAREN) {
            self.error(format!(
                "expected `(` to start the parameter list, found {}",
                self.current().describe()
            ));
            return;
        }
        self.start(SyntaxKind::PARAM_LIST);
        self.bump(); // (
        while !self.at_end() && !self.at(SyntaxKind::R_PAREN) {
            match self.current() {
                SyntaxKind::IDENT => {
                    self.param();
                    if !self.eat(SyntaxKind::COMMA) {
                        break;
                    }
                }
                _ => {
                    self.err_recover(
                        format!("expected a parameter, found {}", self.current().describe()),
                        &[SyntaxKind::COMMA, SyntaxKind::R_PAREN],
                    );
                    if !self.eat(SyntaxKind::COMMA) {
                        break;
                    }
                }
            }
        }
        self.expect(SyntaxKind::R_PAREN, "to close the parameter list");
        self.finish();
    }

    fn param(&mut self) {
        self.start(SyntaxKind::PARAM);
        self.name();
        self.expect(SyntaxKind::COLON, "after parameter name");
        self.type_ref();
        self.finish();
    }

    fn block(&mut self) {
        self.start(SyntaxKind::BLOCK);
        if !self.expect(SyntaxKind::L_BRACE, "to start the block") {
            self.finish();
            return;
        }
        while !self.at_end() && !self.at(SyntaxKind::R_BRACE) {
            self.stmt();
        }
        self.expect(SyntaxKind::R_BRACE, "to close the block");
        self.finish();
    }

    fn stmt(&mut self) {
        match self.current() {
            SyntaxKind::LET_KW => self.let_stmt(),
            SyntaxKind::RETURN_KW => self.return_stmt(),
            SyntaxKind::IF_KW | SyntaxKind::L_BRACE => {
                // `if`- and block-expressions used as statements do not
                // require a trailing semicolon.
                let cp = self.checkpoint();
                self.expr(0, true);
                self.start_at(cp, SyntaxKind::EXPR_STMT);
                self.eat(SyntaxKind::SEMICOLON);
                self.finish();
            }
            SyntaxKind::SEMICOLON => {
                // Stray `;` — consume quietly as an empty statement.
                self.bump();
            }
            _ => {
                if !Self::can_start_expr(self.current()) {
                    self.err_recover(
                        format!("expected a statement, found {}", self.current().describe()),
                        &[SyntaxKind::SEMICOLON, SyntaxKind::R_BRACE],
                    );
                    self.eat(SyntaxKind::SEMICOLON);
                    return;
                }
                let cp = self.checkpoint();
                self.expr(0, true);
                match self.current() {
                    SyntaxKind::EQ => {
                        self.start_at(cp, SyntaxKind::ASSIGN_STMT);
                        self.bump(); // =
                        self.expr(0, true);
                        self.expect(SyntaxKind::SEMICOLON, "after assignment");
                        self.finish();
                    }
                    _ => {
                        self.start_at(cp, SyntaxKind::EXPR_STMT);
                        // A semicolon-free expression in tail position is
                        // the block's value; everywhere else `;` is required.
                        if !self.eat(SyntaxKind::SEMICOLON) && !self.at(SyntaxKind::R_BRACE) {
                            self.expect(SyntaxKind::SEMICOLON, "after expression");
                        }
                        self.finish();
                    }
                }
            }
        }
    }

    fn let_stmt(&mut self) {
        self.start(SyntaxKind::LET_STMT);
        self.bump(); // let
        self.name();
        if self.at(SyntaxKind::COLON) {
            self.bump();
            self.type_ref();
        }
        if self.eat(SyntaxKind::EQ) {
            self.expr(0, true);
        }
        self.expect(SyntaxKind::SEMICOLON, "after `let` binding");
        self.finish();
    }

    fn return_stmt(&mut self) {
        self.start(SyntaxKind::RETURN_STMT);
        self.bump(); // return
        if Self::can_start_expr(self.current()) {
            self.expr(0, true);
        }
        self.expect(SyntaxKind::SEMICOLON, "after `return`");
        self.finish();
    }

    fn can_start_expr(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::INT_NUMBER
                | SyntaxKind::FLOAT_NUMBER
                | SyntaxKind::STRING
                | SyntaxKind::TRUE_KW
                | SyntaxKind::FALSE_KW
                | SyntaxKind::IDENT
                | SyntaxKind::L_PAREN
                | SyntaxKind::L_BRACE
                | SyntaxKind::IF_KW
                | SyntaxKind::MINUS
                | SyntaxKind::NOT
        )
    }

    /// Parses an expression. `allow_struct` is false inside `if`
    /// conditions so `if x == S { }` parses `{` as the block, matching
    /// the usual struct-literal ambiguity rule.
    fn expr(&mut self, min_bp: u8, allow_struct: bool) {
        self.expr_bp(min_bp, allow_struct);
    }

    fn expr_bp(&mut self, min_bp: u8, allow_struct: bool) {
        let cp = self.checkpoint();

        // Prefix position.
        match self.current() {
            SyntaxKind::MINUS | SyntaxKind::NOT => {
                self.start(SyntaxKind::PREFIX_EXPR);
                self.bump();
                self.expr_bp(UNARY_BP, allow_struct);
                self.finish();
            }
            _ => self.atom(allow_struct),
        }

        // Postfix position (call / field access), then infix operators.
        // Both loops reuse `cp`, which precedes the LHS, so each wrap
        // nests the previous node correctly.
        loop {
            match self.current() {
                SyntaxKind::L_PAREN => {
                    self.start_at(cp, SyntaxKind::CALL_EXPR);
                    self.arg_list();
                    self.finish();
                }
                SyntaxKind::DOT => {
                    self.start_at(cp, SyntaxKind::FIELD_EXPR);
                    self.bump(); // .
                    self.name_ref();
                    self.finish();
                }
                _ => break,
            }
        }

        while let Some((l_bp, r_bp)) = infix_binding_power(self.current()) {
            if l_bp < min_bp {
                break;
            }
            self.start_at(cp, SyntaxKind::BIN_EXPR);
            self.bump(); // operator
            self.expr_bp(r_bp, allow_struct);
            self.finish();
        }
    }

    fn atom(&mut self, allow_struct: bool) {
        match self.current() {
            SyntaxKind::INT_NUMBER
            | SyntaxKind::FLOAT_NUMBER
            | SyntaxKind::STRING
            | SyntaxKind::TRUE_KW
            | SyntaxKind::FALSE_KW => {
                self.start(SyntaxKind::LITERAL);
                self.bump();
                self.finish();
            }
            SyntaxKind::IF_KW => self.if_expr(),
            SyntaxKind::L_PAREN => {
                self.start(SyntaxKind::PAREN_EXPR);
                self.bump();
                self.expr(0, true);
                self.expect(SyntaxKind::R_PAREN, "to close the parenthesized expression");
                self.finish();
            }
            SyntaxKind::L_BRACE => self.block(),
            SyntaxKind::IDENT => {
                let cp = self.checkpoint();
                self.name_ref();
                if allow_struct && self.at(SyntaxKind::L_BRACE) {
                    self.start_at(cp, SyntaxKind::STRUCT_LIT);
                    self.struct_lit_fields();
                    self.finish();
                }
            }
            k if k.is_reserved_keyword() => {
                self.error(format!(
                    "keyword {} is reserved for future use and cannot appear in an expression",
                    k.describe()
                ));
                self.start(SyntaxKind::ERROR);
                self.bump();
                self.finish();
            }
            _ => {
                self.error(format!(
                    "expected an expression, found {}",
                    self.current().describe()
                ));
                // Emit an empty ERROR node so the parent still sees an
                // expression-shaped child; do not consume sync tokens.
                if self.at_any(&[
                    SyntaxKind::SEMICOLON,
                    SyntaxKind::R_BRACE,
                    SyntaxKind::R_PAREN,
                    SyntaxKind::COMMA,
                ]) || self.at_end()
                {
                    self.start(SyntaxKind::ERROR);
                    self.finish();
                } else {
                    self.start(SyntaxKind::ERROR);
                    self.bump();
                    self.finish();
                }
            }
        }
    }

    fn if_expr(&mut self) {
        self.start(SyntaxKind::IF_EXPR);
        self.bump(); // if
        self.expr(0, false);
        self.block();
        if self.eat(SyntaxKind::ELSE_KW) {
            if self.at(SyntaxKind::IF_KW) {
                self.if_expr();
            } else {
                self.block();
            }
        }
        self.finish();
    }

    fn arg_list(&mut self) {
        self.start(SyntaxKind::ARG_LIST);
        self.bump(); // (
        while !self.at_end() && !self.at(SyntaxKind::R_PAREN) {
            if Self::can_start_expr(self.current()) {
                self.expr(0, true);
            } else {
                self.err_recover(
                    format!("expected an argument, found {}", self.current().describe()),
                    &[SyntaxKind::COMMA, SyntaxKind::R_PAREN],
                );
            }
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        self.expect(SyntaxKind::R_PAREN, "to close the argument list");
        self.finish();
    }

    fn struct_lit_fields(&mut self) {
        self.expect(SyntaxKind::L_BRACE, "to start the struct literal");
        while !self.at_end() && !self.at(SyntaxKind::R_BRACE) {
            match self.current() {
                SyntaxKind::IDENT => {
                    self.start(SyntaxKind::STRUCT_LIT_FIELD);
                    self.name_ref();
                    self.expect(SyntaxKind::COLON, "after field name");
                    self.expr(0, true);
                    self.finish();
                }
                _ => self.err_recover(
                    format!(
                        "expected a field initializer, found {}",
                        self.current().describe()
                    ),
                    &[SyntaxKind::COMMA, SyntaxKind::R_BRACE, SyntaxKind::IDENT],
                ),
            }
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        self.expect(SyntaxKind::R_BRACE, "to close the struct literal");
    }
}

/// Binding power of the unary `-`/`!` prefix operators.
const UNARY_BP: u8 = 13;

/// Infix binding powers `(left, right)`. Higher binds tighter; for
/// left-associative operators `right = left + 1`.
const fn infix_binding_power(kind: SyntaxKind) -> Option<(u8, u8)> {
    use SyntaxKind as K;
    match kind {
        K::OR2 => Some((1, 2)),
        K::AND2 => Some((3, 4)),
        K::EQ2 | K::NEQ => Some((5, 6)),
        K::LT | K::LE | K::GT | K::GE => Some((7, 8)),
        K::PLUS | K::MINUS => Some((9, 10)),
        K::STAR | K::SLASH | K::PERCENT => Some((11, 12)),
        _ => None,
    }
}
