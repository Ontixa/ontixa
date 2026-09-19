//! Places, loans, and regions — the structural layer under borrow
//! enforcement (ADR-0011).
//!
//! A [`Place`] is a path to storage: a binding plus field
//! projections (`x`, `x.f`, `x.f.g`). A [`Loan`] records that a call
//! holds shared or mutable access to a place for a [`Region`]. With
//! today's surface language there are no `&` expressions — borrows
//! exist only as inferred call contracts — so a loan's region is the
//! call's own extent: it is born when the argument is evaluated and
//! dies when the call returns. That is the non-lexical-lifetime
//! foundation: when `&`/`&mut` expressions arrive, loans will carry
//! real regions (sets of program points) instead of a single call
//! extent, but the conflict rules and place machinery stay the same.
//!
//! Overlap is prefix-based on the projection path: `x` overlaps `x.f`
//! and `x.f.g`, but `x.f` and `x.g` are disjoint.

use ontixa_hir::{HirBody, HirExprKind};
use ontixa_source::{ExprId, InternId, Interner, Span, SymbolId};

/// A path to storage: `base` projected through `fields` in order.
/// `x.f.g` = `Place { base: x, fields: [f, g] }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// Root binding (body-local or module-level symbol).
    pub base: SymbolId,
    /// Field projections by interned field name.
    pub fields: Vec<InternId>,
    /// Source extent of the place expression.
    pub span: Span,
}

impl Place {
    /// `self` reads/writes storage that `other` also touches — same
    /// root binding and one projection path prefixes the other.
    pub fn overlaps(&self, other: &Place) -> bool {
        self.base == other.base
            && (self.fields.starts_with(&other.fields) || other.fields.starts_with(&self.fields))
    }

    /// Renders `x.f.g` for diagnostics.
    pub fn describe(&self, interner: &Interner) -> String {
        // The base name is resolved by the caller (needs the owning
        // body's symbol arena); fields render here.
        let mut s = String::new();
        for f in &self.fields {
            s.push('.');
            s.push_str(interner.resolve(*f));
        }
        s
    }
}

/// Extracts the place an expression denotes, if it denotes one
/// (`x` or a field-projection chain). Non-place expressions —
/// literals, calls, struct literals — return `None`.
pub fn place_of(body: &HirBody, id: ExprId) -> Option<Place> {
    let mut fields = Vec::new();
    let mut cur = id;
    let span = body.expr(id).span;
    loop {
        match &body.expr(cur).kind {
            HirExprKind::Var(sym) => {
                fields.reverse();
                return Some(Place {
                    base: *sym,
                    fields,
                    span,
                });
            }
            HirExprKind::Field { base, name, .. } => {
                fields.push(name.id);
                cur = *base;
            }
            _ => return None,
        }
    }
}

/// Whether a loan reads or mutates its place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoanKind {
    /// Read access (`borrow` contract).
    Shared,
    /// Write access (`borrow_mut` contract).
    Mut,
}

/// The program region a loan is live for. Today's loans exist only
/// at call sites — `Region::Call(call_expr)` is the call's own
/// extent. Named as a type so `&`-expressions can introduce
/// point-range regions without changing the conflict machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Region {
    /// The loan lives for the duration of this call expression.
    Call(ExprId),
}

/// A live loan: the place borrowed, the access kind, where the loan
/// was taken (`at`), and the region it is live for.
#[derive(Debug, Clone)]
pub struct Loan {
    /// The borrowed place.
    pub place: Place,
    /// Shared or mutable access.
    pub kind: LoanKind,
    /// Span of the argument expression the loan was created for.
    pub at: Span,
    /// The extent the loan is live for.
    pub region: Region,
}
