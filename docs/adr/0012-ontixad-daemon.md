# ADR-0012: `ontixad` — the persistent compile daemon

## Status

Accepted (Milestone 2, Phases 21–25)

## Context

The query engine (`ADR-0008`) only pays off when its state survives
between requests — a CLI invocation throws away the memo table, the
interner, and the ownership oracle at process exit. Agents iterating
on a file (`edit → check → explain → edit`) need the session object.

At the same time, `explain` answered *what* a contract is (`borrow`,
`move`) but not *why* — there was no machine-readable evidence for
the flags that produced it.

## Decision

1. **`ontixad` binary** (`src/bin/ontixad.rs` in `ontixa-cli`,
   sharing `envelope`/`explain` through a new `ontixa_cli` lib
   target). NDJSON protocol on stdio: one request per line, one
   compact schema-1 envelope per line. No sockets, no auth, no
   locking — a child process is the deployment unit; transport is a
   separate concern.

2. **Ops**: `open` / `set` (path + text — the edit op) / `check` /
   `explain` (with `symbol`) / `stats` / `close` / `shutdown`.
   `set` auto-binds unknown paths, so "edit" never needs a separate
   open handshake.

3. **Observable incrementality is the contract.** Every `check` and
   `explain` response carries `result.evaluated` — the query keys
   that actually ran (`HirBody(DefKey(0:3))`-style). `stats` reports
   cumulative `QueryStats` plus oracle `facts_collected` /
   `facts_reused` / `rounds`. A recheck on unmodified source reports
   `evaluated: []` — verified, not assumed.

4. **Evidence-carrying contracts.** `ParamSummary` replaces the bare
   escapes map in `OwnershipTables`: per param, `escapes` (exit
   kinds) plus `evidence` — `(EvidenceKind, Span)` sites that set the
   `read`/`mutated`/`moved`/`escaped` flags. `explain` emits them;
   agents can point at the exact use that made a parameter
   `borrow_mut`. Evidence rides the same memoized fact entries —
   incrementality for free.

5. **`AstModule.span` covers items only**, not the whole root text
   range — a trailing comment at EOF no longer widens the module and
   re-runs `Scope`/`AstItem` for every def. (Found by a daemon test;
   trivia-only edits now cut off at the AST.)

## Rejected

- **JSON-RPC over TCP.** Premature: no remote consumers, and stdio
  framing is trivially testable (`spawn` + pipe). A TCP wrapper can
  wrap `ontixad` unchanged.
- **Watch mode / filesystem notifications.** The client sends `set`
  with the full text — the daemon never races the filesystem.
- **salsa** (revisited): the custom engine's `Db` is already the
  session object; the daemon adds no new consistency requirements.

## Consequences

- `cargo run --bin ontixad` speaks NDJSON; `ontixa` CLI unchanged.
- 4 integration tests exercise the loop: cold check → recheck
  `evaluated: []` → body-local edit re-runs one chain → trailing
  comment cuts off at `Ast` → malformed lines get envelopes, not
  crashes.
- `ParamSummary` is the explanation surface ADR-0011 promised; loans
  and regions can extend it without schema churn.

## Addendum: isolated inline contract comparison

Agents and editors need to compare two unsaved buffer sets without
creating temporary source directories or replacing the sources in their
live session. `diff` accepts both complete workspaces in one NDJSON
request: `{"op":"diff","semantic":true,"before":{"entry":"main",
"sources":[{"module":"main","text":"..."}]},"after":{...}}`.

The operation deliberately uses two fresh, isolated `Db` instances.
It does not read or write source files, access the persistent
`Session`, or change its revision, caches or counters. This is not an
incremental comparison. Comparison coverage and existing command schemas
remain the same. The accompanying parser/lexer file-tag correction fixes
dependency diagnostic locations in existing commands too; see
[diagnostics](../diagnostics.md). There is no execution or patch/apply
integration.

Each side supplies unique ASCII-identifier module names and must include
its entry module. Entry names must match across sides. Register the entry
first, then other modules in name order, and compare only definitions
reachable from the entry through `use`. Synthetic labels such as
`before/main.ixa` identify diagnostics without implying filesystem
access. Input provenance carries `source_kind: "inline"`, and coverage
describes inline loading explicitly.

Reuse the CLI's `command: "diff"` schema-1 envelope and
`comparison_schema: 1` payload: the same qualified identities, selected
fields, uncertainty disclosure and limits apply. Differences are
successful results. Reject malformed requests, unknown fields,
missing/false `semantic`, duplicate or invalid modules and missing entry
sources as `invalid_comparison_request` with `result: null`. Mismatched
roots or compiler source errors use `comparison_unavailable`, retaining
per-input statuses and side-tagged diagnostics. Each failure is one
response; subsequent requests continue to work.

See [the comparison contract](../semantic-diff.md#daemon-inline-inputs)
and [`inline.ndjson`](../../examples/semantic-diff/inline.ndjson).
