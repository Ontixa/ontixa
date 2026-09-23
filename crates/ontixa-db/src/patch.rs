//! Structured semantic patches: plan → preview → shadow-compile →
//! revision-guarded apply.
//!
//! A patch is the generalized form of the rename transaction
//! (ADR-0014/0015): a *bounded* list of [`PatchOp`]s, each resolved
//! against the workspace scope (the same name resolution bodies use —
//! never text matching), spliced into candidate sources, validated by
//! a shadow compile on a scratch `Db`, and committed through the same
//! provenance-bound atomic apply. Any semantic failure rejects the
//! whole patch with zero writes.
//!
//! The op vocabulary (see `docs/adr/0016` and its addendum):
//!
//! - [`PatchOp::ReplaceBody`] — replace a `fn`'s `{ ... }` body
//!   block; the signature, name, and every caller site are untouched.
//! - [`PatchOp::RemoveDef`] — remove a top-level `fn`/`data`
//!   declaration; references left dangling fail the shadow compile.
//! - [`PatchOp::AddDef`] — append one new top-level item (`fn` or
//!   `data`) to a reachable module's file.
//! - [`PatchOp::RenameParam`] — rename one `fn` parameter: its decl
//!   token plus every body-local `Var`/assign-base site bound to it
//!   (the same site rule as `plan_rename_at`). The name and body
//!   block are otherwise preserved.
//! - [`PatchOp::SetParamType`] — retype one `fn` parameter in place.
//! - [`PatchOp::SetRetType`] — add, replace, or remove a `fn`'s
//!   `-> T` annotation in place.
//! - [`PatchOp::AddUse`] / [`PatchOp::RemoveUse`] — splice one `use`
//!   declaration into / out of a file's `use` block, following the
//!   file's own line conventions.
//! - [`PatchOp::AddField`] / [`PatchOp::RemoveField`] /
//!   [`PatchOp::RenameField`] / [`PatchOp::SetFieldType`] — `data`
//!   field edits. `rename_field` rewrites the decl site plus every
//!   resolved field-access, struct-literal, and assign-place site in
//!   the workspace; the others are local to the declaration.
//!
//! Honest limits: a patch guarantees **compile integrity**, not
//! semantic preservation. Unlike a rename, a patch is *supposed* to
//! change behavior, so there is no binding-correspondence check —
//! the guarantee is that the patched workspace compiles with no new
//! errors, or nothing is applied. Ops cannot reorder items, cannot
//! touch comments or non-item text, and cannot create or delete
//! files. `use` edits are whole-declaration splices — the `use`
//! block's own text — not arbitrary header edits; a comment sitting
//! in a seam an op would have to delete (inside a removed field's
//! leading trivia, between `->` and its type, between a signature
//! and `{`) refuses the op rather than silently dropping the text.

use ontixa_ast::{DataDecl, FnDecl, Item};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics, Severity};
use ontixa_hir::{HirExprKind, HirStmt, ModuleScope, TypeRef};
use ontixa_source::{DefId, FileId, InternId, Span};
use ontixa_types::Ty;
use rustc_hash::FxHashMap;

use crate::db::Db;
use crate::query::{QueryKey, Value};
use crate::rename::{
    RenameEdit, body_stmts, is_ident, local_binding_sites, module_file_of, resolve_target_diag,
    sources_fingerprint,
};

/// The largest op count one patch may carry — the "bounded" in
/// bounded structured edit set.
const MAX_PATCH_OPS: usize = 64;

/// The largest single op payload (`body`/`text`), in bytes.
const MAX_OP_TEXT: usize = 256 * 1024;

/// One structured edit in a patch spec. Ops are data — the CLI and
/// the daemon build them from the JSON spec; [`Db::plan_patch`]
/// resolves each against the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchOp {
    /// Replace the body block (`{ ... }`, braces included) of the
    /// `fn` named by `symbol` (`x` or `m::x`, resolved exactly like
    /// a rename target). The signature is untouched; type errors in
    /// the new body surface through the shadow compile. A `fn` whose
    /// signature is followed by a comment before `{` is refused —
    /// splicing there would drop the comment.
    ReplaceBody {
        /// The target function (`x` or `m::x`).
        symbol: String,
        /// The complete replacement block, braces included.
        body: String,
    },
    /// Remove the top-level `fn`/`data` declaration named by
    /// `symbol`. Callers left referencing it fail the shadow
    /// compile — removal is refused, never dangling.
    RemoveDef {
        /// The target definition (`x` or `m::x`).
        symbol: String,
    },
    /// Append one new top-level item to the file providing `module`
    /// (the workspace root when `module` is `None`). `text` is the
    /// item's complete source: exactly one `fn` or `data`, no `use`
    /// declarations. The module must be `use`-reachable from the
    /// root — a patch never writes a file outside the workspace's
    /// semantic scope.
    AddDef {
        /// Target module (file stem); `None` = the root file.
        module: Option<String>,
        /// The item's complete source text.
        text: String,
    },
    /// Rename the parameter `param` of the `fn` named by `symbol`
    /// to `to`, in place. Every body-local site bound to that
    /// parameter (decl, `Var` references, assign-place bases)
    /// rewrites — the function's name, other params, and body shape
    /// are preserved. `to` already bound anywhere in the body
    /// rejects with `E_NAME_CONFLICT` before the shadow compile.
    RenameParam {
        /// The target function (`x` or `m::x`).
        symbol: String,
        /// The parameter's current name.
        param: String,
        /// The new parameter name.
        to: String,
    },
    /// Replace the declared type of parameter `param` of the `fn`
    /// named by `symbol`. `ty` is type syntax (`i32`, `m::T`,
    /// `[i32]`) — validated by parsing a synthetic parameter, never
    /// spliced verbatim into unchecked text. Callers whose arguments
    /// no longer type-check fail the shadow compile.
    SetParamType {
        /// The target function (`x` or `m::x`).
        symbol: String,
        /// The parameter's current name.
        param: String,
        /// The new declared type.
        ty: String,
    },
    /// Add, replace, or remove the `fn`'s return-type annotation.
    /// `ty` is the new type text; `None` removes `-> T` entirely
    /// (the fn becomes `unit`-returning). Bodies and callers that no
    /// longer check fail the shadow compile.
    SetRetType {
        /// The target function (`x` or `m::x`).
        symbol: String,
        /// The new return type, or `None` to drop the annotation.
        ty: Option<String>,
    },
    /// Add `use path [as alias];` to the file providing `module`
    /// (the workspace root when `module` is `None`). The decl lands
    /// on its own line after the file's last existing `use`, or —
    /// when the file has none — above the first item below any
    /// leading comment banner. `path` is `m` or `m::x`; the `as`
    /// alias is a separate field, not part of `path`. A `use` that
    /// resolves nothing (unknown module/member, duplicate binding)
    /// fails the shadow compile — adding it can also pull a
    /// registered-but-unreachable file into the workspace, exactly
    /// like writing the `use` by hand.
    AddUse {
        /// Target module (file stem); `None` = the root file.
        module: Option<String>,
        /// The import path (`m` or `m::x`).
        path: String,
        /// The `as` alias, when wanted.
        alias: Option<String>,
    },
    /// Remove the `use path [as alias];` declaration from `module`'s
    /// file (`None` = root). `alias` must match the decl's alias
    /// exactly — omitting it matches only an unaliased `use`.
    /// References that lose their binding fail the shadow compile.
    RemoveUse {
        /// Target module (file stem); `None` = the root file.
        module: Option<String>,
        /// The import path as written (`m` or `m::x`).
        path: String,
        /// The `as` alias of the decl to remove.
        alias: Option<String>,
    },
    /// Add field `field: ty` to the `data` named by `symbol`. The
    /// decl lands after the last field, copying the file's field
    /// separator convention (whitespace before the last field's
    /// name). Struct literals missing the new field fail the shadow
    /// compile — `add_field` never invents a value for it.
    AddField {
        /// The target `data` (`x` or `m::x`).
        symbol: String,
        /// The new field's name.
        field: String,
        /// The new field's declared type.
        ty: String,
    },
    /// Remove field `field` from the `data` named by `symbol` —
    /// decl, its leading trivia run, and nothing else. A comment in
    /// that leading seam refuses the op (the comment's attachment is
    /// ambiguous). Sites still referencing the field fail the shadow
    /// compile.
    RemoveField {
        /// The target `data` (`x` or `m::x`).
        symbol: String,
        /// The field's name.
        field: String,
    },
    /// Rename field `field` of the `data` named by `symbol` to `to`.
    /// The decl site plus every site the checker resolved to that
    /// field — `base.field` accesses, `D { field: v }` literals, and
    /// `place.field = v` projections — rewrites, in every reachable
    /// file. Sites the checker could not resolve do not exist on a
    /// clean baseline, so nothing is left dangling silently.
    RenameField {
        /// The target `data` (`x` or `m::x`).
        symbol: String,
        /// The field's current name.
        field: String,
        /// The new field name.
        to: String,
    },
    /// Replace the declared type of field `field` of the `data`
    /// named by `symbol`. Literals and accesses that no longer
    /// type-check fail the shadow compile.
    SetFieldType {
        /// The target `data` (`x` or `m::x`).
        symbol: String,
        /// The field's name.
        field: String,
        /// The new declared type.
        ty: String,
    },
}

