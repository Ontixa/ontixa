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
use std::path::PathBuf;

/// The display name of file `f` in `sfs` (indexed by `FileId`).
fn file_name(sfs: &[SourceFile], f: usize) -> String {
    sfs.get(f)
        .map(|s| s.name().to_string())
        .unwrap_or_else(|| f.to_string())
}

/// The plan as a JSON payload: every edit with its file and span.
pub fn plan_json(plan: &RenamePlan, sfs: &[SourceFile], applied: bool) -> Json {
    json!({
        "symbol": plan.symbol(),
        "old_name": plan.old_name(),
        "new_name": plan.new_name(),
        "target": format!("{:?}", plan.target()),
        "revision": plan.revision(),
        "applied": applied,
        "edits": plan
            .edits()
            .iter()
            .map(|e| json!({
                "file": file_name(sfs, e.file.index()),
                "span": {"start": e.span.start, "end": e.span.end},
                "replace": e.replace,
            }))
            .collect::<Vec<_>>(),
        "files": plan
            .new_sources()
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

/// Persists every `new_sources` entry through the staged
/// transaction engine in [`crate::persist`]:
///
/// * **stale guard** — each destination's disk bytes must equal the
///   source snapshot the plan validated, or nothing is written;
/// * **journal + staging** — candidates land in `*.stage` siblings,
///   never truncating a live file;
/// * **swap + rollback** — `dest → .bak` then `.stage → dest` per
///   file, with progress in the journal; a mid-commit failure
///   restores every swapped file, and a rollback failure leaves the
///   journal for [`crate::persist::recover`].
pub fn persist(plan: &RenamePlan, sfs: &[SourceFile]) -> Result<(), String> {
    let mut files = Vec::with_capacity(plan.new_sources().len());
    let mut root: Option<PathBuf> = None;
    for (f, text) in plan.new_sources() {
        let Some(path) = sfs[*f].path() else {
            continue;
        };
        // `before` is the validated snapshot — the disk stale guard
        // compares live bytes against exactly this.
        files.push(crate::persist::TxFile {
            path: path.clone(),
            before: sfs[*f].text().as_bytes().to_vec(),
            after: text.clone().into_bytes(),
        });
        root = Some(match root {
            None => crate::persist::parent_or_root(path),
            Some(r) => crate::persist::common_ancestor(&r, &crate::persist::parent_or_root(path)),
        });
    }
    let Some(root) = root else {
        return Ok(());
    };
    crate::persist::persist_tx(&root, &files).map_err(|e| e.to_string())
}

/// Human preview of a plan: one line per edit, stable order.
pub fn print_preview(plan: &RenamePlan, sfs: &[SourceFile]) {
    println!(
        "{} → {}: {} edit(s) in {} file(s)",
        plan.symbol(),
        plan.new_name(),
        plan.edits().len(),
        plan.new_sources().len(),
    );
    for e in plan.edits() {
        println!(
            "  {}:{}..{}  → {}",
            file_name(sfs, e.file.index()),
            e.span.start,
            e.span.end,
            e.replace,
        );
    }
}
