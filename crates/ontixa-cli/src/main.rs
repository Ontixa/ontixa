//! `ontixa` — the Ontixa developer tool.
//!
//! Commands: `check`, `run`, `tokens`, `ast`, `mir`, `graph`,
//! `explain`, `rename`, `patch`, `recover`, `fmt`. Exit codes are part of the
//! tool contract:
//!
//! - `0` — success (diagnostics, if any, are warnings)
//! - `1` — source error diagnostics or an unresolved recovery conflict
//! - `2` — runtime trap, missing entry function, or unreadable input
//! - `3` — internal compiler error (ICE); never the user's fault
//!
//! Machine output: every `--json` invocation prints **exactly one**
//! envelope document (`schema: 1`) — see `envelope.rs` and
//! `docs/diagnostics.md`.

use clap::{Parser, Subcommand};
use ontixa_cli::envelope::{
    CompileFailure, Envelope, emit_failure, emit_human_diags, print_timings,
};
use ontixa_cli::explain;
use ontixa_db::{Artifacts, CheckReport, Db};
use ontixa_diagnostics::{Code, Diagnostic, Diagnostics, Severity};
use ontixa_interpreter::Value;
use ontixa_source::{FileId, SourceFile, Span};
use serde_json::{Value as Json, json};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "ontixa",
    version,
    about = "The Ontixa compiler and developer tool",
    long_about = "Ontixa is a semantic-first systems language. This tool checks, \
                  runs, and introspects `.ixa` programs."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Compile a file and report diagnostics.
    Check {
        /// The `.ixa` source file.
        file: PathBuf,
        /// Emit machine-readable JSON (one envelope document).
        #[arg(long)]
        json: bool,
        /// Include per-stage compilation timings.
        #[arg(long)]
        timings: bool,
    },
    /// Compile and execute a function (default `main`).
    Run {
        /// The `.ixa` source file.
        file: PathBuf,
        /// Entry function name.
        #[arg(long, default_value = "main")]
        entry: String,
        /// Emit machine-readable JSON (one envelope document).
        #[arg(long)]
        json: bool,
        /// Include per-stage compilation timings.
        #[arg(long)]
        timings: bool,
    },
    /// Dump the token stream.
    Tokens {
        /// The `.ixa` source file.
        file: PathBuf,
        /// Emit machine-readable JSON (one envelope document).
        #[arg(long)]
        json: bool,
    },
    /// Dump the canonical AST.
    Ast {
        /// The `.ixa` source file.
        file: PathBuf,
        /// Emit the schema-1 envelope (payload under `result.ast`).
        /// Without it, prints the bare AST JSON for humans.
        #[arg(long)]
        json: bool,
    },
    /// Dump typed MIR.
    Mir {
        /// The `.ixa` source file.
        file: PathBuf,
        /// Emit the schema-1 envelope (payload under `result.mir`).
        #[arg(long)]
        json: bool,
    },
    /// Dump the Semantic Program Graph.
    Graph {
        /// The `.ixa` source file.
        file: PathBuf,
        /// Emit the schema-1 envelope (payload under `result.graph`).
        /// Without it, prints the bare graph JSON for humans.
        #[arg(long)]
        json: bool,
    },
    /// Explain what the compiler inferred: signatures, ownership
    /// contracts — or a single symbol when named.
    Explain {
        /// The `.ixa` source file.
        file: PathBuf,
        /// Optional symbol to explain (def, param, local, field).
        symbol: Option<String>,
        /// Emit machine-readable JSON (one envelope document).
        #[arg(long)]
        json: bool,
        /// Include per-stage compilation timings.
        #[arg(long)]
        timings: bool,
    },
    /// Preview or apply a semantic rename across the workspace.
    /// Edits are found through the compiler's own name resolution —
    /// the decl site, `use` paths, and every call/type/literal path
    /// that resolves to the definition — never text matching.
    Rename {
        /// The `.ixa` source file (workspace root).
        file: PathBuf,
        /// What to rename: a top-level definition (`name` or
        /// `module::name`), or `@<byte-offset>` to select the
        /// parameter/`let` binding at that position.
        symbol: String,
        /// The new name — must be a valid identifier that does not
        /// collide with an existing binding.
        new_name: String,
        /// Validate and write the renamed files to disk. Without
        /// this flag the command only previews the planned edits.
        #[arg(long)]
        apply: bool,
        /// Emit machine-readable JSON (one envelope document).
        #[arg(long)]
        json: bool,
    },
    /// Preview or apply a structured semantic patch — a bounded op
    /// list (item ops `replace_body`/`remove_def`/`add_def`,
    /// signature ops `rename_param`/`set_param_type`/`set_ret_type`,
    /// `use` ops `add_use`/`remove_use`, and field ops `add_field`/
    /// `remove_field`/`rename_field`/`set_field_type`) resolved
    /// through the workspace scope, validated by a shadow compile,
    /// and committed atomically like a rename. The spec is JSON:
    /// `{"ops": [...]}` or a bare op array.
    Patch {
        /// The `.ixa` source file (workspace root).
        file: PathBuf,
        /// The patch spec: `-` reads stdin, `@path` reads a file, an
        /// argument starting with `{` or `[` is inline JSON, and
        /// anything else is read as a spec file path.
        spec: String,
        /// Validate and write the patched files to disk. Without
        /// this flag the command only previews the planned edits.
        #[arg(long)]
        apply: bool,
        /// Emit machine-readable JSON (one envelope document).
        #[arg(long)]
        json: bool,
    },
    /// Resolve a pending source-transaction journal under a
    /// workspace directory — finishes a committed transaction or
    /// rolls one back that never swapped. Safe to run on a clean
    /// tree; reports `clean` when nothing is pending.
    Recover {
        /// The workspace directory to inspect for `.ontixa-tx-*.journal`.
        dir: PathBuf,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Format `.ixa` files canonically — a transform over the
    /// lossless CST, so comments are preserved and the result is
    /// idempotent. Without flags, prints each file's canonical form
    /// (the preview — nothing is written); `--write` overwrites in
    /// place through the staged transaction engine; `--check`
    /// reports which files would change and exits non-zero.
    Fmt {
        /// The `.ixa` source files.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Report files whose canonical form differs (one path per
        /// line) and exit non-zero; never writes.
        #[arg(long, conflicts_with = "write")]
        check: bool,
        /// Write the canonical form back to each changed file.
        #[arg(long)]
        write: bool,
        /// Emit machine-readable JSON (one envelope document).
        #[arg(long)]
        json: bool,
    },
}

