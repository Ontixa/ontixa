# ADR-0003: Ownership by inference, not annotation

Status: accepted (milestone 1) — the defining bet of the language.

## Context

Rust's borrow checker is the state of the art for GC-free safety, but
its cost is annotation + lifetime machinery and a steep learning
curve. The claim behind Ontixa: most functions' parameter behavior is
mechanically inferable, and inference + enforcement beats annotation
+ verification for ergonomics without giving up safety.

## Decision

No ownership syntax exists. `ontixa-memory` computes a
`ParamBehavior` per parameter (`copy`/`borrow`/`borrow_mut`/`move`/
`escape`/`unknown`) via a two-phase analysis: usage-fact collection
with carrier tainting, iterated to a call-graph fixpoint; then an
enforcement walk emitting `E_USE_AFTER_MOVE`/`E_UNINITIALIZED` with
branch-merged binding states.

Contracts are semantic *facts* — they describe what the body does,
not what the author claims.

## Alternatives

- Rust-style annotations: maximally expressive, maximal syntax.
- Pure move semantics (always consume): safe but hostile.
- Reference counting / GC: rejected by constitution §3.

## Consequences

- Interprocedural fixpoint handles recursion by converging to
  `unknown` (conservative) rather than failing.
- `escape` is richer than `move` — it feeds future region/arena
  allocation, which plain move semantics cannot express.
- Enforced limits today: no first-class reference types, no
  closures, no generics — each is a deliberate future extension.
