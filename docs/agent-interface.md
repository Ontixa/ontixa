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

## `ontixad` (milestone 2)

A persistent compile daemon over the query engine — the same
commands without re-reading the world. NDJSON on stdio: one request
per line, one schema-1 envelope per line.

```json
{"op":"open",    "path":"x.ixa"}                  → {"file": N}
{"op":"set",     "path":"x.ixa", "text":"..."}    → the edit op (auto-opens)
{"op":"check",   "path":"x.ixa"}                  → check envelope
{"op":"explain", "path":"x.ixa", "symbol":"s"}    → explain envelope
{"op":"stats"}                                   → counters + last_evaluated
{"op":"close",   "path":"x.ixa"}
{"op":"shutdown"}
```

`check`/`explain` results carry `evaluated`: the query keys that
actually ran this demand — e.g. `HirBody(DefKey(0:3))` after a
body-local edit, `[]` on an unmodified recheck. `stats` adds
cumulative `QueryStats` and oracle fact-reuse counters.

Parameter contracts now carry `evidence` — the `(kind, span)` sites
that produced each flag — and `escapes` (`return`, `call \`f\``), so
an agent sees *why* `p` is `borrow_mut`, not just that it is.

The SPG exposes ownership transfer structurally: every call argument
has a `passes` edge from the argument expression to the callee's
parameter node, carrying `position` and `behavior`. `borrow`/
`borrow_mut` edges mean shared storage for the call's extent;
`move`/`escape` mean ownership transfers. Ownership flow is a graph
query, not an inference task.

## Planned (roadmap)

- Semantic patches: `apply` an AST-level edit, serialized as spans +
  nodes, preserving untouched source exactly.
- Evidence-carrying diffs: a patch ships with the diagnostics it
  resolved and the contract deltas it caused.