/// One textual replacement a patch installs: the bytes `span`
/// covers in `file` become `replace`. An empty span is a pure
/// insertion. `op` indexes back into the patch's op list so a
/// preview can attribute every edit to its op.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PatchEdit {
    /// The file containing the site.
    pub file: FileId,
    /// Byte range replaced (empty = insertion point).
    pub span: Span,
    /// Replacement text.
    pub replace: String,
    /// Index of the op that produced this edit.
    pub op: usize,
    /// The bytes `span` covered at plan time — the apply-time
    /// expected-bytes guard. Private: a consumer can never tamper
    /// with the snapshot the plan was validated against.
    #[serde(skip_serializing)]
    expected: String,
}

/// A validated patch. All fields are private: a plan is only
/// constructible by [`Db::plan_patch`] after validation, so its
/// candidate bytes can never be substituted and it can never be
/// applied to a different database or source snapshot. Apply with
/// [`Db::apply_patch`]; pass `revision` back over the wire for the
/// stale guard.
#[derive(Debug, Clone)]
pub struct PatchPlan {
    /// The workspace root the plan was computed under.
    root: usize,
    /// One-line op summaries, in spec order — the preview's TOC.
    ops: Vec<String>,
    /// `Db::revision` at plan time — the apply-time stale guard.
    revision: u64,
    /// Incarnation of the `Db` that produced this plan — a plan can
    /// never cross database sessions.
    db_id: u64,
    /// Fingerprint of every registered file's module name and raw
    /// source bytes at plan time.
    workspace_fp: u64,
    /// Fingerprint of `new_sources` — proves at apply time the
    /// candidate payload is the one validation approved.
    candidate_fp: u64,
    /// Every edit, sorted by `(file, span.start, op)` — the preview.
    edits: Vec<PatchEdit>,
    /// `(file, post-edit text)` for every touched file — what apply
    /// installs atomically.
    new_sources: Vec<(usize, String)>,
}

impl PatchPlan {
    /// The workspace root the plan was computed under.
    pub fn root(&self) -> usize {
        self.root
    }

    /// One-line op summaries, in spec order.
    pub fn ops(&self) -> &[String] {
        &self.ops
    }

    /// `Db::revision` at plan time — the apply-time stale guard.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Every edit, sorted by `(file, span.start, op)` — the preview.
    pub fn edits(&self) -> &[PatchEdit] {
        &self.edits
    }

    /// `(file, post-edit text)` for every touched file — what apply
    /// installs atomically.
    pub fn new_sources(&self) -> &[(usize, String)] {
        &self.new_sources
    }

    /// The subject label attached to plan diagnostics.
    fn subject(&self) -> String {
        format!("patch: {}", self.ops.join(", "))
    }
}

/// What an applied patch reports back.
#[derive(Debug)]
pub struct PatchReport {
    /// Files whose text changed.
    pub files: Vec<usize>,
    /// Number of ops in the patch.
    pub ops: usize,
    /// Number of text edits spliced.
    pub edits: usize,
    /// Post-patch diagnostics for the workspace (a fresh `check`).
    pub diags: Diagnostics,
    /// The revision the patch landed at.
    pub revision: u64,
}

/// Why a patch was rejected. Every variant maps to diagnostics so
/// CLI and daemon output stay on the schema-1 contract.
#[derive(Debug)]
pub enum PatchError {
    /// A `symbol` resolved to nothing (`E_UNKNOWN_SYMBOL`).
    UnknownSymbol(Box<Diagnostic>),
    /// A bare `symbol` matched defs in several modules
    /// (`E_AMBIGUOUS_SYMBOL`).
    AmbiguousSymbol(Box<Diagnostic>),
    /// An `add_def` named a module no reachable file provides
    /// (`E_UNKNOWN_MODULE`).
    UnknownModule(Box<Diagnostic>),
    /// The op cannot apply to the resolved target — e.g.
    /// `replace_body` on a `data` def, a field op on a `fn`, or a
    /// comment sitting in a seam the op would have to drop
    /// (`E_UNSUPPORTED_TARGET`).
    Unsupported(Box<Diagnostic>),
    /// A `to` name is already bound in the scope the op would
    /// rewrite — `rename_param` refuses to capture a body-local
    /// binding (`E_NAME_CONFLICT`).
    Conflict(Box<Diagnostic>),
    /// The spec itself is invalid: empty op list, over the op/payload
    /// bound, a `body` that is not a `{ ... }` block, an `add_def`
    /// text that is not exactly one item, or edits that overlap
    /// (`E_MALFORMED_PATCH`).
    Malformed(Box<Diagnostic>),
    /// The shadow-compile produced error diagnostics — the patch is
    /// refused, nothing was applied (`E_PATCH_REJECTED`, plus the
    /// offending diagnostics).
    Rejected(Box<Diagnostic>, Vec<Diagnostic>),
    /// The workspace already reports error diagnostics — patch
    /// requires a clean baseline (`E_BASELINE_ERRORS`).
    BaselineErrors(Box<Diagnostic>),
    /// The plan failed provenance checks at apply time: wrong
    /// database, workspace fingerprint drift, a tampered candidate,
    /// or unexpected bytes under an edit (`E_PLAN_MISMATCH`).
    PlanMismatch(Box<Diagnostic>),
    /// The workspace changed between plan and apply
    /// (`E_STALE_REVISION`).
    Stale {
        /// Revision the plan was computed at.
        planned: u64,
        /// Revision at apply time.
        current: u64,
    },
}

