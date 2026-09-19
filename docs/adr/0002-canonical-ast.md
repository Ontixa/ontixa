# ADR-0002: Canonical AST separate from CST

Status: accepted (milestone 1)

## Context

The CST faithfully mirrors syntax — including noise: optional
semicolons, parenthesization, trivia attachment. Semantic passes want
a normalized, owned tree, not a faithful one.

## Decision

`ontixa-ast` lowers CST → `AstModule`: an owned tree where
desugaring already happened (e.g. `if` chains normalize, tail
expressions are explicit, struct-literal fields are name+expr pairs).
AST nodes are `Serialize` — `ontixa ast` dumps them as JSON.

## Alternatives

- HIR directly from CST: mixes name resolution with desugaring,
  tangling two concerns and making each harder to test.
- Annotated CST as the only IR: every downstream pass pays the
  noise tax.

## Consequences

- The AST is the stability boundary for "what the syntax means";
  syntax sugar can change without touching HIR.
- Errors lower to `Error` placeholders — the tree is always complete.
