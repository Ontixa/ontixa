//! Staged multi-file source persistence with a crash journal.
//!
//! A rename's in-memory apply is atomic; the disk side is not —
//! several files must be replaced and the process can die anywhere.
//! This module keeps every write recoverable instead of pretending
//! the whole operation is atomic:
//!
//! * **Prepare** — every destination's disk bytes are compared to
//!   the exact snapshot the plan validated (`before`). An external
//!   edit between plan and persist is a rejection, never a silent
//!   overwrite. Nothing is written in this phase.
//! * **Journal** — `<root>/.ontixa-tx-<id>.journal` records the tx
//!   id, per-file fingerprints (`before`/`after`), stage and backup
//!   paths, and progress. It exists before the first staged byte.
//! * **Stage** — each candidate goes to `<file>.ontixa-tx-<id>.stage`
//!   next to its destination (same directory ⇒ same filesystem, so
//!   the later promote is a rename). Live files are never truncated.
//! * **Commit** — per file: `dest → .bak`, then `.stage → dest`,
//!   recording each swap in the journal. Both steps are single
//!   renames; a crash mid-commit leaves a coherent journal entry.
//! * **Finalize** — backups removed, journal deleted.
//!
//! A journal found on a later run means a transaction died
//! in-flight: [`recover`] inspects disk fingerprints and finishes
//! or rolls the transaction back — idempotently.
//!
//! Honest limits: this protects readers and writers that respect
//! the journal. It does not stop an external writer racing the
//! commit between the prepare-phase check and a swap — the window
//! is small but real, and a power loss can leave artifacts the next
//! [`recover`] call must clean up.

use ontixa_source::SourceFile;
use serde::Deserialize;
use serde::Serialize;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// FNV-1a-64 over raw bytes — the same digest the `Db` uses for
/// workspace fingerprints. Content identity, never timestamps.
fn fp(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The directory a file lives in — `.` for bare filenames (an
/// empty parent would make `read_dir` fail silently and every
/// `starts_with` check vacuous).
pub fn parent_or_root(p: &std::path::Path) -> PathBuf {
    match p.parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// The longest common ancestor of two paths — the transaction root
/// for a multi-directory write.
pub fn common_ancestor(a: &std::path::Path, b: &std::path::Path) -> PathBuf {
    let mut cur = a.to_path_buf();
    while !b.starts_with(&cur) {
        if !cur.pop() {
            break;
        }
    }
    cur
}

/// The directory that should hold the journal for a write touching
/// `paths` — the common ancestor of every file's parent.
pub fn tx_root<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Option<PathBuf> {
    paths
        .into_iter()
        .map(parent_or_root)
        .reduce(|a, b| common_ancestor(&a, &b))
}

/// One file in a persistence transaction.
pub struct TxFile {
    /// Destination path — must live under the workspace root.
    pub path: PathBuf,
    /// The exact bytes the plan validated. The disk stale guard
    /// compares the live file against this, raw-byte for raw-byte.
    pub before: Vec<u8>,
    /// Candidate bytes to install.
    pub after: Vec<u8>,
}

/// The phase a [`PersistError`] came from — tells the caller how
/// much of the transaction had already happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Stale-guard/prepare: nothing was written anywhere.
    Prepare,
    /// Journal or stage write failed: journal may exist, no live
    /// file was touched.
    Staging,
    /// A swap failed mid-commit; rollback was attempted.
    /// `rollback_errors` on the error says whether it worked.
    Commit,
    /// Rollback itself failed: journal and backups are preserved —
    /// `recover` or manual repair is required.
    RecoveryRequired,
}

/// Why a persistence transaction did not complete.
#[derive(Debug)]
pub struct PersistError {
    /// Phase the transaction reached.
    pub phase: Phase,
    /// Human-readable cause.
    pub message: String,
    /// Journal path, once it exists — the recovery handle.
    pub journal: Option<PathBuf>,
    /// Non-fatal rollback failures already attempted (empty ⇒ the
    /// rollback that ran fully succeeded).
    pub rollback_errors: Vec<String>,
}

impl PersistError {
    fn new(phase: Phase, message: impl Into<String>) -> Self {
        Self {
            phase,
            message: message.into(),
            journal: None,
            rollback_errors: Vec::new(),
        }
    }
}

impl std::fmt::Display for PersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(j) = &self.journal {
            write!(f, " (journal: {})", j.display())?;
        }
        if !self.rollback_errors.is_empty() {
            write!(f, "; rollback errors: {}", self.rollback_errors.join("; "))?;
        }
        Ok(())
    }
}

