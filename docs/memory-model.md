# Memory Model

Ontixa gives Rust-class memory safety with no ownership syntax.
Single ownership, scope-bound borrows, deterministic destruction —
all inferred, all enforced.

## The contract lattice

Every function parameter gets an inferred `ParamBehavior`:

```text
copy < borrow < borrow_mut < move < escape
                             └ unknown (conservative ≈ move)
```

| behavior     | callee may...                        | caller afterwards |
| ------------ | ------------------------------------ | ----------------- |
| `copy`       | read its own copy                    | fully usable      |
| `borrow`     | read the argument                    | fully usable      |
| `borrow_mut` | write through field projections      | fully usable      |
| `move`       | consume it                           | moved — unusable  |
| `escape`     | consume it; it may reach the return  | moved — unusable  |
| `unknown`    | anything (analysis inconclusive)     | moved — unusable  |

`copy` applies only to `Copy` types (today: `i32`/`i64`/`f64`/`bool`
and friends). `data` values are never `Copy`.

## How inference works

Two phases over the typed HIR (`ontixa-memory`):

**Phase A — facts + fixpoint.** Each function body is walked for
carrier-tainted uses: reads taint `borrow`, field-writes taint
`borrow_mut`, move-position uses taint `move`, and positions that can
reach the return value (returns, struct constructions, calls to
escaping callees) taint `escape`. Contracts propagate along `calls`
edges, so per-function contracts iterate to a fixpoint — recursion
converges to `unknown` rather than hanging or guessing.

**Phase B — enforcement.** A per-function walk tracks each binding's
state (`live`, `moved`, `maybe-moved`, `uninit`) and merges states
across `if` branches:

- read of `moved`/`maybe-moved` → `E_USE_AFTER_MOVE`
- read of `uninit` → `E_UNINITIALIZED`
- `borrow`/`borrow_mut`/`copy` calls leave the argument's state alone
- `move`/`escape`/`unknown` calls consume it
- assignment to a `moved` binding is a legal re-initialization

## Mutability (immutable by default)

Bindings are **immutable unless declared `mut`**:

```ixa
let p = Point { x: 1, y: 2 };   // immutable
let mut q = Point { x: 0, y: 0 }; // mutable local
fn bump(mut p: Point) { ... }    // mutable parameter
```

- Assignment to a non-`mut` binding → `E_IMMUTABLE_ASSIGNMENT`
  (declaration site labeled, `let mut` suggested).
- **Deferred init** — `let x: T; x = v;` — is allowed once on an
  immutable binding (write-once initialization). Re-assignment, or a
  write after a move, requires `mut`.
- Field writes `p.x = v` require `mut` on the root binding.
- A `borrow_mut` argument must be a **mutable place**: `bump(q)` with
  immutable `q` → `E_MUTABLE_BORROW_OF_IMMUTABLE`. A fresh temporary
  (`bump(P { x: 1 })`) needs no authority — the mutation is contained.
- The compiler never silently upgrades a binding to `mut`.

## Field-carrier precision

`return p.x` where `x: i32` only *reads* `p` — a `Copy` field carries
no ownership, so `p` stays `borrow`. When the field type is not
`Copy`, `p` is the carrier and taints accordingly. Struct literals
propagate the same way field-by-field.

## Places, loans, and regions

A **place** is a path to storage: a binding plus field projections
(`x`, `x.f.g`). A `borrow`/`borrow_mut` argument creates a **loan** on
that place for the call's extent — the **region** `Region::Call(id)`
(the NLL foundation: when `&` expressions arrive, regions become
point ranges and the rest is unchanged).

Live loans are a stack; each nested call pushes its own. Conflict
rules (see ADR-0011):

- shared + shared on overlapping places: fine.
- any loan overlapping a live mutable loan, or a mutable loan over a
  live shared one → `E_BORROW_CONFLICT` (`mix(q, q)`).
- moving a place under any live loan → `E_MOVE_WHILE_BORROWED`
  (`take(q, q)` where `take` borrows `a`, moves `b`).
- disjoint projections never conflict: `m(q.a, q.b)` is fine even
  with two `borrow_mut` params.

**Escape summaries** (`OwnershipTables::escapes`) record where each
parameter's value may exit — `Return` or `ViaCall(def)` — and surface
through `ontixa explain` as `params[].escapes`.

## Escape dominance

`escape` dominates `move`: a value returned to the caller is "more
consumed" than one merely dropped — the distinction matters because
an escaping argument's ownership transfers through the return edge,
which future region/arena allocation will use for placement.

## What the runtime does

The interpreter executes contracts literally: `borrow`/`borrow_mut`
arguments pass the caller's storage *cell* (writes land in place);
consuming positions receive fresh cells. Ownership bugs are rejected
at compile time, so execution never checks them again.
