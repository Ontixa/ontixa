# ADR-0007: Semantic Program Graph as the tooling interface

Status: accepted (milestone 1)

## Context

Constitution §1: compiler knowledge is data. Agents, linters,
refactors, doc generators, and IDEs all need the same semantic
facts — calls, types, scopes, contracts — and should never reparse
or re-infer them.

## Decision

`ontixa-semantic` builds `SemanticGraph`: typed nodes (`module`,
`function`, `data`, `param`, `local`, `field`, `type`, `expr`),
typed edges (`defines`, `has_param`, `has_field`, `typed`, `calls`,
`contains`, `reads`, `writes`), a `schema` version, deterministic
order, deduplicated type nodes, spans on everything syntax-derived.
Inferred parameter behavior lives on `has_param` edge attrs.

## Alternatives

- Per-tool APIs over HIR: every consumer reinvents graph walks.
- Language-server-only access: agents aren't IDE-shaped; a
  serializable artifact is more universal.

## Consequences

- `ontixa graph` is the machine-readable whole-program answer.
- The graph is the join point: diagnostics carry `subject` names that
  resolve to SPG symbols; MIR calls carry the same contracts the
  edges show.
- Growth path: more edge kinds (effects, capabilities) are additive
  under the same schema version rules.
