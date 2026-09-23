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
//! The deliberately minimal op set (see `docs/adr/0016`):
//!
//! - [`PatchOp::ReplaceBody`] — replace a `fn`'s `{ ... }` body
//!   block; the signature, name, and every caller site are untouched.
//! - [`PatchOp::RemoveDef`] — remove a top-level `fn`/`data`
//!   declaration; references left dangling fail the shadow compile.
//! - [`PatchOp::AddDef`] — append one new top-level item (`fn` or
//!   `data`) to a reachable module's file.
//!
//! Honest limits: a patch guarantees **compile integrity**, not
//! semantic preservation. Unlike a rename, a patch is *supposed* to
//! change behavior, so there is no binding-correspondence check —
//! the guarantee is that the patched workspace compiles with no new
//! errors, or nothing is applied. Ops cannot add or remove `use`
//! declarations, cannot reorder items, cannot touch comments or
//! non-item text, and cannot create or delete files.

use ontixa_ast::Item;
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics, Severity};
use ontixa_hir::ModuleScope;
use ontixa_source::{DefId, FileId, Span};
use rustc_hash::FxHashMap;

use crate::db::Db;
use crate::rename::{RenameEdit, module_file_of, resolve_target_diag, sources_fingerprint};

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
    /// `replace_body` on a `data` def (`E_UNSUPPORTED_TARGET`).
    Unsupported(Box<Diagnostic>),
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
    ///    parse of `add_def` (`E_MALFORMED_PATCH`);
    /// 2. **clean baseline** (`E_BASELINE_ERRORS`);
    /// 3. **semantic resolution** — every `symbol` resolves through
    ///    the workspace scope, every `add_def` module names a
    ///    reachable file (`E_UNKNOWN_SYMBOL`, `E_AMBIGUOUS_SYMBOL`,
    ///    `E_UNKNOWN_MODULE`, `E_UNSUPPORTED_TARGET`);
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
                let (file, item) = self.target_item(scope, root, symbol)?;
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
                let (file, item) = self.target_item(scope, root, symbol)?;
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
                let file = match module {
                    Some(m) => module_file_of(scope, &self.interner, m).ok_or_else(|| {
                        PatchError::UnknownModule(Box::new(
                            Diagnostic::error(
                                Code::UnknownModule,
                                format!(
                                    "module `{m}` is not reachable from this \
                                         workspace's root — a patch can only add \
                                         to files in the `use` graph"
                                ),
                            )
                            .subject(m.clone()),
                        ))
                    })?,
                    None => FileId::new(root as u32),
                };
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
        }
    }

    /// Resolves `symbol` to its `(file, AST item)` pair through the
    /// workspace scope — the same resolution a rename target uses.
    fn target_item(
        &mut self,
        scope: &ModuleScope,
        root: usize,
        symbol: &str,
    ) -> Result<(FileId, Item), PatchError> {
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
        Ok((def.file, item))
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
