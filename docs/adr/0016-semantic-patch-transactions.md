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
  move items or touch comments — a `fn` whose signature is followed
  by a comment before `{` is refused (`E_UNSUPPORTED_TARGET`)
  rather than silently dropping it. *(The `use` half of this limit
  was lifted by the addendum below: whole-declaration `use` splices
  arrived with the wider op vocabulary.)*
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

## Addendum — the wider op vocabulary (signature/`use`/field ops)

The protocol is unchanged: same bounded spec, same pipeline order,
same provenance and stale guards, same compile-integrity guarantee.
What widened is the op vocabulary — nine more kinds, each resolved
through the same machinery:

- **Signature** (`fn` targets only):
  - `{"op":"rename_param","symbol":"m::f","param":"x","to":"acc"}` —
    rewrites the parameter's decl token plus every body-local site
    bound to it (the `Var`/assign-base site rule of positional
    rename); a `to` already bound in that body is
    `E_NAME_CONFLICT`. The fn's name and body block are preserved.
  - `{"op":"set_param_type","symbol":"m::f","param":"x","ty":"i64"}`
    — replaces the declared type in place.
  - `{"op":"set_ret_type","symbol":"m::f","ty":"i32"}` — adds,
    replaces, or removes `-> T`; `"ty":null` is the deliberate
    remove form (an absent `ty` is `E_MALFORMED_PATCH`).
  - Retyped signatures that break the body or any caller fail the
    shadow compile — integer literals adopt expected types inward,
    so `-> i64` on `return 5` checks.
- **`use`** (file targets — `module`, absent = root):
  - `{"op":"add_use","module":"m","path":"dep::T","as":"T2"}` —
    splices one `use` decl after the file's last `use`, or above
    the first item below a leading comment banner; `as` is its own
    field — `path` never carries an alias. Adding a `use` can pull
    a registered-but-unreachable file into scope.
  - `{"op":"remove_use","module":"m","path":"dep","as":"x"}` —
    removes the decl whose path and alias match exactly; omitting
    `as` matches only unaliased decls. References losing their
    binding fail the shadow compile.
- **`data` fields** (`data` targets only):
  - `add_field` appends `field: ty;` after the last field, copying
    the file's separator convention (into `data D {}` it inserts
    after the `{`). It never invents literal values — struct
    literals missing the new field fail the shadow compile and must
    be fixed by a sibling op in the same patch.
  - `remove_field` deletes the field's whole entry span *including
    its leading trivia run* — a comment in that seam is
    `E_UNSUPPORTED_TARGET`, since the comment's attachment is
    ambiguous and the splice would silently drop it.
  - `rename_field` rewrites the decl token plus every site the
    checker resolved to that field: `base.field` accesses (matched
    by resolved index on a `Struct` base), `D { field: v }` literal
    names, and `place.field = v` projections — in every reachable
    file.
  - `set_field_type` replaces the field's declared type.

New resolution failures reuse existing codes: `E_UNKNOWN_FIELD`
for a field the `data` lacks, `E_UNKNOWN_SYMBOL` for a `param` or
`use` decl that does not resolve, `E_NAME_CONFLICT` for
`rename_param`'s `to`. Type payloads parse through a synthetic
`fn f(p: <ty>) {}` probe and `path` payloads through a `use`
probe — a payload can never smuggle trailing text or a second
item into the splice.

Standing boundary, unchanged: ops still cannot reorder items,
edit comments, create/delete files, or write outside the `use`
graph; `use` edits are whole-declaration splices, not arbitrary
header text. The 64-op / 256 KiB bounds and every apply-time
guard cover the new vocabulary identically.
