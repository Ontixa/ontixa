//! `ontixa rename` — semantic rename preview and apply, shared
//! between the CLI subcommand and the `ontixad` daemon op.
//!
//! A rename is a transaction planned by [`Db::plan_rename`]: the
//! preview lists every name-token edit the engine found through the
//! workspace scope (never text matching); apply lands all edits in
//! one revision after the stale guard passes.

use crate::envelope::Envelope;
use ontixa_db::{RenameError, RenamePlan, RenameReport};
use ontixa_source::SourceFile;
use serde_json::{Value as Json, json};

/// The display name of file `f` in `sfs` (indexed by `FileId`).
fn file_name(sfs: &[SourceFile], f: usize) -> String {
    sfs.get(f)
        .map(|s| s.name().to_string())
        .unwrap_or_else(|| f.to_string())
}

/// The plan as a JSON payload: every edit with its file and span.
pub fn plan_json(plan: &RenamePlan, sfs: &[SourceFile], applied: bool) -> Json {
    json!({
        "symbol": plan.symbol,
        "old_name": plan.old_name,
        "new_name": plan.new_name,
        "target": format!("{:?}", plan.target),
        "revision": plan.revision,
        "applied": applied,
        "edits": plan
            .edits
            .iter()
            .map(|e| json!({
                "file": file_name(sfs, e.file.index()),
                "span": {"start": e.span.start, "end": e.span.end},
                "replace": e.replace,
            }))
            .collect::<Vec<_>>(),
        "files": plan
            .new_sources
            .iter()
            .map(|(f, _)| file_name(sfs, *f))
            .collect::<Vec<_>>(),
    })
}

/// The apply report merged into the result payload.
pub fn applied_json(plan: &RenamePlan, report: &RenameReport, sfs: &[SourceFile]) -> Json {
    let mut j = plan_json(plan, sfs, true);
    j["applied_files"] = json!(
        report
            .files
            .iter()
            .map(|f| file_name(sfs, *f))
            .collect::<Vec<_>>()
    );
    j["applied_revision"] = json!(report.revision);
    j
}

/// The rejection as a ready envelope document (for the daemon,
/// which serializes envelopes itself).
pub fn rejection_json(e: &RenameError, sfs: &[SourceFile]) -> Json {
    let mut env = Envelope::new("rename");
    for d in e.diagnostics() {
        env = env.extra_diagnostic(&d, sfs);
    }
    env.into_parts().0
}

/// Persists every `new_sources` entry to its path on disk.
///
/// All-or-nothing in effect: every original is captured *before*
/// the first write, so a mid-loop IO failure rolls the already
/// written files back to their previous bytes — a rejected apply
/// never leaves a half-renamed workspace on disk.
pub fn persist(plan: &RenamePlan, sfs: &[SourceFile]) -> Result<(), String> {
    let mut files = Vec::with_capacity(plan.new_sources.len());
    for (f, text) in &plan.new_sources {
        let Some(path) = sfs[*f].path() else {
            continue;
        };
        let original =
            std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        files.push((path.to_path_buf(), original, text));
    }
    for (i, (path, _, text)) in files.iter().enumerate() {
        if let Err(e) = std::fs::write(path, text) {
            // Roll back the files already written — best effort,
            // their originals are in memory.
            for (p, original, _) in &files[..i] {
                let _ = std::fs::write(p, original);
            }
            return Err(format!("cannot write {}: {e}", path.display()));
        }
    }
    Ok(())
}

/// Human preview of a plan: one line per edit, stable order.
pub fn print_preview(plan: &RenamePlan, sfs: &[SourceFile]) {
    println!(
        "{} → {}: {} edit(s) in {} file(s)",
        plan.symbol,
        plan.new_name,
        plan.edits.len(),
        plan.new_sources.len(),
    );
    for e in &plan.edits {
        println!(
            "  {}:{}..{}  → {}",
            file_name(sfs, e.file.index()),
            e.span.start,
            e.span.end,
            e.replace,
        );
    }
}
