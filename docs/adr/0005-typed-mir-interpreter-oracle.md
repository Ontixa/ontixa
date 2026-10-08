# ADR-0005: Typed MIR + interpreter as the semantics oracle

Status: accepted (milestone 1)

## Context

The language needs a defined, executable semantics *before* any
native backend exists. Two options: interpret HIR directly, or lower
to an execution IR and interpret that.

## Decision

Lower to `ontixa-mir`: a typed CFG (locals, temporaries, places with
field projections, `Return`/`Branch`/`Goto` terminators). `Call`
rvalues carry the callee's inferred contract. The reference
interpreter executes MIR with cell-based storage — `borrow`/
`borrow_mut` args share the caller's cells, consuming positions get
fresh cells.

## Alternatives

- HIR/tree-walk interpreter: simpler, but hides the CFG structure
  codegen needs and bakes semantics into a form that never ships.
- Bytecode VM: a second IR nobody needs yet; MIR + blocks is already
  the right shape for a backend.

## Consequences

- The interpreter is the semantics oracle: future backends diff
  against it.
- Contract-aware calls prove at runtime that borrow/mut sharing is
  real — `borrow_mut` writes land in caller storage (tested).
- Milestone-1 limits were no stack-depth management, eager `&&`/`||`,
  and an `i128` arithmetic superset instead of width-exact ops. The
  eager-boolean limit is superseded by the milestone-3 decision below;
  the other limits remain.

## Milestone-2 hardening

The interpreter no longer merely *executes* contracts — it validates
them. `borrow` args are deep-snapshotted and compared post-call (a
callee write traps); `move`/`escape`/`unknown` args get fresh cells
while the caller's cell is poisoned with `Hole` on return (later
reads or re-moves trap). These traps are unreachable through valid
source — the static pass rejects violations first — so they exist as
a loud failure mode for a wrong contract (compiler bug or hand-built
MIR), verified by contract-tampering tests.

## Milestone-3 boolean short-circuit evaluation

`lhs && rhs` evaluates `lhs` once and evaluates `rhs` only when `lhs`
is true. `lhs || rhs` evaluates `rhs` only when `lhs` is false. The
skipped result is respectively false or true. Required RHS evaluation
keeps its ordinary effects, traps, and returns; a skipped RHS has none.
Both operands must still type-check as `bool`, including an unreachable
RHS. Precedence is unchanged: `&&` binds tighter than `||`. Single
`&` and `|` remain parse errors.

Lowering uses the existing MIR `Branch`/`Goto`/`Return` vocabulary and
a shared boolean temporary. It introduces no MIR schema change or new
interpreter operation. Native and WASM execution remain future work.

Ownership enforcement joins the post-LHS state with the completing
RHS state. Initialization or reinitialization only in the RHS is not
definite; a conditional move is maybe-moved. A RHS that always returns
does not contribute to the continuing state. Analysis still checks
both paths, without literal-condition folding or interprocedural
reachability. Inferred contracts conservatively include possible RHS
uses, even when a literal LHS would skip them at runtime.

This completes a documented milestone limit and changes observable
behavior. Programs relying on eager RHS mutation, traps, or returns
must use explicit sequential statements for unconditional work.
Programs relying on RHS-only initialization may now report
`E_UNINITIALIZED` or `E_USE_AFTER_MOVE`; no diagnostic code changes.
See `examples/short-circuit.ixa` for guarded indexing and mutation.

### Function completion after short-circuiting

PR #19 exposed an older type-checker hole: a discarded nested block's
value tail was treated as a function return. For example,
`fn main() -> i32 { { false && { return 7; true } }; }` used to return
7 through eager RHS evaluation, but short-circuiting made it reach the
end and return unit. The checker now rejects this with the existing
`E_MISSING_RETURN` diagnostic; `check` and `run` stop before execution.
Short-circuit evaluation itself is unchanged.

Completion follows required evaluation in source order. Producing a
nested value does not leave the function, and discarding that value
cannot satisfy its return type. A logical RHS alone cannot prove all
paths return; its LHS can. All alternatives must return for an if or
exhaustive match to stop completion. Loop bodies may execute zero times,
while their iterables and range bounds are required. Calls and literal
conditions remain conservative, without termination inference or
constant folding. A completing root tail must match the declared result;
a completing non-unit body without a tail needs an explicit return.

The shared completion check also removes pre-existing false errors for
nested explicit returns and returns followed by dead statements, and
recognizes nested returning match arms. All expressions and explicit
return values are still type-checked, including unreachable children.
This added no never type or general coercions; mixed return/value if
typing and ownership joins were left to the bounded correction below.
Diagnostic codes, schemas, ownership contracts, and interpreter operations
are unchanged.
See `examples/return-completion.ixa` for an early return plus fallback.

### Returning branches of `if`

An `if` with an `else` now uses only normally completing branches to
determine its value type, following the existing match-arm convention.
`if c { return 7; } else { 42 }` therefore has an `i32` continuing
value. If neither arm completes, the type is the expected type or unit.
An `if` without `else` still discards its then-value and has unit type;
the skipped path can continue. Both arms and every explicit return
payload remain checked, including unreachable children.

Ownership enforcement likewise joins only completing branch states.
`if c { return p; } p` no longer reports a move from the returning path,
and initialization in the only continuing branch is definite. Moves or
missing initialization on any continuing path remain errors. Both arms
retain loan checking, including loans from an enclosing call. If neither
arm completes, the saved state is used for subsequent dead-code checking,
as for an all-returning match.

These correct pre-existing false rejections, independently of the
missing-return hole fixed above. The existing structural completion
checks stay conservative: logical RHS paths are optional, loop bodies
may run zero times, and calls and literal conditions do not establish
termination. There is no general never type, constant folding, or
interprocedural termination inference. MIR lowering, contract inference,
escape summaries, diagnostic codes, and schemas are unchanged; the interpreter
continues to execute the existing branches and returns. The broader
tail-return milestone remains open. See `examples/returning-branches.ixa`.

### Numeric context through arithmetic

Arithmetic (`+`, `-`, `*`, `/`, `%`) and numeric negation preserve a
required numeric result type when checking their operands. This lets
`fn f() -> i64 { 40 + 2 }` and `let x: f32 = -(1.0 + 2.0);` adopt the
declared type at their literal leaves. Previously these expressions
lost that context and were rejected as default-width values. The same
rule applies wherever the checker already supplies a numeric context,
including call arguments, aggregate elements, and continuing branch tails.

The left operand's actual type still guides the right operand; existing
typed values are never coerced. Without context, literals still default
to `i32`/`f64`. Comparisons and boolean operators do not propagate their
result context into numeric operands. String concatenation is unchanged.
Every adopted integer literal is range-checked, including folded signed
minima: `-128 + 0` fits `i8`, while `-(128 + 0)` contains an overflowing
positive `i8` literal and is rejected. This is not constant folding.

This changes only checking and inferred expression types. MIR operations,
ownership contracts, diagnostic codes and schemas stay the same. The
interpreter's existing `i128`/`f64` arithmetic representation remains;
width-exact arithmetic overflow and float rounding are separate work.
See `examples/numeric-context.ixa`.
