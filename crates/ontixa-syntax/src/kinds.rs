//! Syntax kind tags shared by tokens and tree nodes.
//!
//! The ordering is irrelevant; numeric values are stable only within a
//! compiler version. Reserved-but-unimplemented keywords lex as their own
//! kinds so that the parser can produce "reserved for future use"
//! diagnostics rather than silently accepting them as identifiers.

/// All token and node kinds of the Ontixa grammar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
#[allow(non_camel_case_types)]
pub enum SyntaxKind {
    // ---- Trivia -------------------------------------------------------
    /// Runs of spaces, tabs, and newlines.
    WHITESPACE,
    /// `// ...` and `/* ... */` comments.
    COMMENT,

    // ---- Literals & identifiers ---------------------------------------
    /// Identifier (or unreserved name).
    IDENT,
    /// Decimal integer literal.
    INT_NUMBER,
    /// Decimal floating-point literal.
    FLOAT_NUMBER,
    /// `"..."` string literal (raw token including quotes).
    STRING,
    /// A byte the lexer could not attribute to any token.
    ERROR_TOKEN,

    // ---- Keywords -----------------------------------------------------
    DATA_KW,
    FN_KW,
    LET_KW,
    RETURN_KW,
    IF_KW,
    ELSE_KW,
    TRUE_KW,
    FALSE_KW,
    /// `use m;` / `use m::x [as y];` — workspace imports.
    USE_KW,
    /// `as` — the alias keyword in `use` declarations.
    AS_KW,
    // Reserved for future use.
    UNSAFE_KW,
    MUT_KW,
    SHARED_KW,
    REGION_KW,
    PARALLEL_KW,
    TRAIT_KW,
    IMPL_KW,
    MATCH_KW,
    FOR_KW,
    WHILE_KW,
    LOOP_KW,
    BREAK_KW,
    CONTINUE_KW,
    STRUCT_KW,
    ENUM_KW,
    PUB_KW,
    MOD_KW,
    CONST_KW,
    STATIC_KW,
    TYPE_KW,
    WHERE_KW,
    IN_KW,
    MOVE_KW,
    ASYNC_KW,
    AWAIT_KW,
    DYN_KW,
    TRY_KW,
    SELF_KW,
    SELF_TYPE_KW,
    SUPER_KW,
    CRATE_KW,
    REQUIRES_KW,
    ENSURES_KW,
    INVARIANT_KW,

    // ---- Punctuation & operators --------------------------------------
    L_PAREN,
    R_PAREN,
    L_BRACE,
    R_BRACE,
    L_BRACKET,
    R_BRACKET,
    COMMA,
    SEMICOLON,
    COLON,
    COLON2, // ::
    DOT,
    DOT2,      // ..
    ARROW,     // ->
    FAT_ARROW, // =>
    PLUS,
    MINUS,
    STAR,
    SLASH,
    PERCENT,
    EQ,  // =
    EQ2, // ==
    NEQ, // !=
    LT,
    LE, // <=
    GT,
    GE,   // >=
    NOT,  // !
    AND2, // &&
    OR2,  // ||
    AMP,  // &
    PIPE, // |
    CARET,
    TILDE,
    SHL, // <<
    SHR, // >>
    PLUS_EQ,
    MINUS_EQ,
    STAR_EQ,
    SLASH_EQ,
    PERCENT_EQ,
    QUESTION,
    AT,
    POUND,

