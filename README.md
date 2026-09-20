# Ontixa

Ontixa is a semantic-first systems programming language and
software-construction platform, designed from the start for both human
programmers and autonomous machine programmers.

It is **not** a toy language, a syntax experiment, a weekend
interpreter, or "Rust with easier syntax". The repository is the
canonical implementation: a real compiler pipeline with structured
semantics exposed as first-class data.

## Status

`0.0.1-dev` — semantic workspace: multi-module compilation with
incremental, per-definition semantics and rename transactions.

What works today, end to end:

```text
Ontixa sources (.ixa files — file stems are module names; use m / m::x)
    -> lexer + lossless CST (rowan)
    -> canonical AST
    -> HIR (workspace name resolution, per-file envs)
    -> type analysis
    -> ownership / borrow / move / escape inference
    -> Semantic Program Graph (JSON)
    -> typed MIR (CFG)
    -> reference interpreter
    -> structured diagnostics (human + JSON, file-tagged)
    -> incremental query engine (per-DefKey early cutoff)
    -> semantic rename transactions (plan -> preview -> apply)
```

Concretely, this program compiles and runs:

```ontixa
data Point {
    x: i32;
    y: i32;
}

fn distance2(p: Point) -> i32 {
    return p.x * p.x + p.y * p.y;
}

fn main() -> i32 {
    let p = Point { x: 3, y: 4 };
    return distance2(p);
}
```

```console
$ ontixa run examples/structs.ixa
25
```

## Ownership without annotations

Ontixa source has **no ownership syntax**. The compiler infers what
each function does with each parameter and records it as semantic
data:

| behavior      | meaning                                            |
| ------------- | -------------------------------------------------- |
| `copy`        | bitwise copy; caller's value unaffected (Copy types) |
| `borrow`      | read-only access; caller keeps ownership           |
| `borrow_mut`  | writes through field projections; caller keeps it  |
| `move`        | callee consumes the argument                       |
| `escape`      | argument may flow into the callee's return value   |
| `unknown`     | analysis inconclusive; treated as consuming        |

Inference is enforced, not decorative: `move`/`escape` arguments are
consumed at call sites, and reads of moved values are rejected with
`E_USE_AFTER_MOVE`.

```console
$ ontixa explain examples/borrow-inference.ixa
examples/borrow-inference.ixa
  data Point { x, y }
  fn read(p: borrow)
  fn bump(p: borrow_mut)
  fn eat(p: move)
  fn keep(p: escape)
  fn add(a: copy, b: copy)
  fn main()
```

## The tool

```console
$ ontixa check file.ixa            # compile; diagnostics, exit 0/1
$ ontixa run file.ixa              # compile + interpret, print result
$ ontixa tokens file.ixa           # token stream
$ ontixa ast file.ixa              # canonical AST (JSON)
$ ontixa mir file.ixa              # typed MIR (JSON)
$ ontixa graph file.ixa            # Semantic Program Graph (JSON)
$ ontixa explain file.ixa          # inferred contracts + timings
$ ontixa explain file.ixa m::sym   # a single symbol, qualified ok
$ ontixa rename file.ixa m::old new         # preview edits
$ ontixa rename file.ixa m::old new --apply # guarded atomic apply
```

`ontixad` is the persistent daemon (NDJSON on stdio): `open`, `set`,
`check`, `explain`, `rename`, `stats`, `close`, `shutdown` — see
[docs/agent-interface.md](docs/agent-interface.md).

Common flags: `--json` (machine-readable output), `--timings`
(per-stage latency). Exit codes: `0` ok, `1` source errors, `2`
runtime trap, `3` internal compiler error.

## Layout

```text
crates/
  ontixa-source       spans, files, interner
  ontixa-diagnostics  structured diagnostics (human + JSON)
  ontixa-syntax       lexer + lossless CST (rowan)
  ontixa-ast          canonical AST
  ontixa-hir          HIR + name resolution + scopes
  ontixa-types        type analysis (total, poison-tolerant)
  ontixa-memory       ownership/borrow/move/escape inference
  ontixa-semantic     Semantic Program Graph
  ontixa-mir          typed CFG
  ontixa-db           incremental query engine + rename transactions
  ontixa-interpreter  reference executor (semantics oracle)
  ontixa-cli          the `ontixa` tool + `ontixad` daemon
docs/                 design documentation
docs/adr/             architecture decision records
examples/             runnable .ixa programs (+ workspace/ multi-module)
```

## Design docs

- [docs/constitution.md](docs/constitution.md) — non-negotiable
  invariants
- [docs/architecture.md](docs/architecture.md) — the pipeline and why
- [docs/memory-model.md](docs/memory-model.md) — ownership inference
- [docs/spg.md](docs/spg.md) — the Semantic Program Graph
- [docs/diagnostics.md](docs/diagnostics.md) — codes and JSON schema
- [docs/agent-interface.md](docs/agent-interface.md) — programming
  Ontixa programmatically
- [docs/semantic-identity.md](docs/semantic-identity.md) — `DefKey`,
  item-relative spans, incremental guarantees
- [docs/roadmap.md](docs/roadmap.md) — where this is going
- [docs/adr/](docs/adr/) — decision records

## Development

```console
$ cargo build --workspace
$ cargo test --workspace
$ cargo clippy --workspace -- -D warnings
$ cargo fmt --check
```

License: MIT OR Apache-2.0.
