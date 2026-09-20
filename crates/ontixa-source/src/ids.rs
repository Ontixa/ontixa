//! Domain ID types.
//!
//! Every semantic entity in the compiler is identified by a small `u32`
//! wrapper rather than a raw index or pointer. The wrappers are `Copy`,
//! `Ord`, and cheap to store in every IR node.
//!
//! Invariants:
//!
//! - An ID is only meaningful together with the arena that issued it.
//! - IDs are never reused within a compilation session.
//! - Ordering of IDs reflects allocation order, which is deterministic
//!   because compilation is single-pass deterministic.

use std::fmt;

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
        #[serde(transparent)]
        pub struct $name(pub u32);

        impl $name {
            /// Creates an ID from a raw index. Intended for arena
            /// allocation code, not for general use.
            pub const fn new(raw: u32) -> Self {
                Self(raw)
            }

            /// The raw arena index. Useful for indexing into `Vec`-backed
            /// arenas after bounds are already guaranteed.
            pub const fn index(self) -> usize {
                self.0 as usize
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, concat!(stringify!($name), "({})"), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }
    };
}

define_id!(
    /// Identity of a source file within a compilation session.
    FileId
);

define_id!(
    /// Identity of a module. In milestone 1 there is one module per file.
    ModuleId
);

define_id!(
    /// Identity of a top-level definition (function or data declaration).
    DefId
);

define_id!(
    /// Identity of any named symbol: definitions, parameters, locals,
    /// and data fields all have symbol entries in the module symbol table.
    SymbolId
);

define_id!(
    /// Identity of an expression node inside the module-wide HIR arena.
    ExprId
);

define_id!(
    /// Identity of an interned type in the type table.
    TypeId
);

define_id!(
    /// Identity of a MIR local (parameter, user local, or temporary).
    LocalId
);

define_id!(
    /// Identity of a MIR basic block.
    BlockId
);

define_id!(
    /// Identity of an interned string.
    InternId
);

impl SymbolId {
    /// High bit marking a *body-local* symbol. Parameter and `let`
    /// binding ids index their owning [`HirBody`](hir)'s symbol
    /// arena — not the module `SymbolTable` — so inserting or editing
    /// one function can never renumber another function's bindings.
    /// (Stable symbol identity is what makes early-cutoff
    /// incrementality meaningful; see `docs/semantic-identity.md`.)
    pub const LOCAL_BIT: u32 = 1 << 31;

    /// The body-local id for arena index `raw`.
    pub const fn local(raw: u32) -> Self {
        Self(Self::LOCAL_BIT | raw)
    }

    /// Whether this id is body-local (param or `let` binding).
    pub const fn is_local(self) -> bool {
        self.0 & Self::LOCAL_BIT != 0
    }

    /// Index into the owning body's local symbol arena. Only valid
    /// when [`Self::is_local`].
    pub const fn local_index(self) -> usize {
        (self.0 & !Self::LOCAL_BIT) as usize
    }
}

/// Stable identity of a top-level definition across edits and
/// sessions: the workspace root it was resolved under, the file that
/// declares it, plus its interned name.
///
/// [`DefId`] is a dense per-scope index — it shifts when defs are
/// inserted or reordered, and the *same* def carries different
/// `DefId`s in different workspaces. The `root` component therefore
/// belongs to the key: a body lowered under workspace A is not the
/// same value as one lowered under workspace B, even for the same
/// file. `DefKey` is what incremental queries and cross-revision
/// state key on.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct DefKey {
    /// The workspace root this def was resolved under.
    pub root: FileId,
    /// Declaring file.
    pub file: FileId,
    /// Definition name (interned in the `Db`-level interner).
    pub name: InternId,
}

impl DefKey {
    /// Creates a key for the def named `name` in `file`, resolved
    /// under workspace `root`.
    pub const fn new(root: FileId, file: FileId, name: InternId) -> Self {
        Self { root, file, name }
    }
}

impl fmt::Debug for DefKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DefKey({}:{}:{})", self.root.0, self.file.0, self.name.0)
    }
}
