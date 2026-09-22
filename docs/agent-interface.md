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
{"op":"rename",  "path":"x.ixa", "symbol":"m::s", "to":"new",
                 "apply":true, "revision":R}      → preview or apply
{"op":"fmt",     "path":"x.ixa"}                  → fmt envelope: canonical
                                                  text, no mutation
{"op":"stats"}                                   → counters + last_evaluated
{"op":"close",   "path":"x.ixa"}
{"op":"shutdown"}
```

`check`/`explain` results carry `evaluated`: the query keys that
actually ran this demand — e.g. `HirBody(DefKey(0:1:4))` after a
body-local edit, `[]` on an unmodified recheck. `stats` adds
cumulative `QueryStats` and oracle fact-reuse counters.

`fmt` returns `result.formatted` — the bound source in canonical
layout — plus `changed`. It is syntactic and per-file (no workspace
load, no evaluated keys), it never mutates the session — installing
the result is an explicit `set` — and a file with parse errors
surfaces them as diagnostics instead of a rewrite.

Parameter contracts now carry `evidence` — the `(kind, span)` sites
that produced each flag — and `escapes` (`return`, `call \`f\``), so
an agent sees *why* `p` is `borrow_mut`, not just that it is.

The SPG exposes ownership transfer structurally: every call argument
has a `passes` edge from the argument expression to the callee's
parameter node, carrying `position` and `behavior`. `borrow`/
`borrow_mut` edges mean shared storage for the call's extent;
`move`/`escape` mean ownership transfers. Ownership flow is a graph
query, not an inference task.

## Workspaces + rename transactions (semantic-workspace)

Files provide modules named by their stem; `use m;` / `use m::x [as
y]` / `m::x` resolve through the workspace's per-file envs — never
text matching. `check`/`explain`/`graph`/`run` on a root file follow
`use` edges across `.ixa` siblings; diagnostics are file-tagged and
render against their own `SourceFile`. `DefKey(root:file:name)` is
the cross-revision symbol identity an agent should hold.

Renames are transactions, not edits (ADR-0014, ADR-0015):

1. `ontixa rename main.ixa math::double twice` (or daemon `rename`
   without `apply`) returns the **validated plan**: exact per-file
   spans, post-edit sources, and the `revision` it was planned
   against — the live `Db` is untouched. The plan is bound to the
   `Db` incarnation, workspace fingerprint, and candidate payload —
   it cannot cross sessions or carry swapped bytes (`E_PLAN_MISMATCH`).
2. Target selection: `symbol` names a top-level def (`x` or
   `m::x`); daemon `"at":<byte-offset>` or CLI `@<byte-offset>`
   selects a body-local binding or parameter positionally through
   the function's HIR — shadowing resolves as name resolution does.
   `E_UNSUPPORTED_TARGET` covers non-renameable selections.
3. Validation happens before any apply: `E_BASELINE_ERRORS` (rename
   needs an error-free workspace; warnings don't block),
   `E_INVALID_NAME`, `E_UNKNOWN_SYMBOL`/`E_AMBIGUOUS_SYMBOL`,
   `E_NAME_CONFLICT` (file-tagged), and a shadow compile that
   rejects `E_RENAME_REJECTED` on any new error **or any binding
   drift** — a reference that would rebind (capture) is refused even
   when the candidate compiles.
4. `apply` (CLI `--apply` / daemon `"apply":true,"revision":R`)
   commits only if the workspace revision still matches —
   `E_STALE_REVISION` otherwise, with zero mutation. On success all
   files land in one `set_sources` revision bump — no partial state.
5. **Disk persistence** (CLI `--apply` only; the daemon only
   mutates memory and returns `new_sources`): candidates are staged
   as sibling `.ontixa-tx-*.stage` files behind a
   `.ontixa-tx-*.journal`, then swapped in per file. Disk bytes must
   equal the validated snapshot or nothing writes; a pending journal
   blocks new transactions; `ontixa recover <dir>` resolves a dead
   transaction forward or back, preserving evidence on conflict.

Alias semantics: `use m::x` rebinds, so bare `x` refs rewrite with
the member segment; `use m::x as y` keeps `y` — only the member
segment rewrites.

## Planned (roadmap)

- Semantic patches: `apply` an AST-level edit, serialized as spans +
  nodes, preserving untouched source exactly — the rename
  transaction is the first instance of this shape.
- Evidence-carrying diffs: a patch ships with the diagnostics it
  resolved and the contract deltas it caused.
