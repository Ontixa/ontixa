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
    /// A `use` declaration or `m::x` path named a module that no file
    /// in the workspace provides.
    UnknownModule,
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
    /// Two overlapping borrows of one place conflict at a call site
    /// (a mutable loan colliding with any live loan).
    BorrowConflict,
    /// A place was moved while an in-progress call holds a loan on it.
    MoveWhileBorrowed,
    /// A name lookup (e.g. `explain <symbol>`) matched more than one
    /// semantic symbol.
    AmbiguousSymbol,
    /// A rename's requested new name is not a valid identifier.
    InvalidName,
    /// A rename's new name is already bound in an affected file.
    NameConflict,
    /// A rename's validation (shadow-compile) produced diagnostics
    /// the original workspace did not have — nothing was applied.
    RenameRejected,
    /// A source-transaction apply (rename or patch) arrived with a
    /// plan revision older than the current workspace revision.
    StaleRevision,
    /// A source transaction was requested on a workspace that
    /// already reports error diagnostics — validation requires a
    /// clean baseline.
    BaselineErrors,
    /// A rename or patch plan failed provenance checks at apply
    /// time: wrong database/session, workspace fingerprint drift,
    /// or a candidate payload that no longer matches the validated
    /// one.
    PlanMismatch,
    /// The requested symbol kind cannot be renamed (e.g. a module
    /// name, which is the file stem — renaming it renames the file).
    UnsupportedTarget,
    /// A semantic-patch spec was structurally invalid: bad shape,
    /// unknown op, missing field, oversized payload, or edits that
    /// overlap — rejected before any planning.
    MalformedPatch,
    /// A semantic patch's validation (shadow-compile of the patched
    /// sources) produced error diagnostics — nothing was applied.
    PatchRejected,
    /// A non-unit function can complete without returning a value.
    MissingReturn,
    /// An integer literal does not fit its required type.
    LiteralOverflow,
    /// An operation on a type that does not support it
    /// (e.g. `==` between struct values).
    UnsupportedOperation,
    /// A `match` over an enum `data` value does not cover every
    /// variant (and has no `_`/binding catch-all arm).
    NonExhaustive,
    /// A `match` arm can never run — a previous arm already covers
    /// every value it could match.
    UnreachableArm,
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
            Code::UnknownModule => "E_UNKNOWN_MODULE",
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
            Code::BorrowConflict => "E_BORROW_CONFLICT",
            Code::MoveWhileBorrowed => "E_MOVE_WHILE_BORROWED",
            Code::AmbiguousSymbol => "E_AMBIGUOUS_SYMBOL",
            Code::InvalidName => "E_INVALID_NAME",
            Code::NameConflict => "E_NAME_CONFLICT",
            Code::RenameRejected => "E_RENAME_REJECTED",
            Code::StaleRevision => "E_STALE_REVISION",
            Code::BaselineErrors => "E_BASELINE_ERRORS",
            Code::PlanMismatch => "E_PLAN_MISMATCH",
            Code::UnsupportedTarget => "E_UNSUPPORTED_TARGET",
            Code::MalformedPatch => "E_MALFORMED_PATCH",
            Code::PatchRejected => "E_PATCH_REJECTED",
            Code::MissingReturn => "E_MISSING_RETURN",
            Code::LiteralOverflow => "E_LITERAL_OVERFLOW",
            Code::UnsupportedOperation => "E_UNSUPPORTED_OP",
            Code::NonExhaustive => "E_NON_EXHAUSTIVE",
            Code::UnreachableArm => "W_UNREACHABLE_ARM",
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
