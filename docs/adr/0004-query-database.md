# ADR-0004: Compiler database — custom now, salsa-shaped

Status: accepted (milestone 1)

## Context

The compiler must be incremental and query-oriented (constitution §7)
— the daemon, IDE support, and agents all consume *queries*, not a
batch pipeline. salsa is the proven Rust framework for this, but its
API churned through 26 breaking releases during evaluation.

## Decision

Ship a minimal `ontixa-db`: a `Db` holding per-file artifact bundles
keyed by content revision, with per-stage timings. The public shape
is query-shaped (`compile(file) -> &Artifacts`), so callers never
see the pipeline — but invalidation is whole-file and coarse.

## Alternatives

- salsa now: real fine-grained incrementality, but API churn and
  heavy generic machinery for a milestone that compiles one file.
- No DB: the CLI wires stages directly — but then `ontixad` would
  need the plumbing rebuilt anyway.

## Migration path to salsa

- `compile` → per-stage queries (`parse`/`hir`/`types`/`ownership`/
  `mir`) as salsa tracked functions.
- `set_source` → input setter with per-stage durability tracking.
- Early cutoff: unchanged HIR downstream of an edit stops cascades.
- The `StageTiming` plumbing carries over verbatim.

## Consequences

- Milestone-1 "incrementality" is honest: a file edit rebuilds that
  file. The speed claim is deferred until M3, with timings already
  instrumented so the win will be measurable.