impl PatchError {
    /// All diagnostics describing the rejection.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        match self {
            PatchError::UnknownSymbol(d)
            | PatchError::AmbiguousSymbol(d)
            | PatchError::UnknownModule(d)
            | PatchError::Unsupported(d)
            | PatchError::Conflict(d)
            | PatchError::Malformed(d)
            | PatchError::BaselineErrors(d)
            | PatchError::PlanMismatch(d) => vec![d.as_ref().clone()],
            PatchError::Rejected(d, extra) => {
                let mut v = vec![d.as_ref().clone()];
                v.extend(extra.iter().cloned());
                v
            }
            PatchError::Stale { planned, current } => vec![Diagnostic::error(
                Code::StaleRevision,
                format!(
                    "workspace changed since the patch was planned \
                     (revision {planned} → {current}); re-plan and retry"
                ),
            )],
        }
    }
}

impl Db {
    /// Plans and validates a patch — a bounded list of [`PatchOp`]s —
    /// over the workspace rooted at `root`. Pure: the `Db` is only
    /// read (demands may populate memos, but no source changes).
    ///
    /// Pipeline, cheapest first:
    ///
    /// 1. **spec bounds and shapes** — op count, payload sizes, the
    ///    `{ ... }` block shape of `replace_body`, the single-item
    ///    parse of `add_def`, the type/identifier/`use`-path shapes
    ///    of the member ops (`E_MALFORMED_PATCH`);
    /// 2. **clean baseline** (`E_BASELINE_ERRORS`);
    /// 3. **semantic resolution** — every `symbol` resolves through
    ///    the workspace scope, every `module` names a reachable
    ///    file, params/fields/`use` decls resolve inside their
    ///    resolved target, and `rename_param`'s `to` is free in the
    ///    body it rewrites (`E_UNKNOWN_SYMBOL`, `E_UNKNOWN_FIELD`,
    ///    `E_AMBIGUOUS_SYMBOL`, `E_UNKNOWN_MODULE`,
    ///    `E_UNSUPPORTED_TARGET`, `E_NAME_CONFLICT`);
    /// 4. **overlap** — two ops editing the same span reject
    ///    (`E_MALFORMED_PATCH`);
    /// 5. **shadow compile** — the patched sources must produce zero
    ///    error diagnostics (`E_PATCH_REJECTED`, carrying them).
    pub fn plan_patch(&mut self, root: usize, ops: &[PatchOp]) -> Result<PatchPlan, PatchError> {
        if ops.is_empty() {
            return Err(malformed("the patch contains no operations"));
        }
        if ops.len() > MAX_PATCH_OPS {
            return Err(malformed(format!(
                "the patch has {} operations; the limit is {MAX_PATCH_OPS}",
                ops.len()
            )));
        }
        let mut added = Vec::with_capacity(ops.len());
        for (i, op) in ops.iter().enumerate() {
            added.push(check_op(i, op)?);
        }

        let subject = format!("patch ({} op(s))", ops.len());
        self.check_baseline(root, &subject, "patch")
            .map_err(PatchError::BaselineErrors)?;

        let scope = self.scope(root);
        let mut edits = Vec::new();
        let mut summaries = Vec::with_capacity(ops.len());
        for (i, op) in ops.iter().enumerate() {
            let (es, summary) = self.op_edits(&scope, root, i, op, added[i].as_deref())?;
            edits.extend(es);
            summaries.push(summary);
        }
        check_overlap(&edits)?;

        // Splice order: descending spans per file. Among same-offset
        // insertions (several `add_def`s at one EOF) the *later* op
        // splices first, so op order in the spec becomes op order in
        // the file — `splice_sources`' stable per-file sort keeps it.
        let mut order: Vec<&PatchEdit> = edits.iter().collect();
        order.sort_by(|a, b| b.span.start.cmp(&a.span.start).then(b.op.cmp(&a.op)));
        let triples: Vec<RenameEdit> = order
            .iter()
            .map(|e| RenameEdit {
                file: e.file,
                span: e.span,
                replace: e.replace.clone(),
            })
            .collect();
        let new_sources = self.splice_sources(&triples);

        // Shadow-compile: the baseline is clean, so any error the
        // candidate produces is new and rejects the whole patch.
        let mut scratch = self.shadow_db(&new_sources);
        let fresh: Vec<Diagnostic> = scratch
            .check(root)
            .diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .cloned()
            .collect();
        if !fresh.is_empty() {
            let mut d = Diagnostic::error(
                Code::PatchRejected,
                format!(
                    "applying the patch would produce {} new error(s); \
                     workspace unchanged",
                    fresh.len()
                ),
            )
            .subject(subject);
            for e in &fresh {
                d = d.label(
                    e.primary.unwrap_or(Span::new(0, 0)),
                    format!("{}: {}", e.code.as_str(), e.message),
                );
            }
            return Err(PatchError::Rejected(Box::new(d), fresh));
        }

        // Deterministic preview order.
        edits.sort_by_key(|e| (e.file, e.span.start, e.op));

        Ok(PatchPlan {
            root,
            ops: summaries,
            revision: self.revision,
            db_id: self.id(),
            workspace_fp: self.workspace_fingerprint(),
            candidate_fp: sources_fingerprint(&new_sources),
            edits,
            new_sources,
        })
    }

    /// Applies a validated patch plan. Provenance is re-verified
    /// before any mutation, in the same order as `apply_rename`:
    ///
    /// 1. **database incarnation** (`E_PLAN_MISMATCH`),
    /// 2. **revision** — the stale guard (`E_STALE_REVISION`),
    /// 3. **workspace fingerprint** (`E_PLAN_MISMATCH`),
    /// 4. **candidate fingerprint** (`E_PLAN_MISMATCH`),
    /// 5. **expected bytes** — every edit's span must still cover
    ///    the exact bytes the plan recorded (`E_PLAN_MISMATCH`).
    ///
    /// Only then do all edited files land in one `set_sources`
    /// revision bump — rejection happens before any mutation.
    pub fn apply_patch(&mut self, plan: &PatchPlan) -> Result<PatchReport, PatchError> {
        if plan.db_id != self.id() {
            return Err(PatchError::PlanMismatch(Box::new(
                Diagnostic::error(
                    Code::PlanMismatch,
                    "patch plan was produced by a different database \
                     session; re-plan against this workspace",
                )
                .subject(plan.subject()),
            )));
        }
        if self.revision != plan.revision {
            return Err(PatchError::Stale {
                planned: plan.revision,
                current: self.revision,
            });
        }
        if self.workspace_fingerprint() != plan.workspace_fp {
            return Err(PatchError::PlanMismatch(Box::new(
                Diagnostic::error(
                    Code::PlanMismatch,
                    "workspace contents changed since the patch plan \
                     was validated; re-plan and retry",
                )
                .subject(plan.subject()),
            )));
        }
        if sources_fingerprint(&plan.new_sources) != plan.candidate_fp {
            return Err(PatchError::PlanMismatch(Box::new(
                Diagnostic::error(
                    Code::PlanMismatch,
                    "patch plan's candidate sources differ from the \
                     validated payload; re-plan and retry",
                )
                .subject(plan.subject()),
            )));
        }
        for e in &plan.edits {
            let ok = self
                .source(e.file.index())
                .get(e.span.start as usize..e.span.end as usize)
                .is_some_and(|s| s == e.expected);
            if !ok {
                return Err(PatchError::PlanMismatch(Box::new(
                    Diagnostic::error(
                        Code::PlanMismatch,
                        "edit site no longer covers the bytes the plan \
                         recorded; the plan does not match this source",
                    )
                    .primary(e.span)
                    .subject(plan.subject()),
                )));
            }
        }
        let files: Vec<usize> = plan.new_sources.iter().map(|(f, _)| *f).collect();
        self.set_sources(&plan.new_sources);
        let report = self.check(plan.root);
        Ok(PatchReport {
            files,
            ops: plan.ops.len(),
            edits: plan.edits.len(),
            diags: report.diags,
            revision: self.revision,
        })
    }

