# ADR-0006: Structured diagnostics with stable codes

Status: accepted (milestone 1)

## Context

Diagnostics are a public API — agents consume them programmatically.
Rust's compiler internals taught us that "rendered string" as the
only output makes tooling grep for prose.

## Decision

`Diagnostic` is a struct: `code` (stable enum), `severity`,
`message`, `primary` span, `labels`, `notes`, `help`, `subject`
(symbol name for machine joins), `details` (structured extras).
Two renderers: human (`render_all`) and versioned JSON (`to_json`,
schema v1). Codes are never repurposed; `I_INTERNAL` is reserved for
ICEs.

## Alternatives

- miette/annotate-snippets: nice rendering, wrong boundary — we need
  to *own* the diagnostic struct and its JSON contract.
- String messages + spans: fine for humans, useless for agents.

## Consequences

- `--json` on `check` emits a schema-versioned document.
- Exit codes are contractual: 0 ok / 1 errors / 2 trap / 3 ICE.
- Adding a code is additive; changing one is a schema break.
