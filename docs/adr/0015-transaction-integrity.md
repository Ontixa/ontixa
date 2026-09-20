# ADR-0015: Rename transaction integrity — provenance, binding correspondence, staged persistence

## Status

Accepted (transaction-integrity campaign)

## Context

ADR-0014 shipped the rename *shape* — plan → validate → guarded
apply — but the guarantees didn't match the implementation:

- `persist()` wrote live files with `std::fs::write` in a loop; a
  mid-loop failure restored only the files *before* the failed one,
  never the one it had just truncated, and swallowed rollback errors.
- Nothing compared disk bytes to the validated snapshot — an
  external edit between plan and persist was silently overwritten.
- Candidate validation compared error *multisets* by `contains_key`
  — a second error of the same `(code, file)` passed undetected.
- `RenamePlan` fields were public and `apply_rename` checked only
  the revision: a plan crossed databases freely, a tampered
  `new_sources` payload applied as if validated, a forged file index
  panicked mid-commit leaving a partial revision.
- Rename resolved top-level defs only — no locals/parameters, and no
  check that references still bind the same declaration (a rename
  that *captures* a reference compiled cleanly and applied).

## Decision

1. **Clean-baseline policy.** Rename requires an error-free
   workspace (`E_BASELINE_ERRORS`); warnings do not block. The
   shadow compile then treats *any* candidate error as new — no
   diagnostic multiset matching anywhere in the validation path.

2. **Plans carry provenance.** `RenamePlan` fields are private; the
   plan records the `Db` incarnation id, plan-time revision, a
   workspace fingerprint (FNV-1a-64 over module names + raw source
   bytes — no CRLF/Unicode normalization, no mtimes), a fingerprint
   of the candidate payload, and the expected bytes under every
   edit. `apply_rename` re-verifies all of it, in order, before
   `set_sources` — a plan can never cross sessions, snapshots, or
   carry a swapped payload (`E_PLAN_MISMATCH`).

3. **Binding correspondence, not just compile-success.** Validation
   diffs the semantic binding sequence (variable sites, assignment
   bases, `let` decls, calls, struct literals) between the baseline
   and the shadow-compiled candidate. A candidate that still
   compiles but rebinds a site — capture, drift — is rejected
   (`E_RENAME_REJECTED`).

4. **Local/parameter rename by position.** `plan_rename_at(root,
   file, offset, new)` selects a body-local binding or parameter by
   byte offset and finds its sites through the function's own HIR —
   never a name guess — so shadowing resolves exactly as name
   resolution does (`RenameTarget::Local { def, sym }`). Selecting a
   function's own name is `E_UNSUPPORTED_TARGET`; data fields and
   other kinds are rejected rather than text-replaced. CLI:
   `rename <file> @<byte-offset> <new>`; daemon: `"at"`.

5. **Staged disk persistence with a journal.** `persist()` never
   truncates a live file. Prepare verifies every destination's disk
   bytes against the validated snapshot (external edits, missing
   files, directories, symlinks, out-of-root paths all reject before
   a byte is written) and refuses to start while a `.ontixa-tx-*.journal`
   exists. Candidates go to sibling `.stage` files (same filesystem);
   the journal records fingerprints, paths and swap progress before
   staging; commit is `dest → .bak` then `.stage → dest` per file.
   Mid-commit failure rolls swapped files back and *reports*
   rollback failures — never `let _ =`.

6. **Recovery is a first-class op.** `ontixa recover <dir>` resolves
   journals a dead transaction left: finishes a committed one,
   rolls back one that never swapped, and reports `conflict` —
   journal, staged bytes and backups preserved — when disk bytes
   match neither snapshot. Idempotent; clean trees are a no-op.

7. **Panic containment.** The daemon marks a session `poisoned`
   after a caught panic and refuses further ops on it — no plan or
   memo from a possibly half-mutated `Db` can leak through.

## Alternatives considered

- **Keep the direct-write loop + better rollback.** Still no
  journal, no crash story, and rollback itself can fail — rejected;
  the staged design makes every intermediate state inspectable and
  recoverable.
- **Copy whole workspace to a temp dir, then rename files over.** A
  mid-commit crash still leaves a mix of old/new files with no
  record of which is which — the journal is what makes recovery
  decidable.
- **Advisory file locking.** Doesn't stop non-cooperating writers
  anyway; the honest design is fingerprint checks + a small race
  window, documented as such.

## Consequences — honest guarantee boundaries

- **A. In-memory apply is atomic.** All provenance checks pass
  before `set_sources`; one revision bump publishes every file —
  no partial state is observable.
- **B. Disk persistence is recoverable, not atomic.** Any crash
  leaves a journal a later `recover` resolves deterministically:
  forward-complete or roll back, with evidence preserved on
  conflict.
- **C. External writers are detected, not prevented.** A disk edit
  between plan and persist is rejected; a *concurrent* writer racing
  the swap window can still interleave — the journal protocol only
  governs participants that respect it.
- A `FailPoint` seam (library-level, unreachable via CLI input)
  drives the failure-injection suite: journal/stage/sync/swap/
  promote failures and simulated kills, including a real
  child-process kill recovered forward by its parent.
