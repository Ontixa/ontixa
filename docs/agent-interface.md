# Agent Interface

Ontixa is designed for autonomous machine programmers as first-class
users. The rule: an agent gets the same semantic access a human tool
gets — no privileged back door, and no LLM inside the compiler.

## What an agent consumes

| surface                  | command/API              | gives the agent                        |
| ------------------------ | ------------------------ | -------------------------------------- |
| Semantic Program Graph   | `ontixa graph f.ixa`     | defs, calls, types, inferred contracts |
| Diagnostics (JSON)       | `ontixa check --json`    | stable codes + spans + structured details |
| Canonical AST            | `ontixa ast f.ixa`       | owned tree, positions preserved        |
| Typed MIR                | `ontixa mir f.ixa`       | execution-facing CFG                   |
| Contracts                | `ontixa explain --json`  | per-param behavior summary             |
| Stage timings            | `--timings`              | pipeline latency budget                |

## Why this shape

- **Semantic, not textual.** Agents query `calls`/`has_param` edges
  instead of grepping. "What does `bump` do to its argument?" is a
  graph lookup, not an inference task.
- **Structured failure.** `E_USE_AFTER_MOVE` at byte 347–348 with a
  `moved here` label is an actionable error record, not prose.
- **Deterministic.** Same input → same bytes out; an agent can diff
  artifacts meaningfully.
- **No black box.** The compiler never calls a model; its outputs are
  checkable and its invariants machine-readable.

## Near-term agent workflow (milestone 1)

1. Read `graph` JSON to locate the edit site semantically.
2. Rewrite the `.ixa` source region (lossless spans make region math
   exact).
3. `check --json` → act on structured diagnostics.
4. `run` / tests → verify behavior.
5. Diff the SPG to review semantic impact — not just the text diff.

## Planned (roadmap)

- `ontixad`: the same queries over a persistent session; edit/update
  without re-reading the world.
- Semantic patches: `apply` an AST-level edit, serialized as spans +
  nodes, preserving untouched source exactly.
- Evidence-carrying diffs: a patch ships with the diagnostics it
  resolved and the contract deltas it caused.
