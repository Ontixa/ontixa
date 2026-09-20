//! Semantic rename transactions: plan → validate → apply.
//!
//! A rename is a *transaction* over the workspace, not a text
//! substitution:
//!
//! - **plan** resolves the symbol through the workspace scope (the
//!   same name resolution bodies use), scans every reachable file's
//!   AST for sites that resolve to the target definition — the decl
//!   name, `use` paths, call/type/struct-literal paths — and produces
//!   one [`RenameEdit`] per name token.
//! - **validate** rejects an invalid new name, a name that would
//!   collide with an existing binding in any affected file, or a
//!   rename whose *shadow-compile* (a scratch `Db` built from the
//!   edited sources) produces diagnostics the original did not.
//! - **apply** checks the workspace revision is still the one the
//!   plan was computed against (`E_STALE_REVISION` otherwise) and
//!   installs every edited file in a single `set_sources` revision
//!   bump — failed validation never mutates, and there is no
//!   partial mutation: either all edits land or none do.
//!
//! Aliased imports (`use m::x as y`) keep their local name: only the
//! path's member segment rewrites, references through `y` are
//! untouched. Unaliased imports (`use m::x`) rebind under the new
//! name, so bare `x` references in that file rewrite too.

use std::sync::Arc;

use ontixa_ast::{AstModule, Expr, Item, Path, Stmt};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics};
use ontixa_hir::{FileEnv, ModuleScope};
use ontixa_source::{DefId, DefKey, FileId, InternId, Interner, Span};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::db::Db;
use crate::query::{QueryKey, Value};

/// One textual replacement: the name token at `span` in `file`
/// becomes `replace`. Spans are absolute within their file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RenameEdit {
    /// The file containing the site.
    pub file: FileId,
    /// Byte range of the name token to rewrite.
    pub span: Span,
    /// Replacement text (the new name).
    pub replace: String,
}

/// A validated rename: the edits and the revision they were
/// computed against. Apply with [`Db::apply_rename`]; pass `revision`
/// back over the wire for the stale guard.
#[derive(Debug, Clone)]
pub struct RenamePlan {
    /// The workspace root the plan was computed under.
    pub root: usize,
    /// The symbol as written (`x` or `m::x`).
    pub symbol: String,
    /// The target's current name.
    pub old_name: String,
    /// The requested new name.
    pub new_name: String,
    /// Root-aware identity of the renamed definition.
    pub target: DefKey,
    /// `Db::revision` at plan time — the apply-time stale guard.
    pub revision: u64,
    /// Every edit, sorted by `(file, span.start)` — the preview.
    pub edits: Vec<RenameEdit>,
    /// `(file, post-edit text)` for every touched file — what apply
    /// installs atomically.
    pub new_sources: Vec<(usize, String)>,
}

/// What an applied rename reports back.
#[derive(Debug)]
pub struct RenameReport {
    /// Files whose text changed.
    pub files: Vec<usize>,
    /// Number of name tokens rewritten.
    pub edits: usize,
    /// Post-rename diagnostics for the workspace (a fresh `check`).
    pub diags: Diagnostics,
    /// The revision the rename landed at.
    pub revision: u64,
}

/// Why a rename was rejected. Every variant maps to diagnostics so
/// CLI and daemon output stay on the schema-1 contract.
#[derive(Debug)]
pub enum RenameError {
    /// The symbol resolved to nothing (`E_UNKNOWN_SYMBOL`).
    UnknownSymbol(Box<Diagnostic>),
    /// A bare name matched defs in several modules (`E_AMBIGUOUS_SYMBOL`).
    AmbiguousSymbol(Box<Diagnostic>),
    /// The new name is not a valid identifier (`E_INVALID_NAME`).
    InvalidName(Box<Diagnostic>),
    /// The new name is already bound in an affected file (`E_NAME_CONFLICT`).
    Conflict(Box<Diagnostic>),
    /// The shadow-compile produced diagnostics the original workspace
    /// did not (`E_RENAME_REJECTED`, plus the offending diagnostics).
    ValidationFailed(Box<Diagnostic>, Vec<Diagnostic>),
    /// The workspace changed between plan and apply (`E_STALE_REVISION`).
    Stale {
        /// Revision the plan was computed at.
        planned: u64,
        /// Revision at apply time.
        current: u64,
    },
}

