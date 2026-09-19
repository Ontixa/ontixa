# Milestone 2 Baseline Audit

Recorded before any milestone-2 change. All numbers measured on
`D:\Github\ontixa`, Windows host, GNU Rust toolchain 1.96.0
(`stable-x86_64-pc-windows-gnu` — the MSVC toolchain resolves `link`
to coreutils `link`, so GNU is forced via `RUSTUP_TOOLCHAIN`).

## HEAD

```text
0025d9f compiler: milestone-1 vertical slice — .ixa source to executed MIR
```

Working tree clean. Parent: `a8476eb` (initial commit, `.gitattributes` only).

## Test state

```text
cargo test --workspace     89 tests passed, 0 failed
cargo clippy --workspace -- -D warnings   clean
cargo fmt --all -- --check                clean
```

Per-crate: ast 7, db 4, hir 10, interpreter 11, memory 19, mir 3,
semantic 4, source 5, syntax 14, types 12.

## CI state

`.github/workflows/ci.yml` — Linux-only at baseline. `check` job:
fmt, build, test, clippy. `cli-gates` job: run examples, move-error
rejection, E_USE_AFTER_MOVE JSON, graph behavior strings.

## CLI surface

```text
check <file> [--json] [--timings]
run <file> [--entry name] [--json] [--timings]
tokens <file> [--json]
ast <file>          (always JSON)
mir <file>          (always JSON)
graph <file>        (always JSON)
explain <file> [--json]
```

Exit codes: 0 ok / 1 source errors / 2 runtime trap or missing entry /
3 ICE via catch_unwind.

## Verified acceptance behavior

```text
ontixa run examples/hello.ixa              -> 42
ontixa run examples/structs.ixa            -> 25
ontixa run examples/branches.ixa           -> 42
ontixa run examples/borrow-inference.ixa   -> 14
ontixa check examples/move-error.ixa       -> E_USE_AFTER_MOVE, exit 1
ontixa graph  -> has_param edges carry behavior attrs
ontixa explain -> per-param contracts listed
```

## Baseline timings (debug build, borrow-inference.ixa, 7 defs)

```text
lex+parse  ~1.1–1.5 ms
ast        ~0.6–1.0 ms
hir        ~0.2–0.4 ms
types      ~0.1–0.2 ms
ownership  ~0.2–0.3 ms
graph      ~0.4–0.9 ms
mir        ~0.1–0.2 ms
```

(structs.ixa is ~2x faster across the board; timings are cold-build,
single-process, unscientific — recorded for order of magnitude.)

## Baseline invalidation model

`ontixa-db`: one `FileEntry { text, revision, cached: Artifacts }` per
file. `set_source` bumps `revision` and drops the entire artifact
bundle. `compile(file)` rebuilds every stage when
`built_revision != revision`. There is no intra-file granularity: an
edit anywhere re-runs parse → mir for the whole file. No query
dependency graph, no early cutoff, no per-definition caching.

## Known limitations (verified, not assumed)

1. **No mutability enforcement.** `let p = P { x: 1 }; p.x = 2;` and
   `let x = 1; x = 2;` both compile and execute — the assignment
   writes through the cell. `mut` does not exist as syntax.
2. **BorrowMut needs no authority.** `bump(p)` where `bump` mutates
   `p` accepts an immutable `p`; the interpreter mutates it in place.
3. **MAX_ROUNDS = 16 fixpoint.** Contract propagation is a
   fixed-iteration loop; >16-deep propagation chains degrade to
   `Unknown` (verification: `f32 -> ... -> f0` chains beyond 16
   hops lose contract precision).
4. **Whole-file rebuild.** Any edit — including a comment —
   re-executes all seven stages.
5. **`--json` emits multiple documents.** `check --json --timings`
   prints the diagnostics document then the timings array as a second
   top-level JSON value. `run --json --timings` similarly.
6. **`explain` cannot target a symbol.** It lists every def.
7. **`tokens` output includes trivia rows** (documented behavior, but
   JSON has no schema envelope).
8. **`ast`/`mir`/`graph` emit diagnostics JSON then artifact JSON** —
   two concatenated documents when diagnostics exist.
9. **Single-file Db.** No multi-file workspace model.
10. **IDs are dense per-compile indices.** `DefId`, `SymbolId`,
    `ExprId` are vector positions — stable within one compile, not
    stable across edits.
11. **No loans/regions.** `Borrow`/`BorrowMut` are summary
    classifications, not tracked place loans; there is no borrow
    conflict checking, no last-use extent, no region inference.
12. **`&&`/`||` are eager** (MIR lowers both operands as binary ops).
13. **Non-Copy field moves move the whole binding** — no partial
    moves.
14. **Interpreter uses the Rust stack** — deep recursion can overflow.
15. **Escape is a flag, not a destination** — no distinction between
    return vs aggregate vs caller-owned escape paths.

## Architecture inventory

12 crates: source, diagnostics, syntax, ast, hir, types, memory,
semantic, mir, db, interpreter, cli. ADRs 0001–0007 in `docs/adr/`.
Examples: 5 `.ixa` files. No benchmarks harness yet
(`benchmarks/` is empty).
