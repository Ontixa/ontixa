# ADR-0008: Incremental Query Engine

## Status

Accepted (Milestone 2)

## Context

Milestone 1's `Db` caches one `Artifacts` bundle per file; any
`set_source` invalidates everything. Milestone 2 requires genuine
query-driven incrementality: per-definition reuse, dependency
tracking, early cutoff, and observable metrics.

## Options considered

### salsa (0.28.x, crates.io)

rust-analyzer's engine; production-proven red/green memoization,
cycle recovery, interned inputs. Rejected for now because:

- Salsa owns the identity model: tracked structs, interned values,
  `Update` bounds, `Arc`-based sharing. Adopting it means replacing
  our arena IDs (`DefId`, `SymbolId`, `ExprId`) with salsa-interned
  identities — precisely the layer this milestone must control
  (stable `DefKey`s, per-body arenas, semantic-identity rules).
- The crate ships a breaking release every few weeks (26 breaking
  versions since 0.18). Pinning means tracking a moving API while the
  milestone is in flight.
- We need first-class instrumentation (per-query executed/reused/
  invalidated counters surfaced in the CLI and daemon). Salsa exposes
  durability/`report_synthetic_read` machinery but not a flat
  per-query ledger we can serialize.

### rustc-style query system

Macro-generated query table over a `TyCtxt`. Proven, but sized for a
compiler 100x our scale and entangled with rustc internals. Its
*algorithm* (memoized queries + dep tracking + red/green early
cutoff) is what we adopt, not the code.

### Custom engine (chosen)

A small memo table in `ontixa-db`:

- `QueryKey` enum — one variant per query (`Parse(FileId)`,
  `Scope(FileId)`, `HirBody(DefKey)`, `BodyTypes(DefKey)`,
  `MirBody(DefKey)`, `Ownership(ModuleId)`, `Graph(ModuleId)`,
  `Diagnostics(ModuleId)`, …).
- Each memo entry stores `{value, deps, computed_at, verified_at}`
  against a monotonically increasing `Revision` bumped by
  `set_source`.
- Verification walks deps recursively: a dep whose `changed_at >
  computed_at` marks the entry stale; recomputed values equal to the
  old value (`PartialEq`) keep `changed_at` — early cutoff.
- `QueryStats` counts executions, reuses, and invalidations per
  query — machine-readable in the CLI envelope and `ontixad`.

## Key structural prerequisite

Early cutoff only works if unchanged code produces *identical*
outputs. That forces the identity refactor shipped with this
milestone:

- `DefKey = (FileId, InternId)` — content-stable identity for defs;
  `DefId` remains the in-module dense index inside a `Scope` value.
- Per-body arenas: `HirBody` owns its `exprs` arena and its local
  symbol table (`SymbolId` high bit = body-local). A body edit in
  `f` can no longer renumber `g`'s exprs or locals, so
  `hir_body(g)` compares equal and downstream queries cut off.
- The `Db` interner is persistent across edits so `InternId`s are
  stable across revisions.

## Ownership analysis under incrementality

Interprocedural contracts are a fixpoint — not a tree query. The
`Ownership` query memoizes per-def `ParamFacts` keyed by the
callee-contract values each collection consumed; when contracts
re-run, only defs whose body or consumed callee contracts changed
re-walk their HIR. The worklist solver (ADR-0010) is unchanged.

## Known limitation: absolute spans

`Span`s are file-absolute. A length-changing edit shifts the offsets
of every item that follows it, so their `AstItem`/`HirBody` values
compare unequal and their chains re-run — even though their *content*
didn't change. Cutoff still holds for items textually *before* the
edit and for whole-file edits that preserve offsets (e.g.
same-length rewrites). Def spans deliberately cover only the
declaration *name* (`Def.span`), so a body edit inside `f` leaves
`Scope` equal.

Body-relative spans (rebased against the item start at diagnostic
render time) would remove this limitation; that's future work and is
recorded in `docs/semantic-identity.md`.

## Exit strategy

If the custom engine becomes the bottleneck (parallel evaluation,
persistence to disk), the `QueryKey` boundary maps onto salsa
queries one-for-one; adopting salsa later is a backend swap, not a
redesign.