impl RenameError {
    /// All diagnostics describing the rejection.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        match self {
            RenameError::UnknownSymbol(d)
            | RenameError::AmbiguousSymbol(d)
            | RenameError::InvalidName(d)
            | RenameError::Conflict(d) => vec![d.as_ref().clone()],
            RenameError::ValidationFailed(d, extra) => {
                let mut v = vec![d.as_ref().clone()];
                v.extend(extra.iter().cloned());
                v
            }
            RenameError::Stale { planned, current } => vec![Diagnostic::error(
                Code::StaleRevision,
                format!(
                    "workspace changed since the rename was planned \
                     (revision {planned} → {current}); re-plan and retry"
                ),
            )],
        }
    }
}

impl Db {
    /// Plans and validates a rename of `symbol` (a def name, `x` or
    /// `m::x`) to `new_name` under the workspace rooted at `root`.
    /// Pure — the `Db` is only read (demands may populate memos, but
    /// no source changes).
    pub fn plan_rename(
        &mut self,
        root: usize,
        symbol: &str,
        new_name: &str,
    ) -> Result<RenamePlan, RenameError> {
        if !is_ident(new_name) {
            return Err(RenameError::InvalidName(Box::new(
                Diagnostic::error(
                    Code::InvalidName,
                    format!("`{new_name}` is not a valid identifier"),
                )
                .subject(new_name.to_string()),
            )));
        }
        let scope = self.scope(root);
        let target = resolve_target(&scope, &self.interner, root, symbol)?;
        let def_file = scope.def(target).file;
        let old_name = self
            .interner
            .resolve(scope.symbols.get(scope.def(target).name).name)
            .to_string();

        // Scan every reachable file: `use` member paths, the decl
        // site, and every expr/type path that resolves to `target`.
        let mut asts: FxHashMap<FileId, Arc<AstModule>> = FxHashMap::default();
        let mut edits: Vec<RenameEdit> = Vec::new();
        for &f in &scope.files {
            let ast = self.ast_of(f);
            edits.extend(scan_file(f, &ast, &scope, &self.interner, target, new_name));
            asts.insert(f, ast);
        }
        edits.sort_by_key(|e| (e.file, e.span.start));
        edits.dedup_by_key(|e| (e.file, e.span.start, e.span.end));

        // The new name must not already bind in the declaring file's
        // env, nor in any file that will gain an unaliased `new`
        // binding (`use m::x` → `use m::new`).
        check_conflicts(&scope, &asts, &self.interner, def_file, target, new_name)?;

        // Splice edits into post-edit sources (per file, descending
        // spans so earlier offsets stay valid).
        let mut by_file: FxHashMap<FileId, Vec<&RenameEdit>> = FxHashMap::default();
        for e in &edits {
            by_file.entry(e.file).or_default().push(e);
        }
        let mut new_sources = Vec::new();
        for (f, mut file_edits) in by_file {
            file_edits.sort_by_key(|e| std::cmp::Reverse(e.span.start));
            let mut text = self.source(f.index()).to_string();
            for e in file_edits {
                text.replace_range(e.span.start as usize..e.span.end as usize, &e.replace);
            }
            new_sources.push((f.index(), text));
        }
        new_sources.sort_by_key(|(f, _)| *f);

        // Shadow-compile: build a scratch Db from the edited sources
        // and demand its diagnostics. Any error the original
        // workspace did not already produce rejects the rename.
        let baseline = error_signature(&self.check(root).diags);
        let mut scratch = Db::new();
        for i in 0..self.file_count() {
            let f = FileId::new(i as u32);
            let text = new_sources
                .iter()
                .find(|(ef, _)| *ef == f.index())
                .map(|(_, t)| t.clone())
                .unwrap_or_else(|| self.source(f.index()).to_string());
            scratch.add_source_named(self.file_module(f).to_string(), text);
        }
        let shadow = scratch.check(root);
        let fresh: Vec<Diagnostic> = shadow
            .diags
            .iter()
            .filter(|d| {
                d.severity == ontixa_diagnostics::Severity::Error
                    && !baseline.contains_key(&(d.code, d.file))
            })
            .cloned()
            .collect();
        if !fresh.is_empty() {
            let mut d = Diagnostic::error(
                Code::RenameRejected,
                format!(
                    "renaming `{old_name}` to `{new_name}` would produce \
                     {} new error(s); workspace unchanged",
                    fresh.len()
                ),
            )
            .subject(format!("{symbol} → {new_name}"));
            for e in &fresh {
                d = d.label(
                    e.primary.unwrap_or(Span::new(0, 0)),
                    format!("{}: {}", e.code.as_str(), e.message),
                );
            }
            return Err(RenameError::ValidationFailed(Box::new(d), fresh));
        }

        Ok(RenamePlan {
            root,
            symbol: symbol.to_string(),
            old_name,
            new_name: new_name.to_string(),
            target: scope.def_key(target),
            revision: self.revision,
            edits,
            new_sources,
        })
    }

