# Ontixa

Ontixa is a semantic-first systems programming language and
software-construction platform, designed from the start for both human
programmers and autonomous machine programmers.

The compiler exposes types, inferred ownership contracts, and program
relationships as structured data that tools and agents can inspect.

## Status

`0.0.1-dev` — semantic workspace: multi-module compilation with
incremental, per-definition semantics and rename transactions.

This is an experimental, pre-release implementation. Programs run in a
reference interpreter; native code generation, a standard library, and
a package manager are not available. Syntax and APIs may change.

## Quickstart from source

You need Git and a stable Rust toolchain with Cargo (Rust 1.85 or newer),
plus the native linker required by your Rust installation. On Windows,
the default MSVC toolchain needs the Visual Studio C++ build tools. The
repository's `rust-toolchain.toml` selects stable Rust.

If Windows reports `link.exe` missing, install the C++ build tools and
run from their developer shell. If you already use Rust's GNU toolchain
with MinGW-w64 installed, replace `cargo` in these commands with
`cargo +stable-x86_64-pc-windows-gnu`.

These commands work in PowerShell, Bash, and Zsh:

```sh
git clone https://github.com/Ontixa/ontixa.git
cd ontixa
cargo build --workspace --locked
cargo run --quiet --locked -p ontixa-cli --bin ontixa -- run examples/hello.ixa
```

The last command prints `42`. From the same directory, inspect inferred
ownership contracts and get machine-readable compiler diagnostics:

```sh
cargo run --quiet --locked -p ontixa-cli --bin ontixa -- explain examples/borrow-inference.ixa
cargo run --quiet --locked -p ontixa-cli --bin ontixa -- check examples/hello.ixa --json
```

`explain` shows `borrow`, `borrow_mut`, `move`, `escape`, and `copy`
parameter behavior. `check --json` emits one JSON document with
`"schema": 1`, `"success": true`, and an empty `diagnostics` array.

Commands later in this README use the shorter `ontixa` spelling. You
can always replace it with `cargo run --quiet --locked -p ontixa-cli
--bin ontixa --` from the repository root; no global installation is
required. See [the agent interface](docs/agent-interface.md) for JSON
responses, the persistent daemon, and transactional renames.

## Implemented pipeline

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
    -> canonical formatting over the lossless CST (`ontixa fmt`)
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
$ ontixa rename file.ixa @138 new           # local/param rename by byte offset
$ ontixa rename file.ixa m::old new --apply # guarded apply; staged+journaled to disk
$ ontixa patch file.ixa '{"ops":[...]}'     # structured patch preview (spec: -/@file/inline JSON)
$ ontixa patch file.ixa @p.json --apply     # guarded apply; staged+journaled to disk
$ ontixa recover dir/                       # resolve a transaction journal after a crash
$ ontixa fmt file.ixa                       # canonical format to stdout (CST-driven, keeps comments)
$ ontixa fmt file.ixa --check               # exit 1 if the file would change — CI gate
$ ontixa fmt file.ixa --write               # overwrite in place (staged+journaled)
```

`ontixad` is the persistent daemon (NDJSON on stdio): `open`, `set`,
`check`, `explain`, `rename`, `patch`, `fmt`, `stats`, `close`,
`shutdown` — see
[docs/agent-interface.md](docs/agent-interface.md).

Common flags: `--json` (machine-readable output), `--timings`
(per-stage latency). Exit codes: `0` ok, `1` source errors or recovery
conflict, `2` runtime trap/unreadable input, `3` internal compiler error.

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
  ontixa-db           incremental query engine + source transactions (rename, patch)
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