    /// Resolves one op into its text edits plus a one-line summary
    /// for the preview. `added` is the item name `check_op` parsed
    /// out of an `add_def` payload.
    fn op_edits(
        &mut self,
        scope: &ModuleScope,
        root: usize,
        seq: usize,
        op: &PatchOp,
        added: Option<&str>,
    ) -> Result<(Vec<PatchEdit>, String), PatchError> {
        match op {
            PatchOp::ReplaceBody { symbol, body } => {
                let (_, file, item) = self.target_def(scope, root, symbol)?;
                let Item::Fn(f) = item else {
                    return Err(PatchError::Unsupported(Box::new(
                        Diagnostic::error(
                            Code::UnsupportedTarget,
                            format!(
                                "`{symbol}` is a data definition — replace_body \
                                 applies to `fn` bodies only"
                            ),
                        )
                        .subject(symbol.clone()),
                    )));
                };
                let src = self.source(file.index());
                // `Block::span` is the syntax node's range — it covers
                // the trivia between the signature and `{`. Trim
                // leading whitespace so the replacement block drops in
                // verbatim; if a comment sits in the seam the site is
                // not editable without touching it — refuse.
                let mut start = f.body.span.start as usize;
                while src
                    .as_bytes()
                    .get(start)
                    .is_some_and(u8::is_ascii_whitespace)
                {
                    start += 1;
                }
                let span = Span::new(start as u32, f.body.span.end);
                if src.as_bytes().get(start) != Some(&b'{') {
                    return Err(PatchError::Unsupported(Box::new(
                        Diagnostic::error(
                            Code::UnsupportedTarget,
                            format!(
                                "`{symbol}` has a comment between its signature \
                                 and body — replace_body would have to drop it; \
                                 move the comment and re-plan"
                            ),
                        )
                        .primary(span)
                        .subject(symbol.clone()),
                    )));
                }
                Ok((
                    vec![PatchEdit {
                        file,
                        span,
                        replace: body.clone(),
                        op: seq,
                        expected: covered(src, span),
                    }],
                    format!("replace_body {symbol}"),
                ))
            }
            PatchOp::RemoveDef { symbol } => {
                let (_, file, item) = self.target_def(scope, root, symbol)?;
                let src = self.source(file.index());
                let mut span = item.span();
                // Swallow one following newline so the removal does
                // not leave a stray blank line. Cosmetic only —
                // `ontixa fmt` owns canonical layout.
                if src[span.end as usize..].starts_with("\r\n") {
                    span = Span::new(span.start, span.end + 2);
                } else if src.as_bytes().get(span.end as usize) == Some(&b'\n') {
                    span = Span::new(span.start, span.end + 1);
                }
                Ok((
                    vec![PatchEdit {
                        file,
                        span,
                        replace: String::new(),
                        op: seq,
                        expected: covered(src, span),
                    }],
                    format!("remove_def {symbol}"),
                ))
            }
            PatchOp::AddDef { module, text } => {
                let file = self.module_file(scope, root, module)?;
                let src = self.source(file.index());
                let at = src.len() as u32;
                // Splice verbatim, normalizing only the seam: the
                // file gets its `\n` terminator and the item gets
                // one trailing newline.
                let mut insert = String::with_capacity(text.len() + 2);
                if !src.is_empty() && !src.ends_with('\n') {
                    insert.push('\n');
                }
                insert.push_str(text.trim());
                insert.push('\n');
                let module = scope
                    .file_name(file)
                    .map(|n| self.interner.resolve(n).to_string())
                    .unwrap_or_else(|| "?".to_string());
                Ok((
                    vec![PatchEdit {
                        file,
                        span: Span::empty(at),
                        replace: insert,
                        op: seq,
                        expected: String::new(),
                    }],
                    format!("add_def {} → {module}", added.unwrap_or("item")),
                ))
            }
            PatchOp::RenameParam { symbol, param, to } => {
                let (def, file, f) = self.fn_target(scope, root, symbol, "rename_param")?;
                let sig = scope.fn_sig(def).expect("an `fn` item has a signature");
                let param_id = self.interner.get(param.as_str());
                let Some(pdef) = param_id.and_then(|id| sig.params.iter().find(|p| p.name == id))
                else {
                    return Err(PatchError::UnknownSymbol(Box::new(
                        Diagnostic::error(
                            Code::UnknownSymbol,
                            format!("`{symbol}` has no parameter `{param}`"),
                        )
                        .subject(format!("{symbol}::{param}")),
                    )));
                };
                let sym = pdef.symbol;
                let key = scope.def_key(def);
                let body = match self.demand(QueryKey::HirBody(key)) {
                    Value::Hir(Some(b)) => b,
                    _ => unreachable!("a resolved `fn` has a lowered body"),
                };
                // `to` must not bind in this body — a same-named
                // `let`/`for` binding would silently capture the
                // rewritten references even though it compiles.
                let to = to.trim();
                if let Some(tid) = self.interner.get(to) {
                    if let Some(rec) = body.local_symbols.iter().find(|s| s.name == tid) {
                        return Err(PatchError::Conflict(Box::new(
                            Diagnostic::error(
                                Code::NameConflict,
                                format!("name `{to}` is already bound in `{symbol}`"),
                            )
                            .primary(rec.span.abs(f.span.start))
                            .subject(to.to_string()),
                        )));
                    }
                }
                let src = self.source(file.index());
                let edits = local_binding_sites(&body, sym, param, src, f.span.start)
                    .into_iter()
                    .map(|span| PatchEdit {
                        file,
                        span,
                        replace: to.to_string(),
                        op: seq,
                        expected: covered(src, span),
                    })
                    .collect();
                Ok((edits, format!("rename_param {symbol} {param} → {to}")))
            }
            PatchOp::SetParamType { symbol, param, ty } => {
                let (_, file, f) = self.fn_target(scope, root, symbol, "set_param_type")?;
                let Some(p) = f.params.iter().find(|p| p.name.name == *param) else {
                    return Err(PatchError::UnknownSymbol(Box::new(
                        Diagnostic::error(
                            Code::UnknownSymbol,
                            format!("`{symbol}` has no parameter `{param}`"),
                        )
                        .subject(format!("{symbol}::{param}")),
                    )));
                };
                let src = self.source(file.index());
                let span = ty_edit_span(src, p.ty.span()).ok_or_else(|| {
                    PatchError::Unsupported(Box::new(
                        unsupported(format!(
                            "`{symbol}`'s parameter `{param}` has a comment inside \
                             its type annotation — move the comment and re-plan"
                        ))
                        .primary(p.ty.span())
                        .subject(symbol.clone()),
                    ))
                })?;
                Ok((
                    vec![PatchEdit {
                        file,
                        span,
                        replace: ty.trim().to_string(),
                        op: seq,
                        expected: covered(src, span),
                    }],
                    format!("set_param_type {symbol} {param}: {}", ty.trim()),
                ))
            }
            PatchOp::SetRetType { symbol, ty } => {
                let (_, file, f) = self.fn_target(scope, root, symbol, "set_ret_type")?;
                let src = self.source(file.index());
                let (span, replace) = match (&f.ret, ty) {
                    (Some(ret), Some(t)) => {
                        let span = ty_edit_span(src, ret.span()).ok_or_else(|| {
                            PatchError::Unsupported(Box::new(
                                unsupported(format!(
                                    "`{symbol}`'s return type has a comment inside \
                                     it — move the comment and re-plan"
                                ))
                                .primary(ret.span())
                                .subject(symbol.clone()),
                            ))
                        })?;
                        (span, t.trim().to_string())
                    }
                    (Some(ret), None) => {
                        // Delete `-> T`: walk back over whitespace
                        // from the type's trimmed start to the
                        // `->` token — anything else (a comment in
                        // the seam) refuses the op.
                        let tspan = ty_edit_span(src, ret.span()).ok_or_else(|| {
                            PatchError::Unsupported(Box::new(
                                unsupported(format!(
                                    "`{symbol}`'s return type has a comment inside \
                                     it — move the comment and re-plan"
                                ))
                                .primary(ret.span())
                                .subject(symbol.clone()),
                            ))
                        })?;
                        let mut p = tspan.start as usize;
                        while p > 0 && src.as_bytes()[p - 1].is_ascii_whitespace() {
                            p -= 1;
                        }
                        let arrow = p >= 2
                            && src.as_bytes()[p - 1] == b'>'
                            && src.as_bytes()[p - 2] == b'-';
                        if !arrow {
                            return Err(PatchError::Unsupported(Box::new(
                                unsupported(format!(
                                    "`{symbol}` has a comment inside its `-> T` \
                                     annotation — move the comment and re-plan"
                                ))
                                .primary(ret.span())
                                .subject(symbol.clone()),
                            )));
                        }
                        // Swallow the inline whitespace before `->`
                        // too so `fn f() -> T` becomes `fn f()`, not
                        // `fn f() `. Line breaks stay — they belong
                        // to the file's own layout.
                        let mut s = p - 2;
                        while s > 0 && matches!(src.as_bytes()[s - 1], b' ' | b'\t') {
                            s -= 1;
                        }
                        (Span::new(s as u32, tspan.end), String::new())
                    }
                    (None, Some(t)) => {
                        // Insert ` -> T` right after the parameter
                        // list's `)` — the byte before the body's
                        // span (whose leading trivia keeps any
                        // `)`-and-`{` comment where the author put it).
                        let mut p = f.body.span.start as usize;
                        while p > 0 && src.as_bytes()[p - 1].is_ascii_whitespace() {
                            p -= 1;
                        }
                        if src.as_bytes().get(p.wrapping_sub(1)) != Some(&b')') {
                            return Err(PatchError::Unsupported(Box::new(
                                unsupported(format!(
                                    "`{symbol}` has no parameter list to attach \
                                     a return type to"
                                ))
                                .primary(f.body.span)
                                .subject(symbol.clone()),
                            )));
                        }
                        (Span::empty(p as u32), format!(" -> {}", t.trim()))
                    }
                    (None, None) => {
                        return Err(PatchError::Unsupported(Box::new(
                            unsupported(format!("`{symbol}` has no return type to remove"))
                                .subject(symbol.clone()),
                        )));
                    }
                };
                Ok((
                    vec![PatchEdit {
                        file,
                        span,
                        replace,
                        op: seq,
                        expected: covered(src, span),
                    }],
                    match ty {
                        Some(t) => format!("set_ret_type {symbol} -> {}", t.trim()),
                        None => format!("set_ret_type {symbol} -> unit"),
                    },
                ))
            }
            PatchOp::AddUse {
                module,
                path,
                alias,
            } => {
                let file = self.module_file(scope, root, module)?;
                let ast = self.ast_of(file);
                let src = self.source(file.index());
                let decl = format!(
                    "use {}{};",
                    norm_path(path),
                    alias
                        .as_deref()
                        .map(|a| format!(" as {}", a.trim()))
                        .unwrap_or_default()
                );
                let nl = line_ending(src);
                let (at, insert) = if let Some(u) = ast.uses.last() {
                    // The line after the last `use` — a trailing
                    // comment on that line stays put.
                    match src[u.span.end as usize..].find('\n') {
                        Some(off) => {
                            let at = u.span.end as usize + off + 1;
                            (at as u32, format!("{decl}{nl}"))
                        }
                        // The last `use` ends an unterminated file.
                        None => (src.len() as u32, format!("{nl}{decl}{nl}")),
                    }
                } else if let Some(first) = ast.items.first() {
                    // No `use` block yet — above the first item,
                    // below any leading comment banner.
                    (first.span().start, format!("{decl}{nl}"))
                } else {
                    (0, format!("{decl}{nl}"))
                };
                let module = scope
                    .file_name(file)
                    .map(|n| self.interner.resolve(n).to_string())
                    .unwrap_or_else(|| "?".to_string());
                Ok((
                    vec![PatchEdit {
                        file,
                        span: Span::empty(at),
                        replace: insert,
                        op: seq,
                        expected: String::new(),
                    }],
                    match alias {
                        Some(a) => format!("add_use {path} as {a} → {module}"),
                        None => format!("add_use {path} → {module}"),
                    },
                ))
            }
            PatchOp::RemoveUse {
                module,
                path,
                alias,
            } => {
                let file = self.module_file(scope, root, module)?;
                let ast = self.ast_of(file);
                let module_name = scope
                    .file_name(file)
                    .map(|n| self.interner.resolve(n).to_string())
                    .unwrap_or_else(|| "?".to_string());
                let want = norm_path(path);
                let u = ast
                    .uses
                    .iter()
                    .find(|u| {
                        u.path.display() == want
                            && u.alias.as_ref().map(|a| a.name.as_str())
                                == alias.as_deref().map(str::trim)
                    })
                    .ok_or_else(|| {
                        let decl = match alias {
                            Some(a) => format!("use {want} as {};", a.trim()),
                            None => format!("use {want};"),
                        };
                        PatchError::UnknownSymbol(Box::new(
                            Diagnostic::error(
                                Code::UnknownSymbol,
                                format!("`{module_name}` has no `{decl}` declaration"),
                            )
                            .subject(decl.clone()),
                        ))
                    })?;
                let src = self.source(file.index());
                let mut span = u.span;
                // Same trailing-newline swallow as `remove_def`.
                if src[span.end as usize..].starts_with("\r\n") {
                    span = Span::new(span.start, span.end + 2);
                } else if src.as_bytes().get(span.end as usize) == Some(&b'\n') {
                    span = Span::new(span.start, span.end + 1);
                }
                Ok((
                    vec![PatchEdit {
                        file,
                        span,
                        replace: String::new(),
                        op: seq,
                        expected: covered(src, span),
                    }],
                    format!("remove_use {path} ← {module_name}"),
                ))
            }
            PatchOp::AddField { symbol, field, ty } => {
                let (_, file, d) = self.data_target(scope, root, symbol, "add_field")?;
                let src = self.source(file.index());
                let decl = format!("{}: {};", field.trim(), ty.trim());
                let (at, insert) = if let Some(last) = d.fields.last() {
                    // Copy the separator in front of the last field —
                    // its whitespace run carries the file's own
                    // newline + indent convention.
                    let mut ws = last.name.span.start as usize;
                    while ws > 0 && src.as_bytes()[ws - 1].is_ascii_whitespace() {
                        ws -= 1;
                    }
                    (
                        last.span.end,
                        format!("{}{decl}", &src[ws..last.name.span.start as usize]),
                    )
                } else {
                    // `data D {}` — find the `{`, skipping comments
                    // in the `D {` seam so they stay put.
                    let brace = find_byte(src, d.name.span.end as usize, d.span.end as usize, b'{')
                        .ok_or_else(|| {
                            PatchError::Unsupported(Box::new(
                                unsupported(format!("`{symbol}` has no field list to add to"))
                                    .subject(symbol.clone()),
                            ))
                        })?;
                    (brace as u32 + 1, format!(" {decl}"))
                };
                Ok((
                    vec![PatchEdit {
                        file,
                        span: Span::empty(at),
                        replace: insert,
                        op: seq,
                        expected: String::new(),
                    }],
                    format!("add_field {symbol} {field}: {}", ty.trim()),
                ))
            }
            PatchOp::RemoveField { symbol, field } => {
                let (_, file, d) = self.data_target(scope, root, symbol, "remove_field")?;
                let fdecl = d
                    .fields
                    .iter()
                    .find(|f| f.name.name == *field)
                    .ok_or_else(|| unknown_field(symbol, field))?;
                let src = self.source(file.index());
                // `field.span` covers the leading trivia run — a
                // comment inside it would be silently dropped, so
                // the op refuses rather than guess at attachment.
                let seam = &src[fdecl.span.start as usize..fdecl.name.span.start as usize];
                if seam.contains("//") || seam.contains("/*") {
                    return Err(PatchError::Unsupported(Box::new(
                        unsupported(format!(
                            "`{symbol}.{field}` has a comment in its leading seam — \
                             remove_field would have to drop it; move the comment \
                             and re-plan"
                        ))
                        .primary(fdecl.span)
                        .subject(symbol.clone()),
                    )));
                }
                Ok((
                    vec![PatchEdit {
                        file,
                        span: fdecl.span,
                        replace: String::new(),
                        op: seq,
                        expected: covered(src, fdecl.span),
                    }],
                    format!("remove_field {symbol} {field}"),
                ))
            }
            PatchOp::RenameField { symbol, field, to } => {
                let (def, file, d) = self.data_target(scope, root, symbol, "rename_field")?;
                let shape = scope.data_shape(def).expect("a `data` item has a shape");
                let field_id = self.interner.get(field.as_str());
                let Some(&index) = field_id.and_then(|id| shape.field_index.get(&id)) else {
                    return Err(unknown_field(symbol, field));
                };
                let to = to.trim();
                let decl_span = d.fields[index as usize].name.span;
                let expected = covered(self.source(file.index()), decl_span);
                let mut edits: Vec<PatchEdit> = vec![PatchEdit {
                    file,
                    span: decl_span,
                    replace: to.to_string(),
                    op: seq,
                    expected,
                }];
                let fname = field_id.expect("the resolved field name is interned");
                for (f2, span) in self.field_sites(scope, def, index, fname) {
                    let s2 = self.source(f2.index());
                    edits.push(PatchEdit {
                        file: f2,
                        span,
                        replace: to.to_string(),
                        op: seq,
                        expected: covered(s2, span),
                    });
                }
                Ok((edits, format!("rename_field {symbol} {field} → {to}")))
            }
            PatchOp::SetFieldType { symbol, field, ty } => {
                let (_, file, d) = self.data_target(scope, root, symbol, "set_field_type")?;
                let fdecl = d
                    .fields
                    .iter()
                    .find(|f| f.name.name == *field)
                    .ok_or_else(|| unknown_field(symbol, field))?;
                let src = self.source(file.index());
                let span = ty_edit_span(src, fdecl.ty.span()).ok_or_else(|| {
                    PatchError::Unsupported(Box::new(
                        unsupported(format!(
                            "`{symbol}.{field}` has a comment inside its type \
                             annotation — move the comment and re-plan"
                        ))
                        .primary(fdecl.ty.span())
                        .subject(symbol.clone()),
                    ))
                })?;
                Ok((
                    vec![PatchEdit {
                        file,
                        span,
                        replace: ty.trim().to_string(),
                        op: seq,
                        expected: covered(src, span),
                    }],
                    format!("set_field_type {symbol} {field}: {}", ty.trim()),
                ))
            }
        }
    }

