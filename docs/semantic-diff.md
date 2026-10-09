# Contract and signature comparison

Contract and signature comparison reports changes in selected compiler
facts between two explicit source snapshots. The CLI reads source files;
the daemon accepts inline source text. Both compile the workspaces
independently with the same compiler and never execute the programs or
write their source files.

## CLI file inputs

```sh
ontixa diff --semantic before/main.ixa after/main.ixa
ontixa diff --semantic before/main.ixa after/main.ixa --json
```

The `--semantic` flag is required. The CLI accepts file paths, not inline
workspaces. There is no implicit Git baseline, daemon session, patch plan,
or serialized-graph input.

Each entry file uses the existing workspace loader: sibling `.ixa`
files are registered, and only `use`-reachable modules enter the
comparison. Supply stable source snapshots while the files are read.
Unreachable sibling definitions are outside the report.

Both entry files must have the same module name (file stem), spelled
as an ASCII identifier. For example, `old/main.ixa` and `new/main.ixa`
match; `before.ixa` and `after.ixa` do not. No root aliases are invented.
Directory prefixes are input labels, not semantic identities.

## Daemon inline inputs

`ontixad` accepts one NDJSON request containing both complete source
workspaces. This lets a client compare two unsaved buffer sets without
temporary directories or altering its live compile session:

```json
{"op":"diff","semantic":true,"before":{"entry":"main","sources":[{"module":"main","text":"fn f(x: i32) -> i32 { x }"}]},"after":{"entry":"main","sources":[{"module":"main","text":"fn f(value: i32) -> i32 { value }"}]}}
```

- `semantic` must be `true`; no other comparison mode is implied
- `before` and `after` each contain an `entry` string and a `sources`
  array of objects with `module` and `text` strings
- Every module name, including `entry`, must match
  `[A-Za-z_][A-Za-z0-9_]*`. Names must be unique within each side, and
  `sources` must include that side's entry module
- Entry module names must match across the two sides
- Unknown fields at the request, workspace or source level are rejected

Each side owns its entire source workspace: omitted modules cannot come
from disk or an existing session. All supplied sources are registered,
entry-first and then in module-name order, regardless of array order.
Only definitions in the entry and its `use`-reachable modules enter the
comparison; unreachable supplied definitions are outside the report.
No `main` function is required.

Every request creates two fresh, isolated `Db` instances. The operation
does not read or write files, access or change the persistent `Session`,
or affect its revision, cache or counters. It is not an incremental
comparison and does not use a patch plan or apply edits.

Input provenance includes `source_kind: "inline"`. Entry `path` labels
are synthetic, such as `before/main.ixa` and `after/main.ixa`; diagnostics
use the same side/module convention for each supplied source. These
labels are not filesystem locations. `coverage.inputs` explicitly
describes inline sources and `use` reachability rather than the CLI's
sibling-file loader.

Parser and lexer diagnostics are tagged with their source module before
workspace collection. This also corrects an existing CLI/daemon bug where
dependency parse errors used entry-module labels, and identical errors in
different modules could collapse. Codes, messages and byte offsets keep
their meaning; `comparison_side` identifies the input snapshot.

Run the synthetic multi-module example from Bash or Zsh:

```sh
cargo run --quiet --locked -p ontixa-cli --bin ontixad < examples/semantic-diff/inline.ndjson
```

PowerShell: `Get-Content examples/semantic-diff/inline.ndjson | cargo run
--quiet --locked -p ontixa-cli --bin ontixad`. The response reports
`params[0].behavior` changing from `borrow` to `move` for `geom::read`
and its caller `main::relay`, without running either program.

## Shared identity rules

Definitions are matched by their declaring module and name. Imported
modules keep their actual names. Resolved nominal types and escape
callees are qualified the same way: `a::Point` and `b::Point` remain
distinct, including inside arrays. Import-alias spelling is not
compared, but a changed alias target is visible wherever it changes a
compared resolved type or escape target. A definition or module rename
is reported as removal and addition; correspondence is never guessed.

## Compared fields

- Top-level `fn`/`data` presence and kind
- Function parameters in position order: name, resolved type, declared
  `mut`, inferred ownership behavior, and escape target set
- Resolved return type (`unit` includes an omitted return annotation)
- Data shape (`record`/`enum`), fields in declaration order with name
  and resolved type, and variants in declaration order with name,
  discriminant and ordered payload types
- Escape targets as `return` or a qualified callee, sorted and
  deduplicated as a set; changes can propagate from callees into callers

Definition declaration order, source coordinates, evidence sites,
comments and whitespace do not affect equality. Parameter, field,
variant and payload order do. Parameters are compared positionally;
the tool does not infer parameter moves or renames.