impl std::error::Error for PersistError {}

/// What `recover` did about a journal it found.
#[derive(Debug, PartialEq, Eq)]
pub enum RecoveryOutcome {
    /// No pending journal — nothing to do.
    Clean,
    /// The transaction had committed (or was finished now): every
    /// file holds its `after` bytes; artifacts cleaned.
    Committed { tx: String, files: usize },
    /// Nothing had swapped; staged files and journal removed —
    /// every file still holds its `before` bytes.
    RolledBack { tx: String, files: usize },
    /// A file holds neither `before` nor `after` — someone else
    /// wrote it. Journal and backups are preserved; do not touch.
    Conflict { tx: String, path: PathBuf },
}

// ---------------- journal ----------------

/// Serialized journal entry — written under the workspace root so
/// a later process can find pending transactions.
#[derive(Serialize, Deserialize)]
struct Journal {
    /// Transaction id (`ontixa-tx-<pid>-<n>`).
    tx: String,
    /// `prepared` → `staged` → `swapping` → `done`.
    state: String,
    files: Vec<JournalFile>,
}

#[derive(Serialize, Deserialize)]
struct JournalFile {
    path: PathBuf,
    stage: PathBuf,
    bak: PathBuf,
    before_fp: u64,
    after_fp: u64,
    /// `dest → bak` + `stage → dest` both completed.
    swapped: bool,
}

fn journal_path(root: &Path, tx: &str) -> PathBuf {
    root.join(format!(".{tx}.journal"))
}

fn write_journal(root: &Path, j: &Journal) -> io::Result<PathBuf> {
    let path = journal_path(root, &j.tx);
    let data = serde_json::to_vec_pretty(j).expect("journal serializes");
    let mut f = fs::File::create(&path)?;
    io::Write::write_all(&mut f, &data)?;
    f.sync_data()?;
    Ok(path)
}

/// Finds `*.ontixa-tx-*.journal` files under `root`.
fn pending_journals(root: &Path) -> Vec<PathBuf> {
    let Ok(rd) = fs::read_dir(root) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".ontixa-tx-") && n.ends_with(".journal"))
        })
        .collect()
}

// ---------------- failpoints (test seam) ----------------

/// Injected failure sites — library-level only, never reachable
/// through CLI input. Used by the failure-injection suite.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailPoint {
    /// Journal creation fails.
    JournalWrite,
    /// `stage[i]` write fails before any byte lands.
    StageWrite(usize),
    /// `stage[i]` write succeeds but `sync_data` fails (partial
    /// content may exist on disk).
    StageSync(usize),
    /// Journal state update to `staged` fails.
    JournalUpdate,
    /// `dest → bak` rename fails for file `i` mid-commit.
    Swap(usize),
    /// `stage → dest` rename fails for file `i` — `dest` is
    /// already its backup.
    Promote(usize),
    /// Simulates process death right after file `i` was staged —
    /// journal exists, staged bytes on disk, no live file touched.
    StageDie(usize),
    /// Simulates process death after file `i` swapped: returns
    /// without rollback, leaving journal + staged files + backups
    /// exactly as a kill would. (Test-only — run in a child
    /// process or catch the error.)
    Die(usize),
}

struct Hooks {
    fail: Option<FailPoint>,
}

impl Hooks {
    fn check(&self, p: FailPoint) -> io::Result<()> {
        if self.fail == Some(p) {
            return Err(io::Error::other(format!("injected failure at {p:?}")));
        }
        Ok(())
    }
}

// ---------------- transaction ----------------

fn tx_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    format!(
        "ontixa-tx-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    )
}

/// Runs the full staged transaction. See module docs for phases;
/// `fail` is the test-only injection point (`None` in production).
pub fn persist_tx(root: &Path, files: &[TxFile]) -> Result<(), PersistError> {
    persist_tx_hooks(root, files, None)
}