/// What `main` returns: the process exit code.
fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Check {
            file,
            json,
            timings,
        } => check(file, json, timings),
        Cmd::Run {
            file,
            entry,
            json,
            timings,
        } => run(file, entry, json, timings),
        Cmd::Tokens { file, json } => tokens(file, json),
        Cmd::Ast { file, json } => dump(file, json, "ast"),
        Cmd::Mir { file, json } => dump(file, json, "mir"),
        Cmd::Graph { file, json } => dump(file, json, "graph"),
        Cmd::Explain {
            file,
            symbol,
            json,
            timings,
        } => explain_cmd(file, symbol, json, timings),
        Cmd::Rename {
            file,
            symbol,
            new_name,
            apply,
            json,
        } => rename_cmd(file, symbol, new_name, apply, json),
        Cmd::Patch {
            file,
            spec,
            apply,
            json,
        } => patch_cmd(file, spec, apply, json),
        Cmd::Recover { dir, json } => recover_cmd(dir, json),
        Cmd::Fmt {
            files,
            check,
            write,
            json,
        } => fmt_cmd(files, check, write, json),
    }
}

// ---------- shared plumbing ----------

/// Reads a file and its workspace (every sibling `.ixa`), runs
/// `work` on a fresh `Db` inside `catch_unwind`, and returns the
/// source files plus `work`'s output. The ICE boundary lives here:
/// any panic inside the pipeline becomes an `I_INTERNAL` diagnostic,
/// never a bare crash.
///
/// The workspace registers eagerly: each file provides a module
/// named by its stem, which `use m;` declarations resolve against.
/// Only files reachable through `use`s enter the compiled scope.
fn with_db<T>(
    file: &Path,
    work: impl FnOnce(&mut Db, usize) -> T,
) -> Result<(Vec<SourceFile>, T), CompileFailure> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            return Err(CompileFailure::Io(format!(
                "cannot read {}: {e}",
                file.display()
            )));
        }
    };
    let mut sources = Vec::new();
    for (path, sib_text) in ontixa_cli::workspace::workspace_files(file) {
        let text = match sib_text {
            Some(t) => t,
            None => text.clone(), // the root — already read above
        };
        sources.push((ontixa_cli::workspace::module_name(&path), path, text));
    }
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut db = Db::new();
        let mut sfs = Vec::with_capacity(sources.len());
        for (module, path, text) in sources {
            let f = db.add_source_named(module, text.clone());
            sfs.push(SourceFile::new(
                FileId::new(f as u32),
                path.display().to_string(),
                Some(path),
                text,
            ));
        }
        (sfs, work(&mut db, 0))
    }));
    match result {
        Ok(a) => Ok(a),
        Err(payload) => Err(ice_failure(file, text, &payload)),
    }
}

