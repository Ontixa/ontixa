# Semantic identity

How Ontixa names things — the invariant that makes incremental
recompilation, cross-revision tool state, and future multi-file
references possible.

## The rules

| Identity | Scope | Stability |
|---|---|---|
| `FileId` | `Db` session | Stable: dense index of `add_source` order |
| `InternId` | `Db` session | Stable: the session interner is append-only |
| `DefKey` | `(FileId, InternId)` | **Stable across revisions** — a def's identity is its file + name |
| `DefId` | one `ModuleScope` value | Per-revision: dense index; shifts when defs are added/removed/reordered |
| `SymbolId` (module) | one `ModuleScope` value | Module-level symbols only: defs and `data` fields |
| `SymbolId` (local, `LOCAL_BIT` set) | one `HirBody` | Body-local: params then `let` bindings, in order |
| `ExprId` | one `HirBody` | Index into that body's `exprs` arena |
| `LocalId`, `BlockId` | one `MirBody` | Per-body MIR arenas |

The dividing line: **module-level identity** (`DefId`, module
`SymbolId`) indexes things that exist exactly once per compilation of
a scope; **body-local identity** (`SymbolId::local`, `ExprId`) indexes
things that exist per function body. Everything body-local lives in
`HirBody` arenas, so inserting, removing, or editing one function can
never renumber another function's ids.

## Why

Early cutoff (ADR-0008) compares a recomputed value with the memoized
one; the comparison is only meaningful when ids mean the same thing on
both sides. With a module-wide expression arena, appending a function
would shift every earlier/later `ExprId` and force a full rebuild.
Per-body arenas make a body's ids a function of *that body alone*.

## Resolving body-local ids

A body-local `SymbolId` is meaningless without its owning body. Use:

- `HirBody::symbol(&scope, id)` — dispatches to `local_symbols` for
  local ids, `scope.symbols` for module-level ones.
- `HirBody::expr(id)` — the body's expression arena.

Maps that key symbols or expressions across bodies must include the
owning def — the semantic graph keys `symbol_nodes`/`expr_nodes` by
`(DefId, SymbolId)`/`(DefId, ExprId)`.

## Parameters

`FnSig::params[i].symbol == SymbolId::local(i)`: the signature assigns
the id, and the body's `local_symbols[i]` is the same parameter's
`Symbol` record once lowered. `ParamDef` also carries `name`,
`mutable`, `ty`, and `span` directly, so signature consumers never
touch the body arena.

## Diagnostics

`Db` entries record *transitive* diagnostics: an entry's `diags` are
its dependencies' diagnostics (demand order) plus its own eval's.
The `Diagnostics` query therefore only has to collect direct deps.

## Known limitation

`Span` offsets are file-absolute. An edit that changes the byte length
of one item shifts the spans of every following item, so those items'
`AstItem`/`HirBody` values compare unequal and re-run. Cutoff is exact
for same-length edits, edits inside a single body (that body's chain
re-runs alone), and trailing-only changes. Making spans body-relative
(offsets from the item's start, rebased at diagnostic render) would
remove this class of false invalidation — deferred to a later
milestone.
