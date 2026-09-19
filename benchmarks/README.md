# Benchmarks

Ontixa's performance claims are measured or they are not made — see
`docs/benchmark-philosophy.md`. This directory holds the corpus; the
harness lives in `crates/ontixa-db/tests/bench_incremental.rs`.

## Running

```text
cargo test -p ontixa-db --test bench_incremental -- --ignored --nocapture
```

Prints a markdown table: cold compile vs no-op recompile vs
single-body edit, plus the number of query evaluations each triggers.
Dev profile, interpreter pipeline — these numbers say nothing about
native codegen (which does not exist yet).

## Corpus

`corpus/` holds deterministic seeds. `chain-N.ixa` is a call chain
(`f_i` calls `f_{i+1}`); `wide-N.ixa` is N independent functions.
The harness generates the same shapes programmatically, so the seeds
are for inspection and external tooling, not required by the bench.

## Measured results

Machine: Windows 11, `stable-x86_64-pc-windows-gnu`, dev profile.
Run once on this repository's hardware; numbers below are from the
Milestone-2 measurement session, not a CI gate.

| workload | cold ms | cold evals | no-op ms (med) | body-edit ms (med) | body-edit evals |
|---|---|---|---|---|---|
| chain-64  | 16.8 | 267  | 0.001 | 15.0 | 75  |
| chain-256 | 66.0 | 1035 | 0.001 | 67.0 | 267 |
| wide-64   | 13.1 | 267  | 0.001 | 12.7 | 75  |
| wide-256  | 57.1 | 1035 | 0.001 | 58.2 | 267 |

## What the numbers actually show

- **No-op recompile is free** — ~1µs of dependency verification.
- **Per-definition invalidation works** — a body-local edit re-runs
  ~1 def's `hir`/`types`/`mir` chain plus file-level queries,
  not 1035 evals.
- **Wall-clock is dominated by file-granular stages** — `lex+parse`,
  `ast`, and `graph` redo whole-file work on any edit (~50% of edit
  time at N=256), and the ~N `AstItem` verification evals add O(n)
  scans (~34µs each). This is the honest weakness: incremental
  *correctness* is proven, incremental *latency* needs finer-grained
  file-level queries (per-item parse, incremental graph) — roadmap
  work, not a claim we make today.
- Before the `Arc`-shared `Ast` value fix, `ast-item` alone cost
  142ms on chain-256 (whole-module clone per demand); the fix cut
  cold compile ~4x at N=256. Regression evidence lives in git history.
