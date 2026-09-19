# Architecture

## The pipeline

```text
Source text (.ixa)
  │
  ▼  ontixa-syntax — lexer + recursive-descent parser
Lossless CST (rowan green tree)
  │
  ▼  ontixa-ast — canonical owned AST
AstModule
  │
  ▼  ontixa-hir — name resolution, scopes, interning
HirModule (ModuleScope + bodies, ExprId arena)
  │
  ▼  ontixa-types — total type analysis, fills field indices
TypeTables
  │
  ▼  ontixa-memory — ownership/borrow/move/escape inference
OwnershipTables (per-function param contracts)
  │
  ├─▶ ontixa-semantic — Semantic Program Graph (JSON)
  │
  ▼  ontixa-mir — typed CFG, temporaries, terminators
MirModule
  │
  ▼  ontixa-interpreter — reference executor
Value
```

`ontixa-db` wraps the whole pipeline in a memoized `Db`; `ontixa-cli`
exposes it as the `ontixa` binary.

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
structures carry `u32`s, not strings.

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

### The database is file-granular (for now)

`Db` memoizes per-file artifact bundles keyed by content revision.
The salsa-style fine-grained design — per-query early cutoff — is
ADR-0004; milestone 1 needs the *shape*, not the sophistication.
