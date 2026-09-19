//! The canonical type representation.
//!
//! [`Ty`] is what type checking assigns to every expression and local.
//! It is a separate type from [`ontixa_hir::TypeRef`] on purpose:
//! `TypeRef` records *what the programmer wrote* (or failed to
//! resolve), while `Ty` is the semantic type downstream passes
//! (ownership analysis, MIR, codegen) reason about. Today they are
//! isomorphic; they diverge as soon as generics, references, or region
//! types arrive — the split keeps that evolution from touching every
//! signature in the compiler.

use ontixa_hir::TypeRef;
use ontixa_source::DefId;
use serde::Serialize;

/// A semantic type.
///
/// `Serialize` is implemented by hand: `Struct(DefId)` cannot use an
/// internally-tagged enum representation (serde_json cannot serialize
/// a tagged newtype containing an integer), so every variant emits
/// `{"kind": name}` plus `{"def": id}` for structs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    /// `bool`
    Bool,
    /// `i32`
    I32,
    /// `i64`
    I64,
    /// `u32`
    U32,
    /// `u64`
    U64,
    /// `f32`
    F32,
    /// `f64`
    F64,
    /// `str`
    Str,
    /// `unit`
    Unit,
    /// A user `data` type.
    Struct(DefId),
    /// A type that could not be determined because an upstream pass
    /// already emitted a diagnostic. Poison unifies with everything so
    /// one error never cascades into a waterfall of secondary errors.
    Poison,
}

impl Ty {
    /// Converts a resolved [`TypeRef`] into a [`Ty`].
    pub fn from_ref(t: TypeRef) -> Self {
        match t {
            TypeRef::Bool => Ty::Bool,
            TypeRef::I32 => Ty::I32,
            TypeRef::I64 => Ty::I64,
            TypeRef::U32 => Ty::U32,
            TypeRef::U64 => Ty::U64,
            TypeRef::F32 => Ty::F32,
            TypeRef::F64 => Ty::F64,
            TypeRef::Str => Ty::Str,
            TypeRef::Unit => Ty::Unit,
            TypeRef::Struct(d) => Ty::Struct(d),
            TypeRef::Poison => Ty::Poison,
        }
    }

    /// Whether this is a numeric type (supports `+ - * / %` and
    /// ordering comparisons).
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            Ty::I32 | Ty::I64 | Ty::U32 | Ty::U64 | Ty::F32 | Ty::F64
        )
    }

    /// Whether this is an integer type (supports `%` and literal
    /// range checks).
    pub fn is_integer(self) -> bool {
        matches!(self, Ty::I32 | Ty::I64 | Ty::U32 | Ty::U64)
    }

    /// Whether a value of this type is copied bitwise on use
    /// (`copy` semantics) rather than moved. Struct values move.
    pub fn is_copy(self) -> bool {
        !matches!(self, Ty::Struct(_) | Ty::Poison)
    }

    /// Whether `self` and `other` are the same type, treating `Poison`
    /// as unifying with anything (an error was already reported).
    pub fn compatible(self, other: Ty) -> bool {
        self == Ty::Poison || other == Ty::Poison || self == other
    }

    /// Stable lowercase name — the JSON `kind` and human form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Ty::Bool => "bool",
            Ty::I32 => "i32",
            Ty::I64 => "i64",
            Ty::U32 => "u32",
            Ty::U64 => "u64",
            Ty::F32 => "f32",
            Ty::F64 => "f64",
            Ty::Str => "str",
            Ty::Unit => "unit",
            Ty::Struct(_) => "struct",
            Ty::Poison => "poison",
        }
    }
}

impl Serialize for Ty {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let len = if matches!(self, Ty::Struct(_)) { 2 } else { 1 };
        let mut m = s.serialize_map(Some(len))?;
        m.serialize_entry("kind", self.as_str())?;
        if let Ty::Struct(d) = self {
            m.serialize_entry("def", d)?;
        }
        m.end()
    }
}

impl From<TypeRef> for Ty {
    fn from(t: TypeRef) -> Self {
        Ty::from_ref(t)
    }
}