/// [`persist_tx`] with an injected [`FailPoint`] — integration
/// tests only.
#[doc(hidden)]
pub fn persist_tx_hooks(
    root: &Path,
    files: &[TxFile],
    fail: Option<FailPoint>,
) -> Result<(), PersistError> {
    let hooks = Hooks { fail };

    // --- Prepare: refuse to start while another transaction is
    //    pending — the journal protocol only works if participants
    //    respect it, and a second writer over the same workspace
    //    would make recovery ambiguous.
    if let Some(pending) = pending_journals(root).into_iter().next() {
        return Err(PersistError::new(
            Phase::Prepare,
            format!(
                "a previous transaction is pending ({}); \
                 run recovery before writing",
                pending.display()
            ),
        ));
    }

    // --- Prepare: verify every destination against the snapshot.
    //    Nothing is written in this phase, so a rejection here can
    //    never leave a partial state. Paths are canonicalized for
    //    the root check — `main.ixa` vs `./math.ixa` vs absolute
    //    spellings of the same file must compare equal.
    let root_canon = fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    for f in files {
        let path_canon = fs::canonicalize(&f.path).map_err(|e| {
            PersistError::new(
                Phase::Prepare,
                format!("cannot resolve {}: {e}", f.path.display()),
            )
        })?;
        if !path_canon.starts_with(&root_canon) {
            return Err(PersistError::new(
                Phase::Prepare,
                format!("{} is outside the workspace root", f.path.display()),
            ));
        }
        let meta = fs::symlink_metadata(&f.path).map_err(|e| {
            PersistError::new(
                Phase::Prepare,
                format!("cannot stat {}: {e}", f.path.display()),
            )
        })?;
        if meta.file_type().is_symlink() {
            return Err(PersistError::new(
                Phase::Prepare,
                format!("{} is a symlink; refusing to swap it", f.path.display()),
            ));
        }
        if !meta.is_file() {
            return Err(PersistError::new(
                Phase::Prepare,
                format!("{} is not a regular file", f.path.display()),
            ));
        }
        let disk = fs::read(&f.path).map_err(|e| {
            PersistError::new(
                Phase::Prepare,
                format!("cannot read {}: {e}", f.path.display()),
            )
        })?;
        if disk != f.before {
            return Err(PersistError::new(
                Phase::Prepare,
                format!(
                    "{} changed on disk since the rename was validated; \
                     not overwriting external edits",
                    f.path.display()
                ),
            ));
        }
    }

    // --- Journal: exists before the first staged byte.
    let tx = tx_id();
    let mut journal = Journal {
        tx: tx.clone(),
        state: "prepared".into(),
        files: files
            .iter()
            .map(|f| {
                let stage = f
                    .path
                    .with_file_name(format!("{}.{}.stage", file_name(&f.path), tx));
                let bak = f
                    .path
                    .with_file_name(format!("{}.{}.bak", file_name(&f.path), tx));
                JournalFile {
                    path: f.path.clone(),
                    stage,
                    bak,
                    before_fp: fp(&f.before),
                    after_fp: fp(&f.after),
                    swapped: false,
                }
            })
            .collect(),
    };
    let jpath = match hooks
        .check(FailPoint::JournalWrite)
        .and_then(|()| write_journal(root, &journal))
    {
        Ok(p) => p,
        Err(e) => {
            return Err(PersistError::new(
                Phase::Staging,
                format!("cannot write journal: {e}"),
            ));
        }
    };

    // --- Stage: candidate bytes beside each destination.
    for (i, (f, jf)) in files.iter().zip(journal.files.iter()).enumerate() {
        let staged = (|| -> io::Result<()> {
            hooks.check(FailPoint::StageWrite(i))?;
            {
                let mut s = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&jf.stage)?;
                io::Write::write_all(&mut s, &f.after)?;
                hooks.check(FailPoint::StageSync(i))?;
                s.sync_data()?;
            }
            Ok(())
        })();
        if let Err(e) = staged {
            // Nothing live was touched — drop the journal and any
            // staged bytes, then report.
            let mut rollback_errors = Vec::new();
            for jf2 in &journal.files {
                if let Err(e2) = fs::remove_file(&jf2.stage) {
                    if e2.kind() != io::ErrorKind::NotFound {
                        rollback_errors.push(format!("{}: {e2}", jf2.stage.display()));
                    }
                }
            }
            if let Err(e2) = fs::remove_file(&jpath) {
                rollback_errors.push(format!("{}: {e2}", jpath.display()));
            }
            return Err(PersistError {
                phase: Phase::Staging,
                message: format!("cannot stage {}: {e}", jf.stage.display()),
                journal: if rollback_errors.is_empty() {
                    None
                } else {
                    Some(jpath)
                },
                rollback_errors,
            });
        }
        if hooks.fail == Some(FailPoint::StageDie(i)) {
            // Simulated kill after staging: journal + staged bytes
            // stay exactly as a dead process would leave them.
            return Err(PersistError {
                phase: Phase::RecoveryRequired,
                message: format!(
                    "transaction {} died while staging {}",
                    journal.tx,
                    jf.stage.display()
                ),
                journal: Some(jpath),
                rollback_errors: Vec::new(),
            });
        }
    }
    journal.state = "staged".into();
    if let Err(e) = hooks
        .check(FailPoint::JournalUpdate)
        .and_then(|()| write_journal(root, &journal))
    {
        // Stages exist but the journal does not describe them —
        // remove both so nothing orphaned is mistaken for a
        // pending transaction.
        for jf in &journal.files {
            let _ = fs::remove_file(&jf.stage);
        }
        let _ = fs::remove_file(&jpath);
        return Err(PersistError::new(
            Phase::Staging,
            format!("cannot update journal: {e}"),
        ));
    }

    // --- Commit: swap each file, recording progress in the journal.
    journal.state = "swapping".into();
    let _ = write_journal(root, &journal);
    for i in 0..journal.files.len() {
        let (dest, bak, stage) = {
            let jf = &journal.files[i];
            (jf.path.clone(), jf.bak.clone(), jf.stage.clone())
        };
        let swapped = (|| -> io::Result<()> {
            hooks.check(FailPoint::Swap(i))?;
            fs::rename(&dest, &bak)?;
            hooks.check(FailPoint::Promote(i))?;
            fs::rename(&stage, &dest)?;
            Ok(())
        })();
        match swapped {
            Ok(()) => {
                journal.files[i].swapped = true;
                let _ = write_journal(root, &journal);
                if hooks.fail == Some(FailPoint::Die(i)) {
                    // Simulated kill: leave everything as-is.
                    return Err(PersistError {
                        phase: Phase::RecoveryRequired,
                        message: format!(
                            "transaction {} died after swapping {dest}",
                            journal.tx,
                            dest = dest.display()
                        ),
                        journal: Some(jpath),
                        rollback_errors: Vec::new(),
                    });
                }
            }
            Err(e) => {
                // Roll back every file swapped so far; delete the
                // staged bytes of the files not yet swapped.
                let mut rollback_errors = Vec::new();
                for prev in journal.files.iter().filter(|p| p.swapped) {
                    if let Err(e2) = fs::rename(&prev.bak, &prev.path) {
                        rollback_errors.push(format!("restore {}: {e2}", prev.path.display()));
                    }
                }
                for unswapped in journal.files.iter().filter(|p| !p.swapped) {
                    let _ = fs::remove_file(&unswapped.stage);
                }
                // File i itself may sit as `.bak` if the promote step
                // failed after the dest→bak rename — restore it too.
                if bak.exists() {
                    if let Err(e2) = fs::rename(&bak, &dest) {
                        rollback_errors.push(format!("restore {}: {e2}", dest.display()));
                    }
                }
                if rollback_errors.is_empty() {
                    let _ = fs::remove_file(&jpath);
                    return Err(PersistError::new(
                        Phase::Commit,
                        format!("cannot swap {}: {e} (rolled back)", dest.display()),
                    ));
                }
                return Err(PersistError {
                    phase: Phase::RecoveryRequired,
                    message: format!(
                        "cannot swap {}: {e}; rollback incomplete — run recovery",
                        dest.display()
                    ),
                    journal: Some(jpath),
                    rollback_errors,
                });
            }
        }
    }

    // --- Finalize: remove backups, then the journal.
    let mut cleanup_warnings = Vec::new();
    for jf in &journal.files {
        if let Err(e) = fs::remove_file(&jf.bak) {
            cleanup_warnings.push(format!("{}: {e}", jf.bak.display()));
        }
    }
    journal.state = "done".into();
    let _ = write_journal(root, &journal);
    let _ = fs::remove_file(&jpath);
    if !cleanup_warnings.is_empty() {
        return Err(PersistError {
            phase: Phase::Commit,
            message: format!(
                "files committed but backups could not be removed: {}",
                cleanup_warnings.join("; ")
            ),
            journal: None,
            rollback_errors: Vec::new(),
        });
    }
    Ok(())
}

