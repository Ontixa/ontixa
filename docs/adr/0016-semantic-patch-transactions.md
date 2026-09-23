# ADR-0016: Structured semantic-patch transactions

## Status

Accepted (transaction-integrity campaign)

## Context

ADR-0014 shipped the rename transaction shape and ADR-0015 hardened
its integrity — but rename was the *only* transaction: the shape
existed, the generality didn't. Agents that want to change a
function's body, delete a dead def, or add a new one had to fall
back to raw source rewrites outside the transaction machinery —
no preview, no shadow validation, no revision guard, no atomicity.

The generalization has a real risk: an "arbitrary edit" API that
accepts serialized AST nodes or raw spans would export the compiler
front-end's internals as a wire format — unversioned, unforgeable
to validate, and able to express states no `.ixa` file can hold.

## Decision

1. **A patch is a bounded list of ops, not a tree diff.** The wire
   spec is `{"ops": [...]}` (a bare array is also accepted). Each op
   is one of exactly three, all top-level-item-grained:

   - `{"op":"replace_body","symbol":"m::f","body":"{ ... }"}` —
     replaces the `fn`'s body block; the signature is untouched.
   - `{"op":"remove_def","symbol":"m::x"}` — removes a top-level
     `fn`/`data` declaration.
   - `{"op":"add_def","module":"m","text":"fn g() ..."}` — appends
     one new item to the file providing module `m` (`module` absent
     = the root file); `text` must parse as exactly one `fn`/`data`.

   A spec holds at most 64 ops and 256 KiB per payload — the
   "bounded" is load-bearing: a patch is a transaction, not a file
   transfer protocol.

2. **Targets resolve semantically, edits splice textually.** Every
   `symbol` resolves through the workspace scope exactly like a
   rename target (`E_UNKNOWN_SYMBOL`, `E_AMBIGUOUS_SYMBOL`,
   `E_UNSUPPORTED_TARGET`); every `add_def` module must be a file
   reachable through the root's `use` graph (`E_UNKNOWN_MODULE` — a
   patch never writes outside the workspace's semantic scope). The
   resolved item's *span* then produces a plain text edit — AST
   gives the coordinates, source keeps the bytes.

3. **The transaction core is shared with rename.** `plan_patch`
   runs the same pipeline: spec-shape checks (`E_MALFORMED_PATCH`)
   → clean baseline (`E_BASELINE_ERRORS`) → semantic resolution →
   overlap rejection (`E_MALFORMED_PATCH` — two ops may not cover
   the same span; same-point insertions order by spec position) →
   shadow compile on a scratch `Db` (`E_PATCH_REJECTED`, carrying
   the fresh diagnostics). `apply_patch` re-verifies the full
   provenance chain — `Db` incarnation, revision (`E_STALE_REVISION`),
   workspace fingerprint, candidate fingerprint, expected bytes
   under every edit (`E_PLAN_MISMATCH`) — then lands all files in
   one `set_sources` bump. `PatchPlan` fields are private for the
   same reason `RenamePlan`'s are.

4. **Surfaces mirror rename exactly.** CLI `ontixa patch <file>
   <spec> [--apply] [--json]` — spec is `-` (stdin), `@path`,
   inline JSON, or a file path; preview by default, `--apply`
   persists through the same staged journaled engine. Daemon op
   `patch` takes `"ops":[...]` plus `apply`/`revision` and returns
   `edits`, `revision`, `new_sources` on apply — memory-only, the
   client persists.

## Honest limits — what a patch does *not* guarantee

- **Compile integrity, not semantic preservation.** A rename proves
  every reference still binds the same def; a patch is *supposed*
  to change behavior, so there is no binding-correspondence check.
  The guarantee is: the patched workspace compiles with zero errors,
  or nothing is applied.
- **No `use` edits, no reordering, no non-item text.** Ops cannot
  add/remove imports, move items, or touch comments — a `fn` whose
  signature is followed by a comment before `{` is refused
  (`E_UNSUPPORTED_TARGET`) rather than silently dropping it.
- **No file creation or deletion.** `add_def` targets a reachable
  module's existing file.
- **Whitespace seams are normalized, not styled.** `remove_def`
  swallows one trailing newline; `add_def` terminates the file's
  last line before appending. Canonical layout stays `ontixa fmt`'s
  job.

## Alternatives considered

- **Serialized AST edits.** Unbounded expressiveness, but it turns
  `ontixa-ast`'s internal shape into a wire contract — every field
  becomes a compatibility surface, and "is this tree still
  well-formed?" is unanswerable without reimplementing the parser.
  Rejected: the op vocabulary is the contract, the AST stays
  private.
- **Arbitrary span splicing.** A `{"span":..., "text":...}` op can
  express everything and validate nothing — it is `set` with extra
  steps and no semantic anchoring. Rejected: every op must resolve
  a semantic target before it produces bytes.
- **Post-apply rollback.** Mutating the live `Db` and undoing on
  failure needs a second (untested) reverse path; shadow-compile
  keeps rejection zero-mutation by construction.

## Consequences

- The "semantic patches" roadmap item lands with an honest subset:
  three ops, item-grained, comment-preserving, file-stable.
- The persistence engine generalizes: `persist_sources` takes any
  `(file, text)` pairs — rename and patch share the journal,
  staging, and recovery protocol.
- A wider op set (signature edits, `use` management, field-level
  `data` edits) can be added per-op without changing the
  transaction protocol — each new op is new resolution logic inside
  the same plan/shadow/apply shape.
