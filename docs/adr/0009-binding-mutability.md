# ADR-0009: Binding Mutability (`let mut`, `mut` params)

## Status

Accepted (Milestone 2)

## Context

The language constitution requires values to be immutable by default.
Milestone 1 had no mutation syntax at all: every binding was implicitly
assignable, and ownership inference could produce `BorrowMut` for a
callee without any check that the *caller* had authority to mutate the
argument. Two contract violations resulted:

1. `let q = P { x: 1 }; q = P { x: 2 };` compiled silently.
2. `fn bump(p: P) { p.x = ... }` inferred `BorrowMut`, and calling
   `bump(q)` on an immutable `q` mutated it anyway.

## Decision

- **Syntax:** `let mut x = ...` for locals and `mut p: T` for
  parameters — the Rust-familiar prefix form. Chosen over a standalone
  `mut x = ...` statement because the `let`-prefix keeps declarations
  visually uniform and matches prior art users already know.
- **Representation:** mutability is a property of the *binding*
  (`Symbol::mutable`), not of the type. It is carried from CST → AST
  (`Param::mutable`, `Stmt::Let::mutable`) → HIR (`Symbol::mutable`).
- **Enforcement (Phase B of `infer_ownership`):**
  - Assigning to a non-`mut` binding emits `E_IMMUTABLE_ASSIGNMENT`
    with the declaration site labeled and `let mut` repair help.
  - **Deferred initialization** (`let x: T; x = v;`) is permitted on an
    immutable binding — it is write-once initialization, not mutation.
    A second assignment, or a write after a move, requires `mut`.
  - Field writes (`p.x = v`) require `mut` authority on the root
    binding, because they write into an already-initialized value.
  - Passing an immutable place to a `borrow_mut`-contract parameter
    emits `E_MUTABLE_BORROW_OF_IMMUTABLE`. A fresh temporary
    (`bump(P { x: 1 })`) needs no authority — the mutation is
    contained.
- **No silent upgrade:** the compiler never rewrites an immutable
  binding to mutable; it diagnoses and suggests `mut`.

## Consequences

- All mutation is syntactically explicit and locally visible.
- `BorrowMut` is now an *authority-checked* contract, not merely a
  description of what the callee happens to do.
- Deferred-init preserves ergonomics for `let x: T; ...; x = v;`.
- Limitations: `mut` is whole-binding only (no field-granular
  mutability); `mut` on a param does not by itself make the contract
  `BorrowMut` — inference still decides from body usage.
