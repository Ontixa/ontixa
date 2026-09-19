# Milestone 2 Report — Incremental Semantic Core

Status: complete. Baseline: `0025d9f`; head: `7acea6f` (+ follow-ups).
Scope note: this milestone pulled `ontixad` and fine-grained
incrementality forward from the roadmap's M3 — the semantic core
came before language breadth.

## What landed

| commit | change |
|---|---|
| `1f8e493` | Immutable bindings by default; `let mut`, `mut` params (ADR-0009) |
| `76c6042` | One schema-1 JSON envelope per `--json` command; `explain <symbol>` |
| `772dc2c` | Formal ownership domain; reverse-worklist fixpoint (ADR-0010) |
| `2eac63d` | Per-body arenas, `DefKey` identity, custom query engine (ADR-0008) |
| `cd0a0e6` | Places, loans, borrow conflicts, call-extent regions, escape summaries (ADR-0011) |
| `61ca156` | `ontixad` daemon; evidence-carrying contracts (ADR-0012) |
| `7acea6f` | Determinism gates, SPG `passes` edges, MIR `mutable`, interpreter contract oracle, benchmarks |

## Acceptance gates — measured

| gate | claim | evidence |
|---|---|---|
| A | Unchanged recheck evaluates zero queries | `unchanged_recompile_evaluates_nothing` (db) |
| B | Body-local edit re-runs only that body's chain | `body_edit_invalidates_only_that_body` — `hir`/`types`/`mir` evals = `["f"]`, oracle `1 collected / 2 reused` |
| C | Comment-only edit cuts off at the AST | `whitespace_edit_cuts_off_at_ast`, `daemon_trailing_comment_cuts_off_at_ast` |
| D | Callee contract change propagates to callers | `contract_change_propagates_to_caller` — caller MIR re-lowers though its body never changed |
| E | Ownership enforcement is structural | 50 memory tests: shared/mut conflict matrix, disjoint vs overlapping field places, move-under-loan, call-extent loan death; `E_BORROW_CONFLICT`/`E_MOVE_WHILE_BORROWED` surface in `check --json` (CLI test) |
| F | Explanations carry evidence | `daemon_explain_and_stats` + SPG param-node `evidence`/`escapes` attrs; ambiguity is `E_AMBIGUOUS_SYMBOL`, not a guess |
| G | Daemon is persistent + robust | NDJSON, one envelope per request, `evaluated` keys per response; malformed input → error envelopes, never a crash (CLI tests) |
| H | Runtime validates contracts | Contract-tampering tests: a `borrow_mut`→`borrow` lie traps on callee write (incl. nested fields); second move from a consumed cell traps; post-move reads trap |
| I | Output is byte-deterministic | `artifacts_are_byte_deterministic_across_sessions`, `json_output_is_byte_deterministic` (separate processes), CI gate diffs `graph`/`mir --json` across runs |

## Benchmarks

Harness: `crates/ontixa-db/tests/bench_incremental.rs`
(`--ignored --nocapture`). Full numbers + interpretation:
`benchmarks/README.md`. Headline (dev profile, chain-256):

- cold compile ~66ms / 1035 query evals
- no-op recheck ~1µs / 0 evals
- single-body edit ~67ms wall / 267 evals — per-def deep work is
  isolated (hir/types/mir ≈ 0.1ms), but file-granular stages
  (`lex+parse`, `ast`, `graph`) dominate latency. Honest weakness;
  finer-grained file-level queries are roadmap work.
- fixed: `Value::Ast` was cloned per `AstItem` demand (O(n²)); now
  `Arc`'d — `ast-item` stage 142ms → 8.7ms at N=256.

## Known limitations (carried forward)

- File-absolute spans: length-changing edits shift following items'
  spans and re-run their chains. Body-relative spans are future work.
- `Ownership`/`Graph`/`Diagnostics` are file-granular queries — any
  edit re-runs them (cheap relative to per-body work, but real).
- `AstItem` verification is an O(items) scan per def.
- The interpreter oracle checks contracts at call boundaries only;
  it is defense-in-depth, not a memory-safety mechanism (the static
  pass is).
- Deep recursion traps by Rust stack overflow — documented limit.
