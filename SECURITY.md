# Security Policy

## Scope

Ontixa is a pre-release compiler (`0.0.1-dev`). The security model is
therefore about *the compiler itself*, not about deployed programs.

What counts as a security issue today:

- The compiler executing arbitrary code from `.ixa` source beyond
  what the interpreter intentionally evaluates.
- Malformed input causing memory unsafety in compiler code (Rust UB —
  the compiler itself is safe Rust; report any `unsafe` you find).
- Diagnostics or JSON output that leaks unintended filesystem or
  environment data.

What does not (yet) count:

- Denial of service via pathological inputs (deep recursion is a
  known interpreter limit — documented in ADR-0005).
- Sandboxing of executed programs — the interpreter has no I/O.

## Reporting

Report vulnerabilities privately via GitHub's "Report a
vulnerability" flow on `Ontixa/ontixa`, or email the maintainers
listed in the org profile. Do not open public issues for suspected
vulnerabilities.

Include: the `.ixa` input or invocation, observed behavior, expected
behavior, and `ontixa --version`.

## Supply chain

- Dependencies are minimal and pinned via `Cargo.lock` (committed).
- New dependencies require an ADR and a version ≥ 7 days old.
- CI builds with `--locked` to guarantee the pinned graph.
