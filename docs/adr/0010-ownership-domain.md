# ADR-0010: Ownership Domain and Fixpoint Strategy

## Status

Accepted (Milestone 2)

## Context

Milestone 1 inferred `ParamBehavior` directly via a round-robin loop
capped at `MAX_ROUNDS = 16`. Correctness depended on a magic number:
any dependency needing >16 propagation rounds collapsed to `Unknown`.

## Decision

1. **Two-level domain.** The analysis domain is `ParamFacts` — four
   boolean dimensions (`read`, `mutated`, `moved`, `escaped`), one
   per semantic axis. The public `ParamBehavior` enum is *derived* by
   `classify`. This separates "what the body does" (facts, gathered)
   from "what callers must promise" (behavior, derived). Documented in
   `docs/ownership-lattice.md`.

2. **Reverse-dependency worklist fixpoint.** `callees_of` extracts
   call edges per body; the callee→caller map drives a worklist:
   evaluate a function, and only when its contract changed re-enqueue
   its callers. Callee lists are sorted for deterministic enqueue
   order.

3. **`Unknown` is deliberate.** Nothing in the fixpoint produces
   `Unknown`. The variant remains for constructs the analysis cannot
   see (future indirect calls, FFI). Callers already treat it as
   consuming — conservatively correct.

## Why not SCC scheduling

SCC condensation (process callee components first, iterate only
inside cycles) is the textbook-efficient order, but the worklist
already attains the same asymptotic bound — each contract ascends a
5-point lattice, so total re-evaluations are `O(lattice_height ×
edges)` — while being substantially simpler and naturally handling
cycles without a separate intra-SCC loop. SCCs may be revisited if
profiling shows fact collection dominating; the worklist abstraction
does not preclude it.

## Termination argument

- `contracts[f]` starts at bottom (`Copy` for copy types — immutable
  by construction — else `Borrow`).
- `collect_facts` under stronger callee contracts produces
  monotonically more facts; `classify` is monotone in facts.
- Hence `contracts[f]` only ascends `Borrow < BorrowMut < Move <
  Escape`, at most 3 strengthenings per non-copy param.
- Re-enqueue happens only on change ⇒ worklist is finite.
- Result: unique least fixpoint, independent of evaluation order.

## Consequences

- 100-deep call chains propagate `escape`/`move` correctly (tested).
- Direct and mutual recursion converge to `borrow`/`escape` as
  appropriate (tested).
- `Unknown` regains its honest meaning: "information unavailable",
  not "the compiler got tired".
