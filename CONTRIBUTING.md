# Contributing

Ontixa is early. The bar for contributions is correctness and honesty
about scope, not volume.

## Ground rules

- Read [docs/constitution.md](docs/constitution.md) first — the
  invariants are non-negotiable.
- Code speaks through tests: every pass change comes with a failing
  test that then passes.
- Diagnostics are an API — new codes are additive only; see
  [docs/diagnostics.md](docs/diagnostics.md).
- No new dependencies without an ADR; prefer versions ≥ 7 days old.
- Keep modules focused; files approaching ~400 lines should justify
  themselves or split.

## Workflow

```console
$ cargo build --workspace
$ cargo test --workspace
$ cargo clippy --workspace -- -D warnings
$ cargo fmt
```

All four must pass. The CI workflow runs exactly these gates.

## Commit style

- Imperative, scoped subjects: `mir: close blocks lazily after return`.
- Explain *why* in the body; the diff already shows *what*.
- Breaking the SPG schema or diagnostic codes requires an ADR update.

## Where to start

- `crates/ontixa-memory` — the inference core; good first deep read.
- `examples/` — every behavior needs a runnable example.
- `docs/adr/` — decision context before proposing design changes.
