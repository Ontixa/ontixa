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

## Understanding a diagnostic

After `check` reports an ownership error, look up its stable code:

```sh
cargo run --quiet --locked -p ontixa-cli --bin ontixa -- diagnostic E_USE_AFTER_MOVE
cargo run --quiet --locked -p ontixa-cli --bin ontixa -- diagnostic E_UNINITIALIZED --json
```

The guide includes the meaning, a common cause, and complete failing and
corrected examples. It works offline without a source file. Human `check`
output points to a guide when one is available. The initial guides cover
`E_USE_AFTER_MOVE`, `E_UNINITIALIZED`, `E_IMMUTABLE_ASSIGNMENT`, and
`E_MUTABLE_BORROW_OF_IMMUTABLE`; see [diagnostic lookup](docs/diagnostics.md#offline-code-guides)
for output and coverage. `explain file.ixa [symbol]` continues to inspect
inferred program contracts.

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

## Boolean guards

`&&` and `||` short-circuit from left to right: `false && rhs` and
`true || rhs` skip `rhs`, including its mutations, traps, and returns.
Both operands still have to be valid `bool` expressions. `&&` binds
tighter than `||`. Run `examples/short-circuit.ixa` for guarded string
indexing and conditional mutation.

This supersedes the early interpreter's eager boolean evaluation.
Use separate statements for effects that must always happen; assigning
a binding only in the RHS does not make it definitely initialized.
See [ADR-0005](docs/adr/0005-typed-mir-interpreter-oracle.md).

## Numeric expressions

Declared numeric types guide literals through `+`, `-`, `*`, `/`, `%`,
and unary negation, including nested expressions. For example,
`fn answer() -> i64 { 40 + 2 }`, `let x: i8 = -128 + 1;`, and
`let y: f32 = -(1.0 + 2.0);` all keep their declared types. This also
works in assignments, call arguments, fields, arrays, and branch tails.
Run `examples/numeric-context.ixa` for a complete example.

A bare numeric literal, or one negation of it, can also adopt a typed
peer's numeric type in arithmetic and comparisons. With `x: i64`,
both `0 < x` and `x > 0` work, and `let y = 1 + x;` infers `i64`.
With `x: f32`, the same applies to `0.0 < x` and `-1.0 + x`.
A declared arithmetic result type takes priority. Left literals can use
variables, calls, fields, and indexed values as peers, including block
tails, negation, and arithmetic rooted in those expressions. Other peer
shapes retain left-to-right checking: `(1 + 2) < x`, `0 < (1 + x)`,
and `0 < if true { x } else { x }` with `x: i64` still need a typed
intermediate binding. Two literal operands keep their defaults. Run
`examples/numeric-peers.ixa` for an example.

Without a numeric context, integer and float literals still default to
`i32` and `f64`. A directly negated integer literal is checked after its
sign, so `let x = -2147483648;` fits the default `i32`, as does
`let y = -2147483648 + 1;`. This does not fold compound expressions:
`-(2147483648 + 0)` still rejects the overflowing positive literal.
Run `examples/default-integer-minimum.ixa` for an example.
Typed values never change width implicitly. Each integer
literal must fit its adopted type (`E_LITERAL_OVERFLOW`); arithmetic
itself still uses the reference interpreter's `i128`/`f64` superset,
not width-exact overflow or rounding semantics (see ADR-0005).
Floating-point `%` uses truncating remainder: `5.5 % 2.0` is `1.5`,
and `-5.5 % 2.0` is `-1.5`. The result keeps the dividend's sign,
including signed zero. A zero divisor, infinite dividend, or NaN operand
produces NaN; a finite dividend modulo infinity returns the dividend.
Integer remainder by zero still traps. See
`examples/float-remainder.ixa` and ADR-0005 for the reference semantics.

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
$ ontixa diagnostic E_USE_AFTER_MOVE # offline explanation + correction examples
$ ontixa run file.ixa              # compile + interpret, print result
$ ontixa tokens file.ixa           # token stream
$ ontixa ast file.ixa              # canonical AST (JSON)
$ ontixa mir file.ixa              # typed MIR (JSON)
$ ontixa graph file.ixa            # Semantic Program Graph (JSON)
$ ontixa explain file.ixa          # inferred contracts + timings
$ ontixa explain file.ixa m::sym   # a single symbol, qualified ok
$ ontixa diff --semantic old/main.ixa new/main.ixa # selected signature + contract changes
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
`check`, `explain`, `diff`, `rename`, `patch`, `fmt`, `stats`, `close`,
`shutdown` — see
[docs/agent-interface.md](docs/agent-interface.md).

`diff --semantic` independently compiles two source snapshots with the
same entry module name. It reports qualified definition, signature,
data-shape and inferred ownership changes; it never runs or edits the
programs. `No changes in compared fields` is not a claim of behavioral
equality or compatibility. Differences still exit 0; source/input
failures make comparison unavailable. See
[the comparison contract](docs/semantic-diff.md) for coverage and JSON.

The daemon's `diff` operation compares two unsaved buffer sets supplied
in one request, without temporary directories or changes to the live
session. Each side supplies an `entry` module and `sources` containing
module names and source text; `"semantic": true` is required. It uses
two fresh, isolated compilations and reports the same selected facts
and limits as the CLI. Try the synthetic NDJSON example from Bash or
Zsh:

```sh
cargo run --quiet --locked -p ontixa-cli --bin ontixad < examples/semantic-diff/inline.ndjson
```

In PowerShell, use `Get-Content examples/semantic-diff/inline.ndjson |
cargo run --quiet --locked -p ontixa-cli --bin ontixad`. The response
reports `borrow` changing to `move` in `geom::read` and `main::relay`;
the daemon does not execute either program. See
[inline comparison inputs](docs/semantic-diff.md#daemon-inline-inputs)
for the request and failure contract.

Common flags: `--json` (machine-readable output), `--timings`
(per-stage latency). Exit codes: `0` ok, `1` source errors or recovery
conflict, `2` runtime trap/unreadable input or unsuccessful diagnostic lookup,
`3` internal compiler error.

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