For example, changing `return p.x;` to `let q = p; return q.x;` can
change a non-Copy parameter from `borrow` to `move` without changing
its declared type. The report identifies `params[0].behavior` and
shows both values, including affected caller contracts.

The checked-in example demonstrates this change in `read` and its
caller `relay`, without requiring or running a `main` function:

```sh
ontixa diff --semantic examples/semantic-diff/before/main.ixa examples/semantic-diff/after/main.ixa
```

## Meaning and limits

`No changes in compared fields` means exactly that. A changed function
body can compute a different result while leaving every compared field
unchanged. The report does not compare bodies, local bindings, call
graphs, runtime behavior, capabilities, effects or other planned
contract families. It makes no compatibility, safety, semantic-equivalence
or behavioral-equality guarantee. The existing ownership analysis's
precision limits still apply.

An explicit `unknown` ownership value remains `unknown` in the compared
record and is listed in that input's `uncertainties`. Human output also
discloses it, including when both sides have equal unknown values and
the report says `No changes in compared fields`. Equality of unknown
values does not establish the underlying ownership behavior.
The current finite ownership fixpoint does not emit `unknown` for
recursion; preserving it is defensive support for the public inference
domain, not a claim that recursion is unanalyzable.

If either input has error diagnostics, unreadable input, a missing
parameter contract, a poison type, or ambiguous/unrepresentable
identity, comparison is unavailable. Warnings are retained and do not
block comparison. The report does not interpret incomplete information
as equality. It neither changes inference nor checks whether a proposed
change preserves any particular property.

## Output contract

Human output names the compared fields and limits, identifies both
inputs, and lists added, removed and modified definitions. Modified
definitions show field paths such as `params[0].behavior`; changed
array lengths show the whole array. Unchanged reports use the phrase
`No changes in compared fields`.

CLI `--json` emits exactly one existing schema-1 envelope with
`command: "diff"`. The daemon returns the same envelope as one compact
NDJSON line per request. A valid comparison request has a `result` with:

- `comparison_schema: 1`: the version of the comparison payload
- `coverage`: explicit compared/excluded fields, identity/input rules
  and the limited claim made by the report
- `inputs.before` / `inputs.after`: path, logical module, status, an
  error message (or `null`), and `uncertainties` (each naming a
  definition, parameter position/name and the `unknown` behavior).
  Daemon inline inputs additionally identify `source_kind: "inline"`
- `status`: `changed`, `unchanged` or `unavailable`
- `changes`: a definition-sorted array, or `null` when unavailable

Each change has `definition` (`module::name`), `kind` (`added`,
`removed`, `modified`), normalized `before`/`after` records (`null`
for the absent side), and `fields`. For modifications, `fields` lists
deterministic `{field, before, after}` deltas. Added/removed records
carry their full selected fields and an empty `fields` list.

Function records contain `kind: "fn"`, `params` and `returns`.
Parameters contain `name`, `type`, `mutable`, `behavior` and `escapes`.
An escape is `{"kind":"return"}` or
`{"kind":"call","callee":"module::function"}`. Data records contain
`kind: "data"`, `shape`, `fields` (`name`/`type`) and `variants`
(`name`/`discriminant`/`payload`). All types are resolved strings:
primitive names, `module::Data`, or `[element]`.

Every top-level diagnostic carries `comparison_side: "before"` or
`"after"`, in addition to its ordinary file attribution. Unavailable
reports have `success: false` and `error.kind: "comparison_unavailable"`;
the per-input status distinguishes `source_errors`, `io`, `internal`,
`unresolved` and `root_mismatch`. Available inputs have status `ok`.
Existing command schemas, diagnostic codes and SPG schemas are unchanged.

Malformed daemon `diff` requests use `success: false`,
`command: "diff"`, `error.kind: "invalid_comparison_request"` and
`result: null`. This includes missing/false `semantic`, wrong shapes or
types, unknown fields, duplicate modules, invalid module/entry names,
and a missing entry source. A well-formed request with mismatched entry
names or compiler errors instead uses `comparison_unavailable` and the
ordinary comparison payload described above. The daemon keeps serving
after either failure; a failed comparison does not terminate its loop.
`changed` and `unchanged` are both successful responses.

CLI exit codes:

- `0`: comparison completed, with or without differences
- `1`: source errors, mismatched roots or unavailable normalized facts
- `2`: unreadable input (or CLI usage error)
- `3`: internal compiler error

If both sides fail, the highest failure code is returned. Differences
are data in the result, not an unsuccessful command or a compatibility
verdict. Timings remain empty. CLI results are deterministic for the same
input paths, source bytes and compiler version; inline results are
deterministic for the same entry names, module/text sets and compiler
version, independent of source-array order.