/// Stages a transaction plan's `new_sources` — `(file index,
/// post-edit text)` pairs — to disk under the journal protocol
/// above. This is the shared disk commit for every transaction
/// kind (rename, patch):
///
/// * each destination's disk bytes must equal `sfs[f]` — the source
///   snapshot the plan validated — or nothing is written;
/// * files without a disk path (in-memory sessions) are skipped;
/// * a plan that touches no path-bearing file is a no-op.
pub fn persist_sources(
    new_sources: &[(usize, String)],
    sfs: &[SourceFile],
) -> Result<(), PersistError> {
    let mut files = Vec::with_capacity(new_sources.len());
    let mut root: Option<PathBuf> = None;
    for (f, text) in new_sources {
        let Some(path) = sfs[*f].path() else {
            continue;
        };
        // `before` is the validated snapshot — the disk stale guard
        // compares live bytes against exactly this.
        files.push(TxFile {
            path: path.clone(),
            before: sfs[*f].text().as_bytes().to_vec(),
            after: text.clone().into_bytes(),
        });
        root = Some(match root {
            None => parent_or_root(path),
            Some(r) => common_ancestor(&r, &parent_or_root(path)),
        });
    }
    let Some(root) = root else {
        return Ok(());
    };
    persist_tx(&root, &files)
}