    /// Resolves `symbol` to its `(DefId, file, AST item)` triple
    /// through the workspace scope — the same resolution a rename
    /// target uses.
    fn target_def(
        &mut self,
        scope: &ModuleScope,
        root: usize,
        symbol: &str,
    ) -> Result<(DefId, FileId, Item), PatchError> {
        let target: DefId =
            resolve_target_diag(scope, &self.interner, root, symbol).map_err(|d| {
                if d.code == Code::AmbiguousSymbol {
                    PatchError::AmbiguousSymbol(d)
                } else {
                    PatchError::UnknownSymbol(d)
                }
            })?;
        let def = scope.def(target);
        let ast = self.ast_of(def.file);
        let item = ast.items[def.item as usize].clone();
        Ok((target, def.file, item))
    }

    /// The `fn` declaration `symbol` resolved to — member ops refuse
    /// a `data` target with `E_UNSUPPORTED_TARGET`.
    fn fn_target(
        &mut self,
        scope: &ModuleScope,
        root: usize,
        symbol: &str,
        op: &str,
    ) -> Result<(DefId, FileId, FnDecl), PatchError> {
        let (def, file, item) = self.target_def(scope, root, symbol)?;
        let Item::Fn(f) = item else {
            return Err(PatchError::Unsupported(Box::new(
                unsupported(format!(
                    "`{symbol}` is a data definition — {op} applies to `fn` targets only"
                ))
                .subject(symbol.to_string()),
            )));
        };
        Ok((def, file, f))
    }