/// The `I_INTERNAL` diagnostic for a panic that crossed the compile
/// boundary, packaged as a [`CompileFailure::Ice`] (exit 3). `file`
/// and `text` identify the input being processed when it panicked.
fn ice_failure(file: &Path, text: String, payload: &(dyn std::any::Any + Send)) -> CompileFailure {
    let msg = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic".into());
    let sf = SourceFile::new(
        FileId::new(0),
        file.display().to_string(),
        Some(file.to_path_buf()),
        text,
    );
    let d = Diagnostic {
        code: Code::Internal,
        severity: Severity::Error,
        message: format!("internal compiler error: {msg}"),
        primary: Some(Span::empty(0)),
        labels: Vec::new(),
        notes: Vec::new(),
        help: vec![
            "this is a compiler bug — please report it at \
             https://github.com/Ontixa/ontixa/issues"
                .to_string(),
        ],
        subject: None,
        details: Default::default(),
        origin: None,
        file: None,
    };
    CompileFailure::Ice(sf, Box::new(d))
}

/// Compiles, or emits the failure in the active mode and returns its
/// exit code. Sorts diagnostics for deterministic output.
fn compiled(
    file: &Path,
    command: &'static str,
    json: bool,
) -> Result<(Vec<SourceFile>, Artifacts), ExitCode> {
    match compile(file) {
        Ok((sfs, mut a)) => {
            a.diags.sort();
            Ok((sfs, a))
        }
        Err(f) => Err(emit_failure(f, command, json)),
    }
}

/// Compiles a workspace through `Db::compile` — the full artifact
/// path (graph + MIR), for commands that consume them.
fn compile(file: &Path) -> Result<(Vec<SourceFile>, Artifacts), CompileFailure> {
    with_db(file, |db, f| db.compile_owned(f))
}

/// Checks a workspace through `Db::check` — diagnostics only, no
/// graph or MIR. The cheap path for `check`.
fn checked(
    file: &Path,
    command: &'static str,
    json: bool,
) -> Result<(Vec<SourceFile>, CheckReport), ExitCode> {
    match with_db(file, |db, f| db.check(f)) {
        Ok(x) => Ok(x),
        Err(f) => Err(emit_failure(f, command, json)),
    }
}

// ---------- commands ----------

fn check(file: PathBuf, json: bool, timings: bool) -> ExitCode {
    let (sfs, report) = match checked(&file, "check", json) {
        Ok(x) => x,
        Err(c) => return c,
    };
    if json {
        let mut e = Envelope::new("check").diagnostics(&report.diags, &sfs);
        if timings {
            e = e.timings(&report.timings);
        }
        return e.emit();
    }
    let code = emit_human_diags(&report.diags, &sfs);
    if timings {
        print_timings(&report.timings);
    }
    if code == ExitCode::SUCCESS {
        eprintln!("{}: ok", file.display());
    }
    code
}

