# Benchmark Philosophy

Ontixa's performance claims are measured or they are not made.
The `benchmarks/` directory is where evidence lives.

## Rules

1. **No vibes.** Every performance statement in docs or release notes
   links to a reproducible benchmark under `benchmarks/`.
2. **Benchmark the artifact, not the hope.** Interpreter numbers are
   interpreter numbers; they say nothing about native codegen.
3. **Compiler speed is a feature.** `ontixa check --timings` reports
   per-stage latency; the DB's memoization is benchmarked on
   edit-compile cycles, not just cold builds.
4. **Determinism enables regression gates.** Because artifacts are
   byte-stable, a CI gate can diff graph/MIR outputs and wall-clock
   timings across revisions.
5. **Adversarial honesty.** Benchmarks include the cases where we
   lose (e.g. deep recursion on the Rust stack, coarse file-granular
   rebuilds). A benchmark suite that never shows a weakness is
   marketing, not measurement.

## Milestone-1 state

`benchmarks/` holds seeds only. The honest numbers today are
`--timings` stage costs on the `examples/` corpus — sub-millisecond
for all stages at this scale. Claims about C/Rust-class native
performance are *targets* (see constitution + roadmap), not results.
