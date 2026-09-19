//! Source-level primitives for the Ontixa compiler.
//!
//! This crate owns the vocabulary every other compiler crate shares:
//!
//! - [`Span`] — byte-offset ranges into a source file.
//! - [`SourceFile`] — an immutable UTF-8 source text with a line index.
//! - [`Interner`] — string interning for identifiers.
//! - Semantic ID types ([`FileId`], [`ModuleId`], [`DefId`], [`SymbolId`],
//!   [`ExprId`], [`TypeId`], [`LocalId`], [`BlockId`], [`InternId`]).
//!
//! # Span conventions
//!
//! Spans are half-open byte ranges `[start, end)` into the UTF-8 text of a
//! single [`SourceFile`]. `end` is **exclusive**. Line/column numbers are
//! derived from the file's line index and are always 1-based when rendered
//! for humans.
//!
//! # ID stability
//!
//! IDs are dense `u32` indices into per-module arenas. Within one compiler
//! session they are stable and unique. Across sessions (serialization,
//! `ontixad`, incremental recompilation) persistence is provided by
//! content-derived keys — see `docs/architecture.md` — not by reusing raw
//! index values. Do not serialize bare IDs across sessions without the
//! owning arena.

mod file;
mod ids;
mod intern;
mod span;

pub use file::{LineCol, SourceFile};
pub use ids::{BlockId, DefId, ExprId, FileId, InternId, LocalId, ModuleId, SymbolId, TypeId};
pub use intern::Interner;
pub use span::Span;
