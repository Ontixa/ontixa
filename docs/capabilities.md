# Capabilities (design direction)

Ontixa treats ambient authority as a bug class. A function should not
be able to open a socket, read a file, or spawn a thread unless that
power is visible in its signature.

## The principle

```text
authority = a value you must hold
```

There is no global `fs` or `net` namespace in the semantic sense —
capabilities are *passed*, inferred flows make them visible in the
SPG, and the compiler can answer "what can this function touch?"
without running it.

## Milestone 1 state

Not implemented — the interpreter has no I/O at all, so every program
is trivially pure. What exists today is the *machinery capabilities
will ride on*:

- **Effects as data**: the SPG already exposes call graphs and
  inferred contracts; capability edges will be another edge kind.
- **Inference precedent**: ownership inference proves the compiler can
  derive behavior from bodies instead of trusting annotations — the
  same trick applies to effect inference.
- **Structured diagnostics**: capability violations will be
  first-class codes, not runtime denials.

## Non-goals

- Not an OS-style sandbox (that is deployment, not language).
- Not capability *objects* bolted onto ambient APIs — the surface
  must not exist ambiently to begin with.

## Open questions (tracked in roadmap)

- Capability granularity: per-path `fs` vs. coarse `fs` root.
- Effect polymorphism: can a `map` be pure if its callback is not?
- Attenuation: passing `read-only sub-capability` without wrappers.