    // ---- Nodes ---------------------------------------------------------
    /// Root of every file.
    SOURCE_FILE,
    /// `use m;` / `use m::x [as y];` — a workspace import.
    USE_DECL,
    /// `data Name { ... }`.
    DATA_DECL,
    /// One `name: Type;` field inside a data declaration.
    FIELD,
    /// `fn name(params) -> ret { ... }`.
    FN_DECL,
    /// Parenthesized parameter list.
    PARAM_LIST,
    /// `name: Type`.
    PARAM,
    /// `-> Type`.
    RET_TYPE,
    /// A type position (currently always a named type).
    TYPE_REF,
    /// Declaring identifier (`NAME` wraps the `IDENT` token).
    NAME,
    /// Referring identifier.
    NAME_REF,
    /// `{ ... }` block of statements with optional tail expression.
    BLOCK,
    LET_STMT,
    ASSIGN_STMT,
    RETURN_STMT,
    EXPR_STMT,
    /// `if cond { } else { }`.
    IF_EXPR,
    /// Infix binary operation.
    BIN_EXPR,
    /// Prefix `-x` / `!x`.
    PREFIX_EXPR,
    /// `callee(args)`.
    CALL_EXPR,
    /// Positional argument list.
    ARG_LIST,
    /// `expr.field`.
    FIELD_EXPR,
    /// `m::x` — a module-qualified name in expression position.
    PATH_EXPR,
    /// `(expr)`.
    PAREN_EXPR,
    /// Literal expression node wrapping a literal token.
    LITERAL,
    /// `Name { f: v, ... }`.
    STRUCT_LIT,
    /// `name: expr` inside a struct literal.
    STRUCT_LIT_FIELD,
    /// `base[i]` or `base[lo..hi]` — string indexing and slicing.
    INDEX_EXPR,
    /// `lo .. hi` inside brackets — either bound may be absent.
    RANGE,
    /// Parser error recovery node; wraps skipped tokens.
    ERROR,
    /// End of input marker used by the parser internally.
    EOF,
}

impl SyntaxKind {
    /// Whether this kind is whitespace or comment trivia.
    pub const fn is_trivia(self) -> bool {
        matches!(self, SyntaxKind::WHITESPACE | SyntaxKind::COMMENT)
    }

    /// Whether this kind is any keyword, reserved or usable.
    pub const fn is_keyword(self) -> bool {
        (self as u16) >= (SyntaxKind::DATA_KW as u16)
            && (self as u16) <= (SyntaxKind::INVARIANT_KW as u16)
    }

    /// Whether the keyword is merely reserved (not yet part of the
    /// grammar) and therefore cannot be used as an identifier.
    pub const fn is_reserved_keyword(self) -> bool {
        (self as u16) >= (SyntaxKind::UNSAFE_KW as u16)
            && (self as u16) <= (SyntaxKind::INVARIANT_KW as u16)
    }