fn run(file: PathBuf, entry: String, json: bool, timings: bool) -> ExitCode {
    let (sfs, a) = match compiled(&file, "run", json) {
        Ok(x) => x,
        Err(c) => return c,
    };
    if a.diags.has_errors() {
        return if json {
            let mut e = Envelope::new("run").diagnostics(&a.diags, &sfs);
            if timings {
                e = e.timings(&a.timings);
            }
            e.emit()
        } else {
            emit_human_diags(&a.diags, &sfs)
        };
    }
    let interp = ontixa_interpreter::Interp::new(&a.mir, &a.module, &a.interner);
    match interp.run(&entry) {
        Ok(v) => {
            if json {
                let mut e = Envelope::new("run")
                    .diagnostics(&a.diags, &sfs)
                    .result(json!({
                        "entry": entry,
                        "value": value_json(&v, &a),
                        "display": interp.show(&v),
                    }));
                if timings {
                    e = e.timings(&a.timings);
                }
                e.emit()
            } else {
                println!("{}", interp.show(&v));
                if timings {
                    print_timings(&a.timings);
                }
                ExitCode::SUCCESS
            }
        }
        Err(e) => {
            if json {
                let mut env = Envelope::new("run").diagnostics(&a.diags, &sfs).error(
                    "runtime",
                    e.to_string(),
                    2,
                );
                if timings {
                    env = env.timings(&a.timings);
                }
                env.emit()
            } else {
                eprintln!("error: {e}");
                ExitCode::from(2)
            }
        }
    }
}

/// A runtime value as JSON: scalars natively, structs as
/// `{data: name, fields: {field: value}}`.
/// serde_json cannot represent bare `i128` — emit a number when the
/// value fits 64 bits, else a string.
fn int_json(i: i128) -> Json {
    if let Ok(v) = i64::try_from(i) {
        json!(v)
    } else if let Ok(v) = u64::try_from(i) {
        json!(v)
    } else {
        json!(i.to_string())
    }
}

fn value_json(v: &Value, a: &Artifacts) -> Json {
    match v {
        Value::Int(i) => int_json(*i),
        Value::Float(f) => json!(f),
        Value::Str(s) => json!(s.as_ref()),
        Value::Char(c) => json!(c.to_string()),
        Value::Bool(b) => json!(b),
        Value::Unit => Json::Null,
        Value::Hole => json!("<uninitialized>"),
        Value::Struct(d, fields) => {
            let name = a
                .interner
                .resolve(a.module.scope.symbols.get(a.module.scope.def(*d).name).name)
                .to_string();
            let mut fmap = serde_json::Map::new();
            if let Some(shape) = a.module.scope.data_shape(*d) {
                for (fdef, cell) in shape.fields.iter().zip(fields.iter()) {
                    let fname = a
                        .interner
                        .resolve(a.module.scope.symbols.get(fdef.symbol).name)
                        .to_string();
                    fmap.insert(fname, value_json(&cell.borrow(), a));
                }
            }
            json!({"data": name, "fields": Json::Object(fmap)})
        }
        Value::Array(elems) => {
            Json::Array(elems.iter().map(|c| value_json(&c.borrow(), a)).collect())
        }
        Value::Variant { def, tag, payload } => {
            let name = a
                .interner
                .resolve(
                    a.module
                        .scope
                        .symbols
                        .get(a.module.scope.def(*def).name)
                        .name,
                )
                .to_string();
            let variant = a
                .module
                .scope
                .data_shape(*def)
                .and_then(|s| s.variants.get(*tag as usize))
                .map(|v| {
                    a.interner
                        .resolve(a.module.scope.symbols.get(v.symbol).name)
                        .to_string()
                })
                .unwrap_or_else(|| tag.to_string());
            json!({
                "data": name,
                "variant": variant,
                "payload": Json::Array(
                    payload.iter().map(|c| value_json(&c.borrow(), a)).collect()
                ),
            })
        }
    }
}

