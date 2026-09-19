//! `ontixa` — the Ontixa developer tool.
//!
//! Commands: `check`, `run`, `tokens`, `ast`, `mir`, `graph`,
//! `explain`. Exit codes are part of the tool contract:
//!
//! - `0` — success (diagnostics, if any, are warnings)
//! - `1` — the source produced error diagnostics
//! - `2` — runtime trap (interpreter) or missing entry function
//! - `3` — internal compiler error (ICE); never the user's fault

use clap::{Parser, Subcommand};
use ontixa_db::{Artifacts, Db};
use ontixa_diagnostics::{Code, Diagnostic, Severity};
use ontixa_source::{FileId, SourceFile, Span};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
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
        /// Emit machine-readable JSON diagnostics.
        #[arg(long)]
        json: bool,
        /// Report per-stage compilation timings.
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
        /// Emit the result as JSON.
        #[arg(long)]
        json: bool,
        /// Report per-stage compilation timings.
        #[arg(long)]
        timings: bool,
    },
    /// Dump the token stream.
    Tokens {
        /// The `.ixa` source file.
        file: PathBuf,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },
    /// Dump the canonical AST.
    Ast {
        /// The `.ixa` source file.
        file: PathBuf,
    },
    /// Dump typed MIR.
    Mir {
        /// The `.ixa` source file.
        file: PathBuf,
    },
    /// Dump the Semantic Program Graph as JSON.
    Graph {
        /// The `.ixa` source file.
        file: PathBuf,
    },
    /// Explain what the compiler inferred: signatures, ownership
    /// contracts, timings.
    Explain {
        /// The `.ixa` source file.
        file: PathBuf,
        /// Emit machine-readable JSON.
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
        Cmd::Ast { file } => ast(file),
        Cmd::Mir { file } => mir(file),
        Cmd::Graph { file } => graph(file),
        Cmd::Explain { file, json } => explain(file, json),
    }
}

// ---------- shared plumbing ----------

/// Reads a file, compiles it through `Db` inside `catch_unwind`, and
/// returns the source file handle plus artifacts. `Err(code)` means
/// the output was already emitted.
fn compile(file: &PathBuf) -> Result<(SourceFile, Artifacts), ExitCode> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", file.display());
            return Err(ExitCode::from(2));
        }
    };
    let sf = SourceFile::new(
        FileId::new(0),
        file.display().to_string(),
        Some(file.clone()),
        text.clone(),
    );
    // The ICE boundary: any panic inside the pipeline becomes an
    // `I_INTERNAL` diagnostic, never a bare crash.
    let result = catch_unwind(AssertUnwindSafe(|| {
        let mut db = Db::new();
        let f = db.add_source(text);
        db.compile_owned(f)
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
            };
            eprint!("{}", ontixa_diagnostics::render(&d, &sf));
            Err(ExitCode::from(3))
        }
    }
}

/// Emits diagnostics; returns the exit code reflecting severity.
fn emit_diags(a: &Artifacts, sf: &SourceFile, json: bool) -> ExitCode {
    if a.diags.is_empty() {
        return ExitCode::SUCCESS;
    }
    let diags: Vec<Diagnostic> = a.diags.iter().cloned().collect();
    if json {
        let doc = ontixa_diagnostics::to_json(&diags, sf);
        println!("{}", serde_json::to_string_pretty(&doc).unwrap());
    } else {
        eprint!("{}", ontixa_diagnostics::render_all(&diags, sf));
    }
    if a.diags.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

/// Prints `--timings` output when requested.
fn emit_timings(a: &Artifacts, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(&a.timings).unwrap());
    } else {
        eprintln!("timings:");
        for t in &a.timings {
            eprintln!("  {:>9}  {:>7} µs", t.stage, t.nanos / 1000);
        }
    }
}

// ---------- commands ----------

fn check(file: PathBuf, json: bool, timings: bool) -> ExitCode {
    let (sf, a) = match compile(&file) {
        Ok(x) => x,
        Err(c) => return c,
    };
    let code = emit_diags(&a, &sf, json);
    if timings {
        emit_timings(&a, json);
    }
    if code == ExitCode::SUCCESS && !json {
        eprintln!("{}: ok", file.display());
    }
    code
}

