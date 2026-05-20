//! Filesystem layer for the Spec API.
//!
//! The storage root holds:
//!
//! ```text
//! <root>/
//!   pages/<id>/state-machine.derived.json   IR document (camelCase, authoring-time)
//!   pages/<id>/spec.uibridge.json           Bundled-page projection (generated)
//!   pages/<id>/notes.md                     Optional human notes
//!   architecture/, design-system/, contracts/   reserved for sections 8/11
//! ```
//!
//! All write operations use a temp-file + atomic-rename strategy so a crash
//! mid-write cannot leave a half-written file.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use include_dir::{include_dir, Dir};

use super::projection::project_to_pretty_json;
use super::types::IrPageSpec;

#[cfg(feature = "spec-authoring")]
use super::types::ProposalStatus;

/// Compile-time snapshot of `<runner>/specs/pages/`. Section 4 ships an
/// offline-capable runner: production binaries serve specs from this embedded
/// bundle when the on-disk path is missing.
///
/// Resolution order in `read_ir` / `read_projection` / `list_pages` /
/// `read_notes` is filesystem-first, embedded-second:
/// - Filesystem hits during dev (hot-reload, /update-spec writes).
/// - Embedded fallback when the binary is shipped without its sibling specs/
///   tree (e.g. a standalone build).
static EMBEDDED_PAGES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../specs/pages");

