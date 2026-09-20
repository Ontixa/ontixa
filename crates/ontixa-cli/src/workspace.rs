//! Workspace file loading: the multi-file side of `ontixa` commands.
//!
//! Ontixa has no project manifest — a workspace is "a root file plus
//! every sibling `.ixa`". Each file provides a module named by its
//! stem, which is what `use m;` and `m::x` resolve against. Loading
//! is eager (the whole directory registers) while dependency
//! discovery inside the compiler is lazy (only `use`-reachable files
//! enter the scope).

use std::path::{Path, PathBuf};

/// The module name a `.ixa` path provides — its file stem. Stems
/// that aren't valid identifiers simply can't be `use`d.
pub fn module_name(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
        .to_string()
}

/// Every `.ixa` file that should register alongside `root`: the root
/// first (its contents are read by the caller separately — this
/// returns the path only), then siblings in sorted order for
/// determinism.
///
/// Returns `(path, text)` pairs; the root entry carries an empty
/// text marker (`None`) so callers that already hold the root's text
/// don't read it twice.
pub fn workspace_files(root: &Path) -> Vec<(PathBuf, Option<String>)> {
    let mut out = vec![(root.to_path_buf(), None)];
    let root_stem = module_name(root);
    // `x.ixa` has no parent prefix — `""` reads nothing; mean `.`.
    let dir = match root.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    let mut siblings: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ixa"))
        .filter(|p| module_name(p) != root_stem)
        .collect();
    siblings.sort();
    for p in siblings {
        // Unreadable siblings are skipped — a `use` of one reports
        // E_UNKNOWN_MODULE like any other missing module.
        if let Ok(text) = std::fs::read_to_string(&p) {
            out.push((p, Some(text)));
        }
    }
    out
}

/// A canonical-ish key for deduplicating path spellings — the
/// canonicalized display string when the path exists, else the path
/// as given. Daemon sessions key their file table by this.
pub fn path_key(path: &Path) -> String {
    std::fs::canonicalize(path)
        .map(|p| {
            // `\\?\` verbatim prefixes render badly in diagnostics.
            let s = p.display().to_string();
            s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
        })
        .unwrap_or_else(|_| path.display().to_string())
}