// ---------------- recovery ----------------

/// Inspects every pending journal under `root` and resolves it:
/// finishes a committed transaction, rolls back one that never
/// swapped, or reports a conflict without touching anything.
/// Idempotent — safe to run repeatedly and on a clean workspace.
pub fn recover(root: &Path) -> Vec<RecoveryOutcome> {
    let journals = pending_journals(root);
    if journals.is_empty() {
        return vec![RecoveryOutcome::Clean];
    }
    journals.iter().map(|jp| recover_one(root, jp)).collect()
}

fn recover_one(root: &Path, jp: &Path) -> RecoveryOutcome {
    let Ok(data) = fs::read(jp) else {
        return RecoveryOutcome::Conflict {
            tx: jp.display().to_string(),
            path: jp.to_path_buf(),
        };
    };
    let Ok(mut j) = serde_json::from_slice::<Journal>(&data) else {
        return RecoveryOutcome::Conflict {
            tx: jp.display().to_string(),
            path: jp.to_path_buf(),
        };
    };

    // Classify every file by what is actually on disk.
    let mut swapped_any = false;
    let mut pending: Vec<usize> = Vec::new();
    for (i, jf) in j.files.iter().enumerate() {
        let disk = fs::read(&jf.path).map(|b| fp(&b));
        match disk {
            Ok(h) if h == jf.after_fp => swapped_any = true,
            Ok(h) if h == jf.before_fp => pending.push(i),
            _ => {
                // Missing, or bytes neither before nor after —
                // never guess whose write this is.
                return RecoveryOutcome::Conflict {
                    tx: j.tx,
                    path: jf.path.clone(),
                };
            }
        }
    }

    if pending.is_empty() {
        // Everything already holds `after` — the transaction had
        // committed; just clean artifacts.
        for jf in &j.files {
            let _ = fs::remove_file(&jf.stage);
            let _ = fs::remove_file(&jf.bak);
        }
        let _ = fs::remove_file(jp);
        return RecoveryOutcome::Committed {
            tx: j.tx,
            files: j.files.len(),
        };
    }

    if swapped_any {
        // Commit point passed — finish the swap for the pending
        // files, then clean. A file whose staged bytes are missing
        // or wrong cannot be completed.
        for &i in &pending {
            let jf = &mut j.files[i];
            let stage_ok = fs::read(&jf.stage)
                .map(|b| fp(&b) == jf.after_fp)
                .unwrap_or(false);
            if !stage_ok {
                return RecoveryOutcome::Conflict {
                    tx: j.tx,
                    path: jf.stage.clone(),
                };
            }
            if fs::rename(&jf.path, &jf.bak).is_err() || fs::rename(&jf.stage, &jf.path).is_err() {
                return RecoveryOutcome::Conflict {
                    tx: j.tx,
                    path: jf.path.clone(),
                };
            }
            jf.swapped = true;
            let _ = write_journal(root, &j);
        }
        for jf in &j.files {
            let _ = fs::remove_file(&jf.bak);
        }
        j.state = "done".into();
        let _ = write_journal(root, &j);
        let _ = fs::remove_file(jp);
        return RecoveryOutcome::Committed {
            tx: j.tx,
            files: j.files.len(),
        };
    }

    // Nothing swapped — roll back: delete staged bytes + journal.
    for jf in &j.files {
        let _ = fs::remove_file(&jf.stage);
        let _ = fs::remove_file(&jf.bak);
    }
    let _ = fs::remove_file(jp);
    RecoveryOutcome::RolledBack {
        tx: j.tx,
        files: j.files.len(),
    }
}

/// Basename helper — stage/bak names live next to the destination.
fn file_name(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string()
}