    /// The `data` declaration `symbol` resolved to — field ops
    /// refuse a `fn` target with `E_UNSUPPORTED_TARGET`.
    fn data_target(
        &mut self,
        scope: &ModuleScope,
        root: usize,
        symbol: &str,
        op: &str,
    ) -> Result<(DefId, FileId, DataDecl), PatchError> {
        let (def, file, item) = self.target_def(scope, root, symbol)?;
        let Item::Data(d) = item else {
            return Err(PatchError::Unsupported(Box::new(
                unsupported(format!(
                    "`{symbol}` is a function — {op} applies to `data` targets only"
                ))
                .subject(symbol.to_string()),
            )));
        };
        Ok((def, file, d))
    }

    /// The file a `module`/`None` module field names — `add_def`,
    /// `add_use`, and `remove_use` share this reachability rule.
    fn module_file(
        &mut self,
        scope: &ModuleScope,
        root: usize,
        module: &Option<String>,
    ) -> Result<FileId, PatchError> {
        match module {
            Some(m) => module_file_of(scope, &self.interner, m).ok_or_else(|| {
                PatchError::UnknownModule(Box::new(
                    Diagnostic::error(
                        Code::UnknownModule,
                        format!(
                            "module `{m}` is not reachable from this \
                             workspace's root — a patch can only edit \
                             files in the `use` graph"
                        ),
                    )
                    .subject(m.clone()),
                ))
            }),
            None => Ok(FileId::new(root as u32)),
        }
    }