    /// Applies a planned rename: stale-guard first, then all edited
    /// files land in one revision. Validation ran at plan time —
    /// a matching revision means the inputs are identical, so the
    /// plan is still valid.
    pub fn apply_rename(&mut self, plan: &RenamePlan) -> Result<RenameReport, RenameError> {
        if self.revision != plan.revision {
            return Err(RenameError::Stale {
                planned: plan.revision,
                current: self.revision,
            });
        }
        let files: Vec<usize> = plan.new_sources.iter().map(|(f, _)| *f).collect();
        let edits = plan.edits.len();
        self.set_sources(&plan.new_sources);
        let report = self.check(plan.root);
        Ok(RenameReport {
            files,
            edits,
            diags: report.diags,
            revision: self.revision,
        })
    }

    /// A file's AST via the memoized `Ast` query.
    fn ast_of(&mut self, f: FileId) -> Arc<AstModule> {
        match self.demand(QueryKey::Ast(f)) {
            Value::Ast(a) => a,
            _ => unreachable!("Ast produced wrong value"),
        }
    }
}

/// `(code, file)` multiset of error diagnostics — what the
/// shadow-compile must not grow.
fn error_signature(diags: &Diagnostics) -> FxHashMap<(Code, Option<FileId>), usize> {
    let mut sig = FxHashMap::default();
    for d in diags {
        if d.severity == ontixa_diagnostics::Severity::Error {
            *sig.entry((d.code, d.file)).or_insert(0) += 1;
        }
    }
    sig
}

/// `new_name` must lex as exactly one identifier token — keywords
/// and anything with trailing junk are rejected.
fn is_ident(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let (toks, diags) = ontixa_syntax::lex(name);
    diags.is_empty()
        && matches!(toks.as_slice(), [t] if t.kind == ontixa_syntax::SyntaxKind::IDENT
            && t.start == 0
            && t.end as usize == name.len())
}

