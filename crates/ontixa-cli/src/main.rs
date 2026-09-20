//! `ontixa` — the Ontixa developer tool.
//!
//! Commands: `check`, `run`, `tokens`, `ast`, `mir`, `graph`,
//! `explain`. Exit codes are part of the tool contract:
//!
//! - `0` — success (diagnostics, if any, are warnings)
//! - `1` — the source produced error diagnostics
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
use ontixa_diagnostics::{Code, Diagnostic, Severity};
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
    }
}

// ---------- shared plumbing ----------

/// Reads a file, runs `work` on a fresh `Db` inside `catch_unwind`,
/// and returns the source file handle plus `work`'s output. The ICE
/// boundary lives here: any panic inside the pipeline becomes an
/// `I_INTERNAL` diagnostic, never a bare crash.
fn with_db<T>(
    file: &Path,
    work: impl FnOnce(&mut Db, usize) -> T,
) -> Result<(SourceFile, T), CompileFailure> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            return Err(CompileFailure::Io(format!(
                "cannot read {}: {e}",
                file.display()
            )));
        }
    };
    let sf = SourceFile::new(
        FileId::new(0),
        file.display().to_string(),
        Some(file.to_path_buf()),
        text.clone(),
    );
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut db = Db::new();
        let f = db.add_source(text);
        work(&mut db, f)
    }));
    match result {
        Ok(a) => Ok((sf, a)),
        Err(payload) => {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
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
            };
            Err(CompileFailure::Ice(sf, Box::new(d)))
        }
    }
}

/// Compiles, or emits the failure in the active mode and returns its
/// exit code. Sorts diagnostics for deterministic output.
fn compiled(
    file: &Path,
    command: &'static str,
    json: bool,
) -> Result<(SourceFile, Artifacts), ExitCode> {
    match compile(file) {
        Ok((sf, mut a)) => {
            a.diags.sort();
            Ok((sf, a))
        }
        Err(f) => Err(emit_failure(f, command, json)),
    }
}

/// Compiles a file through `Db::compile` — the full artifact path
/// (graph + MIR), for commands that consume them.
fn compile(file: &Path) -> Result<(SourceFile, Artifacts), CompileFailure> {
    with_db(file, |db, f| db.compile_owned(f))
}

/// Checks a file through `Db::check` — diagnostics only, no graph or
/// MIR. The cheap path for `check`.
fn checked(
    file: &Path,
    command: &'static str,
    json: bool,
) -> Result<(SourceFile, CheckReport), ExitCode> {
    match with_db(file, |db, f| db.check(f)) {
        Ok(x) => Ok(x),
        Err(f) => Err(emit_failure(f, command, json)),
    }
}

// ---------- commands ----------

fn check(file: PathBuf, json: bool, timings: bool) -> ExitCode {
    let (sf, report) = match checked(&file, "check", json) {
        Ok(x) => x,
        Err(c) => return c,
    };
    if json {
        let mut e = Envelope::new("check").diagnostics(&report.diags, &sf);
        if timings {
            e = e.timings(&report.timings);
        }
        return e.emit();
    }
    let code = emit_human_diags(&report.diags, &sf);
    if timings {
        print_timings(&report.timings);
    }
    if code == ExitCode::SUCCESS {
        eprintln!("{}: ok", file.display());
    }
    code
}

fn run(file: PathBuf, entry: String, json: bool, timings: bool) -> ExitCode {
    let (sf, a) = match compiled(&file, "run", json) {
        Ok(x) => x,
        Err(c) => return c,
    };
    if a.diags.has_errors() {
        return if json {
            let mut e = Envelope::new("run").diagnostics(&a.diags, &sf);
            if timings {
                e = e.timings(&a.timings);
            }
            e.emit()
        } else {
            emit_human_diags(&a.diags, &sf)
        };
    }
    let interp = ontixa_interpreter::Interp::new(&a.mir, &a.module, &a.interner);
    match interp.run(&entry) {
        Ok(v) => {
            if json {
                let mut e = Envelope::new("run")
                    .diagnostics(&a.diags, &sf)
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
                let mut env = Envelope::new("run").diagnostics(&a.diags, &sf).error(
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
            .diagnostics(&lex_diags, &sf)
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
    let (sf, a) = match compiled(&file, what, json) {
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
            .diagnostics(&a.diags, &sf)
            .result(Json::Object(res))
            .emit();
    }
    let code = emit_human_diags(&a.diags, &sf);
    println!("{}", serde_json::to_string_pretty(&payload).unwrap());
    code
}

fn explain_cmd(file: PathBuf, symbol: Option<String>, json: bool, timings: bool) -> ExitCode {
    let (sf, a) = match compiled(&file, "explain", json) {
        Ok(x) => x,
        Err(c) => return c,
    };
    explain::run(&file, sf, a, symbol.as_deref(), json, timings)
}