fn tokens(file: PathBuf, json: bool) -> ExitCode {
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            return emit_failure(
                CompileFailure::Io(format!("cannot read {}: {e}", file.display())),
                "tokens",
                json,
            );
        }
    };
    let sf = SourceFile::new(
        FileId::new(0),
        file.display().to_string(),
        Some(file.clone()),
        text.clone(),
    );
    let (toks, lex_diags) = ontixa_syntax::lex(&text);
    if json {
        let items: Vec<Json> = toks
            .iter()
            .map(|t| {
                json!({
                    "kind": format!("{:?}", t.kind),
                    "start": t.start,
                    "end": t.end,
                    "text": &text[t.start as usize..t.end as usize],
                })
            })
            .collect();
        return Envelope::new("tokens")
            .diagnostics(&lex_diags, std::slice::from_ref(&sf))
            .result(json!({"tokens": items}))
            .emit();
    }
    for t in &toks {
        let text = &text[t.start as usize..t.end as usize];
        println!("{:>6}..{:<6} {:?} {text:?}", t.start, t.end, t.kind);
    }
    if !lex_diags.is_empty() {
        let diags: Vec<Diagnostic> = lex_diags.iter().cloned().collect();
        eprint!("{}", ontixa_diagnostics::render_all(&diags, &sf));
    }
    if lex_diags.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Serializes an artifact to a `serde_json::Value`. Some artifacts
/// contain maps with non-string keys (`DefId`, `SymbolId`), which
/// `to_value` rejects — round-tripping through `to_string` normalizes
/// them to string keys.
fn to_json_value<T: serde::Serialize>(v: &T) -> Json {
    serde_json::to_string(v)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Json::Null)
}

/// `ast` / `mir` / `graph` — same contract, different payload.
fn dump(file: PathBuf, json: bool, what: &'static str) -> ExitCode {
    let (sfs, a) = match compiled(&file, what, json) {
        Ok(x) => x,
        Err(c) => return c,
    };
    let payload = match what {
        "ast" => to_json_value(&a.ast),
        "mir" => to_json_value(&a.mir),
        _ => to_json_value(&a.graph),
    };
    if json {
        let mut res = serde_json::Map::new();
        res.insert(what.to_string(), payload);
        return Envelope::new(what)
            .diagnostics(&a.diags, &sfs)
            .result(Json::Object(res))
            .emit();
    }
    let code = emit_human_diags(&a.diags, &sfs);
    println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    code
}

fn explain_cmd(file: PathBuf, symbol: Option<String>, json: bool, timings: bool) -> ExitCode {
    let (sfs, a) = match compiled(&file, "explain", json) {
        Ok(x) => x,
        Err(c) => return c,
    };
    explain::run(&file, sfs, a, symbol.as_deref(), json, timings)
}

/// `rename`: plan → (preview | validate+apply+write). Rejections are
/// diagnostics — never a mutation.
fn rename_cmd(
    file: PathBuf,
    symbol: String,
    new_name: String,
    apply: bool,
    json: bool,
) -> ExitCode {
    // `@<byte-offset>` selects a body-local binding or parameter
    // positionally; anything else resolves as a top-level symbol.
    let at = symbol.strip_prefix('@').and_then(|s| s.parse::<u32>().ok());
    let (sfs, outcome) = match with_db(&file, |db, f| {
        let planned = match at {
            Some(off) => db.plan_rename_at(f, f, off, &new_name),
            None => db.plan_rename(f, &symbol, &new_name),
        };
        match planned {
            Err(e) => Err(e),
            Ok(plan) if apply => db.apply_rename(&plan).map(|r| (plan, Some(r))),
            Ok(plan) => Ok((plan, None)),
        }
    }) {
        Ok(x) => x,
        Err(f) => return emit_failure(f, "rename", json),
    };
    match outcome {
        Err(e) => {
            let diags = e.diagnostics();
            if json {
                let mut env = Envelope::new("rename");
                for d in &diags {
                    env = env.extra_diagnostic(d, &sfs);
                }
                env.emit()
            } else {
                eprint!("{}", ontixa_diagnostics::render_all_in(&diags, &sfs));
                ExitCode::from(1)
            }
        }
        Ok((plan, report)) => {
            if let Some(rep) = &report {
                // Apply landed in the Db — persist every touched
                // file; a mid-write IO failure rolls back to the
                // captured originals.
                if let Err(e) = ontixa_cli::rename::persist(&plan, &sfs) {
                    return emit_failure(CompileFailure::Io(e), "rename", json);
                }
                if json {
                    return Envelope::new("rename")
                        .diagnostics(&rep.diags, &sfs)
                        .result(ontixa_cli::rename::applied_json(&plan, rep, &sfs))
                        .emit();
                }
                let code = emit_human_diags(&rep.diags, &sfs);
                println!(
                    "renamed {} → {}: {} edit(s), {} file(s) written",
                    plan.old_name(),
                    plan.new_name(),
                    rep.edits,
                    rep.files.len(),
                );
                return code;
            }
            if json {
                return Envelope::new("rename")
                    .result(ontixa_cli::rename::plan_json(&plan, &sfs, false))
                    .emit();
            }
            ontixa_cli::rename::print_preview(&plan, &sfs);
            eprintln!("dry run — pass --apply to write the changes");
            ExitCode::SUCCESS
        }
    }
}

