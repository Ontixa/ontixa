# ADR-0014: Semantic rename transactions

## Status

Accepted (semantic-workspace campaign, PR #5)

## Context

ADR-0013 gave an agent multi-module semantic understanding. The next
capability is *changing names correctly*: rename `math::double` to
`twice` and have every reference — declaration, `use` path, qualified
call, type position, struct literal, bare ref through an unaliased
import — rewritten exactly, with proof nothing else moved and a hard
refusal when the workspace can no longer take the edit.

Text matching can't do this (`double` appears in comments, strings,
and other modules' unrelated defs), and a naive "find + replace + recompile" gives the agent no preview, no atomicity, and no stale
guard between "plan" and "apply".

## Decision

1. **Rename resolves through the compiler, never through text.**
   `plan_rename(root, symbol, new)` resolves the target by env (the
   same `path_def` machinery bodies use), then scans every reachable
   file's AST for sites that *resolve to that `DefId`* — decl names,
   `use` member segments, path-expression final segments, call and
   struct-literal type paths, and bare refs whose binding is an
   unaliased member import of the target. Only the final path
   segment is rewritten: `math::double` → `math::twice`, qualifiers
   untouched.

2. **Alias semantics mirror the resolver.** `use m::x` binds `x` in
   the local env, so bare `x` refs rewrite with the member segment.
   `use m::x as y` binds `y` — the member segment `x` rewrites (the
   import now reads `use m::new as y`) but `y` and its refs stay.

3. **Validation is layered, cheapest first.** Identifier check
   (`E_INVALID_NAME`) → ambiguity/unknown (`E_UNKNOWN_SYMBOL`,
   `E_AMBIGUOUS_SYMBOL`) → binding conflicts in the declaring file
   and in files whose unaliased import would rebind the new name
   (`E_NAME_CONFLICT`, file-tagged) → **shadow compile**: apply the
   edits to a scratch `Db`, re-check, and reject if any new error
   appears that the baseline didn't have (`E_RENAME_REJECTED`,
   carrying the fresh diagnostics).

4. **`plan_rename` never mutates the live `Db`.** The plan carries
   edits, post-edit sources, the planned revision, and diagnostics —
   pure data the CLI renders and the daemon returns as `edits` +
   `new_sources` + `revision`.

5. **`apply_rename` is a guarded atomic commit.** It compares the
   plan's revision to `Db::revision` — a mismatch means the world
   moved since planning (`E_STALE_REVISION`, no mutation). On match
   it installs all edited sources through one `set_sources` call —
   one revision bump, all-or-nothing — then re-checks and returns
   affected files, edit count, diagnostics, and the new revision.

6. **Surfaces:** CLI `ontixa rename <file> <symbol> <new> [--apply]
   [--json]` (preview by default; `--apply` writes via staged
   backups that roll back on mid-loop IO failure — disk never
   half-renames); daemon `rename` op returning `edits`,
   `new_sources`, `revision`, and on apply `applied` + post-check
   diagnostics. Agents plan on one revision and must re-plan if the
   guard fires.

## Alternatives considered

- **Text rewrite + recompile.** Greps hit comments/strings/wrong
  modules and can't see import bindings — rejected; every rewrite
  site here is resolution-verified.
- **Mutate-then-validate.** Checking after mutation forces a
  rollback path on failure; shadow compile gives rejection with the
  live `Db` provably untouched.
- **Per-file apply loop in the daemon.** Two files, one revision —
  a per-file write would leave the workspace mid-rename visible to
  interleaved demands. `set_sources` exists precisely to make the
  commit atomic.

## Consequences

- Renames are *transactions*: plan → inspect → apply-if-still-valid,
  with structured refusals at every gate — the interface ADR-0012's
  "semantic patches" roadmap item converges on.
- `HirExprKind::Call` keys callees by `DefId`, so a rename preserves
  every caller's semantic identity — untouched defs keep their
  per-def query values across the apply (stamp-equality evidence in
  `crates/ontixa-db/tests/rename.rs`).
- The same pattern generalizes to future semantic patches: resolve
  sites semantically → shadow-check → revision-guarded atomic apply.
