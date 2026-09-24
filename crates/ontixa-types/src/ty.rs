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

use ontixa_hir::{ElemRef, TypeRef};
use ontixa_source::DefId;
use serde::Serialize;

/// A semantic type.
///
/// `Serialize` is implemented by hand: `Struct(DefId)` cannot use an
/// internally-tagged enum representation (serde_json cannot serialize
/// a tagged newtype containing an integer), so every variant emits
/// `{"kind": name}` plus `{"def": id}` for structs and `{"elem": ty}`
/// for arrays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    /// `bool`
    Bool,
    /// `i8`
    I8,
    /// `i16`
    I16,
    /// `i32`
    I32,
    /// `i64`
    I64,
    /// `isize` — pointer-width signed (64-bit semantics).
    Isize,
    /// `u8`
    U8,
    /// `u16`
    U16,
    /// `u32`
    U32,
    /// `u64`
    U64,
    /// `usize` — pointer-width unsigned (64-bit semantics).
    Usize,
    /// `f32`
    F32,
    /// `f64`
    F64,
    /// `str`
    Str,
    /// `char` — a single Unicode scalar.
    Char,
    /// `unit`
    Unit,
    /// A user `data` type.
    Struct(DefId),
    /// `[T]` — a homogeneous array. `elem` is flat by construction:
    /// nested arrays are rejected at type resolution.
    Array(ElemTy),
    /// A type that could not be determined because an upstream pass
    /// already emitted a diagnostic. Poison unifies with everything so
    /// one error never cascades into a waterfall of secondary errors.
    Poison,
}

/// A semantic array element type — every [`Ty`] except `Array`
/// itself (nested arrays are not supported yet), `Unit`, and
/// `Poison`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ElemTy {
    /// `bool`
    Bool,
    /// `i8`
    I8,
    /// `i16`
    I16,
    /// `i32`
    I32,
    /// `i64`
    I64,
    /// `isize` — pointer-width signed (64-bit semantics).
    Isize,
    /// `u8`
    U8,
    /// `u16`
    U16,
    /// `u32`
    U32,
    /// `u64`
    U64,
    /// `usize` — pointer-width unsigned (64-bit semantics).
    Usize,
    /// `f32`
    F32,
    /// `f64`
    F64,
    /// `str`
    Str,
    /// `char` — a single Unicode scalar.
    Char,
    /// A user `data` type.
    Struct(DefId),
}

impl ElemTy {
    /// The full [`Ty`] this element type stands for.
    pub const fn ty(self) -> Ty {
        match self {
            ElemTy::Bool => Ty::Bool,
            ElemTy::I8 => Ty::I8,
            ElemTy::I16 => Ty::I16,
            ElemTy::I32 => Ty::I32,
            ElemTy::I64 => Ty::I64,
            ElemTy::Isize => Ty::Isize,
            ElemTy::U8 => Ty::U8,
            ElemTy::U16 => Ty::U16,
            ElemTy::U32 => Ty::U32,
            ElemTy::U64 => Ty::U64,
            ElemTy::Usize => Ty::Usize,
            ElemTy::F32 => Ty::F32,
            ElemTy::F64 => Ty::F64,
            ElemTy::Str => Ty::Str,
            ElemTy::Char => Ty::Char,
            ElemTy::Struct(d) => Ty::Struct(d),
        }
    }

    /// The element type for a resolved [`ElemRef`].
    pub fn from_ref(e: ElemRef) -> Self {
        match e {
            ElemRef::Bool => ElemTy::Bool,
            ElemRef::I8 => ElemTy::I8,
            ElemRef::I16 => ElemTy::I16,
            ElemRef::I32 => ElemTy::I32,
            ElemRef::I64 => ElemTy::I64,
            ElemRef::Isize => ElemTy::Isize,
            ElemRef::U8 => ElemTy::U8,
            ElemRef::U16 => ElemTy::U16,
            ElemRef::U32 => ElemTy::U32,
            ElemRef::U64 => ElemTy::U64,
            ElemRef::Usize => ElemTy::Usize,
            ElemRef::F32 => ElemTy::F32,
            ElemRef::F64 => ElemTy::F64,
            ElemRef::Str => ElemTy::Str,
            ElemRef::Char => ElemTy::Char,
            ElemRef::Struct(d) => ElemTy::Struct(d),
        }
    }
}