/// Resolve the storage root for the Spec API.
///
/// Resolution order:
///   1. `QONTINUI_SPECS_ROOT` env var (absolute path).
///   2. `<runner-repo>/specs/` resolved relative to the current working dir.
///
/// We do NOT auto-create the root — if it's missing, calls return errors so
/// the caller knows storage is unwired (avoids the "silent empty stub"
/// failure mode the Section 2 plan calls out).
pub fn resolve_specs_root() -> PathBuf {
    if let Ok(override_path) = std::env::var("QONTINUI_SPECS_ROOT") {
        return PathBuf::from(override_path);
    }
    // Default to <CARGO_MANIFEST_DIR>/../specs (i.e. <runner>/specs) when
    // running tests; else current dir + ../specs which matches the runner
    // layout when launched from src-tauri/.
    if let Some(manifest_dir) = option_env!("CARGO_MANIFEST_DIR") {
        let p = PathBuf::from(manifest_dir).join("..").join("specs");
        if let Ok(canon) = p.canonicalize() {
            return canon;
        }
        return p;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    cwd.join("specs")
}

/// Page-level paths. Cheap to construct from a page id.
pub struct PagePaths {
    pub root: PathBuf,
    pub page_dir: PathBuf,
    pub ir_path: PathBuf,
    pub projection_path: PathBuf,
    pub notes_path: PathBuf,
}

impl PagePaths {
    pub fn for_page(root: &Path, page_id: &str) -> Self {
        let page_dir = root.join("pages").join(page_id);
        Self {
            root: root.to_path_buf(),
            ir_path: page_dir.join("state-machine.derived.json"),
            projection_path: page_dir.join("spec.uibridge.json"),
            notes_path: page_dir.join("notes.md"),
            page_dir,
        }
    }
}

/// List the page IDs that exist under `<root>/pages/`. Returns an empty
/// vector if the directory does not exist (callers should attach a `reason`
/// before returning to the user).
///
/// Falls back to enumerating the compile-time `EMBEDDED_PAGES` snapshot when
/// the on-disk pages directory is missing or empty. Filesystem entries take
/// precedence on collisions; the embedded set is merged in only as a fallback
/// to cover production binaries shipped without a sibling specs/ tree.
pub fn list_pages(root: &Path) -> std::io::Result<Vec<String>> {
    let pages_dir = root.join("pages");
    let mut ids: Vec<String> = Vec::new();
    if pages_dir.exists() {
        for entry in fs::read_dir(&pages_dir)? {
            let entry = entry?;
            let ft = entry.file_type()?;
            if ft.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    ids.push(name.to_string());
                }
            }
        }
    }
    if ids.is_empty() {
        for embedded_dir in EMBEDDED_PAGES.dirs() {
            if let Some(name) = embedded_dir.path().file_name().and_then(|n| n.to_str()) {
                ids.push(name.to_string());
            }
        }
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

/// Read an IR document from disk. Returns `Ok(None)` if the file is missing
/// (so callers can return a `reason: "page-not-found"` rather than an
/// internal error).
///
/// Filesystem-first, embedded-second. The embedded snapshot
/// (`EMBEDDED_PAGES`) only kicks in when the on-disk file is absent — dev
/// hot-reload and `/update-spec` writes always win.
pub fn read_ir(root: &Path, page_id: &str) -> Result<Option<IrPageSpec>, String> {
    let paths = PagePaths::for_page(root, page_id);
    if paths.ir_path.exists() {
        let data = fs::read_to_string(&paths.ir_path)
            .map_err(|e| format!("read {} failed: {}", paths.ir_path.display(), e))?;
        let doc: IrPageSpec = serde_json::from_str(&data)
            .map_err(|e| format!("parse {} failed: {}", paths.ir_path.display(), e))?;
        return Ok(Some(doc));
    }
    // Embedded fallback.
    let embedded_rel = format!("{}/state-machine.derived.json", page_id);
    if let Some(file) = EMBEDDED_PAGES.get_file(&embedded_rel) {
        let doc: IrPageSpec = serde_json::from_slice(file.contents())
            .map_err(|e| format!("parse embedded {} failed: {}", file.path().display(), e))?;
        return Ok(Some(doc));
    }
    Ok(None)
}

/// Read the bundled projection (pretty JSON) as a `serde_json::Value`.
///
/// Filesystem-first, embedded-second (see [`read_ir`] for the rationale).
pub fn read_projection(root: &Path, page_id: &str) -> Result<Option<serde_json::Value>, String> {
    let paths = PagePaths::for_page(root, page_id);
    if paths.projection_path.exists() {
        let data = fs::read_to_string(&paths.projection_path)
            .map_err(|e| format!("read {} failed: {}", paths.projection_path.display(), e))?;
        let v: serde_json::Value = serde_json::from_str(&data)
            .map_err(|e| format!("parse {} failed: {}", paths.projection_path.display(), e))?;
        return Ok(Some(v));
    }
    let embedded_rel = format!("{}/spec.uibridge.json", page_id);
    if let Some(file) = EMBEDDED_PAGES.get_file(&embedded_rel) {
        let v: serde_json::Value = serde_json::from_slice(file.contents())
            .map_err(|e| format!("parse embedded {} failed: {}", file.path().display(), e))?;
        return Ok(Some(v));
    }
    Ok(None)
}

/// Read the notes companion file. Returns `None` if absent. Empty/whitespace
/// content normalizes to `None`.
///
/// Filesystem-first, embedded-second (see [`read_ir`] for the rationale).
pub fn read_notes(root: &Path, page_id: &str) -> Result<Option<String>, String> {
    let paths = PagePaths::for_page(root, page_id);
    if paths.notes_path.exists() {
        let s = fs::read_to_string(&paths.notes_path)
            .map_err(|e| format!("read {} failed: {}", paths.notes_path.display(), e))?;
        let trimmed = s.trim().to_string();
        return if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed))
        };
    }
    let embedded_rel = format!("{}/notes.md", page_id);
    if let Some(file) = EMBEDDED_PAGES.get_file(&embedded_rel) {
        let s = std::str::from_utf8(file.contents())
            .map_err(|e| format!("parse embedded {} failed: {}", file.path().display(), e))?;
        let trimmed = s.trim().to_string();
        return if trimmed.is_empty() {
            Ok(None)
        } else {
            Ok(Some(trimmed))
        };
    }
    Ok(None)
}

