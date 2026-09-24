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
- string ops + `str` slices: `s + t` concatenation, `s.len` (a
  read-only `i32` property), `s[i]` character indexing, `s[lo..hi]`
  / `s[lo..]` / `s[..hi]` / `s[..]` slicing, lexicographic ordering.
  Indices count Unicode scalars; out-of-bounds traps. Demo:
  `examples/strings.ixa`
- structured semantic patches: a bounded op vocabulary
  (`replace_body`, `remove_def`, `add_def`) through the rename
  transaction pipeline — plan → preview → shadow-compile →
  revision-guarded atomic apply; CLI `patch` + daemon `patch` op
  (ADR-0016). Compile integrity, not semantic preservation; no
  reordering, comments, or file create/delete.
- wider patch ops on the same pipeline (ADR-0016 addendum):
  signature edits (`rename_param`, `set_param_type`,
  `set_ret_type` — name and body preserved), `use` management
  (`add_use`, `remove_use` — whole-declaration splices following
  the file's line conventions), and `data` field edits
  (`add_field`, `remove_field`, `rename_field`, `set_field_type` —
  `rename_field` rewrites decl + every resolved access/literal/
  assign site). Still out: reordering items, comment edits, file
  create/delete, and arbitrary non-item text.
- arrays/slices + `for` loops: homogeneous `[e, ...]` literals with
  inferred element types (`[T]` annotations, required for `[]`),
  `a.len`, `a[i]` indexing, `a[lo..hi]` slicing into a fresh array,
  `for x in a` element iteration over a snapshot of the iterable,
  `for i in lo..hi` counted ranges (bounds typed uniformly — a
  literal bound adopts the other bound's integer type); assigning to
  the loop variable requires `for mut x`. Out-of-bounds indices and
  invalid slices trap; unbounded ranges are rejected (no `break`
  yet). Demo: `examples/arrays.ixa`
- enum `data` variants + `match` selection: `data Opt { Some(i32);
  None; }` declares variants; `Opt::Some(v)` / `m::Opt::Some(v)`
  construct; `match o { Opt::Some(v) => v, _ => 0 }` selects on the
  discriminant and binds payload elements positionally. A bare
  identifier arm binds the whole scrutinee (an owned copy — matching
  borrows); `_` ignores it. Matches must be exhaustive (`E_NON_EXHAUSTIVE`
  lists the missing variants); a catch-all makes unreachable later
  arms a `W_UNREACHABLE_ARM` warning. Carriers propagate through
  non-`Copy` pattern binds, so `return v` still escapes the
  scrutinee's source. Demo: `examples/match.ixa`

Remaining:
- `return`-less tail returns everywhere (blocks already tail-expr)
- more primitives (`char` — `u*`/`f32`/`f64`/`str` already resolve)
- still-wider patch ops — the protocol is general; each op adds
  resolution logic (item reordering, comment/header edits, and
  file-level create/delete are the standing boundary)
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
