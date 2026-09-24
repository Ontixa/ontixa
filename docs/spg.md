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
| `module`   | one workspace file's module (stem-named); the root node owns the workspace |
| `function` | a `fn` definition                       |
| `data`     | a `data` definition (attr `shape` = `record` \| `enum`) |
| `param`    | a function parameter symbol (attrs: `behavior`, `position`, `escapes`, `evidence`) |
| `local`    | a `let` binding or `match` pattern binding |
| `field`    | a `data` field                          |
| `variant`  | an enum `data` variant                  |
| `arm`      | a `match` arm (attrs: `pattern` = `variant` \| `bind` \| `wildcard`, `variant` = discriminant) |
| `type`     | a canonical `Ty` (deduplicated)         |
| `expr`     | a HIR expression node (`expr_kind` attr) |
| `stmt`     | a HIR statement node (`stmt_kind` attr)  |

## Edge kinds

| kind             | meaning                                        |
| ---------------- | ---------------------------------------------- |
| `declares`       | module → definition                            |
| `has_param`      | function → param (attrs: `behavior`)           |
| `returns`        | function → its return type node                |
| `has_field`      | data → field (attr: `position`)                |
| `has_variant`    | data → variant (attr: `discriminant`)          |
| `has_local`      | function → local                               |
| `typed_as`       | symbol/expr/stmt → type node (attr `position` on variant payload types) |
| `contains`       | parent → child (block→stmt, expr→subexpr; attrs: `position` on array-literal elements and match arms, `role` = `lo`/`hi`/`iter`/`body` on ranges and loops, `role` = `scrutinee`/`body` on match exprs and arms) |
| `matches`        | match arm → the variant its pattern selects    |
| `calls`          | call expr → callee function                    |
| `passes`         | call arg expr → callee param (attrs: `position`, `behavior`) — the ownership/memory edge |
| `reads`          | var expr → referenced symbol                   |
| `writes`         | assign stmt → written local/param              |
| `accesses_field` | field expr → accessed field symbol             |
| `constructs`     | struct-literal expr → constructed data def; variant-literal expr → constructed variant (attr `variant` = discriminant on the expr node) |
| `binds`          | let stmt → bound local; match arm → pattern-bound local (attr `position` for payload binds) |

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