/// Atomic write: write to `<target>.tmp` then rename. Cleans up the tmp
/// file on failure.
fn atomic_write(target: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = target.with_extension(format!(
        "{}.tmp",
        target
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("write")
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, target).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Write an IR document and regenerate its projection. Returns the absolute
/// path to the projection file on success.
///
/// Writes always hit the filesystem — `EMBEDDED_PAGES` is a compile-time
/// snapshot and cannot be mutated at runtime. After this call, subsequent
/// `read_ir` / `read_projection` calls will resolve to the freshly-written
/// file (filesystem wins over embedded).
pub fn write_ir_and_regenerate(root: &Path, doc: &IrPageSpec) -> Result<PathBuf, String> {
    let paths = PagePaths::for_page(root, &doc.id);
    let ir_json =
        serde_json::to_string_pretty(doc).map_err(|e| format!("serialize IR failed: {}", e))?;
    let mut ir_buf = ir_json.into_bytes();
    ir_buf.push(b'\n');
    atomic_write(&paths.ir_path, &ir_buf)
        .map_err(|e| format!("write {} failed: {}", paths.ir_path.display(), e))?;

    let notes = read_notes(root, &doc.id).unwrap_or(None);
    let projection = project_to_pretty_json(doc, notes.as_deref());
    atomic_write(&paths.projection_path, projection.as_bytes())
        .map_err(|e| format!("write {} failed: {}", paths.projection_path.display(), e))?;

    Ok(paths.projection_path)
}

/// Read raw file contents at `<rel>` inside the storage root. Performs path
/// traversal protection: the canonicalized resolved path must stay within
/// the canonicalized root.
pub fn read_within_root(root: &Path, rel: &str) -> Result<Vec<u8>, ReadWithinRootError> {
    let root_canon = root
        .canonicalize()
        .map_err(|_| ReadWithinRootError::RootMissing)?;
    let candidate = root_canon.join(rel);
    // Reject obvious traversal patterns even before canonicalize — on
    // Windows `\\?\` paths and `..` segments need belt-and-suspenders.
    let candidate_canon = candidate
        .canonicalize()
        .map_err(|_| ReadWithinRootError::FileNotFound)?;
    if !candidate_canon.starts_with(&root_canon) {
        return Err(ReadWithinRootError::OutsideRoot);
    }
    fs::read(&candidate_canon).map_err(|_| ReadWithinRootError::FileNotFound)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReadWithinRootError {
    RootMissing,
    FileNotFound,
    OutsideRoot,
}

/// Stat-based mtime check; returns the modified time as seconds since epoch
/// for IR + projection files. Used by `/spec/diff?since=`.
///
/// Embedded-only pages (no on-disk files) return `Some(0)` so callers' `since=`
/// cursor behaves predictably — `0` is older than any real epoch timestamp,
/// so a client polling with a non-zero cursor won't see spurious "modified"
/// signals for content that hasn't actually changed.
pub fn newest_mtime_for_page(root: &Path, page_id: &str) -> Option<u64> {
    let paths = PagePaths::for_page(root, page_id);
    let mut newest: Option<u64> = None;
    let mut any_on_disk = false;
    for p in [&paths.ir_path, &paths.projection_path, &paths.notes_path] {
        if let Ok(meta) = fs::metadata(p) {
            any_on_disk = true;
            if let Ok(modified) = meta.modified() {
                if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                    let secs = dur.as_secs();
                    newest = Some(newest.map_or(secs, |cur| cur.max(secs)));
                }
            }
        }
    }
    if newest.is_some() {
        return newest;
    }
    // If nothing was on disk but the page exists in the embedded snapshot,
    // return 0 so the caller still sees "the page exists" (vs `None`, which
    // means "no such page").
    if !any_on_disk {
        let ir_rel = format!("{}/state-machine.derived.json", page_id);
        let proj_rel = format!("{}/spec.uibridge.json", page_id);
        let notes_rel = format!("{}/notes.md", page_id);
        if EMBEDDED_PAGES.get_file(&ir_rel).is_some()
            || EMBEDDED_PAGES.get_file(&proj_rel).is_some()
            || EMBEDDED_PAGES.get_file(&notes_rel).is_some()
        {
            return Some(0);
        }
    }
    None
}

// =============================================================================
// _pending/ staging (Stream E — Flywheel, Step 9)
// =============================================================================
//
// Coverage-growth candidates produced by `spec_authoring::author_candidate`
// land in `<root>/pages/<id>/_pending/` rather than the canonical
// `<root>/pages/<id>/` location. The 2-green sweep handler in
// `proposals.rs::post_sweep_pending` either promotes (`pages/<id>/`) or demotes
// (rm -rf `_pending/`) candidates based on whether they pass Gate-B-execution
// on consecutive sweeps. See `qontinui-dev-notes/spec-check-v1/05-flywheel.md`
// § Step 9 for the full design context.
//
// All four helpers below are gated behind `spec-authoring` — the runtime
// staging area is invisible to v1.0 release builds.

#[cfg(feature = "spec-authoring")]
fn pending_dir(root: &Path, page_id: &str) -> PathBuf {
    root.join("pages").join(page_id).join("_pending")
}

#[cfg(feature = "spec-authoring")]
fn pending_ir_path(root: &Path, page_id: &str) -> PathBuf {
    pending_dir(root, page_id).join("state-machine.derived.json")
}

#[cfg(feature = "spec-authoring")]
fn pending_projection_path(root: &Path, page_id: &str) -> PathBuf {
    pending_dir(root, page_id).join("spec.uibridge.json")
}

/// Write `ir` to `<root>/pages/<page_id>/_pending/state-machine.derived.json`
/// + regenerate the projection sibling. Stamps `provenance.status = Pending`
/// on the doc-level provenance before serialization. Atomic via temp-rename.
///
/// The caller's `ir` is NOT mutated — the status flip happens on a clone
/// owned by this function. Returns the absolute path to the IR file written
/// on success.
///
/// Auto-creates the `_pending/` subdirectory. If a `provenance` block is
/// absent on the input, one is synthesized with `source: "ai-generated"`
/// (the only `_pending/` producer in v1.0) and `status: Some(Pending)`.
#[cfg(feature = "spec-authoring")]
pub fn write_pending_ir(root: &Path, ir: &IrPageSpec) -> Result<PathBuf, String> {
    let mut doc = ir.clone();
    let mut prov = doc.provenance.unwrap_or(super::types::IrProvenance {
        source: "ai-generated".to_string(),
        app_id: "qontinui-runner".to_string(),
        file: None,
        line: None,
        column: None,
        plugin_version: None,
        status: None,
    });
    prov.status = Some(ProposalStatus::Pending);
    doc.provenance = Some(prov);

    let dir = pending_dir(root, &doc.id);
    fs::create_dir_all(&dir)
        .map_err(|e| format!("create_dir_all {} failed: {}", dir.display(), e))?;

    let ir_path = pending_ir_path(root, &doc.id);
    let projection_path = pending_projection_path(root, &doc.id);

    let mut ir_json = serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("serialize pending IR failed: {}", e))?;
    ir_json.push('\n');
    atomic_write(&ir_path, ir_json.as_bytes())
        .map_err(|e| format!("write {} failed: {}", ir_path.display(), e))?;

    // Regenerate the projection sibling. We don't read on-disk notes for the
    // pending shape — only the canonical `pages/<id>/` location carries
    // notes.md. The projection is purely derived from the IR.
    let projection = project_to_pretty_json(&doc, None);
    atomic_write(&projection_path, projection.as_bytes())
        .map_err(|e| format!("write {} failed: {}", projection_path.display(), e))?;

    Ok(ir_path)
}

/// Read the pending IR (if any) for `page_id`. Returns `Ok(None)` if no
/// `_pending/state-machine.derived.json` exists.
///
/// Mirrors the [`read_ir`] shape (`Result<Option<IrPageSpec>, String>`).
/// Unlike `read_ir`, this NEVER falls back to the embedded snapshot —
/// `EMBEDDED_PAGES` is the immutable promoted corpus; pending candidates
/// only exist on the filesystem.
#[cfg(feature = "spec-authoring")]
pub fn read_pending_ir(root: &Path, page_id: &str) -> Result<Option<IrPageSpec>, String> {
    let ir_path = pending_ir_path(root, page_id);
    if !ir_path.exists() {
        return Ok(None);
    }
    let data = fs::read_to_string(&ir_path)
        .map_err(|e| format!("read {} failed: {}", ir_path.display(), e))?;
    let doc: IrPageSpec = serde_json::from_str(&data)
        .map_err(|e| format!("parse {} failed: {}", ir_path.display(), e))?;
    Ok(Some(doc))
}

/// Atomically promote a `_pending/` candidate to the canonical `pages/<id>/`
/// location:
///
///   1. Acquire the per-page filesystem lock ([`page_lock`]) — serializes
///      concurrent sweeps so a manual `/execute` overlap doesn't race the
///      cron.
///   2. Read `_pending/state-machine.derived.json` (errors if absent).
///   3. Flip `provenance.status` to `Some(ProposalStatus::Promoted)`.
///   4. Call [`write_ir_and_regenerate`] to write the canonical IR + projection.
///   5. `rm -rf _pending/`.
///
/// Returns `Err` if the pending IR is missing or any step fails. The lock is
/// released on Drop regardless of success/failure.
#[cfg(feature = "spec-authoring")]
pub fn promote_pending(root: &Path, page_id: &str) -> Result<(), String> {
    let _guard = page_lock(root, page_id)?;

    let mut doc = read_pending_ir(root, page_id)?
        .ok_or_else(|| format!("promote_pending: no pending IR for page {}", page_id))?;

    let mut prov = doc.provenance.unwrap_or(super::types::IrProvenance {
        source: "ai-generated".to_string(),
        app_id: "qontinui-runner".to_string(),
        file: None,
        line: None,
        column: None,
        plugin_version: None,
        status: None,
    });
    prov.status = Some(ProposalStatus::Promoted);
    doc.provenance = Some(prov);

    // Canonical write + projection regen.
    write_ir_and_regenerate(root, &doc)?;

    // rm -rf _pending/
    let dir = pending_dir(root, page_id);
    if dir.exists() {
        fs::remove_dir_all(&dir)
            .map_err(|e| format!("remove_dir_all {} failed: {}", dir.display(), e))?;
    }
    Ok(())
}

/// `rm -rf <root>/pages/<page_id>/_pending/`. Used on single-red demotion.
/// Idempotent — succeeds silently if the directory doesn't exist.
#[cfg(feature = "spec-authoring")]
pub fn demote_pending(root: &Path, page_id: &str) -> Result<(), String> {
    let dir = pending_dir(root, page_id);
    if !dir.exists() {
        return Ok(());
    }
    fs::remove_dir_all(&dir).map_err(|e| format!("remove_dir_all {} failed: {}", dir.display(), e))
}

// -----------------------------------------------------------------------------
// page_lock — per-page filesystem mutex (lock-by-existence)
// -----------------------------------------------------------------------------
//
// The 2-green sweep handler MAY overlap with a manual `POST /spec/proposals/
// {id}/execute` (or with itself under cron mis-firing). promote_pending +
// demote_pending each have a small critical section spanning a few syscalls;
// we serialize via a per-page lock file using `OpenOptions::create_new`
// (POSIX `O_CREAT | O_EXCL` semantics — atomic on every supported FS).
//
// We deliberately avoid `fs2` / `fd_lock` to keep the dep tree slim — the
// existence-based approach is well-suited to coarse-grained, low-contention
// locks like this one (one acquire per proposal per sweep, no nested
// recursion).

#[cfg(feature = "spec-authoring")]
fn page_lock_path(root: &Path, page_id: &str) -> PathBuf {
    root.join("pages").join(page_id).join(".lock")
}

/// RAII guard returned by [`page_lock`]. On drop, removes the lock file.
#[cfg(feature = "spec-authoring")]
#[derive(Debug)]
pub(crate) struct PageLockGuard {
    lock_path: PathBuf,
}

#[cfg(feature = "spec-authoring")]
impl Drop for PageLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

/// Acquire a per-page filesystem-backed mutex by exclusive-creating a lock
/// file at `<root>/pages/<page_id>/.lock`. Retries with capped exponential
/// backoff up to ~1.5s (10 attempts) before giving up.
///
/// Returns a `PageLockGuard` that removes the lock on Drop. If acquisition
/// fails (e.g. another process holds it and never releases), returns
/// `Err(String)`. Callers MUST treat the error as fatal for the current
/// operation — the alternative (proceeding without the lock) risks racing
/// the canonical write against a concurrent sweep.
#[cfg(feature = "spec-authoring")]
pub(crate) fn page_lock(root: &Path, page_id: &str) -> Result<PageLockGuard, String> {
    let lock_path = page_lock_path(root, page_id);
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("create_dir_all {} failed: {}", parent.display(), e))?;
    }

    // 10 attempts, capped delays: 5, 10, 20, 40, 80, 160, 320, 640, 1280 ms.
    let mut delay_ms: u64 = 5;
    for attempt in 0..10 {
        match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&lock_path)
        {
            Ok(_) => {
                return Ok(PageLockGuard {
                    lock_path: lock_path.clone(),
                });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if attempt == 9 {
                    return Err(format!(
                        "page_lock({}): held by another process after 10 retries",
                        page_id
                    ));
                }
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                delay_ms = (delay_ms * 2).min(1500);
            }
            Err(e) => {
                return Err(format!("page_lock({}) open failed: {}", page_id, e));
            }
        }
    }
    Err(format!("page_lock({}): unreachable", page_id))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(all(test, feature = "spec-authoring"))]
