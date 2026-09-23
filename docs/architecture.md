# Architecture

## The pipeline

```text
Source text (.ixa files — one workspace, file stems are module names)
  │
  ▼  ontixa-syntax — lexer + recursive-descent parser
Lossless CST (rowan green tree)
  │
  ▼  ontixa-ast — canonical owned AST
AstModule (items + use decls + paths)
  │
  ▼  ontixa-hir — workspace name resolution, per-file envs, interning
HirModule (ModuleScope: FileEnv per file + bodies, ExprId arenas)
  │
  ▼  ontixa-types — total type analysis, fills field indices
TypeTables
  │
  ▼  ontixa-memory — ownership/borrow/move/escape inference
OwnershipTables (per-function param contracts, DefKey-keyed facts)
  │
  ├─▶ ontixa-semantic — Semantic Program Graph (JSON, per-file modules)
  │
  ▼  ontixa-mir — typed CFG, temporaries, terminators
MirModule
  │
  ▼  ontixa-interpreter — reference executor
Value
```

`ontixa-db` wraps the whole pipeline in a memoized `Db` — the query
engine (ADR-0008) plus the rename transaction engine (ADR-0014).
`ontixa-cli` exposes it as the `ontixa` binary and the `ontixad`
daemon (ADR-0012).

## Design choices and why

### Lossless CST before AST

The rowan green tree keeps every byte — whitespace, comments, precise
ranges. Formatting, IDE features, and *semantic patches* (rewrites
that preserve untouched source exactly) all need that fidelity.
Lowering to a canonical AST gives later stages a clean owned tree.

### HIR resolves names, never strings

Every binding in HIR is a `SymbolId`; every definition a `DefId`.
Unresolved names become *poison* nodes so later passes never branch
on "did resolution fail". Text is interned (`InternId`) — semantic
structures carry `u32`s, not strings. Resolution is workspace-shaped:
`use` decls and `m::x` paths resolve through each file's `FileEnv`
(members, bound modules, imports) built by `resolve_workspace`
(ADR-0013).

### Total type checking

`check_module` assigns a type to *every* expression, using
`Ty::Poison` where upstream errors occurred. Nothing downstream ever
asks "was this checked?" — poison propagates silently and the
diagnostics already carry the error.

### Ownership inference is a fixpoint

Parameter contracts are inter-procedural: `relay` passing `p` to
`eat` inherits `eat`'s `move` contract. The analysis iterates
contract facts to a fixpoint over the call graph, then a second
enforcement walk emits `E_USE_AFTER_MOVE` / `E_UNINITIALIZED` with
state merged across `if` branches.

### MIR is the analysis/execution boundary

HIR keeps lexical structure; MIR flattens to basic blocks with
explicit temporaries and `Return`/`Branch`/`Goto` terminators.
Call rvalues carry the callee's inferred contract, so the executor
shares caller storage for `borrow`/`borrow_mut` arguments and copies
for consuming ones — borrow semantics are *executed*, not annotated.

### The interpreter is the oracle

`ontixa-interpreter` defines what generated code must compute.
Cell-based storage (`Rc<RefCell<Value>>`) makes borrow/mut sharing
real. When a backend arrives, its differential tests run against
this executor.

### The database is per-definition incremental

`Db` is a salsa-style query engine: `Parse`/`Ast`/`Scope` are
file/workspace-shaped inputs; `AstItem`/`HirBody`/`BodyTypes`/
`MirBody` are per-`DefKey` derived values with early cutoff — a
recomputed value comparing equal stops invalidation from
propagating (ADR-0004/0008). Spans in derived values are
item-relative, so offset-shifting edits preserve semantic identity
(see `docs/semantic-identity.md`). `Db::set_sources` is the atomic
multi-file write source transactions commit through.

### Source edits are transactions

`plan_rename` resolves sites semantically (never text), validates,
and shadow-compiles the edited sources in a scratch `Db` — pure, no
live mutation. `apply_rename` re-checks the planned revision
(`E_STALE_REVISION` on drift) and commits every file in one
revision bump (ADR-0014/0015). `plan_patch`/`apply_patch` run the
same pipeline over a bounded op vocabulary — `replace_body`,
`remove_def`, `add_def` (ADR-0016).