/// Reads a patch spec argument into its raw text: `-` reads stdin,
/// `@path` reads a file, an argument starting with `{`/`[` is inline
/// JSON, and anything else is read as a spec file path.
fn patch_spec_text(spec: &str) -> Result<String, CompileFailure> {
    let io = |e: std::io::Error, what: String| CompileFailure::Io(format!("{what}: {e}"));
    if spec == "-" {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| io(e, "cannot read patch spec from stdin".into()))?;
        return Ok(buf);
    }
    if let Some(path) = spec.strip_prefix('@') {
        return std::fs::read_to_string(path)
            .map_err(|e| io(e, format!("cannot read patch spec {path}")));
    }
    if spec.trim_start().starts_with(['{', '[']) {
        return Ok(spec.to_string());
    }
    std::fs::read_to_string(spec).map_err(|e| io(e, format!("cannot read patch spec {spec}")))
}

/// Emits a spec-level failure (`E_MALFORMED_PATCH`) in the active
/// mode. `sfs` gives the diagnostic a file context to render
/// against — the spec errors carry no spans.
fn patch_spec_failure(d: &Diagnostic, sfs: &[SourceFile], json: bool) -> ExitCode {
    if json {
        Envelope::new("patch").extra_diagnostic(d, sfs).emit()
    } else {
        eprint!(
            "{}",
            ontixa_diagnostics::render_all_in(std::slice::from_ref(d), sfs)
        );
        ExitCode::from(1)
    }
}

/// `patch`: read spec → parse ops → plan → (preview | apply+persist).
/// Rejections are diagnostics — never a mutation; an applied plan
/// persists through the same staged, journaled transaction engine
/// `rename --apply` uses.
fn patch_cmd(file: PathBuf, spec: String, apply: bool, json: bool) -> ExitCode {
    // Read the root up front: spec diagnostics need a file context
    // to render against (the engine keeps them spanless).
    let root_text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            return emit_failure(
                CompileFailure::Io(format!("cannot read {}: {e}", file.display())),
                "patch",
                json,
            );
        }
    };
    let root_sf = [SourceFile::new(
        FileId::new(0),
        file.display().to_string(),
        Some(file.clone()),
        root_text,
    )];
    let spec_text = match patch_spec_text(&spec) {
        Ok(t) => t,
        Err(f) => return emit_failure(f, "patch", json),
    };
    let ops = match serde_json::from_str::<Json>(&spec_text)
        .map_err(|e| {
            Box::new(Diagnostic::error(
                Code::MalformedPatch,
                format!("patch spec is not valid JSON: {e}"),
            ))
        })
        .and_then(|spec| ontixa_cli::patch::parse_ops(&spec))
    {
        Ok(ops) => ops,
        Err(d) => return patch_spec_failure(&d, &root_sf, json),
    };
    let (sfs, outcome) = match with_db(&file, |db, f| match db.plan_patch(f, &ops) {
        Err(e) => Err(e),
        Ok(plan) if apply => db.apply_patch(&plan).map(|r| (plan, Some(r))),
        Ok(plan) => Ok((plan, None)),
    }) {
        Ok(x) => x,
        Err(f) => return emit_failure(f, "patch", json),
    };
    match outcome {
        Err(e) => {
            let diags = e.diagnostics();
            if json {
                let mut env = Envelope::new("patch");
                for d in &diags {
                    env = env.extra_diagnostic(d, &sfs);
                }
                env.emit()
            } else {
                eprint!("{}", ontixa_diagnostics::render_all_in(&diags, &sfs));
                ExitCode::from(1)
            }
        }
        Ok((plan, report)) => {
            if let Some(rep) = &report {
                // Apply landed in the Db — persist every touched
                // file through the staged transaction engine.
                if let Err(e) = ontixa_cli::patch::persist(&plan, &sfs) {
                    return emit_failure(CompileFailure::Io(e), "patch", json);
                }
                if json {
                    return Envelope::new("patch")
                        .diagnostics(&rep.diags, &sfs)
                        .result(ontixa_cli::patch::applied_json(&plan, rep, &sfs))
                        .emit();
                }
                let code = emit_human_diags(&rep.diags, &sfs);
                println!(
                    "applied {} op(s): {} edit(s), {} file(s) written",
                    rep.ops,
                    rep.edits,
                    rep.files.len(),
                );
                return code;
            }
            if json {
                return Envelope::new("patch")
                    .result(ontixa_cli::patch::plan_json(&plan, &sfs, false))
                    .emit();
            }
            ontixa_cli::patch::print_preview(&plan, &sfs);
            eprintln!("dry run — pass --apply to write the changes");
            ExitCode::SUCCESS
        }
    }
}