mod pending_tests {
    use super::*;
    use crate::spec_api::types::{IrPageSpec, IrProvenance, ProposalStatus};

    fn empty_ir(id: &str, provenance: Option<IrProvenance>) -> IrPageSpec {
        IrPageSpec {
            version: "1.0".into(),
            id: id.into(),
            name: id.into(),
            description: None,
            metadata: None,
            provenance,
            states: Vec::new(),
            transitions: Vec::new(),
            synthesized_groups: None,
            initial_state: None,
        }
    }

    #[test]
    fn write_pending_ir_stamps_provenance_pending() {
        let tmp = tempfile::tempdir().unwrap();
        // Input has status = Proposed; write_pending_ir must flip to Pending
        // without mutating the caller's IR.
        let input_prov = IrProvenance {
            source: "ai-generated".into(),
            app_id: "qontinui-runner".to_string(),
            file: None,
            line: None,
            column: None,
            plugin_version: None,
            status: Some(ProposalStatus::Proposed),
        };
        let ir = empty_ir("page-a", Some(input_prov.clone()));
        let path = write_pending_ir(tmp.path(), &ir).expect("write_pending_ir");
        // Caller's IR is unchanged.
        assert_eq!(
            ir.provenance.as_ref().unwrap().status,
            Some(ProposalStatus::Proposed)
        );
        // On-disk doc has Pending.
        let read = read_pending_ir(tmp.path(), "page-a")
            .expect("read_pending_ir")
            .expect("file present");
        assert_eq!(
            read.provenance.as_ref().unwrap().status,
            Some(ProposalStatus::Pending)
        );
        // Sibling projection exists.
        let projection = tmp
            .path()
            .join("pages")
            .join("page-a")
            .join("_pending")
            .join("spec.uibridge.json");
        assert!(
            projection.exists(),
            "projection sibling missing: {}",
            projection.display()
        );
        // Returned path is the IR file.
        assert!(path.ends_with("state-machine.derived.json"));
    }

