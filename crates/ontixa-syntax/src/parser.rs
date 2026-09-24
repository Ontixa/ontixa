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
                SyntaxKind::USE_KW => self.use_decl(),
                SyntaxKind::DATA_KW => self.data_decl(),
                SyntaxKind::FN_KW => self.fn_decl(),
                _ => self.err_recover(
                    format!(
                        "expected `use`, `data` or `fn`, found {}",
                        self.current().describe()
                    ),
                    &[SyntaxKind::USE_KW, SyntaxKind::DATA_KW, SyntaxKind::FN_KW],
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

    /// `use m;` — binds a module name — or `use m::x [as y];` — binds
    /// one member of a module under a (possibly aliased) local name.
    /// Path segments are `NAME_REF`s; the `as` alias is a `NAME`.
    fn use_decl(&mut self) {
        self.start(SyntaxKind::USE_DECL);
        self.bump(); // use
        self.name_ref();
        while self.at(SyntaxKind::COLON2) {
            self.bump(); // ::
            self.name_ref();
        }
        if self.at(SyntaxKind::AS_KW) {
            self.bump(); // as
            self.name();
        }
        self.expect(SyntaxKind::SEMICOLON, "after `use` declaration");
        self.finish();
    }

    /// `Name`, `m::Name`, or `[T]` — a (possibly qualified) named type
    /// or an array type. Nested `[..]` parses here; resolution rejects
    /// it (`[ [T] ]` is not a supported type).
    fn type_ref(&mut self) {
        self.start(SyntaxKind::TYPE_REF);
        if self.at(SyntaxKind::L_BRACKET) {
            self.bump(); // [
            self.type_ref();
            self.expect(SyntaxKind::R_BRACKET, "to close the array type");
        } else {
            self.name();
            while self.at(SyntaxKind::COLON2) {
                self.bump(); // ::
                self.name();
            }
        }
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
        // A `data` body holds either record fields (`name: T;`) or
        // enum variants (`Name(T, ..);` / `Name;`) — never both. The
        // parser accepts both shapes and reports mixing so downstream
        // stages see a well-formed tree either way.
        let mut saw_fields = false;
        let mut saw_variants = false;
        while !self.at_end() && !self.at(SyntaxKind::R_BRACE) {
            match self.current() {
                SyntaxKind::IDENT => match self.nth(1) {
                    SyntaxKind::COLON => {
                        if saw_variants {
                            self.error(
                                "a `data` declaration cannot mix record fields and variants",
                            );
                        }
                        saw_fields = true;
                        self.field();
                    }
                    SyntaxKind::L_PAREN | SyntaxKind::SEMICOLON => {
                        if saw_fields {
                            self.error(
                                "a `data` declaration cannot mix record fields and variants",
                            );
                        }
                        saw_variants = true;
                        self.variant();
                    }
                    // `x T` is most likely a field missing its `:` —
                    // `field()` reports exactly that, consumes the
                    // name, and keeps the member shaped for the AST.
                    _ => self.field(),
                },
                k if k.is_keyword() => {
                    self.start(SyntaxKind::FIELD);
                    self.error(format!(
                        "keyword {} cannot be used as a member name",
                        k.describe()
                    ));
                    self.bump();
                    self.expect(SyntaxKind::COLON, "after member name");
                    if !self.at_any(&[SyntaxKind::SEMICOLON, SyntaxKind::R_BRACE]) {
                        self.type_ref();
                    }
                    self.eat(SyntaxKind::SEMICOLON);
                    self.finish();
                }
                SyntaxKind::SEMICOLON => {
                    // Stray `;` — consume quietly like an empty member;
                    // `err_recover` would never consume a recovery token
                    // and the member loop must always make progress.
                    self.bump();
                }
                _ => self.err_recover(
                    format!(
                        "expected a field or variant declaration, found {}",
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

    /// `Name(T, ..)` / `Name` — one enum variant inside a `data` decl.
    /// Payload elements are type positions; `V()` (empty parens) is a
    /// unit variant.
    fn variant(&mut self) {
        self.start(SyntaxKind::VARIANT);
        self.name();
        if self.at(SyntaxKind::L_PAREN) {
            self.bump(); // (
            while !self.at_end() && !self.at(SyntaxKind::R_PAREN) {
                match self.current() {
                    SyntaxKind::IDENT | SyntaxKind::L_BRACKET => self.type_ref(),
                    _ => self.err_recover(
                        format!(
                            "expected a payload type, found {}",
                            self.current().describe()
                        ),
                        &[SyntaxKind::COMMA, SyntaxKind::R_PAREN],
                    ),
                }
                if !self.eat(SyntaxKind::COMMA) {
                    break;
                }
            }
            self.expect(SyntaxKind::R_PAREN, "to close the variant payload");
        }
        self.expect(SyntaxKind::SEMICOLON, "after variant");
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
                SyntaxKind::IDENT | SyntaxKind::MUT_KW => {
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
        self.eat(SyntaxKind::MUT_KW); // `mut p: T` — mutable binding
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
            SyntaxKind::IF_KW | SyntaxKind::L_BRACE | SyntaxKind::FOR_KW | SyntaxKind::MATCH_KW => {
                // `if`-, `for`-, `match`-, and block-expressions used as
                // statements do not require a trailing semicolon.
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
        self.eat(SyntaxKind::MUT_KW); // `let mut x` — mutable binding
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
                | SyntaxKind::L_BRACKET
                | SyntaxKind::IF_KW
                | SyntaxKind::FOR_KW
                | SyntaxKind::MATCH_KW
                | SyntaxKind::MINUS
                | SyntaxKind::NOT
                | SyntaxKind::DOT2
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
            SyntaxKind::DOT2 => {
                // `..hi` — an open-start range. Parsed everywhere; the
                // checker only accepts it inside `[]` or `for .. in`.
                // A bare `{` never opens a bound — in `for i in .. { }`
                // it is the loop body (parenthesize to bound a block).
                self.start(SyntaxKind::RANGE);
                self.bump(); // ..
                if Self::can_start_expr(self.current()) && !self.at(SyntaxKind::L_BRACE) {
                    self.expr_bp(0, allow_struct);
                }
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
                SyntaxKind::L_BRACKET => {
                    self.start_at(cp, SyntaxKind::INDEX_EXPR);
                    self.bump(); // [
                    self.index_content();
                    self.expect(SyntaxKind::R_BRACKET, "to close the index");
                    self.finish();
                }
                _ => break,
            }
        }

        while let Some((l_bp, r_bp)) = infix_binding_power(self.current()) {
            if l_bp < min_bp {
                break;
            }
            if self.at(SyntaxKind::DOT2) {
                // `lo..hi` — a range, not a binary op; `lo..` leaves the
                // upper bound absent. The loosest operator in the grammar.
                // A bare `{` is never a bound — `for i in 0.. { }` reads
                // it as the loop body, like the struct-literal rule.
                self.start_at(cp, SyntaxKind::RANGE);
                self.bump(); // ..
                if Self::can_start_expr(self.current()) && !self.at(SyntaxKind::L_BRACE) {
                    self.expr_bp(r_bp, allow_struct);
                }
                self.finish();
                continue;
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
            SyntaxKind::FOR_KW => self.for_expr(),
            SyntaxKind::MATCH_KW => self.match_expr(),
            SyntaxKind::L_BRACKET => self.array_lit(),
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
                // `m::x` — a module-qualified path. The `::` chain is
                // part of the name, so it binds tighter than any
                // postfix or infix operator.
                let mut qualified = false;
                while self.at(SyntaxKind::COLON2) {
                    qualified = true;
                    self.bump(); // ::
                    self.name_ref();
                }
                if qualified {
                    self.start_at(cp, SyntaxKind::PATH_EXPR);
                    self.finish();
                }
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

    /// Inside `base[...]`: an index (`i`) or a range (`lo..hi`, `lo..`,
    /// `..hi`, `..`). `..` parses as a `RANGE` expression like anywhere
    /// else — `index_expr` interprets it as slice bounds.
    fn index_content(&mut self) {
        if self.at(SyntaxKind::DOT2) || Self::can_start_expr(self.current()) {
            self.expr(0, true);
        } else {
            self.error(format!(
                "expected an index or `..` range, found {}",
                self.current().describe()
            ));
        }
    }

    /// `[e, ...]` — a homogeneous array literal. `[]` and trailing
    /// commas are legal syntax; the checker requires an element type
    /// (from the elements or an annotation) to give `[]` a type.
    fn array_lit(&mut self) {
        self.start(SyntaxKind::ARRAY_EXPR);
        self.bump(); // [
        while !self.at_end() && !self.at(SyntaxKind::R_BRACKET) {
            if Self::can_start_expr(self.current()) {
                self.expr(0, true);
            } else {
                self.err_recover(
                    format!("expected an element, found {}", self.current().describe()),
                    &[SyntaxKind::COMMA, SyntaxKind::R_BRACKET],
                );
            }
            if !self.eat(SyntaxKind::COMMA) {
                break;
            }
        }
        self.expect(SyntaxKind::R_BRACKET, "to close the array literal");
        self.finish();
    }

    /// `for x in e { .. }` — iterates `e` (an array or a `lo..hi`
    /// integer range), binding each element to a fresh `x`. `for mut x`
    /// makes the loop binding assignable inside the body.
    fn for_expr(&mut self) {
        self.start(SyntaxKind::FOR_EXPR);
        self.bump(); // for
        self.eat(SyntaxKind::MUT_KW); // `for mut x` — mutable binding
        self.name();
        self.expect(SyntaxKind::IN_KW, "after the loop variable");
        // The iterable can't open a struct literal — `for x in S { }`
        // reads `{` as the loop body, like an `if` condition.
        self.expr(0, false);
        self.block();
        self.finish();
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

    /// `match e { pat => expr, .. }` — selection over `data` variants.
    /// The scrutinee can't open a struct literal — `match S { }` reads
    /// `{` as the arm list, the same rule as `if` conditions and `for`
    /// iterables (parenthesize to match a literal: `match (S { .. })`).
    fn match_expr(&mut self) {
        self.start(SyntaxKind::MATCH_EXPR);
        self.bump(); // match
        if Self::can_start_expr(self.current()) {
            self.expr(0, false);
        } else {
            self.error(format!(
                "expected a scrutinee expression after `match`, found {}",
                self.current().describe()
            ));
        }
        self.start(SyntaxKind::MATCH_ARM_LIST);
        if self.at(SyntaxKind::L_BRACE) {
            self.bump(); // {
            while !self.at_end() && !self.at(SyntaxKind::R_BRACE) {
                self.match_arm();
                if !self.eat(SyntaxKind::COMMA) {
                    break;
                }
            }
            self.expect(SyntaxKind::R_BRACE, "to close the match arms");
        } else {
            self.error(format!(
                "expected `{{` to start the match arms, found {}",
                self.current().describe()
            ));
        }
        self.finish(); // MATCH_ARM_LIST
        self.finish(); // MATCH_EXPR
    }

    /// `pat => expr` — one arm of a `match`.
    fn match_arm(&mut self) {
        self.start(SyntaxKind::MATCH_ARM);
        self.pattern();
        self.expect(SyntaxKind::FAT_ARROW, "after the pattern");
        if Self::can_start_expr(self.current()) {
            self.expr(0, true);
        } else {
            self.error(format!(
                "expected an expression after `=>`, found {}",
                self.current().describe()
            ));
        }
        self.finish();
    }

    /// A match pattern. `T::V`, `T::V(b, ..)`, `m::T::V(..)` select a
    /// `data` variant; a bare `name` or `_` binds (or ignores) the
    /// whole scrutinee. A one-segment `V(..)` is legal syntax — name
    /// resolution reports that it names no variant.
    fn pattern(&mut self) {
        match self.current() {
            SyntaxKind::IDENT
                if self.nth(1) == SyntaxKind::COLON2 || self.nth(1) == SyntaxKind::L_PAREN =>
            {
                // Variant pattern. `NAME_REF` segments sit directly in
                // the PAT_VARIANT node; AST lowering collects them.
                self.start(SyntaxKind::PAT_VARIANT);
                self.name_ref();
                while self.at(SyntaxKind::COLON2) {
                    self.bump(); // ::
                    self.name_ref();
                }
                if self.at(SyntaxKind::L_PAREN) {
                    self.bump(); // (
                    while !self.at_end() && !self.at(SyntaxKind::R_PAREN) {
                        if self.at(SyntaxKind::IDENT) {
                            self.start(SyntaxKind::PAT_BIND);
                            self.name();
                            self.finish();
                        } else {
                            self.err_recover(
                                format!(
                                    "expected a binding or `_`, found {}",
                                    self.current().describe()
                                ),
                                &[SyntaxKind::COMMA, SyntaxKind::R_PAREN],
                            );
                        }
                        if !self.eat(SyntaxKind::COMMA) {
                            break;
                        }
                    }
                    self.expect(SyntaxKind::R_PAREN, "to close the pattern bindings");
                }
                self.finish();
            }
            SyntaxKind::IDENT => {
                // `x` binds the scrutinee; `_` ignores it.
                self.start(SyntaxKind::PAT_BIND);
                self.name();
                self.finish();
            }
            k if k.is_keyword() => {
                self.error(format!(
                    "keyword {} cannot be used as a pattern",
                    k.describe()
                ));
                self.bump();
            }
            _ => self.err_recover(
                format!("expected a pattern, found {}", self.current().describe()),
                &[
                    SyntaxKind::FAT_ARROW,
                    SyntaxKind::COMMA,
                    SyntaxKind::R_BRACE,
                ],
            ),
        }
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
/// left-associative operators `right = left + 1`. `..` binds loosest
/// of all — `a + b .. c + d` is `(a+b)..(c+d)`.
const fn infix_binding_power(kind: SyntaxKind) -> Option<(u8, u8)> {
    use SyntaxKind as K;
    match kind {
        K::DOT2 => Some((0, 1)),
        K::OR2 => Some((1, 2)),
        K::AND2 => Some((3, 4)),
        K::EQ2 | K::NEQ => Some((5, 6)),
        K::LT | K::LE | K::GT | K::GE => Some((7, 8)),
        K::PLUS | K::MINUS => Some((9, 10)),
        K::STAR | K::SLASH | K::PERCENT => Some((11, 12)),
        _ => None,
    }
}
