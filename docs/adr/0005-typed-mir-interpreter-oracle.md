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

### Returning blocks in an expected type context

A block without a value tail can adopt its supplied expected type when
the existing structural completion check proves it cannot reach its end.
For example, `id({ return 42; })` in an `i32` function now accepts an
`i32` parameter context without requiring a dead `0` tail. The return
leaves the enclosing function before `id` executes. This also applies
to annotated initializers, assignments, explicit-return operands,
aggregate elements, and arithmetic where a type context already exists.
A logical RHS can use its LHS's boolean type, while the skipped path
still reaches the continuation.

Every statement is checked before the block adopts that context. Return
payloads use the enclosing function's return type, and invalid dead
declarations, later returns, and value tails still report their existing
diagnostics. Completing blocks remain unit; optional returns in logical
RHS expressions, branches without an else, non-exhaustive matches, and
loop bodies cannot establish that the enclosing block always returns.
Calls and literal conditions remain conservative.

Without a supplied type context, tail-less blocks still infer unit. This
does not add a never type or new context propagation: an unannotated
array such as `[1, { return 42; }]` and a logical LHS such as
`{ return 42; } && true` keep their existing errors. Existing typed
returning blocks, including already-valid if/match arms and function
bodies, now record the expected expression type instead of unit. Their
type-table and semantic-graph annotations may therefore change; artifacts
remain deterministic for the same source and compiler version. This is
an intentional diagnostic and annotation improvement, with no new codes
or schemas. Ownership joins, inferred contracts, MIR lowering, and
interpreter operations are unchanged. It does not complete the broader
tail-return milestone. See `examples/returning-blocks.ixa`.

### Numeric context through arithmetic

Arithmetic (`+`, `-`, `*`, `/`, `%`) and numeric negation preserve a
required numeric result type when checking their operands. This lets
`fn f() -> i64 { 40 + 2 }` and `let x: f32 = -(1.0 + 2.0);` adopt the
declared type at their literal leaves. Previously these expressions
lost that context and were rejected as default-width values. The same
rule applies wherever the checker already supplies a numeric context,
including call arguments, aggregate elements, and continuing branch tails.

Except for the literal-peer extension below, the left operand's actual
type guides the right operand; existing typed values are never coerced.
Without context, literals still default
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

### Default integer signs

When a directly negated integer literal has no expected type, its
negated value is checked against the existing `i32` default. Thus
`let x = -2147483648;` and `let y = -2147483648 + 1;` are valid, just
like the corresponding annotated `i32` bindings. Previously the
unannotated path checked the positive magnitude before the sign and
rejected the minimum. This is a narrow default-sign correction.

The existing paths for expected integer and noninteger types are
unchanged. Positive `2147483648`, negative `-2147483649`, and
`-(2147483648 + 0)` still report `E_LITERAL_OVERFLOW` without context.
This does not fold compound expressions, widen default types, convert
numeric families or existing bindings, or extend peer/range inference.
MIR still emits negation, and the interpreter keeps its `i128`/`f64`
representation. See `examples/default-integer-minimum.ixa`.

### Numeric literal peers

Bare numeric literals and a single negation of a numeric literal can
adopt an independently checked peer's numeric type for arithmetic and
equality/ordering comparisons. This deliberately extends the earlier
left-to-right rule: with `x: i64`, `0 < x` now works like `x > 0`, and
`let y = 1 + x;` infers `i64`. The corresponding float cases adopt
`f32` or `f64`; integer literals never become floats or vice versa.

A declared arithmetic result context retains priority. Without that
context, a literal-like left operand is deferred only beside a peer
whose type checking is independent of its incoming expected type:
variables, calls, fields, indexes, and the built-in length property;
blocks with such a tail; negation of such an operand; and arithmetic
with such a left operand. Those checker arms ignore the incoming
context or propagate it only through an already independent child.
The peer is checked once without an expected type, then its numeric
type guides the literal. Other shapes use the original left-to-right
traversal. This preserves contexts needed by literal-valued blocks
with signed minima and if/match peers whose branches all return.
Grouping parentheses do not change these rules. Two literal-like
operands retain their existing defaults and checks. Existing typed
values keep their types, including bindings inferred from literals.

This is not general bidirectional inference. Compound left operands
such as `(1 + 2) < x`, `-(1 + 2) < x`, and `{ 0 } < x` still need an
appropriate declared context or typed intermediate binding when
`x: i64`. Context-dependent right peers such as `0 < (1 + x)` and
`0 < if true { x } else { x }` also retain the earlier behavior;
use a typed intermediate binding when needed. Existing inward
propagation through those peers stays unchanged. Integer ranges and
negated signed-minimum checks remain in
force: `-128 < x` accepts an `i8` peer; `128 > x` and `-129 < x`
report `E_LITERAL_OVERFLOW`. The separate `for` range-bound rule is
unchanged, including its restriction to bare literals.

Only literal syntax with no binding reads, declarations, or effects
is deferred during type checking. HIR operand order, MIR lowering,
runtime evaluation, definite initialization, ownership analysis, and
boolean short-circuiting are unchanged. The interpreter still uses
its `i128`/`f64` arithmetic representation; this does not add casts,
constant folding, or width-exact overflow/rounding semantics. See
`examples/numeric-peers.ixa`.

### Floating-point remainder

The reference interpreter now executes `%` for `f32` and `f64` operands;
previously, valid typed MIR reached an unsupported-operation trap.
Like the existing float arithmetic, it uses Rust's `f64` operation on
the interpreter's `Value::Float(f64)` representation at both declared
widths. This retains the documented arithmetic superset; it does not
introduce width-exact `f32` rounding.

The operation follows [Rust's truncating remainder](https://doc.rust-lang.org/std/ops/trait.Rem.html#impl-Rem-for-f64),
with the dividend's sign: `5.5 % 2.0` is `1.5`, and `-5.5 % 2.0` is
`-1.5` regardless of the divisor's sign. A zero result preserves the
dividend's sign. A zero divisor, infinite dividend, or NaN operand
produces NaN without trapping; a finite dividend modulo either infinity
returns the dividend. Tests check NaN classification, not its sign or
payload. Integer remainder and its zero-divisor trap are unchanged.

This fills the missing interpreter dispatch case without changing
checking, MIR, diagnostic codes, or output schemas. Existing float
output applies: human output displays NaN as `NaN`, while JSON encodes
non-finite float results as `null`. See `examples/float-remainder.ixa`.
