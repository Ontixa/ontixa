# Contracts

"Contract" is the umbrella term for everything the compiler infers
about a definition that callers can rely on. Milestone 1 ships one
contract family (ownership); the design generalizes.

## Families

| family      | status      | what it answers                              |
| ----------- | ----------- | -------------------------------------------- |
| ownership   | shipped (m1)| may the caller use the argument afterwards?  |
| purity      | planned     | does the function observe/mutate the world?  |
| capability  | planned     | what authority does it need?                 |
| termination | planned     | is it total on its domain?                   |
| cost        | planned     | what resources does it consume?              |

## Ownership contracts (milestone 1)

Each `fn` gets a `Vec<ParamBehavior>` — one entry per parameter.
The contract is:

- **Inferred**, never annotated. It describes what the body *does*.
- **Enforced** at every call site (`E_USE_AFTER_MOVE`).
- **Propagated** inter-procedurally to a fixpoint.
- **Exposed** in the SPG (`has_param` edge attrs), in `explain`
  output, and in MIR `Call` rvalues for the executor.

A contract is a *lower bound on caller obligations*: `borrow` means
the callee will not consume the argument — ever — so the compiler
may share the caller's storage directly.

## Why inference over annotation

Annotations make the *author* state intent; inference makes the
*compiler* state fact. Intent can lie (or rot); facts cannot. When a
contract can't be inferred precisely, the answer is `unknown`, which
behaves conservatively — the sound direction.

## Contract stability

Contracts are part of a function's public meaning: a change that
strengthens a contract (e.g. `borrow` → `move`) is a breaking change
to callers, and tooling will diff contracts across versions the same
way it diffs signatures.