/// `ontixa recover <dir>` — resolves pending `.ontixa-tx-*.journal`
/// files left by a persistence transaction that died in-flight.
/// Idempotent; a clean tree reports `clean` and exits 0. A
/// `conflict` exits non-zero — the journal, staged bytes and
/// backups are left in place for manual resolution.
fn recover_cmd(dir: PathBuf, json: bool) -> ExitCode {
    use ontixa_cli::persist::RecoveryOutcome;
    let outcomes = ontixa_cli::persist::recover(&dir);
    let conflict = outcomes
        .iter()
        .any(|o| matches!(o, RecoveryOutcome::Conflict { .. }));
    if json {
        let docs: Vec<serde_json::Value> = outcomes
            .iter()
            .map(|o| match o {
                RecoveryOutcome::Clean => serde_json::json!({"status": "clean"}),
                RecoveryOutcome::Committed { tx, files } => serde_json::json!({
                    "status": "committed", "tx": tx, "files": files,
                }),
                RecoveryOutcome::RolledBack { tx, files } => serde_json::json!({
                    "status": "rolled_back", "tx": tx, "files": files,
                }),
                RecoveryOutcome::Conflict { tx, path } => serde_json::json!({
                    "status": "conflict", "tx": tx, "path": path,
                }),
            })
            .collect();
        let env = Envelope::new("recover").result(serde_json::json!({ "outcomes": docs }));
        return if conflict {
            env.error("conflict", "recovery has unresolved conflicts", 1)
                .emit()
        } else {
            env.emit()
        };
    }
    for o in &outcomes {
        match o {
            RecoveryOutcome::Clean => println!("clean — no pending transaction"),
            RecoveryOutcome::Committed { tx, files } => {
                println!("committed {tx}: {files} file(s) finalized");
            }
            RecoveryOutcome::RolledBack { tx, files } => {
                println!("rolled back {tx}: {files} file(s) untouched");
            }
            RecoveryOutcome::Conflict { tx, path } => {
                eprintln!(
                    "conflict {tx}: {} matches neither the pre- nor \
                     post-transaction snapshot — journal preserved",
                    path.display()
                );
            }
        }
    }
    if conflict {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// `ontixa fmt <files...>` — canonical formatting over the lossless
/// CST ([`ontixa_syntax::format_file`]). Formatting is per-file and
/// syntactic — no workspace is loaded.
///
/// * default: each file's canonical form goes to stdout — the
///   preview; nothing is written;
/// * `--check`: paths that would change go to stdout (one per line)
///   and the exit code is 1 — the file gate for CI;
/// * `--write`: changed files persist in one staged, journaled
///   transaction ([`ontixa_cli::persist`]) — the disk stale guard
///   refuses to overwrite external edits.
///
/// A file with parse diagnostics is never rewritten: its diagnostics
/// surface tagged with the file and the command exits 1. `--check`'s
/// "would change" verdict is `result.would_change` in JSON — the
/// envelope stays `success: true` (the command ran) while the exit
/// code carries the gate result.
fn fmt_cmd(files: Vec<PathBuf>, check: bool, write: bool, json: bool) -> ExitCode {
    let mut sfs: Vec<SourceFile> = Vec::new();
    let mut diags = Diagnostics::new();
    let mut results = Vec::with_capacity(files.len());
    for path in &files {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                return emit_failure(
                    CompileFailure::Io(format!("cannot read {}: {e}", path.display())),
                    "fmt",
                    json,
                );
            }
        };
        let fid = FileId::new(sfs.len() as u32);
        // The formatter sits behind the same ICE boundary as the
        // pipeline: a panic becomes I_INTERNAL, never a bare crash.
        let parsed = match catch_unwind(AssertUnwindSafe(|| ontixa_syntax::format_file(&text))) {
            Ok(r) => r,
            Err(payload) => return emit_failure(ice_failure(path, text, &payload), "fmt", json),
        };
        sfs.push(SourceFile::new(
            fid,
            path.display().to_string(),
            Some(path.clone()),
            text.clone(),
        ));
        match parsed {
            Ok(formatted) => results.push(ontixa_cli::fmt::FileResult {
                file: path.display().to_string(),
                changed: Some(formatted != text),
                formatted: Some(formatted),
                written: false,
            }),
            Err(d) => {
                let mark = diags.len();
                diags.extend(d);
                diags.tag_file_from(mark, fid);
                results.push(ontixa_cli::fmt::FileResult {
                    file: path.display().to_string(),
                    changed: None,
                    formatted: None,
                    written: false,
                });
            }
        }
    }
    let would_change = results.iter().any(|r| r.changed == Some(true));

    // `--write` commits every changed file in one staged transaction;
    // files that failed to parse are simply absent from it.
    if write {
        let tx_files: Vec<ontixa_cli::persist::TxFile> = results
            .iter()
            .enumerate()
            .filter(|(_, r)| r.changed == Some(true))
            .map(|(i, r)| ontixa_cli::persist::TxFile {
                path: files[i].clone(),
                before: sfs[i].text().as_bytes().to_vec(),
                after: r.formatted.clone().unwrap().into_bytes(),
            })
            .collect();
        if !tx_files.is_empty() {
            let root = ontixa_cli::persist::tx_root(tx_files.iter().map(|t| t.path.as_path()))
                .unwrap_or_else(|| PathBuf::from("."));
            if let Err(e) = ontixa_cli::persist::persist_tx(&root, &tx_files) {
                return emit_failure(CompileFailure::Io(e.to_string()), "fmt", json);
            }
            for r in &mut results {
                if r.changed == Some(true) {
                    r.written = true;
                }
            }
        }
    }

    if json {
        let env = Envelope::new("fmt")
            .diagnostics(&diags, &sfs)
            .result(ontixa_cli::fmt::result_json(&results));
        let (doc, code) = env.into_parts();
        println!("{}", serde_json::to_string_pretty(&doc).unwrap());
        // `--check` failing is a verdict, not a command error — the
        // envelope keeps `success: true`; the exit code gates CI.
        let code = if code == 0 && check && would_change {
            1
        } else {
            code
        };
        return ExitCode::from(code);
    }

    let mut code = emit_human_diags(&diags, &sfs);
    if check {
        for r in results.iter().filter(|r| r.changed == Some(true)) {
            println!("{}", r.file);
        }
        if would_change && code == ExitCode::SUCCESS {
            code = ExitCode::from(1);
        }
        return code;
    }
    if write {
        for r in results.iter().filter(|r| r.written) {
            eprintln!("{}: formatted", r.file);
        }
        return code;
    }
    for r in &results {
        if let Some(text) = &r.formatted {
            print!("{text}");
        }
    }
    code
}
