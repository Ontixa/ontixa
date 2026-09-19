# ADR-0011: Places, Loans, and Regions

## Status

Accepted (Milestone 2, Phases 14–20)

## Context

Milestone 1's ownership enforcement tracked *bindings*, not *paths*.
Two consequence-shaped gaps remained:

1. **No borrow conflicts.** A call `mix(q, q)` where one parameter is
   `borrow` and another `borrow_mut` passed silently — the analyzer
   evaluated each argument independently and never asked whether two
   live accesses touched the same storage.
2. **No place model.** `q.a` and `q` were indistinguishable from `q`
   and `r` — field projections had no identity, so disjointness
   (`q.a` vs `q.b`) could not be expressed.

There are no `&`/`&mut` expressions in the surface language yet:
borrows exist only as inferred call contracts. The machinery still
needs real names — places, loans, regions — so that when reference
expressions arrive, only the *region* widens, not the model.

## Decision

1. **Place** (`ontixa-memory::place::Place`): a path to storage —
   a base `SymbolId` plus ordered field projections by `InternId`.
   `x.f.g` is `Place { base: x, fields: [f, g] }`. Overlap is
   prefix-based: `x` overlaps `x.f`; `x.f` and `x.g` are disjoint.
   `place_of` extracts the place a HIR expression denotes
   (`Var`/`Field` chains only — calls and literals return `None`).

2. **Loan**: a live access — `place`, `LoanKind` (`Shared`/`Mut`),
   the argument span it was born at, and its `Region`. Loans are
   created when a `borrow`/`borrow_mut` argument is evaluated and
   popped when the call returns (a `loans.truncate(mark)` — loans
   form a stack, innermost call deepest).

3. **Region**: the extent a loan is live for. Today
   `Region::Call(ExprId)` — the call's own extent — is the only
   inhabitant. This is the NLL foundation: when `&` expressions
   arrive, regions become point ranges, but the conflict rules and
   place machinery are unchanged.

4. **Conflict rules** (checked in `Enforcer::add_loan`):
   - `shared` + `shared` on overlapping places: fine.
   - any loan overlapping a live `Mut` loan: `E_BORROW_CONFLICT`.
   - a `Mut` loan over a place with a live `Shared` loan: conflict.
   - a move (`Ctx::Move` eval) of a place under any live loan:
     `E_MOVE_WHILE_BORROWED`, labeled at the loan's origin.

5. **Escape summaries** (`OwnershipTables::escapes`): for each fn
   parameter, the exits its value may take — `EscapeExit::Return`
   (flows into the return value) or `EscapeExit::ViaCall(def)`
   (flows into a call whose callee may return it). Collected
   alongside `ParamFacts` in the same memoized pass, so they inherit
   the incremental machinery for free. Surfaced through
   `ontixa explain` (`params[].escapes`).

## Why the enforcement pass owns loans

Loans live in `Enforcer` (Phase B), not `FactCollector` (Phase A).
Facts describe what a body *does*; loans describe what is *live at a
program point*. The enforcer already walks bodies in execution order
with control-flow joins — the natural place for a program-point
analysis. The collector only needs contract facts.

## Consequences

- `take(q, q)` where `take` borrows `a` and moves `b` →
  `E_MOVE_WHILE_BORROWED` (tested).
- `mix(q, q)` for any shared/mut combination → `E_BORROW_CONFLICT`
  (tested both orders and mut+mut).
- `m(q.a, q.b)` with both `borrow_mut` → clean (disjoint
  projections); `m(q.a, q)` → conflict (prefix overlap).
- Loans end at call return — a `read(q)` followed by `eat(q)` is
  fine; the shared loan does not leak past its call (tested).
- Escape summaries are incremental: they ride the same `FactsEntry`
  memo entries and hypothesis-verified rounds as `ParamFacts`.

## Deliberate limits (future work)

- **No first-class references.** `&x`/`&mut x` expressions will
  introduce loans whose regions are real point ranges; `Region`
  gains a `Points` variant. The conflict checker is already
  region-agnostic — it asks only whether two live loans overlap.
- **Loan regions do not yet flow through `if`/branch joins**
  beyond the call extent (call-scoped loans never span branches).
- **No two-phase borrows.** A mutably-borrowed arg's place is
  checked eagerly; reservation semantics are unnecessary at call
  granularity.
