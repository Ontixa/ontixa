//! Stable diagnostic codes.
//!
//! Codes are part of the compiler's public API. They must never be
//! reused for a different meaning; retire codes instead of repurposing
//! them. `E_*` codes are errors, `W_*` warnings, `I_*` internal faults.

/// A stable machine-readable diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
#[serde(into = "&'static str")]
pub enum Code {
    /// Malformed syntax; parser could not construct a valid node.
    Parse,
    /// A `"` string literal reached end of file or line unclosed.
    UnterminatedString,
    /// A `/*` comment reached end of file unclosed.
    UnterminatedComment,
    /// A character that has no token role appeared in the source.
    UnexpectedCharacter,
    /// Two top-level definitions share a name.
    DuplicateDef,
    /// An identifier did not resolve to any visible symbol.
    UnknownSymbol,
    /// A type position named something that is not a type.
    UnknownType,
    /// Field access on a value that has no fields.
    NotAStruct,
    /// A struct value has no field of the requested name.
    UnknownField,
    /// A struct literal initializes a field the type does not have.
    ExtraField,
    /// Two fields of one `data` declaration share a name, or a struct
    /// literal initializes the same field twice.
    DuplicateField,
    /// A struct literal omits a required field.
    MissingField,
    /// Call arity does not match the callee signature.
    ArgCount,
    /// The callee of a call expression is not a function.
    NotCallable,
    /// A value's type does not match the type the context requires.
    TypeMismatch,
    /// A `let` binding has neither a type annotation nor an
    /// initializer to infer from.
    CannotInfer,
    /// A value was used after its ownership moved away.
    UseAfterMove,
    /// A binding was read before any value was stored in it.
    Uninitialized,
    /// An assignment or mutation targets a place without `mut`
    /// authority.
    ImmutableAssignment,
    /// A `borrow_mut`-behavior parameter received an immutable place.
    MutableBorrowOfImmutable,
    /// A non-unit function can complete without returning a value.
    MissingReturn,
    /// An integer literal does not fit its required type.
    LiteralOverflow,
    /// An operation on a type that does not support it
    /// (e.g. `==` between struct values).
    UnsupportedOperation,
    /// Internal compiler error. Never caused by user code.
    Internal,
}

impl Code {
    /// The stable string form used in JSON and human output.
    pub const fn as_str(self) -> &'static str {
        match self {
            Code::Parse => "E_PARSE",
            Code::UnterminatedString => "E_UNTERMINATED_STRING",
            Code::UnterminatedComment => "E_UNTERMINATED_COMMENT",
            Code::UnexpectedCharacter => "E_UNEXPECTED_CHAR",
            Code::DuplicateDef => "E_DUPLICATE_DEF",
            Code::UnknownSymbol => "E_UNKNOWN_SYMBOL",
            Code::UnknownType => "E_UNKNOWN_TYPE",
            Code::NotAStruct => "E_NOT_A_STRUCT",
            Code::UnknownField => "E_UNKNOWN_FIELD",
            Code::ExtraField => "E_EXTRA_FIELD",
            Code::DuplicateField => "E_DUPLICATE_FIELD",
            Code::MissingField => "E_MISSING_FIELD",
            Code::ArgCount => "E_ARG_COUNT",
            Code::NotCallable => "E_NOT_CALLABLE",
            Code::TypeMismatch => "E_TYPE_MISMATCH",
            Code::CannotInfer => "E_CANNOT_INFER",
            Code::UseAfterMove => "E_USE_AFTER_MOVE",
            Code::Uninitialized => "E_UNINITIALIZED",
            Code::ImmutableAssignment => "E_IMMUTABLE_ASSIGNMENT",
            Code::MutableBorrowOfImmutable => "E_MUTABLE_BORROW_OF_IMMUTABLE",
            Code::MissingReturn => "E_MISSING_RETURN",
            Code::LiteralOverflow => "E_LITERAL_OVERFLOW",
            Code::UnsupportedOperation => "E_UNSUPPORTED_OP",
            Code::Internal => "I_INTERNAL",
        }
    }
}

impl From<Code> for &'static str {
    fn from(code: Code) -> Self {
        code.as_str()
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
