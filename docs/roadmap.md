# Roadmap

Direction, not promise. Order reflects dependency, not calendar.

## M1 — vertical slice (current)

Done: lossless parsing, canonical AST, HIR + resolution, total type
checking, ownership inference, SPG, typed MIR, interpreter, CLI,
structured diagnostics, memoized DB.

## M2 — language breadth

- `let mut`-free mutation rules finalized (field-write vs rebind)
- arrays/slices + `for` loops
- `match`-like selection over `data` variants (enum data)
- `return`-less tail returns everywhere (blocks already tail-expr)
- string ops + `str` slices
- more primitives (`u*`, `f32`, `char`)

## M3 — real compiler infrastructure

- `ontixad`: persistent compile daemon over `Db`
- fine-grained incrementality (salsa-style early cutoff — ADR-0004)
- multi-file modules + `use`
- `ontixa fmt` over the lossless CST
- semantic patches (structured apply)

## M4 — execution

- native codegen (cranelift or LLVM — ADR pending)
- WASM target
- ownership-guided allocation: escape analysis → region/arena
  placement, stack promotion of non-escaping values
- deterministic drop order guarantees

## M5 — agent platform

- evidence-carrying patches
- contract-diff tooling (`ontixa diff --semantic`)
- capability/effect system (see capabilities.md)
- proof-carrying code changes for verified refactors

## M6 — performance

- SIMD intrinsics + autotuning hooks
- workload specialization (compile-time profile-guided variants)
- benchmark suite with regression gates (benchmark-philosophy.md)

## Explicit non-goals for now

- generics/traits (designed for, not built)
- async/await syntax (structured concurrency lands with effects)
- self-hosted compiler
- package manager
