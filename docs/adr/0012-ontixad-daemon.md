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
