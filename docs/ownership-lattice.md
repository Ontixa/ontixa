# The Ownership Lattice

Ownership inference works on **two levels**: a multi-dimensional fact
domain gathered per body, and a user-facing behavior contract derived
from those facts.

## The fact domain

`ParamFacts` (in `ontixa-memory/src/analyze.rs`) is the semantic
domain — four independent dimensions:

| fact      | dimension        | set when                                   |
| --------- | ---------------- | ------------------------------------------ |
| `read`    | access ≥ read    | the param's value is observed              |
| `mutated` | access = write   | written through a field projection         |
| `moved`   | consumption      | ownership consumed and does not escape     |
| `escaped` | escape           | the value may flow into the return value   |

Facts are gathered by a single taint walk per function under the
*current* callee contracts, so they are monotone in two ways:

- during one walk, flags only go `false → true`;
- between walks, a callee contract can only strengthen, which can only
  add facts to its callers.

## The contract lattice

`ParamBehavior` is derived by `classify(ty, facts)`:

```text
copy < borrow < borrow_mut < move < escape
```

(with `unknown` reserved as a conservative top — see below)

| behavior     | caller obligation                      | callee guarantee                    |
| ------------ | -------------------------------------- | ----------------------------------- |
| `copy`       | none (value is bitwise copied)         | cannot observe or consume the arg   |
| `borrow`     | lend for the call                      | reads; never mutates or consumes    |
| `borrow_mut` | lend + hold `mut` authority            | may write through field projections |
| `move`       | give up the value                      | consumes; does not return it        |
| `escape`     | give up the value; it may be returned  | consumes; value may reach `return`  |

Derivation (`classify`):

```text
ty.is_copy()           -> copy        (fixed by type — never changes)
escaped                -> escape      (dominates: the value may outlive the call)
moved                  -> move
mutated                -> borrow_mut
read                   -> borrow
none of the above      -> move        (an unused non-copy param is still
                                       consumed: ownership transfers in and
                                       the value drops at scope end)
```

### Why this ordering is sound

- `escape` dominates `move`: both consume the argument, but escape
  additionally permits the value to reach the caller through the
  return edge — strictly more caller-visible effect. For the caller
  both are "the binding is dead after the call"; for future region
  allocation they differ, so they are distinct lattice points.
- `move` dominates `borrow_mut`/`borrow`: consumption subsumes any
  read/write performed before the consume.
- `borrow_mut` dominates `borrow`: write access implies read.
- `copy` is bottom *and* fixed: `is_copy` is a type property, so a
  `copy` contract can never strengthen. It is excluded from
  fixpoint dynamics entirely.

`unknown` is **not** a fixpoint result. It is reserved for genuinely
unanalyzable constructs (e.g. future indirect/closure calls) where the
facts are unavailable — never for "iteration did not finish".

## The fixpoint (no round cap)

Phase A runs a reverse-dependency worklist:

1. every fn's contract starts at bottom (`copy` by type, else
   `borrow`);
2. a worklist of all fns is processed; evaluating `f` recomputes its
   facts under current callee contracts and re-classifies;
3. when `f`'s contract changes, only its *callers* are re-enqueued.

Termination is structural: contracts only ascend a finite lattice, so
each fn re-enqueues its callers finitely often. Chains of any depth,
direct recursion, mutual recursion, and multiple SCCs all converge —
there is no `MAX_ROUNDS` and no `unknown`-on-timeout.

The worklist is order-sensitive only in *speed*, not result: the
operator is monotone and the lattice finite, so chaotic iteration
reaches the unique least fixpoint. Caller lists are sorted before
enqueue so evaluation order is deterministic anyway.

## Current limitations (honest)

- Non-copy *field* moves (`return p.q` where `q` is a struct)
  conservatively move the whole binding — no partial-move tracking.
- `borrow_mut` authority is tracked at binding granularity
  (`let mut p`), not per-field.
- Loans/regions are layered on top of this domain in Milestone 2
  (see `docs/region-inference.md` when it lands).