fn run(file: PathBuf, entry: String, json: bool, timings: bool) -> ExitCode {
    let (sf, a) = match compile(&file) {
        Ok(x) => x,
        Err(c) => return c,
    };
    let code = emit_diags(&a, &sf, json);
    if timings {
        emit_timings(&a, json);
    }
    if code != ExitCode::SUCCESS {
        return code;
    }
    let interp = ontixa_interpreter::Interp::new(&a.mir, &a.module, &a.interner);
    match interp.run(&entry) {
        Ok(v) => {
            let shown = interp.show(&v);
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "entry": entry,
                        "value": shown,
                    }))
                    .unwrap()
                );
            } else {
                println!("{shown}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn tokens(file: PathBuf, json: bool) -> ExitCode {
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: cannot read {}: {e}", file.display());
            return ExitCode::from(2);
        }
    };
    let (toks, diags) = ontixa_syntax::lex(&text);
    if json {
        let items: Vec<serde_json::Value> = toks
            .iter()
            .map(|t| {
                serde_json::json!({
                    "kind": format!("{:?}", t.kind),
                    "start": t.start,
                    "end": t.end,
                    "text": &text[t.start as usize..t.end as usize],
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "tokens": items })).unwrap()
        );
    } else {
        for t in &toks {
            let text = &text[t.start as usize..t.end as usize];
            println!("{:>6}..{:<6} {:?} {text:?}", t.start, t.end, t.kind);
        }
    }
    if diags.has_errors() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

fn ast(file: PathBuf) -> ExitCode {
    match compile(&file) {
        Ok((sf, a)) => {
            let code = emit_diags(&a, &sf, true);
            println!("{}", serde_json::to_string_pretty(&a.ast).unwrap());
            code
        }
        Err(c) => c,
    }
}

fn mir(file: PathBuf) -> ExitCode {
    match compile(&file) {
        Ok((sf, a)) => {
            let code = emit_diags(&a, &sf, true);
            println!("{}", serde_json::to_string_pretty(&a.mir).unwrap());
            code
        }
        Err(c) => c,
    }
}

fn graph(file: PathBuf) -> ExitCode {
    match compile(&file) {
        Ok((sf, a)) => {
            let code = emit_diags(&a, &sf, true);
            println!("{}", serde_json::to_string_pretty(&a.graph).unwrap());
            code
        }
        Err(c) => c,
    }
}

fn explain(file: PathBuf, json: bool) -> ExitCode {
    let (sf, a) = match compile(&file) {
        Ok(x) => x,
        Err(c) => return c,
    };
    let code = emit_diags(&a, &sf, json);

    // Per-definition summary: signature + inferred param contracts.
    let mut fns = Vec::new();
    for def in &a.module.scope.defs {
        let name = a
            .interner
            .resolve(a.module.scope.symbols.get(def.name).name)
            .to_string();
        match &def.kind {
            ontixa_hir::DefKind::Function(sig) => {
                let body = a.mir.body(def.id);
                let params: Vec<serde_json::Value> = sig
                    .params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        let pname = a
                            .interner
                            .resolve(a.module.scope.symbols.get(p.symbol).name)
                            .to_string();
                        let behavior = body
                            .and_then(|b| b.param_behaviors.get(i))
                            .map(|b| b.as_str())
                            .unwrap_or("unknown");
                        serde_json::json!({
                            "name": pname,
                            "behavior": behavior,
                        })
                    })
                    .collect();
                fns.push(serde_json::json!({
                    "kind": "fn",
                    "name": name,
                    "params": params,
                }));
            }
            ontixa_hir::DefKind::Data(shape) => {
                let fields: Vec<String> = shape
                    .fields
                    .iter()
                    .map(|f| {
                        a.interner
                            .resolve(a.module.scope.symbols.get(f.symbol).name)
                            .to_string()
                    })
                    .collect();
                fns.push(serde_json::json!({
                    "kind": "data",
                    "name": name,
                    "fields": fields,
                }));
            }
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "file": file.display().to_string(),
                "defs": fns,
                "timings": a.timings,
            }))
            .unwrap()
        );
    } else {
        println!("{}", file.display());
        for d in &fns {
            if d["kind"] == "fn" {
                let params: Vec<String> = d["params"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| {
                        format!(
                            "{}: {}",
                            p["name"].as_str().unwrap(),
                            p["behavior"].as_str().unwrap()
                        )
                    })
                    .collect();
                println!(
                    "  fn {}({})",
                    d["name"].as_str().unwrap(),
                    params.join(", ")
                );
            } else {
                let fields: Vec<&str> = d["fields"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f.as_str().unwrap())
                    .collect();
                println!(
                    "  data {} {{ {} }}",
                    d["name"].as_str().unwrap(),
                    fields.join(", ")
                );
            }
        }
        emit_timings(&a, false);
    }
    code
}
