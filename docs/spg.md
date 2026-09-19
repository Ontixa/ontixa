# Semantic Program Graph (SPG)

The SPG is Ontixa's central data structure: the whole program's
semantics as one serializable graph. `ontixa graph file.ixa` emits it
as JSON; agents and tools consume it without parsing source.

## Shape

```json
{
  "schema": 1,
  "nodes": [ { "id": 0, "kind": "module", "label": "...", "span": {...}, "attrs": {...} } ],
  "edges": [ { "from": 0, "to": 1, "kind": "defines" } ]
}
```

- `schema` — graph format version; bump on any breaking change.
- Nodes are typed (`kind`), labeled, span-bearing, and carry attrs.
- Edges are `(from, to, kind)` triples with optional attrs.

## Node kinds

| kind       | represents                              |
| ---------- | --------------------------------------- |
| `module`   | the compilation unit root               |
| `function` | a `fn` definition                       |
| `data`     | a `data` definition                     |
| `param`    | a function parameter symbol (attrs: `behavior`, `position`, `escapes`, `evidence`) |
| `local`    | a `let` binding                         |
| `field`    | a `data` field                          |
| `type`     | a canonical `Ty` (deduplicated)         |
| `expr`     | a HIR expression node (`expr_kind` attr) |
| `stmt`     | a HIR statement node (`stmt_kind` attr)  |

## Edge kinds

| kind             | meaning                                        |
| ---------------- | ---------------------------------------------- |
| `declares`       | module → definition                            |
| `has_param`      | function → param (attrs: `behavior`)           |
| `returns`        | function → its return type node                |
| `has_field`      | data → field                                   |
| `has_local`      | function → local                               |
| `typed_as`       | symbol/expr/stmt → type node                   |
| `contains`       | parent → child (block→stmt, expr→subexpr)      |
| `calls`          | call expr → callee function                    |
| `passes`         | call arg expr → callee param (attrs: `position`, `behavior`) — the ownership/memory edge |
| `reads`          | var expr → referenced symbol                   |
| `writes`         | assign stmt → written local/param              |
| `accesses_field` | field expr → accessed field symbol             |
| `constructs`     | struct-literal expr → constructed data def     |
| `binds`          | let stmt → bound local                         |

## Inferred behavior on edges

The headline feature: parameter contracts live on `has_param` edges.

```json
{ "from": 2, "to": 10, "kind": "has_param", "attrs": { "behavior": "borrow_mut" } }
```

An agent answering "may I pass `q` here and still use it?" reads the
edge attr — no source analysis, no guessing.

`passes` edges expose the same contract at the *call site*:

```json
{ "from": 20, "to": 6, "kind": "passes", "attrs": { "position": 0, "behavior": "borrow" } }
```

`borrow`/`borrow_mut` mean the callee shares the caller's storage for
the call's extent; `move`/`escape` mean ownership transfers.

## Invariants

- Deterministic: node/edge order depends only on the source.
- Deduplicated: `type` nodes are interned per `Ty`.
- Span-carrying: every node from syntax keeps its byte range.
- Total: poison regions still appear (as poison nodes) so the graph
  is complete even for rejected programs.
