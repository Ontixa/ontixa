//! Inferred parameter behavior.
//!
//! Ontixa has no ownership syntax. Instead, the compiler *infers* what
//! a function does with each parameter from the body's actual uses,
//! producing a [`ParamBehavior`] contract that is part of the
//! function's semantic signature — visible in the Semantic Program
//! Graph and enforced at every call site.
//!
//! The lattice (weakest → strongest contract):
//!
//! ```text
//! copy < borrow < borrow_mut < move < escape
//!                             └ unknown (conservative ≈ move)
//! ```
//!
//! - **Copy** — the argument is copied bitwise; the callee can never
//!   observe or consume the caller's value. Only `Copy` types.
//! - **Borrow** — the callee reads the argument but never mutates or
//!   consumes it. The caller keeps ownership.
//! - **BorrowMut** — the callee may mutate the argument through field
//!   projections but never consumes it. The caller keeps ownership.
//! - **Move** — the callee takes ownership; the argument is consumed
//!   and does not flow into the callee's outward result.
//! - **Escape** — the callee takes ownership and the argument's value
//!   may flow into the function's return value (directly, inside a
//!   constructed value, or through another escaping call).
//! - **Unknown** — the analysis could not converge on a contract
//!   (e.g. a pathological recursion). Callers must treat it as
//!   consuming the argument.

use serde::Serialize;

/// What a function does with one parameter, inferred from its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamBehavior {
    /// Bitwise copy; caller's value is unaffected. (Copy types only.)
    Copy,
    /// Read-only borrow.
    Borrow,
    /// Mutable borrow through field projections.
    BorrowMut,
    /// Ownership consumed, not escaped.
    Move,
    /// Ownership consumed; value may reach the return value.
    Escape,
    /// Analysis could not decide; treated conservatively as `Move`.
    Unknown,
}

impl ParamBehavior {
    /// Whether an argument passed to this parameter leaves the
    /// caller's ownership (the caller may not use it afterwards).
    pub fn consumes_arg(self) -> bool {
        matches!(
            self,
            ParamBehavior::Move | ParamBehavior::Escape | ParamBehavior::Unknown
        )
    }

    /// Whether the caller may use the argument again after the call.
    pub fn caller_keeps_arg(self) -> bool {
        !self.consumes_arg()
    }

    /// Stable string form for JSON and human output.
    pub const fn as_str(self) -> &'static str {
        match self {
            ParamBehavior::Copy => "copy",
            ParamBehavior::Borrow => "borrow",
            ParamBehavior::BorrowMut => "borrow_mut",
            ParamBehavior::Move => "move",
            ParamBehavior::Escape => "escape",
            ParamBehavior::Unknown => "unknown",
        }
    }
}
