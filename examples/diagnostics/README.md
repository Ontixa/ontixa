# Diagnostic example pairs

Each failing file reports exactly the named diagnostic when checked. Its
`-fixed.ixa` counterpart passes. The CLI bundles the same files in its
offline guides; `ontixa diagnostic <CODE>` explains each correction.

| Code | Failing source | Corrected source | Lesson |
| --- | --- | --- | --- |
| `E_USE_AFTER_MOVE` | [use-after-move.ixa](use-after-move.ixa) | [use-after-move-fixed.ixa](use-after-move-fixed.ixa) | Read the returned owner after `keep` transfers ownership |
| `E_UNINITIALIZED` | [uninitialized.ixa](uninitialized.ixa) | [uninitialized-fixed.ixa](uninitialized-fixed.ixa) | Assign before the first read; deferred first initialization does not need `mut` |
| `E_IMMUTABLE_ASSIGNMENT` | [immutable-assignment.ixa](immutable-assignment.ixa) | [immutable-assignment-fixed.ixa](immutable-assignment-fixed.ixa) | Use `let mut` when reassignment is intended |
| `E_MUTABLE_BORROW_OF_IMMUTABLE` | [mutable-borrow-of-immutable.ixa](mutable-borrow-of-immutable.ixa) | [mutable-borrow-of-immutable-fixed.ixa](mutable-borrow-of-immutable-fixed.ixa) | The caller must authorize mutation of its own binding |

For example, from the repository root:

```sh
ontixa check examples/diagnostics/use-after-move.ixa
ontixa diagnostic E_USE_AFTER_MOVE
ontixa check examples/diagnostics/use-after-move-fixed.ixa
```

The first command intentionally exits 1; the last two exit 0. Without a
global installation, replace `ontixa` with
`cargo run --quiet --locked -p ontixa-cli --bin ontixa --`.

See [the diagnostic API](../../docs/diagnostics.md#offline-code-guides) and
[the memory model](../../docs/memory-model.md).
