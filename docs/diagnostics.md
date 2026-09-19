# Diagnostics

Every diagnostic is a structure, not a string. Human rendering and
JSON rendering are two views of the same `Diagnostic` value.

## Anatomy

| field     | meaning                                          |
| --------- | ------------------------------------------------ |
| `code`    | stable machine-readable code (`E_USE_AFTER_MOVE`) |
| `severity`| `error` \| `warning`                              |
| `message` | one sentence: what went wrong                    |
| `primary` | main source span (optional for spanless errors)  |
| `labels`  | secondary spans with messages ("value moved here") |
| `notes`   | context lines (`= note:`)                        |
| `help`    | actionable suggestions (`= help:`)               |
| `subject` | semantic subject symbol name, when meaningful    |
| `details` | structured extras for machine consumers          |

## Codes

Codes are a public contract. Never reuse; retire instead.

| code                   | stage     | meaning                          |
| ---------------------- | --------- | -------------------------------- |
| `E_PARSE`              | parse     | malformed syntax                 |
| `E_UNTERMINATED_STRING`| lex       | unclosed `"` literal             |
| `E_UNTERMINATED_COMMENT`| lex      | unclosed `/*` comment            |
| `E_UNEXPECTED_CHAR`    | lex       | character with no token role     |
| `E_DUPLICATE_DEF`      | resolve   | two defs share a name            |
| `E_UNKNOWN_SYMBOL`     | resolve   | unresolvable identifier          |
| `E_UNKNOWN_TYPE`       | resolve   | type position names a non-type   |
| `E_NOT_A_STRUCT`       | types     | field access on non-data         |
| `E_UNKNOWN_FIELD`      | types     | field the type lacks             |
| `E_EXTRA_FIELD`        | types     | literal initializes unknown field|
| `E_DUPLICATE_FIELD`    | resolve   | field name reused                |
| `E_MISSING_FIELD`      | types     | literal omits a required field   |
| `E_ARG_COUNT`          | types     | call arity mismatch              |
| `E_NOT_CALLABLE`       | types     | callee is not a function         |
| `E_TYPE_MISMATCH`      | types     | wrong type for the context       |
| `E_CANNOT_INFER`       | types     | `let` with no annotation/init    |
| `E_USE_AFTER_MOVE`     | ownership | read of a moved value            |
| `E_UNINITIALIZED`      | ownership | read before any store            |
| `E_IMMUTABLE_ASSIGNMENT` | ownership | write to a non-`mut` binding   |
| `E_MUTABLE_BORROW_OF_IMMUTABLE` | ownership | `borrow_mut` arg lacks `mut` authority |
| `E_BORROW_CONFLICT`    | ownership | overlapping loans, at least one mutable |
| `E_MOVE_WHILE_BORROWED` | ownership | move of a place under a live loan |
| `E_AMBIGUOUS_SYMBOL`   | explain   | symbol query matched >1 symbol   |
| `E_MISSING_RETURN`     | types     | non-unit fn can fall through     |
| `E_LITERAL_OVERFLOW`   | types     | literal exceeds its type         |
| `E_UNSUPPORTED_OP`     | types     | op not defined for operand types |
| `I_INTERNAL`           | any       | compiler bug (ICE); exit 3       |

## JSON schema (version 1)

Every `--json` command emits **exactly one** envelope document on
stdout — never concatenated documents, never bare logs:

```json
{
  "schema": 1,
  "command": "check",
  "success": false,
  "diagnostics": [
    {
      "code": "E_USE_AFTER_MOVE",
      "severity": "error",
      "message": "`q` is used after its value moved",
      "primary": { "file": "examples/move-error.ixa", "start": 347, "end": 348 },
      "labels": [{ "start": 326, "end": 327, "message": "value moved here" }],
      "notes": [],
      "help": [],
      "subject": "q"
    }
  ],
  "result": null,
  "timings": [],
  "error": null
}
```

- `success` is `false` when any error-severity diagnostic exists or
  `error` is non-null.
- `result` carries the command payload (`tokens`, `ast`, `mir`,
  `graph`, `symbol`, or a `run` value); `null` when none.
- `timings` is `[]` unless `--timings` was passed (timings are
  nondeterministic and opt-in).
- `error` is `{ "kind": "io"|"runtime"|"internal", "message": ... }`
  for non-diagnostic failures, else `null`.

Agents should key off `code` + `details`, never off rendered text.
Exit codes: `0` ok, `1` error diagnostics, `2` io/runtime, `3` ICE.