/// Resolves the rename target through the workspace scope. `m::x`
/// selects module `m`'s own member `x`; a bare `x` resolves through
/// the root file's env (own defs, then imports), falling back to a
/// workspace-wide def search that reports ambiguity.
fn resolve_target(
    scope: &ModuleScope,
    interner: &Interner,
    root: usize,
    symbol: &str,
) -> Result<DefId, RenameError> {
    let unknown = || {
        RenameError::UnknownSymbol(Box::new(
            Diagnostic::error(Code::UnknownSymbol, format!("no symbol named `{symbol}`"))
                .subject(symbol.to_string()),
        ))
    };
    if let Some((m, member)) = symbol.split_once("::") {
        let mi = interner.get(m).ok_or_else(unknown)?;
        let file = scope
            .files
            .iter()
            .find(|f| scope.file_name(**f) == Some(mi))
            .copied()
            .ok_or_else(unknown)?;
        let member_id = interner.get(member).ok_or_else(unknown)?;
        let env = scope.env(file).expect("reachable file has env");
        let def = env
            .fns
            .get(&member_id)
            .or_else(|| env.datas.get(&member_id))
            .copied()
            .ok_or_else(unknown)?;
        return Ok(def);
    }
    let id = interner.get(symbol).ok_or_else(unknown)?;
    let root_file = FileId::new(root as u32);
    let env = scope.env(root_file).expect("root env");
    if let Some(&def) = env
        .fns
        .get(&id)
        .or_else(|| env.datas.get(&id))
        .or_else(|| env.imports.get(&id))
    {
        return Ok(def);
    }
    // Workspace-wide fallback: a bare name may be a def in a module
    // the root does not import (or several — ambiguous).
    let mut found: Vec<DefId> = scope
        .files
        .iter()
        .filter_map(|f| scope.env(*f))
        .filter_map(|e| e.fns.get(&id).or_else(|| e.datas.get(&id)))
        .copied()
        .collect();
    found.sort();
    found.dedup();
    match found.len() {
        1 => Ok(found[0]),
        0 => Err(unknown()),
        _ => {
            let names: Vec<String> = found
                .iter()
                .map(|d| {
                    let def = scope.def(*d);
                    let module = scope
                        .file_name(def.file)
                        .map(|n| interner.resolve(n))
                        .unwrap_or("?");
                    format!("{module}::{symbol}")
                })
                .collect();
            let mut d = Diagnostic::error(
                Code::AmbiguousSymbol,
                format!(
                    "`{symbol}` is ambiguous: {} candidates — qualify as `m::{symbol}`",
                    found.len()
                ),
            )
            .subject(symbol.to_string());
            for n in &names {
                d = d.label(Span::new(0, 0), n.clone());
            }
            Err(RenameError::AmbiguousSymbol(Box::new(d)))
        }
    }
}

/// Rejects the rename when `new_name` is already bound in the
/// declaring file's env or in any file whose unaliased member import
/// of the target would rebind under `new_name`.
fn check_conflicts(
    scope: &ModuleScope,
    asts: &FxHashMap<FileId, Arc<AstModule>>,
    interner: &Interner,
    declaring: FileId,
    target: DefId,
    new_name: &str,
) -> Result<(), RenameError> {
    let new_id = interner.get(new_name);
    let Some(new_id) = new_id else {
        // Not interned anywhere — no binding can collide.
        return Ok(());
    };
    // Files that gain a `new` binding: the declaring file (its own
    // def renames) and every file with `use m::old` unaliased.
    // Iterate `scope.files` — discovery order keeps the first
    // reported conflict deterministic.
    let mut affected: Vec<FileId> = vec![declaring];
    for &f in &scope.files {
        if f == declaring {
            continue;
        }
        let ast = &asts[&f];
        for u in &ast.uses {
            if u.alias.is_none() && use_targets(scope, interner, u, target) {
                affected.push(f);
            }
        }
    }
    for f in affected {
        let env = scope.env(f).expect("affected file has env");
        let clash = env
            .fns
            .get(&new_id)
            .or_else(|| env.datas.get(&new_id))
            .or_else(|| env.imports.get(&new_id))
            .copied();
        let Some(existing) = clash else { continue };
        if existing == target {
            continue;
        }
        let module = scope
            .file_name(f)
            .map(|n| interner.resolve(n).to_string())
            .unwrap_or_else(|| "?".to_string());
        let span = binding_span(scope, asts, interner, f, new_id)
            .unwrap_or_else(|| scope.def(target).span);
        let mut d = Diagnostic::error(
            Code::NameConflict,
            format!("name `{new_name}` is already bound in `{module}`"),
        )
        .primary(span)
        .subject(new_name.to_string());
        // The span indexes `f`'s text — tag it so multi-file
        // rendering picks the right source.
        d.file = Some(f);
        return Err(RenameError::Conflict(Box::new(d)));
    }
    Ok(())
}