    /// Human-readable name for diagnostics.
    pub const fn describe(self) -> &'static str {
        match self {
            SyntaxKind::WHITESPACE => "whitespace",
            SyntaxKind::COMMENT => "comment",
            SyntaxKind::IDENT => "identifier",
            SyntaxKind::INT_NUMBER => "integer literal",
            SyntaxKind::FLOAT_NUMBER => "float literal",
            SyntaxKind::STRING => "string literal",
            SyntaxKind::ERROR_TOKEN => "unrecognized character",
            SyntaxKind::DATA_KW => "`data`",
            SyntaxKind::FN_KW => "`fn`",
            SyntaxKind::LET_KW => "`let`",
            SyntaxKind::RETURN_KW => "`return`",
            SyntaxKind::IF_KW => "`if`",
            SyntaxKind::ELSE_KW => "`else`",
            SyntaxKind::TRUE_KW => "`true`",
            SyntaxKind::FALSE_KW => "`false`",
            SyntaxKind::UNSAFE_KW => "`unsafe`",
            SyntaxKind::MUT_KW => "`mut`",
            SyntaxKind::SHARED_KW => "`shared`",
            SyntaxKind::REGION_KW => "`region`",
            SyntaxKind::PARALLEL_KW => "`parallel`",
            SyntaxKind::TRAIT_KW => "`trait`",
            SyntaxKind::IMPL_KW => "`impl`",
            SyntaxKind::MATCH_KW => "`match`",
            SyntaxKind::FOR_KW => "`for`",
            SyntaxKind::WHILE_KW => "`while`",
            SyntaxKind::LOOP_KW => "`loop`",
            SyntaxKind::BREAK_KW => "`break`",
            SyntaxKind::CONTINUE_KW => "`continue`",
            SyntaxKind::STRUCT_KW => "`struct`",
            SyntaxKind::ENUM_KW => "`enum`",
            SyntaxKind::PUB_KW => "`pub`",
            SyntaxKind::USE_KW => "`use`",
            SyntaxKind::MOD_KW => "`mod`",
            SyntaxKind::CONST_KW => "`const`",
            SyntaxKind::STATIC_KW => "`static`",
            SyntaxKind::TYPE_KW => "`type`",
            SyntaxKind::WHERE_KW => "`where`",
            SyntaxKind::AS_KW => "`as`",
            SyntaxKind::IN_KW => "`in`",
            SyntaxKind::MOVE_KW => "`move`",
            SyntaxKind::ASYNC_KW => "`async`",
            SyntaxKind::AWAIT_KW => "`await`",
            SyntaxKind::DYN_KW => "`dyn`",
            SyntaxKind::TRY_KW => "`try`",
            SyntaxKind::SELF_KW => "`self`",
            SyntaxKind::SELF_TYPE_KW => "`Self`",
            SyntaxKind::SUPER_KW => "`super`",
            SyntaxKind::CRATE_KW => "`crate`",
            SyntaxKind::REQUIRES_KW => "`requires`",
            SyntaxKind::ENSURES_KW => "`ensures`",
            SyntaxKind::INVARIANT_KW => "`invariant`",
            SyntaxKind::L_PAREN => "`(`",
            SyntaxKind::R_PAREN => "`)`",
            SyntaxKind::L_BRACE => "`{`",
            SyntaxKind::R_BRACE => "`}`",
            SyntaxKind::L_BRACKET => "`[`",
            SyntaxKind::R_BRACKET => "`]`",
            SyntaxKind::COMMA => "`,`",
            SyntaxKind::SEMICOLON => "`;`",
            SyntaxKind::COLON => "`:`",
            SyntaxKind::COLON2 => "`::`",
            SyntaxKind::DOT => "`.`",
            SyntaxKind::DOT2 => "`..`",
            SyntaxKind::ARROW => "`->`",
            SyntaxKind::FAT_ARROW => "`=>`",
            SyntaxKind::PLUS => "`+`",
            SyntaxKind::MINUS => "`-`",
            SyntaxKind::STAR => "`*`",
            SyntaxKind::SLASH => "`/`",
            SyntaxKind::PERCENT => "`%`",
            SyntaxKind::EQ => "`=`",
            SyntaxKind::EQ2 => "`==`",
            SyntaxKind::NEQ => "`!=`",
            SyntaxKind::LT => "`<`",
            SyntaxKind::LE => "`<=`",
            SyntaxKind::GT => "`>`",
            SyntaxKind::GE => "`>=`",
            SyntaxKind::NOT => "`!`",
            SyntaxKind::AND2 => "`&&`",
            SyntaxKind::OR2 => "`||`",
            SyntaxKind::AMP => "`&`",
            SyntaxKind::PIPE => "`|`",
            SyntaxKind::CARET => "`^`",
            SyntaxKind::TILDE => "`~`",
            SyntaxKind::SHL => "`<<`",
            SyntaxKind::SHR => "`>>`",
            SyntaxKind::PLUS_EQ => "`+=`",
            SyntaxKind::MINUS_EQ => "`-=`",
            SyntaxKind::STAR_EQ => "`*=`",
            SyntaxKind::SLASH_EQ => "`/=`",
            SyntaxKind::PERCENT_EQ => "`%=`",
            SyntaxKind::QUESTION => "`?`",
            SyntaxKind::AT => "`@`",
            SyntaxKind::POUND => "`#`",
            SyntaxKind::EOF => "end of file",
            _ if self.is_reserved_keyword() => "reserved keyword",
            _ => "syntax node",
        }
    }
}

