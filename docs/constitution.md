# Ontixa Constitution

The non-negotiable invariants. Every design decision is judged against
these; a change that violates one is a bug, not a trade-off.

## 1. Semantics are first-class

The compiler's understanding of a program is *data*, not a private
internal state. The Semantic Program Graph, inferred contracts, types,
and diagnostics are all inspectable, serializable, and versioned.
If the compiler knows it, a tool can read it.

## 2. No ownership syntax

Source code carries no `&`, `&mut`, `'a`, `move`, or `borrow`
annotations. Parameter behavior is *inferred* and *enforced*:
`copy`, `borrow`, `borrow_mut`, `move`, `escape`, `unknown`.

## 3. Safety without a tracing GC

Rust-class memory safety is the floor. Values have single ownership;
references exist only as inferred, scope-bound borrows. There is no
tracing garbage collector and there never will be one.

## 4. Total passes

Every compiler stage is *total*: malformed input produces diagnostics
and poison nodes, never a crash and never a half-built structure.
A panic inside the pipeline is an internal compiler error (ICE) and
is reported as `I_INTERNAL`, exit code 3.

## 5. Structured diagnostics

Every diagnostic has a stable code, a severity, a primary span, and
optional labels/notes/help/details. Diagnostics render identically
for humans and machines (`--json`). Codes are never repurposed.

## 6. Determinism

Same source + same compiler version = same artifacts, byte for byte.
Iteration order is deterministic; timestamps live outside artifacts.

## 7. The compiler is a database

Compilation is a set of memoized queries over source inputs. The CLI
is a thin client; `ontixad` will serve the same queries to IDEs,
formatters, and agents.

## 8. Machines are peers

Anything a human tool can do, an agent can do through the same
interfaces: graph queries, typed edits, contract inspection. No LLM
lives inside the compiler; agents consume the SPG like everyone else.