impl Ty {
    /// Converts a resolved [`TypeRef`] into a [`Ty`].
    pub fn from_ref(t: TypeRef) -> Self {
        match t {
            TypeRef::Bool => Ty::Bool,
            TypeRef::I8 => Ty::I8,
            TypeRef::I16 => Ty::I16,
            TypeRef::I32 => Ty::I32,
            TypeRef::I64 => Ty::I64,
            TypeRef::Isize => Ty::Isize,
            TypeRef::U8 => Ty::U8,
            TypeRef::U16 => Ty::U16,
            TypeRef::U32 => Ty::U32,
            TypeRef::U64 => Ty::U64,
            TypeRef::Usize => Ty::Usize,
            TypeRef::F32 => Ty::F32,
            TypeRef::F64 => Ty::F64,
            TypeRef::Str => Ty::Str,
            TypeRef::Char => Ty::Char,
            TypeRef::Unit => Ty::Unit,
            TypeRef::Struct(d) => Ty::Struct(d),
            TypeRef::Array { elem } => Ty::Array(ElemTy::from_ref(elem)),
            TypeRef::Poison => Ty::Poison,
        }
    }

    /// The [`ElemTy`] for a type that can be an array element —
    /// `None` for `unit`, `[_]`, and `Poison`.
    pub fn elem(self) -> Option<ElemTy> {
        Some(match self {
            Ty::Bool => ElemTy::Bool,
            Ty::I8 => ElemTy::I8,
            Ty::I16 => ElemTy::I16,
            Ty::I32 => ElemTy::I32,
            Ty::I64 => ElemTy::I64,
            Ty::Isize => ElemTy::Isize,
            Ty::U8 => ElemTy::U8,
            Ty::U16 => ElemTy::U16,
            Ty::U32 => ElemTy::U32,
            Ty::U64 => ElemTy::U64,
            Ty::Usize => ElemTy::Usize,
            Ty::F32 => ElemTy::F32,
            Ty::F64 => ElemTy::F64,
            Ty::Str => ElemTy::Str,
            Ty::Char => ElemTy::Char,
            Ty::Struct(d) => ElemTy::Struct(d),
            Ty::Unit | Ty::Array(_) | Ty::Poison => return None,
        })
    }

    /// Whether this is a numeric type (supports `+ - * / %` and
    /// ordering comparisons).
    pub fn is_numeric(self) -> bool {
        self.is_integer() || matches!(self, Ty::F32 | Ty::F64)
    }

    /// Whether this is an integer type (supports `%` and literal
    /// range checks).
    pub fn is_integer(self) -> bool {
        matches!(
            self,
            Ty::I8
                | Ty::I16
                | Ty::I32
                | Ty::I64
                | Ty::Isize
                | Ty::U8
                | Ty::U16
                | Ty::U32
                | Ty::U64
                | Ty::Usize
        )
    }

    /// Whether a value of this type is copied bitwise on use
    /// (`copy` semantics) rather than moved. Struct and array values
    /// move.
    pub fn is_copy(self) -> bool {
        !matches!(self, Ty::Struct(_) | Ty::Array(_) | Ty::Poison)
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
            Ty::I8 => "i8",
            Ty::I16 => "i16",
            Ty::I32 => "i32",
            Ty::I64 => "i64",
            Ty::Isize => "isize",
            Ty::U8 => "u8",
            Ty::U16 => "u16",
            Ty::U32 => "u32",
            Ty::U64 => "u64",
            Ty::Usize => "usize",
            Ty::F32 => "f32",
            Ty::F64 => "f64",
            Ty::Str => "str",
            Ty::Char => "char",
            Ty::Unit => "unit",
            Ty::Struct(_) => "struct",
            Ty::Array(_) => "array",
            Ty::Poison => "poison",
        }
    }
}

impl Serialize for Ty {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let len = if matches!(self, Ty::Struct(_) | Ty::Array(_)) {
            2
        } else {
            1
        };
        let mut m = s.serialize_map(Some(len))?;
        m.serialize_entry("kind", self.as_str())?;
        match self {
            Ty::Struct(d) => m.serialize_entry("def", d)?,
            Ty::Array(e) => m.serialize_entry("elem", &e.ty())?,
            _ => {}
        }
        m.end()
    }
}

impl From<TypeRef> for Ty {
    fn from(t: TypeRef) -> Self {
        Ty::from_ref(t)
    }
}
