# ADR-0001: Lossless CST via `rowan`

Status: accepted (milestone 1)

## Context

Formatting, IDE support, and semantic patches all need every byte of
the input preserved — whitespace, comments, exact ranges. A CST that
drops trivia forces each tool to re-derive it.

## Decision

Lex to `Vec<Token>` (trivia included), then a recursive-descent
parser builds a `rowan` green tree. `rowan` is rust-analyzer's tree
library: proven, cheap structural sharing, `SyntaxNode`/`SyntaxToken`
typed over `OntixaLanguage`.

## Alternatives

- Hand-rolled tree: full control, but reimplements what rowan already
  does well (green/red split, immutable sharing).
- Parse directly to AST: loses trivia; would block `ontixa fmt` and
  span-exact patching forever.
- Tree-sitter: wrong layer — we need the grammar owned in-process.

## Consequences

- `root.text() == source` is an enforced invariant (tested).
- Errors are `ERROR` nodes in the tree; parsing is total.
- AST lowering walks the green tree; CST stays available for tools.
