# Transaction-integrity campaign — evidence report

Branch: `fix/transaction-integrity`
Baseline: `c4340e5` (main, semantic-workspace merge)
Scope: rename transaction correctness only — no compiler rebuild, no
codegen, no new language features.

## Audit findings → fixes → regressions

| # | Defect (verified on c4340e5) | Fix | Regression |
|---|---|---|---|
| A | `persist()` truncated live files; rollback skipped the failed file and swallowed errors; no journal | Staged candidates + journal + per-file swap + reported rollback | `crates/ontixa-cli/tests/persist.rs` (19 tests incl. failpoints + child kill) |
| B | Disk bytes never compared to validated snapshot — external edits overwritten | Prepare phase hashes every destination against snapshot; mismatch rejects before byte one | `stale_disk_is_rejected_not_overwritten`, `external_edit_between_plan_and_persist_rejected` |
| C | `error_signature` multiset checked via `contains_key` — duplicate `(code,file)` errors passed | Clean-baseline policy: rename requires an error-free workspace; any candidate error rejects | `dirty_baseline_rejected_*`, `clean_baseline_still_shadow_checks` |
| D | Public plan fields, revision-only apply guard — plans crossed Dbs, tampered payloads applied, forged indices panicked mid-commit | Private fields + incarnation id + workspace/candidate fingerprints + expected bytes, all checked pre-commit | `integrity.rs` (11 tests) |
| E | No local/parameter rename; no binding check — capture compiled and applied | `plan_rename_at` selects bindings through the fn's HIR; shadow-compile binding-sequence diff rejects drift | `local_rename.rs` (19 tests) |
| F | Daemon `catch_unwind` kept serving a possibly half-mutated `Db` | Session marked `poisoned`; subsequent ops refused | `cli.rs` daemon tests |

## Supported rename scope (verified)

- Top-level `fn`/`data`, `use m::x` / `use m::x as y` aliases,
  qualified `m::x` refs, bare refs through unaliased imports.
- `let` bindings and function parameters selected by byte offset,
  through the function's own HIR (shadowing exact).
- Refused: fn-name via `@offset` (use the symbol form), non-binding
  selections, capture-producing new names, baseline with errors,
  stale/cross-session/tampered plans.

## Verification scope

- **Binding correspondence**: shadow-compiled candidate must
  reproduce the baseline's binding sequence (var/assign/let/call/
  struct-literal sites) modulo the target's rename — compile success
  alone is not sufficient.
- **Fingerprint domain**: module names + raw source bytes (FNV-1a-64,
  no CRLF/Unicode normalization, no mtimes).
- **Not** formal equivalence — static semantic verification within
  the supported rename scope.

## Atomicity scope

- **In-memory apply**: atomic — all provenance checks precede
  `set_sources`; one revision bump publishes every file.
- **Disk persistence**: recoverable, not atomic — any crash leaves a
  `.ontixa-tx-*.journal` that `ontixa recover <dir>` resolves
  forward-complete or rollback; conflicts preserve evidence.
- **External writers**: detected (byte-exact stale guard) but not
  prevented — a non-cooperating writer racing the swap window can
  still interleave; the journal protocol governs participants only.

## Failure-injection coverage (`persist.rs` tests, 19)

Journal-write fail · stage-write fail (clean + partial) · stage-sync
fail · journal-update fail · swap fail → rollback · promote fail →
rollback · simulated kill → forward recovery · staged-never-swapped
→ rollback recovery · pending journal blocks new tx until recovery ·
conflict-bytes → evidence preserved · recovery idempotent ×2 ·
missing/directory/symlink/out-of-root destinations · spaces+Unicode
paths · CRLF/LF/multibyte byte fidelity · real child-process kill +
recovery.

## Commands run

```text
cargo fmt --all -- --check
cargo build --workspace --locked
cargo test --workspace --locked            # all suites green
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test -p ontixa-db --test integrity      # 11/11
cargo test -p ontixa-db --test local_rename   # 19/19
cargo test -p ontixa-cli --test persist       # 19/19
examples\workspace\demo.ps1                   # full walkthrough
```

## Demo

`examples/workspace/demo.ps1` (Windows-native PowerShell, runs on a
copied fixture): cross-module rename, `@offset` param rename,
capture rejection, daemon stale-plan rejection, disk apply with
staging, pending-journal block, `recover` conflict → evidence
preserved, CRLF/UTF-8 fidelity, fixture integrity check.
`demo.sh` retained for Unix.
