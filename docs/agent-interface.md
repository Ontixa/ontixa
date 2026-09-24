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
{"op":"patch",   "path":"x.ixa", "ops":[{...}],   → preview or apply;
                 "apply":true, "revision":R}          same guards as rename
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
render against their own `SourceFile`. `explain` carries the same
attribution: every entry in `result.defs`, every `symbol` record, and
every `details.candidates` entry of `E_AMBIGUOUS_SYMBOL` names the
`file` it was resolved in (the same display tag diagnostics use),
and human output groups the def listing per file.
`DefKey(root:file:name)` is the cross-revision symbol identity an
agent should hold.

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

## Structured semantic patches (ADR-0016)

`patch` is the rename transaction generalized to a bounded op
vocabulary — same pipeline (plan → preview → shadow compile →
revision-guarded atomic apply), same envelopes, same persistence.

```json
{"ops": [
  {"op": "replace_body", "symbol": "m::f", "body": "{ return x + 1; }"},
  {"op": "remove_def",   "symbol": "m::dead"},
  {"op": "add_def",      "module": "m",
   "text": "fn g() -> i32 { return 1; }"},
  {"op": "rename_param", "symbol": "m::f", "param": "x", "to": "acc"},
  {"op": "set_param_type", "symbol": "m::f", "param": "x", "ty": "i64"},
  {"op": "set_ret_type", "symbol": "m::f", "ty": "i32"},
  {"op": "add_use",    "module": "m", "path": "dep::T", "as": "T2"},
  {"op": "remove_use", "module": "m", "path": "dep"},
  {"op": "add_field",  "symbol": "m::D", "field": "c", "ty": "i32"},
  {"op": "remove_field", "symbol": "m::D", "field": "dead"},
  {"op": "rename_field", "symbol": "m::D", "field": "a", "to": "b"},
  {"op": "set_field_type", "symbol": "m::D", "field": "a", "ty": "i64"}
]}
```

- CLI: `ontixa patch <file> <spec> [--apply] [--json]` — `<spec>` is
  `-` (stdin), `@path`, inline JSON, or a spec file path. Preview by
  default; `--apply` writes through the same staged journaled
  transaction engine rename uses.
- Daemon: `{"op":"patch","path":...,"ops":[...]}` previews;
  `"apply":true,"revision":R` commits in memory and returns
  `new_sources` for the client to persist.

Op semantics — every `symbol` resolves through the workspace scope
exactly like a rename target:

- **Item ops** (ADR-0016): `replace_body` splices a `fn`'s `{ ... }`
  block (signature untouched), `remove_def` deletes a top-level
  `fn`/`data`, `add_def` appends one item to a reachable module's
  file.
- **Signature ops**: `rename_param` rewrites a parameter's decl
  token plus every body-local site bound to it (`Var` refs and
  assign bases — the same site rule as positional rename) and
  refuses a `to` that is already bound in that body
  (`E_NAME_CONFLICT`); `set_param_type` replaces the parameter's
  declared type; `set_ret_type` adds, replaces, or removes the
  `-> T` annotation (`"ty": null` is the remove form). Name and
  body are preserved; callers and bodies that no longer type-check
  fail the shadow compile.
- **`use` ops**: `add_use` splices `use path [as alias];` onto the
  line after the file's last `use` (or above the first item, below
  a leading comment banner); `remove_use` deletes the declaration
  whose path **and** alias match — omitting `as` matches only an
  unaliased decl. `path` is `m` or `m::x` and never carries `as`.
  `add_use` can pull a registered-but-unreachable file into scope,
  exactly like writing the `use` by hand.
- **Field ops**: `add_field` appends `field: ty;` after the last
  field, copying the file's separator convention (it never invents
  a literal value — construct sites needing the field must be fixed
  by other ops in the same patch); `remove_field` deletes the field
  and its leading-trivia seam (a comment in that seam refuses the
  op); `rename_field` rewrites the decl plus every resolved
  `base.field` access, `D { field: v }` literal name, and
  `place.field = v` projection in the workspace; `set_field_type`
  replaces the declared type.

Rejection codes, in pipeline order: `E_MALFORMED_PATCH` (spec shape,
payload bounds, overlapping edits), `E_BASELINE_ERRORS`,
`E_UNKNOWN_SYMBOL` / `E_AMBIGUOUS_SYMBOL` / `E_UNKNOWN_MODULE` /
`E_UNKNOWN_FIELD` / `E_UNSUPPORTED_TARGET` / `E_NAME_CONFLICT`
(resolution — a comment between signature and `{`, inside `-> T`,
or in a removed field's seam also refuses), `E_PATCH_REJECTED`
(shadow compile surfaced new errors, carrying them),
`E_STALE_REVISION` and `E_PLAN_MISMATCH` at apply. Every rejection
writes nothing.

Honest limits: a patch guarantees *compile integrity*, not semantic
preservation — there is no binding-correspondence check (the patch
is meant to change behavior). Ops cannot reorder items, edit
comments, or create/delete files; `use` edits are whole-declaration
splices, not arbitrary header edits; layout seams are normalized
minimally and `ontixa fmt` owns canonical form.

## Planned (roadmap)

- Evidence-carrying diffs: a patch ships with the diagnostics it
  resolved and the contract deltas it caused.