    /// Every source site bound to `data` field `index` of def
    /// `target`: the decl token in the declaring file plus, in every
    /// reachable file's fns, `base.field` accesses whose checked
    /// base type is `target`, `D { field: v }` initializer names,
    /// and `place.field = v` projections that walk to the field.
    /// `field_name` is the interned written name (the literal and
    /// place walks compare it). Item-relative spans rebase through
    /// each fn's item start — the same rule rename's site scan uses.
    fn field_sites(
        &mut self,
        scope: &ModuleScope,
        target: DefId,
        index: u32,
        field_name: InternId,
    ) -> Vec<(FileId, Span)> {
        let mut sites = Vec::new();
        for &file in &scope.files {
            let ast = self.ast_of(file);
            let Some(env) = scope.env(file) else { continue };
            let mut defs: Vec<DefId> = env.fns.values().copied().collect();
            defs.sort();
            for d in defs {
                let base = ast.items[scope.def(d).item as usize].span().start;
                let checked = match self.demand(QueryKey::BodyTypes(scope.def_key(d))) {
                    Value::Checked(Some(c)) => c,
                    _ => continue,
                };
                for e in &checked.body.exprs {
                    match &e.kind {
                        // `x.f` — resolved by the checker to field
                        // `index` of the type `base` checks to.
                        HirExprKind::Field {
                            base: be,
                            name,
                            field: Some(i),
                        } if *i == index && checked.tables.ty_of(*be) == Ty::Struct(target) => {
                            sites.push((file, name.span.abs(base)));
                        }
                        // `D { f: v, ... }` — the literal's `def` is
                        // the resolved data; written names re-key by
                        // interned name (fields are unique per data).
                        HirExprKind::StructLit { def: sd, fields } if *sd == target => {
                            for (n, _) in fields {
                                if n.id == field_name {
                                    sites.push((file, n.span.abs(base)));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                // `place.f = v` — walk the projection chain through
                // declared types; a hit is the matching field's
                // `Name` token.
                for stmt in body_stmts(&checked.body) {
                    let HirStmt::Assign { target: place, .. } = stmt else {
                        continue;
                    };
                    let mut cur = match checked.tables.local_types.get(&place.base) {
                        Some(Ty::Struct(d)) => Some(*d),
                        _ => None,
                    };
                    for pname in &place.fields {
                        let Some(d2) = cur else { break };
                        let Some(shape) = scope.data_shape(d2) else {
                            break;
                        };
                        let Some(&fi) = shape.field_index.get(&pname.id) else {
                            break;
                        };
                        if d2 == target && fi == index {
                            sites.push((file, pname.span.abs(base)));
                        }
                        // Keep walking — `p.f.f` on a recursive type
                        // holds two tokens bound to the same field.
                        cur = match shape.fields[fi as usize].ty {
                            TypeRef::Struct(nd) => Some(nd),
                            _ => None,
                        };
                    }
                }
            }
        }
        sites
    }
}

/// The bytes `span` covers in `src` — the plan-time snapshot the
/// apply-time expected-bytes guard compares against.
fn covered(src: &str, span: Span) -> String {
    src.get(span.start as usize..span.end as usize)
        .unwrap_or("")
        .to_string()
}

/// An `E_MALFORMED_PATCH` rejection.
fn malformed(message: impl Into<String>) -> PatchError {
    PatchError::Malformed(Box::new(Diagnostic::error(
        Code::MalformedPatch,
        message.into(),
    )))
}

/// Spec-level shape checks on one op — run before any workspace
/// access, like the `is_ident` gate in rename. Returns the declared
/// item's name for `add_def` (the preview uses it), `None` for
/// other ops.
fn check_op(i: usize, op: &PatchOp) -> Result<Option<String>, PatchError> {
    match op {
        PatchOp::ReplaceBody { body, .. } => {
            check_text(i, "replace_body", "body", body)?;
            let t = body.trim();
            if !t.starts_with('{') || !t.ends_with('}') {
                return Err(malformed(format!(
                    "ops[{i}] replace_body: `body` must be a complete `{{ ... }}` block"
                )));
            }
            Ok(None)
        }
        PatchOp::RemoveDef { .. } => Ok(None),
        PatchOp::RemoveField { field, .. } => {
            check_text(i, "remove_field", "field", field)?;
            Ok(None)
        }
        PatchOp::RenameParam { param, to, .. } => {
            check_text(i, "rename_param", "param", param)?;
            check_name(i, "rename_param", "to", to)?;
            Ok(None)
        }
        PatchOp::SetParamType { param, ty, .. } => {
            check_text(i, "set_param_type", "param", param)?;
            check_type_text(i, "set_param_type", ty)?;
            Ok(None)
        }
        PatchOp::SetRetType { ty, .. } => {
            if let Some(t) = ty {
                check_type_text(i, "set_ret_type", t)?;
            }
            Ok(None)
        }
        PatchOp::AddUse { path, alias, .. } => {
            check_use_path(i, "add_use", path)?;
            if let Some(a) = alias {
                check_name(i, "add_use", "as", a)?;
            }
            Ok(None)
        }
        PatchOp::RemoveUse { path, alias, .. } => {
            check_use_path(i, "remove_use", path)?;
            if let Some(a) = alias {
                check_name(i, "remove_use", "as", a)?;
            }
            Ok(None)
        }
        PatchOp::AddField { field, ty, .. } => {
            check_name(i, "add_field", "field", field)?;
            check_type_text(i, "add_field", ty)?;
            Ok(None)
        }
        PatchOp::RenameField { field, to, .. } => {
            check_text(i, "rename_field", "field", field)?;
            check_name(i, "rename_field", "to", to)?;
            Ok(None)
        }
        PatchOp::SetFieldType { field, ty, .. } => {
            check_text(i, "set_field_type", "field", field)?;
            check_type_text(i, "set_field_type", ty)?;
            Ok(None)
        }
        PatchOp::AddDef { text, .. } => {
            check_text(i, "add_def", "text", text)?;
            let (ast, diags) = ontixa_ast::parse_ast(text);
            if let Some(d) = diags.iter().find(|d| d.severity == Severity::Error) {
                return Err(malformed(format!(
                    "ops[{i}] add_def: `text` does not parse — {}",
                    d.message
                )));
            }
            if !ast.uses.is_empty() {
                return Err(malformed(format!(
                    "ops[{i}] add_def: `use` declarations cannot be added by a \
                     patch — qualify paths or edit the file's header instead"
                )));
            }
            let [item] = ast.items.as_slice() else {
                return Err(malformed(format!(
                    "ops[{i}] add_def: `text` must contain exactly one \
                     top-level `fn` or `data` (found {})",
                    ast.items.len()
                )));
            };
            let name = match item {
                Item::Fn(f) => &f.name.name,
                Item::Data(d) => &d.name.name,
            };
            Ok(Some(name.clone()))
        }
    }
}

/// The op payload bound: non-empty and at most `MAX_OP_TEXT` bytes.
fn check_text(i: usize, op: &str, field: &str, text: &str) -> Result<(), PatchError> {
    if text.trim().is_empty() {
        return Err(malformed(format!(
            "ops[{i}] {op}: `{field}` must not be empty"
        )));
    }
    if text.len() > MAX_OP_TEXT {
        return Err(malformed(format!(
            "ops[{i}] {op}: `{field}` is {} bytes; the limit is {MAX_OP_TEXT}",
            text.len()
        )));
    }
    Ok(())
}

/// A new-name payload (`to`, `as`, an added `field`): the bound and
/// the `is_ident` shape — the same gate rename applies to `new_name`.
fn check_name(i: usize, op: &str, field: &str, name: &str) -> Result<(), PatchError> {
    check_text(i, op, field, name)?;
    if !is_ident(name.trim()) {
        return Err(malformed(format!(
            "ops[{i}] {op}: `{field}` must be a single identifier — `{name}` is not"
        )));
    }
    Ok(())
}

/// A `ty` payload must parse as exactly one type — checked by
/// parsing a synthetic `fn f(p: <ty>) {}` so `ty` can never carry
/// trailing text or extra items into the splice.
fn check_type_text(i: usize, op: &str, ty: &str) -> Result<(), PatchError> {
    check_text(i, op, "ty", ty)?;
    let probe = format!("fn __patch(p: {}) {{ }}", ty.trim());
    let (ast, diags) = ontixa_ast::parse_ast(&probe);
    if let Some(d) = diags.iter().find(|d| d.severity == Severity::Error) {
        return Err(malformed(format!(
            "ops[{i}] {op}: `ty` does not parse — {}",
            d.message
        )));
    }
    let ok = matches!(ast.items.as_slice(), [Item::Fn(f)] if f.params.len() == 1);
    if !ok || !ast.uses.is_empty() {
        return Err(malformed(format!(
            "ops[{i}] {op}: `ty` must be a single type (`i32`, `m::T`, `[i32]`)"
        )));
    }
    Ok(())
}

/// A `path` payload for `add_use`/`remove_use` must parse as the
/// path of a `use` decl — `m` or `m::x`, never carrying its own
/// `as` clause or trailing text.
fn check_use_path(i: usize, op: &str, path: &str) -> Result<(), PatchError> {
    check_text(i, op, "path", path)?;
    let probe = format!("use {};", path.trim());
    let (ast, diags) = ontixa_ast::parse_ast(&probe);
    if let Some(d) = diags.iter().find(|d| d.severity == Severity::Error) {
        return Err(malformed(format!(
            "ops[{i}] {op}: `path` does not parse — {}",
            d.message
        )));
    }
    let [u] = ast.uses.as_slice() else {
        return Err(malformed(format!(
            "ops[{i}] {op}: `path` must be a single `use` path"
        )));
    };
    if u.path.segs.len() > 2 || u.alias.is_some() || !ast.items.is_empty() {
        return Err(malformed(format!(
            "ops[{i}] {op}: `path` must be `m` or `m::x` (put an alias \
             in the `as` field, not in `path`)"
        )));
    }
    Ok(())
}

/// An `E_UNSUPPORTED_TARGET` diagnostic builder — the caller adds
/// `subject`/`primary` before boxing it into `PatchError`.
fn unsupported(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(Code::UnsupportedTarget, message.into())
}

/// `E_UNKNOWN_FIELD` — the `data` resolved, the field name did not.
fn unknown_field(symbol: &str, field: &str) -> PatchError {
    PatchError::UnknownSymbol(Box::new(
        Diagnostic::error(
            Code::UnknownField,
            format!("`{symbol}` has no field `{field}`"),
        )
        .subject(format!("{symbol}::{field}")),
    ))
}

/// The span a `ty` payload actually replaces: `span` minus leading
/// whitespace. `None` when a `//`/`/*` comment sits in the seam —
/// splicing there would silently drop it, so the caller refuses.
/// (`TypeExpr::Array` spans ride the CST node and can carry leading
/// trivia; named-type spans are already exact.)
fn ty_edit_span(src: &str, span: Span) -> Option<Span> {
    let mut start = span.start as usize;
    while src
        .as_bytes()
        .get(start)
        .is_some_and(u8::is_ascii_whitespace)
    {
        start += 1;
    }
    let rest = src.get(start..)?;
    if rest.starts_with("//") || rest.starts_with("/*") {
        return None;
    }
    Some(Span::new(start as u32, span.end))
}

/// The file's newline convention — first `"\r\n"` wins, else `"\n"`.
fn line_ending(src: &str) -> &'static str {
    if src.contains("\r\n") { "\r\n" } else { "\n" }
}

/// `path` in canonical `a::b` form — segments trimmed and rejoined
/// (`check_use_path` already proved it parses as a use path).
fn norm_path(path: &str) -> String {
    path.split("::")
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("::")
}

/// The offset of `byte` in `src[from..to]`, skipping `//` and
/// `/* */` comments so a `// {` or `/* { */` in a seam is never
/// mistaken for the real brace. Used to locate `data D`'s `{` when
/// the field list is empty.
fn find_byte(src: &str, from: usize, to: usize, byte: u8) -> Option<usize> {
    let b = src.as_bytes();
    let mut p = from;
    while p < to && p < b.len() {
        match b[p] {
            x if x == byte => return Some(p),
            b'/' if b.get(p + 1) == Some(&b'/') => {
                while p < to && p < b.len() && b[p] != b'\n' {
                    p += 1;
                }
            }
            b'/' if b.get(p + 1) == Some(&b'*') => {
                p += 2;
                while p + 1 < to && p + 1 < b.len() && !(b[p] == b'*' && b[p + 1] == b'/') {
                    p += 1;
                }
                p = (p + 2).min(to).min(b.len());
            }
            _ => p += 1,
        }
    }
    None
}

/// Two edits conflict when their spans overlap within one file —
/// half-open ranges, so an insertion point strictly inside another
/// edit's span rejects while adjacent spans and same-point
/// insertions (ordered by op index) are fine.
fn check_overlap(edits: &[PatchEdit]) -> Result<(), PatchError> {
    let mut by_file: FxHashMap<FileId, Vec<&PatchEdit>> = FxHashMap::default();
    for e in edits {
        by_file.entry(e.file).or_default().push(e);
    }
    for (file, es) in by_file {
        for (i, a) in es.iter().enumerate() {
            for b in es.iter().skip(i + 1) {
                if a.span.start < b.span.end && b.span.start < a.span.end {
                    return Err(malformed(format!(
                        "ops[{}] and ops[{}] edit overlapping spans in file {}",
                        a.op,
                        b.op,
                        file.index()
                    )));
                }
            }
        }
    }
    Ok(())
}