    #[test]
    fn write_pending_ir_synthesizes_provenance_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let ir = empty_ir("page-b", None);
        write_pending_ir(tmp.path(), &ir).expect("write_pending_ir");
        let read = read_pending_ir(tmp.path(), "page-b")
            .expect("read")
            .expect("file present");
        let prov = read.provenance.expect("provenance synthesized");
        assert_eq!(prov.source, "ai-generated");
        assert_eq!(prov.status, Some(ProposalStatus::Pending));
    }

    #[test]
    fn read_pending_ir_returns_none_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let res = read_pending_ir(tmp.path(), "no-such-page").expect("ok");
        assert!(res.is_none(), "expected None for missing page");
    }

    #[test]
    fn promote_pending_moves_to_canonical_and_stamps_promoted() {
        let tmp = tempfile::tempdir().unwrap();
        let ir = empty_ir(
            "page-c",
            Some(IrProvenance {
                source: "ai-generated".into(),
                app_id: "qontinui-runner".to_string(),
                file: None,
                line: None,
                column: None,
                plugin_version: None,
                status: Some(ProposalStatus::Proposed),
            }),
        );
        write_pending_ir(tmp.path(), &ir).expect("stage");

        promote_pending(tmp.path(), "page-c").expect("promote");

        // Canonical files exist.
        let canon_ir = tmp
            .path()
            .join("pages")
            .join("page-c")
            .join("state-machine.derived.json");
        let canon_proj = tmp
            .path()
            .join("pages")
            .join("page-c")
            .join("spec.uibridge.json");
        assert!(canon_ir.exists(), "canonical IR missing");
        assert!(canon_proj.exists(), "canonical projection missing");

        // _pending/ is gone.
        let pending_dir = tmp.path().join("pages").join("page-c").join("_pending");
        assert!(
            !pending_dir.exists(),
            "_pending/ should have been removed: {}",
            pending_dir.display()
        );

        // Canonical IR has status = Promoted.
        let canon = read_ir(tmp.path(), "page-c")
            .expect("read canonical")
            .expect("present");
        assert_eq!(
            canon.provenance.as_ref().unwrap().status,
            Some(ProposalStatus::Promoted)
        );
    }

    #[test]
    fn promote_pending_errors_when_no_pending_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let res = promote_pending(tmp.path(), "absent-page");
        assert!(
            res.is_err(),
            "promote should fail when nothing is staged: {:?}",
            res
        );
    }

    #[test]
    fn demote_pending_removes_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ir = empty_ir("page-d", None);
        write_pending_ir(tmp.path(), &ir).expect("stage");
        let pending_dir = tmp.path().join("pages").join("page-d").join("_pending");
        assert!(pending_dir.exists(), "pending dir must exist after stage");

        demote_pending(tmp.path(), "page-d").expect("demote");
        assert!(
            !pending_dir.exists(),
            "_pending/ should have been removed after demote"
        );
    }

    #[test]
    fn demote_pending_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        // Demote when nothing is staged → ok.
        demote_pending(tmp.path(), "never-staged").expect("idempotent");
        // Twice in a row also ok.
        demote_pending(tmp.path(), "never-staged").expect("idempotent twice");
    }

    #[test]
    fn page_lock_serializes_concurrent_acquires() {
        let tmp = tempfile::tempdir().unwrap();
        let page_id = "page-lock";
        // Pre-create the page dir so the lock-file parent exists deterministically.
        fs::create_dir_all(tmp.path().join("pages").join(page_id)).unwrap();

        // First acquire succeeds.
        let g1 = page_lock(tmp.path(), page_id).expect("first acquire");

        // Second acquire while g1 is held should retry briefly and eventually
        // fail (after 10 attempts) since g1 never releases inside this test.
        let res = page_lock(tmp.path(), page_id);
        assert!(
            res.is_err(),
            "second acquire must fail while first guard held: {:?}",
            res
        );

        // Drop the guard — lock file removed.
        drop(g1);
        let lock_path = tmp.path().join("pages").join(page_id).join(".lock");
        assert!(
            !lock_path.exists(),
            "lock file should be removed on drop: {}",
            lock_path.display()
        );

        // Third acquire after release succeeds.
        let _g3 = page_lock(tmp.path(), page_id).expect("third acquire after release");
    }
}