/// Looks up a keyword kind by its text. Returns `None` for identifiers.
pub fn keyword_kind(text: &str) -> Option<SyntaxKind> {
    use SyntaxKind as K;
    Some(match text {
        "data" => K::DATA_KW,
        "fn" => K::FN_KW,
        "let" => K::LET_KW,
        "return" => K::RETURN_KW,
        "if" => K::IF_KW,
        "else" => K::ELSE_KW,
        "true" => K::TRUE_KW,
        "false" => K::FALSE_KW,
        "unsafe" => K::UNSAFE_KW,
        "mut" => K::MUT_KW,
        "shared" => K::SHARED_KW,
        "region" => K::REGION_KW,
        "parallel" => K::PARALLEL_KW,
        "trait" => K::TRAIT_KW,
        "impl" => K::IMPL_KW,
        "match" => K::MATCH_KW,
        "for" => K::FOR_KW,
        "while" => K::WHILE_KW,
        "loop" => K::LOOP_KW,
        "break" => K::BREAK_KW,
        "continue" => K::CONTINUE_KW,
        "struct" => K::STRUCT_KW,
        "enum" => K::ENUM_KW,
        "pub" => K::PUB_KW,
        "use" => K::USE_KW,
        "mod" => K::MOD_KW,
        "const" => K::CONST_KW,
        "static" => K::STATIC_KW,
        "type" => K::TYPE_KW,
        "where" => K::WHERE_KW,
        "as" => K::AS_KW,
        "in" => K::IN_KW,
        "move" => K::MOVE_KW,
        "async" => K::ASYNC_KW,
        "await" => K::AWAIT_KW,
        "dyn" => K::DYN_KW,
        "try" => K::TRY_KW,
        "self" => K::SELF_KW,
        "Self" => K::SELF_TYPE_KW,
        "super" => K::SUPER_KW,
        "crate" => K::CRATE_KW,
        "requires" => K::REQUIRES_KW,
        "ensures" => K::ENSURES_KW,
        "invariant" => K::INVARIANT_KW,
        _ => return None,
    })
}

/// The Ontixa language marker type for `rowan`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OntixaLanguage {}

impl rowan::Language for OntixaLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        // `SyntaxKind` discriminants are dense and start at 0, so a
        // bounds-checked table lookup avoids any `unsafe` transmute.
        KIND_TABLE
            .get(raw.0 as usize)
            .copied()
            .unwrap_or(SyntaxKind::ERROR)
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        rowan::SyntaxKind(kind as u16)
    }
}

