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
- Milestone-1 limits: no stack-depth management, `&&`/`||` are eager
  (no side effects exist to observe it), `i128` arithmetic superset
  instead of width-exact ops.