/// The site that binds `name` in `file`: the existing def's name
/// span (rebased absolute) or the `use` decl's binding segment.
fn binding_span(
    scope: &ModuleScope,
    asts: &FxHashMap<FileId, Arc<AstModule>>,
    interner: &Interner,
    file: FileId,
    name: InternId,
) -> Option<Span> {
    let env = scope.env(file)?;
    if let Some(&def) = env.fns.get(&name).or_else(|| env.datas.get(&name)) {
        let d = scope.def(def);
        let base = asts.get(&d.file)?.items[d.item as usize].span().start;
        return Some(d.span.abs(base));
    }
    let ast = asts.get(&file)?;
    let want = interner.resolve(name);
    for u in &ast.uses {
        if let Some(b) = u.alias.as_ref().or_else(|| u.path.segs.last()) {
            if b.name == want {
                return Some(b.span);
            }
        }
    }
    None
}

/// Whether a `use` decl `m::x` names `target`. The module segment
/// of a `use` path resolves through the *workspace module table*
/// (file stems), not `env.modules` — `use dep::x` does not itself
/// bind `dep` for the file's bodies.
fn use_targets(
    scope: &ModuleScope,
    interner: &Interner,
    u: &ontixa_ast::UseDecl,
    target: DefId,
) -> bool {
    let [module, member] = u.path.segs.as_slice() else {
        return false;
    };
    let Some(module_file) = module_file_of(scope, interner, &module.name) else {
        return false;
    };
    scope
        .env(module_file)
        .and_then(|e| {
            e.fns
                .get(&interner_lookup(interner, &member.name))
                .or_else(|| e.datas.get(&interner_lookup(interner, &member.name)))
        })
        .is_some_and(|&d| d == target)
}

/// The file providing module `name` in this workspace — the same
/// resolution `use` declarations use (file stems).
fn module_file_of(scope: &ModuleScope, interner: &Interner, name: &str) -> Option<FileId> {
    let id = interner.get(name)?;
    scope
        .files
        .iter()
        .find(|f| scope.file_name(**f) == Some(id))
        .copied()
}

fn interner_lookup(interner: &Interner, text: &str) -> InternId {
    // Scan-time lookups need an id even for names never interned —
    // `get` returns Option; interning-on-read is avoided by treating
    // a miss as "no binding". Use `get` and sentinel-compare instead.
    interner.get(text).unwrap_or(InternId::new(u32::MAX))
}

/// Collects every site in `file`'s AST that resolves to `target`.
fn scan_file(
    file: FileId,
    ast: &AstModule,
    scope: &ModuleScope,
    interner: &Interner,
    target: DefId,
    new_name: &str,
) -> Vec<RenameEdit> {
    Scan::new(file, scope, interner, target, new_name).run(ast)
}

/// Per-file reference scan — bundles the workspace scope, the file's
/// resolver env, and the edits sink so recursive walks stay unary.
struct Scan<'a> {
    file: FileId,
    env: &'a FileEnv,
    scope: &'a ModuleScope,
    interner: &'a Interner,
    target: DefId,
    new_name: &'a str,
    /// Bound names of *unaliased* member imports of the target —
    /// `use m::x` rebinds under the new name, so bare `x` refs
    /// rewrite; `use m::x as y` keeps `y`, so bare `y` refs do not.
    unaliased: FxHashSet<InternId>,
    edits: Vec<RenameEdit>,
}

impl<'a> Scan<'a> {
    fn new(
        file: FileId,
        scope: &'a ModuleScope,
        interner: &'a Interner,
        target: DefId,
        new_name: &'a str,
    ) -> Self {
        Self {
            file,
            env: scope.env(file).expect("scanned file has env"),
            scope,
            interner,
            target,
            new_name,
            unaliased: FxHashSet::default(),
            edits: Vec::new(),
        }
    }

    fn edit(&mut self, span: Span) {
        self.edits.push(RenameEdit {
            file: self.file,
            span,
            replace: self.new_name.to_string(),
        });
    }

