# ADR-0013: Multi-module workspaces

## Status

Accepted (semantic-workspace campaign, PR #4)

## Context

M2's `Db` compiled one file in isolation: `ModuleScope` held a single
flat env, diagnostics carried no file identity, and `DefKey` was
`(file, name)` — which silently assumes every name lives in the file
being compiled. An agent editing a real project needs `use dep;` and
`dep::f` to resolve across files, diagnostics to point at the right
file, and a symbol's identity to survive the same name existing in
two modules.

## Decision

1. **The file stem is the module name.** `math.ixa` provides module
   `math`; no manifest, no `mod` decl. `use math;` binds the module,
   `use math::Vec2;` binds a member (optionally `as alias`), and
   `m::x` paths qualify a member inline. The workspace of a root file
   is the transitive set of `.ixa` siblings its `use` decls reach —
   the CLI/daemon discover siblings by directory scan; `Db` only
   sees the files that were registered.

2. **`DefKey` is `(root, file, name)`.** The same dep file can join
   two workspaces whose roots intern names differently or resolve
   different envs — per-def query values are workspace-relative, so
   identity must carry the root. `DefId` stays per-`ModuleScope`
   (dense, per-revision); `DefKey` is the memo key.

3. **One `ModuleScope` per workspace, one `FileEnv` per file.**
   `resolve_workspace` builds a file→env map: each file's env holds
   its own defs, its `use`-bound modules, and its imported members.
   Body lowering resolves bare names through locals → env members →
   env imports, and `m::x` through env modules — resolution never
   does text matching.

4. **Diagnostics carry `file`.** Passes emit file-tagged
   diagnostics; renderers pick the `SourceFile` by tag instead of
   assuming one input. Untagged diagnostics fall back to the
   demanded file.

5. **Spans are item-relative (PR #3, consumed here).** `AstItem`
   values rebase every span to the item's own start; `Scope` stores
   `.rel(base)` spans. An offset-shifting edit leaves every
   per-definition value comparing equal — this is what makes
   cross-file edits cheap enough for a workspace.

6. **`Db::set_sources` is the atomic multi-file write.** One
   revision bump installs any number of changed sources — the
   mutation boundary rename transactions need.

## Alternatives considered

- **A module manifest / `mod` tree.** Rejected for the milestone:
  file stems + `use` give the semantic surface without a second
  source of truth. A manifest can layer on later without changing
  `DefKey`.
- **`DefId` as the memo key.** `DefId` renumbers when defs are
  added/removed anywhere in the scope — unstable across revisions.
- **Per-file `Scope` queries.** A def's meaning can depend on a
  sibling's exports (`use m::x`), so scope is workspace-shaped by
  construction; per-def `AstItem`/`HirBody`/`BodyTypes`/`MirBody`
  queries still give per-definition cutoff where it matters.

## Consequences

- `check`/`compile`/`explain`/`graph` all take a workspace root and
  follow `use` edges; single-file programs are the degenerate case.
- Cross-module invalidation is localized: editing one dep re-runs
  that file's file-granular queries plus the edited defs' chains;
  untouched modules and untouched defs evaluate nothing (measured
  in `benchmarks/README.md`, `ws-*` rows).
- The ownership oracle keys facts by `DefKey` (root-scoped), so two
  modules may export same-named functions without collision.
- `roots[file]` maps every registered file to its workspace root;
  top-level demands reset it, keeping multi-workspace `Db`s honest.
