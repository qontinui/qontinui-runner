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