    fn run(mut self, ast: &AstModule) -> Vec<RenameEdit> {
        for u in &ast.uses {
            if !use_targets(self.scope, self.interner, u, self.target) {
                continue;
            }
            let member = &u.path.segs[1];
            self.edit(member.span);
            if u.alias.is_none() {
                self.unaliased
                    .insert(interner_lookup(self.interner, &member.name));
            }
        }
        for (i, item) in ast.items.iter().enumerate() {
            // The decl site itself: `(file, item)` is the target's
            // unique identity in the workspace.
            let decl_name = match item {
                Item::Fn(f) => &f.name,
                Item::Data(d) => &d.name,
            };
            let def = self.scope.def(self.target);
            if def.file == self.file && def.item as usize == i {
                self.edit(decl_name.span);
            }
            // Type positions in the declaration head.
            match item {
                Item::Data(d) => {
                    for field in &d.fields {
                        self.site(&field.ty.path);
                    }
                }
                Item::Fn(f) => {
                    for p in &f.params {
                        self.site(&p.ty.path);
                    }
                    if let Some(ret) = &f.ret {
                        self.site(&ret.path);
                    }
                    self.block(&f.body);
                }
            }
        }
        self.edits
    }

    /// Records an edit when `path`'s final segment resolves to the
    /// target. Single-segment names resolve `fns`/`datas` then
    /// `imports` (matching `path_def`); imports-bound names only
    /// rewrite when the import was unaliased.
    fn site(&mut self, path: &Path) {
        match path.segs.as_slice() {
            [one] => {
                let id = interner_lookup(self.interner, &one.name);
                let own = self
                    .env
                    .fns
                    .get(&id)
                    .or_else(|| self.env.datas.get(&id))
                    .copied();
                let hit = match own {
                    Some(d) => d == self.target,
                    None => {
                        self.env.imports.get(&id).is_some_and(|&d| d == self.target)
                            && self.unaliased.contains(&id)
                    }
                };
                if hit {
                    self.edit(one.span);
                }
            }
            [module, member] => {
                let Some(&module_file) = self
                    .env
                    .modules
                    .get(&interner_lookup(self.interner, &module.name))
                else {
                    return;
                };
                let hit = self
                    .scope
                    .env(module_file)
                    .and_then(|e| {
                        e.fns
                            .get(&interner_lookup(self.interner, &member.name))
                            .or_else(|| e.datas.get(&interner_lookup(self.interner, &member.name)))
                    })
                    .is_some_and(|&d| d == self.target);
                if hit {
                    self.edit(member.span);
                }
            }
            _ => {}
        }
    }

    fn block(&mut self, block: &ontixa_ast::Block) {
        for s in &block.stmts {
            match s {
                Stmt::Let { ty, init, .. } => {
                    if let Some(t) = ty {
                        self.site(&t.path);
                    }
                    if let Some(e) = init {
                        self.expr(e);
                    }
                }
                Stmt::Assign { value, .. } => self.expr(value),
                Stmt::Expr { expr, .. } => self.expr(expr),
                Stmt::Return { value, .. } => {
                    if let Some(e) = value {
                        self.expr(e);
                    }
                }
            }
        }
        if let Some(tail) = &block.tail {
            self.expr(tail);
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Call { callee, args, .. } => {
                self.site(callee);
                for a in args {
                    self.expr(a);
                }
            }
            Expr::StructLit { name, fields, .. } => {
                self.site(name);
                for f in fields {
                    self.expr(&f.value);
                }
            }
            Expr::Path { path } => self.site(path),
            Expr::Field { base, .. } => self.expr(base),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Unary { expr, .. } => self.expr(expr),
            Expr::If {
                cond, then, else_, ..
            } => {
                self.expr(cond);
                self.block(then);
                if let Some(e) = else_.as_deref() {
                    self.expr(e);
                }
            }
            Expr::Block { block, .. } => self.block(block),
            // `Var`/`Place` names resolve to locals only — never
            // defs — and `Field`/`FieldInit` names are member names.
            // None are rename sites for a top-level definition.
            Expr::Var { .. } | Expr::Literal { .. } | Expr::Error { .. } => {}
        }
    }
}
