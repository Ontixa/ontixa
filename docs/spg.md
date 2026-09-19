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
| `param`    | a function parameter symbol             |
| `local`    | a `let` binding                         |
| `field`    | a `data` field                          |
| `type`     | a canonical `Ty` (deduplicated)         |
| `expr`     | a HIR expression node                   |

## Edge kinds

| kind          | meaning                                   |
| ------------- | ----------------------------------------- |
| `defines`     | module → definition                       |
| `has_param`   | function → param (attrs: `behavior`)      |
| `has_field`   | data → field                              |
| `declares`    | function → local                          |
| `typed`       | symbol/expr → type node                   |
| `calls`       | call expr → callee function               |
| `contains`    | expr → child expr                         |
| `reads`/`writes` | expr → symbol it accesses              |

## Inferred behavior on edges

The headline feature: parameter contracts live on `has_param` edges.

```json
{ "from": 2, "to": 10, "kind": "has_param", "attrs": { "behavior": "borrow_mut" } }
```

An agent answering "may I pass `q` here and still use it?" reads the
edge attr — no source analysis, no guessing.

## Invariants

- Deterministic: node/edge order depends only on the source.
- Deduplicated: `type` nodes are interned per `Ty`.
- Span-carrying: every node from syntax keeps its byte range.
- Total: poison regions still appear (as poison nodes) so the graph
  is complete even for rejected programs.