/// Dense lookup table mapping raw `u16` values back to [`SyntaxKind`].
/// Generated once at startup; index = raw kind value.
static KIND_TABLE: &[SyntaxKind] = &[
    SyntaxKind::WHITESPACE,
    SyntaxKind::COMMENT,
    SyntaxKind::IDENT,
    SyntaxKind::INT_NUMBER,
    SyntaxKind::FLOAT_NUMBER,
    SyntaxKind::STRING,
    SyntaxKind::ERROR_TOKEN,
    SyntaxKind::DATA_KW,
    SyntaxKind::FN_KW,
    SyntaxKind::LET_KW,
    SyntaxKind::RETURN_KW,
    SyntaxKind::IF_KW,
    SyntaxKind::ELSE_KW,
    SyntaxKind::TRUE_KW,
    SyntaxKind::FALSE_KW,
    SyntaxKind::USE_KW,
    SyntaxKind::AS_KW,
    SyntaxKind::UNSAFE_KW,
    SyntaxKind::MUT_KW,
    SyntaxKind::SHARED_KW,
    SyntaxKind::REGION_KW,
    SyntaxKind::PARALLEL_KW,
    SyntaxKind::TRAIT_KW,
    SyntaxKind::IMPL_KW,
    SyntaxKind::MATCH_KW,
    SyntaxKind::FOR_KW,
    SyntaxKind::WHILE_KW,
    SyntaxKind::LOOP_KW,
    SyntaxKind::BREAK_KW,
    SyntaxKind::CONTINUE_KW,
    SyntaxKind::STRUCT_KW,
    SyntaxKind::ENUM_KW,
    SyntaxKind::PUB_KW,
    SyntaxKind::MOD_KW,
    SyntaxKind::CONST_KW,
    SyntaxKind::STATIC_KW,
    SyntaxKind::TYPE_KW,
    SyntaxKind::WHERE_KW,
    SyntaxKind::IN_KW,
    SyntaxKind::MOVE_KW,
    SyntaxKind::ASYNC_KW,
    SyntaxKind::AWAIT_KW,
    SyntaxKind::DYN_KW,
    SyntaxKind::TRY_KW,
    SyntaxKind::SELF_KW,
    SyntaxKind::SELF_TYPE_KW,
    SyntaxKind::SUPER_KW,
    SyntaxKind::CRATE_KW,
    SyntaxKind::REQUIRES_KW,
    SyntaxKind::ENSURES_KW,
    SyntaxKind::INVARIANT_KW,
    SyntaxKind::L_PAREN,
    SyntaxKind::R_PAREN,
    SyntaxKind::L_BRACE,
    SyntaxKind::R_BRACE,
    SyntaxKind::L_BRACKET,
    SyntaxKind::R_BRACKET,
    SyntaxKind::COMMA,
    SyntaxKind::SEMICOLON,
    SyntaxKind::COLON,
    SyntaxKind::COLON2,
    SyntaxKind::DOT,
    SyntaxKind::DOT2,
    SyntaxKind::ARROW,
    SyntaxKind::FAT_ARROW,
    SyntaxKind::PLUS,
    SyntaxKind::MINUS,
    SyntaxKind::STAR,
    SyntaxKind::SLASH,
    SyntaxKind::PERCENT,
    SyntaxKind::EQ,
    SyntaxKind::EQ2,
    SyntaxKind::NEQ,
    SyntaxKind::LT,
    SyntaxKind::LE,
    SyntaxKind::GT,
    SyntaxKind::GE,
    SyntaxKind::NOT,
    SyntaxKind::AND2,
    SyntaxKind::OR2,
    SyntaxKind::AMP,
    SyntaxKind::PIPE,
    SyntaxKind::CARET,
    SyntaxKind::TILDE,
    SyntaxKind::SHL,
    SyntaxKind::SHR,
    SyntaxKind::PLUS_EQ,
    SyntaxKind::MINUS_EQ,
    SyntaxKind::STAR_EQ,
    SyntaxKind::SLASH_EQ,
    SyntaxKind::PERCENT_EQ,
    SyntaxKind::QUESTION,
    SyntaxKind::AT,
    SyntaxKind::POUND,
    SyntaxKind::SOURCE_FILE,
    SyntaxKind::USE_DECL,
    SyntaxKind::DATA_DECL,
    SyntaxKind::FIELD,
    SyntaxKind::FN_DECL,
    SyntaxKind::PARAM_LIST,
    SyntaxKind::PARAM,
    SyntaxKind::RET_TYPE,
    SyntaxKind::TYPE_REF,
    SyntaxKind::NAME,
    SyntaxKind::NAME_REF,
    SyntaxKind::BLOCK,
    SyntaxKind::LET_STMT,
    SyntaxKind::ASSIGN_STMT,
    SyntaxKind::RETURN_STMT,
    SyntaxKind::EXPR_STMT,
    SyntaxKind::IF_EXPR,
    SyntaxKind::BIN_EXPR,
    SyntaxKind::PREFIX_EXPR,
    SyntaxKind::CALL_EXPR,
    SyntaxKind::ARG_LIST,
    SyntaxKind::FIELD_EXPR,
    SyntaxKind::PATH_EXPR,
    SyntaxKind::PAREN_EXPR,
    SyntaxKind::LITERAL,
    SyntaxKind::STRUCT_LIT,
    SyntaxKind::STRUCT_LIT_FIELD,
    SyntaxKind::INDEX_EXPR,
    SyntaxKind::RANGE,
    SyntaxKind::ERROR,
    SyntaxKind::EOF,
];
