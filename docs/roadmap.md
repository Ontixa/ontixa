# Roadmap

Direction, not promise. Order reflects dependency, not calendar.

## M1 — vertical slice (done)

Done: lossless parsing, canonical AST, HIR + resolution, total type
checking, ownership inference, SPG, typed MIR, interpreter, CLI,
structured diagnostics, memoized DB.

## M2 — incremental semantic core (done)

- immutable bindings by default; `let mut` / `mut` params (ADR-0009)
- one schema-1 JSON envelope per `--json` command; `explain <symbol>`
  with contract evidence and escape summaries
- formal ownership domain; reverse-worklist fixpoint (ADR-0010)
- places, loans, borrow conflicts, call-extent regions (ADR-0011)
- per-definition incremental query engine: `DefKey` identity,
  per-body arenas, early cutoff, persistent oracle (ADR-0008)
- `ontixad` persistent daemon (NDJSON over stdio — ADR-0012)
- byte-deterministic artifacts; SPG `passes` memory edges; MIR
  mutability; interpreter contract oracle
- evidence: `docs/milestones/m2-report.md`, `benchmarks/README.md`

## M3 — language breadth + modules (in progress)

Shipped (semantic-workspace campaign, PRs #3–#5):

- multi-file workspaces: file-stem modules, `use m` / `use m::x [as
  y]` / `m::x` paths, file-tagged diagnostics, `DefKey(root,file,
  name)` identity (ADR-0013)
- item-relative spans: offset-shifting edits preserve per-def
  semantic values (see semantic-identity.md)
- semantic rename transactions: plan → preview → shadow-compile →
  revision-guarded atomic apply; CLI `rename` + daemon `rename` op
  (ADR-0014); demo: `examples/workspace/demo.sh`
- incremental engine fix: `verified_at` freshness (a changed dep no
  longer forces permanent re-eval of equal-valued dependents)
- `ontixa fmt` over the lossless CST: canonical whitespace/indent
  re-emission that preserves comments, idempotent; CLI
  preview/`--check`/`--write` + daemon `fmt` op (formats bound
  sources without mutating)

Remaining:

- arrays/slices + `for` loops
- `match`-like selection over `data` variants (enum data)
- `return`-less tail returns everywhere (blocks already tail-expr)
- string ops + `str` slices
- more primitives (`u*`, `f32`, `char`)
- semantic patches beyond rename (structured apply of arbitrary
  edits — the transaction shape exists; generality doesn't)
- finer-grained file-level queries (per-item parse, incremental
  graph) — the honest weakness stands; see benchmarks/README.md

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
