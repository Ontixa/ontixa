//! The compiler database.
//!
//! [`Db`] owns source text and memoized compilation artifacts per
//! file. Recompilation is *query-shaped* — each pipeline stage runs in
//! order and reports a timing — but invalidation is deliberately
//! coarse in milestone 1: any edit rebuilds the file's whole artifact
//! bundle. ADR-0004 records the salsa migration path for fine-grained
//! early-cutoff incrementality.

use ontixa_ast::AstModule;
use ontixa_diagnostics::Diagnostics;
use ontixa_hir::HirModule;
use ontixa_memory::OwnershipTables;
use ontixa_mir::MirModule;
use ontixa_semantic::SemanticGraph;
use ontixa_source::Interner;
use ontixa_types::TypeTables;
use std::time::Instant;

/// One pipeline stage's measured cost.
#[derive(Debug, Clone, serde::Serialize)]
pub struct StageTiming {
    /// Stage name (`lex+parse`, `ast`, `hir`, `types`, `ownership`,
    /// `graph`, `mir`).
    pub stage: &'static str,
    /// Wall-clock nanoseconds for the last build.
    pub nanos: u64,
}

/// Everything a compiled file produced — the artifacts downstream
/// tools (CLI, daemon, agents) consume.
#[derive(Debug)]
pub struct Artifacts {
    /// Source revision these artifacts were built from.
    pub built_revision: u64,
    /// Canonical AST.
    pub ast: AstModule,
    /// Resolved, lowered HIR (post-typecheck — field indices filled).
    pub module: HirModule,
    /// Interner that `InternId`s in the module resolve against.
    pub interner: Interner,
    /// Per-expression and per-local types.
    pub types: TypeTables,
    /// Inferred ownership contracts.
    pub ownership: OwnershipTables,
    /// Semantic program graph.
    pub graph: SemanticGraph,
    /// Typed MIR.
    pub mir: MirModule,
    /// All diagnostics accumulated across stages.
    pub diags: Diagnostics,
    /// Per-stage build timings.
    pub timings: Vec<StageTiming>,
}

impl Artifacts {
    /// True when no stage produced an error-severity diagnostic.
    pub fn is_valid(&self) -> bool {
        !self.diags.has_errors()
    }
}

struct FileEntry {
    text: String,
    /// Content generation — bumped on every `set_source`.
    revision: u64,
    cached: Option<Artifacts>,
}

/// Query-oriented compiler state. One `Db` is one workspace session.
#[derive(Default)]
pub struct Db {
    files: Vec<FileEntry>,
}

impl Db {
    /// An empty database.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a source file; returns its index.
    pub fn add_source(&mut self, text: impl Into<String>) -> usize {
        self.files.push(FileEntry {
            text: text.into(),
            revision: 0,
            cached: None,
        });
        self.files.len() - 1
    }

    /// Replaces a file's text, invalidating its artifacts.
    pub fn set_source(&mut self, file: usize, text: impl Into<String>) {
        let f = &mut self.files[file];
        f.text = text.into();
        f.revision += 1;
        f.cached = None;
    }

    /// The current source text of a file.
    pub fn source(&self, file: usize) -> &str {
        &self.files[file].text
    }

    /// Compiles (or returns the memoized) artifacts for `file`.
    pub fn compile(&mut self, file: usize) -> &Artifacts {
        let f = &self.files[file];
        let fresh = f
            .cached
            .as_ref()
            .is_some_and(|a| a.built_revision == f.revision);
        if !fresh {
            let revision = f.revision;
            let artifacts = build(&f.text, revision);
            self.files[file].cached = Some(artifacts);
        }
        self.files[file].cached.as_ref().unwrap()
    }

    /// Compiles `file` and returns the artifacts by value, emptying
    /// the cache. For one-shot consumers (the CLI); session consumers
    /// (the daemon) should use [`Db::compile`].
    pub fn compile_owned(&mut self, file: usize) -> Artifacts {
        let _ = self.compile(file);
        self.files[file].cached.take().unwrap()
    }
}

/// Runs the full pipeline, timing each stage.
fn build(src: &str, revision: u64) -> Artifacts {
    let mut timings = Vec::new();
    let mut stage = |name: &'static str, t0: Instant| {
        timings.push(StageTiming {
            stage: name,
            nanos: t0.elapsed().as_nanos() as u64,
        });
    };

    let t = Instant::now();
    let (root, mut diags) = ontixa_syntax::parse_file(src);
    stage("lex+parse", t);

    let t = Instant::now();
    let ast = ontixa_ast::lower_module(&root, &mut diags);
    stage("ast", t);

    let mut interner = Interner::new();
    let t = Instant::now();
    let mut module = ontixa_hir::lower_hir(&ast, &mut interner, &mut diags);
    stage("hir", t);

    let t = Instant::now();
    let types = ontixa_types::check_module(&mut module, &interner, &mut diags);
    stage("types", t);

    let t = Instant::now();
    let ownership = ontixa_memory::infer_ownership(&module, &types, &interner, &mut diags);
    stage("ownership", t);

    let t = Instant::now();
    let graph = ontixa_semantic::build_graph(&module, &types, &ownership, &interner);
    stage("graph", t);

    let t = Instant::now();
    let mir = ontixa_mir::lower_mir(&module, &types, &ownership);
    stage("mir", t);

    Artifacts {
        built_revision: revision,
        ast,
        module,
        interner,
        types,
        ownership,
        graph,
        mir,
        diags,
        timings,
    }
}
