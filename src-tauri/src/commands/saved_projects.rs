//! Saved Projects commands
//!
//! User-curated registry of projects the wizard (or other UI flows) has
//! told us about. Persisted to `settings.json` under the `saved_projects`
//! key via the existing `settings` module.
//!
//! # Contract
//!
//! - [`list_saved_projects`] — returns the persisted list (empty on first
//!   run), after backfilling any entry that predates the `id` field.
//! - [`save_saved_projects`] — atomic replace (wizard "Next" commits here).
//! - [`add_saved_project`] — idempotent append by normalized path.
//! - [`remove_saved_project`] — remove **by id**; no error if absent.
//! - [`discover_projects`] — scan a workspace dir for candidate projects.
//! - [`project_snapshot`] — the joined dashboard view (see
//!   [`crate::projects::snapshot`]).
//! - [`set_project_front_page`] / [`bind_project_processes`] /
//!   [`set_project_terminal_page`] — targeted field updates keyed by id.
//!
//! # Path normalization
//!
//! Every project-scoped join in the dashboard — managed processes by `cwd`,
//! terminal sessions by `working_dir`, AI sessions by touched `file_path` —
//! is a path-prefix test against the project root. Those paths are written
//! by different producers that each spell the same directory differently, so
//! [`normalize_path`] canonicalizes before any comparison. It performs five
//! normalizations (see the function docs); [`is_under`] then supplies the
//! prefix *boundary* so `D:\foo` cannot swallow `D:\foobar`.
//!
//! `std::fs::canonicalize` is deliberately avoided: it requires the path to
//! exist on disk, and the registry can legitimately hold a project whose
//! directory was renamed later — we still want `remove_saved_project` and
//! the joins to behave.

use std::sync::OnceLock;

use tauri::plugin::{Builder as PluginBuilder, TauriPlugin};
use tauri::Runtime;
use tracing::{info, warn};

use crate::settings::{self, SavedProject};

// Re-export so downstream code can `use commands::saved_projects::SavedProject`
// without reaching into the settings module.
pub use crate::settings::SavedProject as SavedProjectModel;

// ============================================================================
// subst-alias resolution
// ============================================================================

/// `("D:", "C:\\Users\\jspin\\Documents")` pairs for every drive letter that
/// is a `subst` alias rather than a physical volume.
type SubstAliases = Vec<(String, String)>;

static SUBST_ALIASES: OnceLock<SubstAliases> = OnceLock::new();

/// Resolve the machine's `subst` table **once**, lazily, and cache it.
///
/// Aliases are DATA, not a hard-coded pair: this box happens to run
/// `D:` → `C:\Users\jspin\Documents`, but the same code has to work on a
/// machine with no substs at all, or a different letter.
fn subst_aliases() -> &'static [(String, String)] {
    SUBST_ALIASES.get_or_init(build_subst_aliases)
}

/// The resolved `subst` table, for callers that must *reverse* the
/// normalization rather than apply it.
///
/// The dashboard's SQL prefix filter is the one such caller: stored
/// `file_path` values are raw, `normalize_path` is not reproducible in SQL
/// (it consults this table), so the read side enumerates every spelling a
/// root could have instead. See `projects::snapshot::prefix_spellings`.
pub(crate) fn subst_alias_pairs() -> &'static [(String, String)] {
    subst_aliases()
}

/// Enumerate `A:`..`Z:` via `QueryDosDeviceW` and keep the ones whose NT
/// target is a `\??\<Drive>:\<path>` form — that shape is exactly what
/// `subst` installs. A physical volume answers `\Device\HarddiskVolumeN`
/// and is skipped.
#[cfg(windows)]
fn build_subst_aliases() -> SubstAliases {
    use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;

    let mut out: SubstAliases = Vec::new();
    for letter in b'A'..=b'Z' {
        let name: [u16; 3] = [letter as u16, u16::from(b':'), 0];
        let mut buf = vec![0u16; 1024];
        // SAFETY: `name` is a NUL-terminated UTF-16 string and `buf` is a
        // writable buffer whose length we pass truthfully. `QueryDosDeviceW`
        // returns 0 on failure (including ERROR_FILE_NOT_FOUND for an
        // unmapped letter), which we treat as "not a subst".
        let written = unsafe { QueryDosDeviceW(name.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
        if written == 0 {
            continue;
        }
        // The result is a NUL-separated, double-NUL-terminated list; the
        // first entry is the active mapping.
        let first_len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        if first_len == 0 {
            continue;
        }
        let target = String::from_utf16_lossy(&buf[..first_len]);
        let Some(rest) = target.strip_prefix("\\??\\") else {
            continue;
        };
        let rest = rest.trim_end_matches('\\');
        if !is_drive_rooted(rest) {
            continue;
        }
        out.push((format!("{}:", letter as char), rest.to_string()));
    }
    if !out.is_empty() {
        info!(
            "Resolved {} subst drive alias(es) for project path normalization: {:?}",
            out.len(),
            out
        );
    }
    out
}

#[cfg(not(windows))]
fn build_subst_aliases() -> SubstAliases {
    // `subst` is a Windows-only concept.
    Vec::new()
}

/// True when `s` starts with `X:\` or `X:/` (a drive-rooted absolute path).
#[cfg(windows)]
fn is_drive_rooted(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

// ============================================================================
// Path normalization
// ============================================================================

/// Normalize a project path so two spellings of the same directory compare
/// equal. Five normalizations, applied in this order:
///
/// 1. **Trailing separators** trimmed (`D:\foo\` → `D:\foo`).
/// 2. **MSYS/Git-Bash roots** rewritten (`/d/foo` → `D:/foo`). Windows only —
///    on a real POSIX system `/d/foo` is a legitimate path.
/// 3. **Slash direction** unified to `\` (Windows only).
/// 4. **`subst` aliases resolved** to their physical target
///    (`D:\qontinui-root` → `C:\Users\jspin\Documents\qontinui-root`), using
///    the machine's live alias table (see [`subst_aliases`]).
/// 5. **Case folded** (Windows only). Windows filesystems are
///    case-insensitive and every producer writes its own casing: the wizard
///    uses the OS-reported name, `session_touched_files` records whatever the
///    agent's tool passed, the terminal uses what the user typed.
///
/// The fold uses `str::to_lowercase` (full Unicode), not
/// `to_ascii_lowercase`, so it agrees with Postgres `lower(file_path)` under
/// the `en_US.utf8` collation — the SQL side of the same join.
///
/// The result is *not* a valid path to hand to the OS; it is a comparison
/// key. Keep the original around for anything that touches disk.
pub(crate) fn normalize_path(path: &str) -> String {
    normalize_path_with_aliases(path, subst_aliases())
}

/// [`normalize_path`] with the alias table injected, so tests can exercise
/// subst resolution deterministically instead of depending on whatever the
/// host machine happens to have `subst`ed.
fn normalize_path_with_aliases(path: &str, aliases: &[(String, String)]) -> String {
    let trimmed = path.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        return String::new();
    }

    #[cfg(windows)]
    {
        // (2) MSYS root — `/d/foo` / `/d` → `D:/foo` / `D:`. Done while the
        // separators are still forward slashes.
        let msys = rewrite_msys_root(trimmed);
        // (3) Slash direction.
        let mut s = msys.replace('/', "\\");
        // (4) subst alias resolution, bounded so a pathological chain (or a
        // cycle installed by hand) can never spin.
        for _ in 0..8 {
            match resolve_subst_once(&s, aliases) {
                Some(next) => s = next,
                None => break,
            }
        }
        // (5) Case fold.
        s.to_lowercase()
    }

    #[cfg(not(windows))]
    {
        let _ = aliases;
        trimmed.to_string()
    }
}

/// Rewrite an MSYS/Git-Bash drive root: `/d/foo/bar` → `D:/foo/bar`,
/// `/d` → `D:`. Anything else is returned unchanged.
#[cfg(windows)]
fn rewrite_msys_root(path: &str) -> String {
    let b = path.as_bytes();
    if b.len() < 2 || b[0] != b'/' || !b[1].is_ascii_alphabetic() {
        return path.to_string();
    }
    match b.get(2) {
        None => format!("{}:", (b[1] as char).to_ascii_uppercase()),
        Some(b'/') => format!(
            "{}:{}",
            (b[1] as char).to_ascii_uppercase(),
            &path[2..] // keeps the leading '/', giving "D:/foo/bar"
        ),
        // `/db/...` etc. — not a drive root.
        Some(_) => path.to_string(),
    }
}

/// One pass of subst resolution. Returns `Some(rewritten)` when `path`'s
/// drive letter is an alias, `None` when it is not (the fixed point).
#[cfg(windows)]
fn resolve_subst_once(path: &str, aliases: &[(String, String)]) -> Option<String> {
    let b = path.as_bytes();
    if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
        return None;
    }
    let drive = &path[..2];
    let rest = &path[2..];
    for (alias, target) in aliases {
        if alias.eq_ignore_ascii_case(drive) {
            let target = target.trim_end_matches(['\\', '/']).replace('/', "\\");
            // A cycle guard: refuse a mapping that would re-introduce the
            // same drive letter we just resolved.
            if target.len() >= 2 && target[..2].eq_ignore_ascii_case(drive) {
                return None;
            }
            return Some(format!("{}{}", target, rest));
        }
    }
    None
}

/// Prefix-boundary test: is `candidate` the same directory as `root`, or a
/// descendant of it?
///
/// **Both arguments must already be [`normalize_path`]-normalized.** A naive
/// `starts_with` would make the project root `D:\foo` swallow `D:\foobar`;
/// this requires the candidate to equal the root or continue with a path
/// separator.
pub(crate) fn is_under(root: &str, candidate: &str) -> bool {
    if root.is_empty() {
        return false;
    }
    if candidate == root {
        return true;
    }
    let Some(rest) = candidate.strip_prefix(root) else {
        return false;
    };
    // A drive-root project (`d:\`) already ends with the separator, so any
    // non-empty remainder is a descendant.
    if root.ends_with(std::path::MAIN_SEPARATOR) {
        return !rest.is_empty();
    }
    rest.starts_with(std::path::MAIN_SEPARATOR)
}

// NOTE: paths are stored **as given**, never normalized on write. The
// normalized form is a comparison key, not a path: it is lowercased and
// subst-resolved, so persisting it would turn a user's
// `D:\projects\Papas-Pizzeria` into
// `c:\users\jspin\documents\projects\papas-pizzeria` on screen and in every
// "where does it live?" surface. Normalize at comparison time instead —
// which is every call site in this module and in `projects::snapshot`.

// ============================================================================
// `id` backfill
// ============================================================================

/// Mint a UUID `id` for any entry that predates the field.
///
/// Returns `true` when at least one entry was changed, so the caller knows to
/// persist. An empty-string `id` is a load-time artifact of
/// `#[serde(default)]`, never a legitimate key — leaving it in place would
/// make every such entry collide on removal and on `active_project_id`.
fn backfill_ids(projects: &mut [SavedProject]) -> bool {
    let mut changed = false;
    for project in projects.iter_mut() {
        if project.id.trim().is_empty() {
            project.id = uuid::Uuid::new_v4().to_string();
            changed = true;
        }
    }
    changed
}

/// Read the persisted list, backfilling (and re-saving) missing ids.
///
/// Every read path goes through here so an id is guaranteed present the
/// moment any caller sees a `SavedProject`.
pub(crate) fn load_projects_backfilled() -> Vec<SavedProject> {
    let mut projects = settings::get_saved_projects();
    if backfill_ids(&mut projects) {
        info!(
            "Backfilled ids for saved projects lacking one ({} entries total)",
            projects.len()
        );
        if let Err(e) = settings::save_saved_projects(projects.clone()) {
            // Non-fatal: the caller still gets usable ids for this call, they
            // just won't be stable across restarts until a write succeeds.
            warn!("Failed to persist backfilled saved-project ids: {}", e);
        }
    }
    projects
}

/// Apply `mutate` to the project with `id` and persist. `Ok(false)` when no
/// entry matched (callers decide whether that is an error).
fn update_project<F: FnOnce(&mut SavedProject)>(id: &str, mutate: F) -> Result<bool, String> {
    let mut projects = load_projects_backfilled();
    let Some(entry) = projects.iter_mut().find(|p| p.id == id) else {
        return Ok(false);
    };
    mutate(entry);
    settings::save_saved_projects(projects)?;
    Ok(true)
}

// ============================================================================
// Tauri commands
// ============================================================================

/// Return the persisted saved-projects list.
///
/// Always `Ok`. First-run (no entry in `settings.json`) yields an empty vec.
#[tauri::command]
pub fn list_saved_projects() -> Result<Vec<SavedProject>, String> {
    Ok(load_projects_backfilled())
}

/// Atomically replace the persisted saved-projects list.
///
/// Used by the setup wizard on "Next" when the user commits their
/// project selection. Every entry is guaranteed an `id` before persisting.
#[tauri::command]
pub fn save_saved_projects(mut projects: Vec<SavedProject>) -> Result<(), String> {
    backfill_ids(&mut projects);
    info!("Saving {} saved project(s)", projects.len());
    settings::save_saved_projects(projects)
}

/// Idempotently add a project to the persisted list.
///
/// If a project with the same normalized `path` is already present, this is
/// a no-op and returns `Ok(())` without touching disk. Otherwise appends
/// and persists, minting an `id` when the caller did not supply one.
#[tauri::command]
pub fn add_saved_project(mut project: SavedProject) -> Result<(), String> {
    let mut current = load_projects_backfilled();
    let incoming = normalize_path(&project.path);

    if current.iter().any(|p| normalize_path(&p.path) == incoming) {
        // Already present — idempotent no-op.
        info!(
            "Saved project already present, skipping add: {}",
            project.path
        );
        return Ok(());
    }

    if project.id.trim().is_empty() {
        project.id = uuid::Uuid::new_v4().to_string();
    }

    info!("Adding saved project: {} ({})", project.path, project.id);
    current.push(project);
    settings::save_saved_projects(current)
}

/// Remove a project from the persisted list **by id**.
///
/// `id` is the stable key — a project's path can move on disk, its identity
/// cannot. If no entry matches, this is a no-op and returns `Ok(())`.
///
/// Clears `active_project_id` when the removed project was the active one,
/// so nothing is left pointing at a deleted entry.
#[tauri::command]
pub fn remove_saved_project(id: String) -> Result<(), String> {
    let mut current = load_projects_backfilled();
    let before = current.len();
    current.retain(|p| p.id != id);
    if current.len() == before {
        // Not found — per contract, no error.
        info!("remove_saved_project: id not present, no-op: {}", id);
        return Ok(());
    }
    info!(
        "Removed saved project ({} -> {} entries): {}",
        before,
        current.len(),
        id
    );
    settings::save_saved_projects(current)?;

    if settings::get_active_project_id().as_deref() == Some(id.as_str()) {
        settings::save_active_project_id(None)?;
    }
    Ok(())
}

/// Scan `workspace_dir` for candidate projects.
///
/// A thin wrapper over the setup wizard's existing discovery: it reuses
/// [`crate::commands::setup_wizard::scan_workspace_for_setup`] to find git
/// roots carrying a recognised manifest, and
/// [`crate::commands::setup_wizard::detect_project_framework_for_setup`] to
/// sharpen the coarse manifest-derived type into a framework key
/// ("react", "fastapi", …). Nothing is persisted — the caller decides which
/// candidates to keep.
///
/// A candidate whose normalized path already matches a saved project keeps
/// that project's `id` (and its user-authored fields), so the UI can render
/// "already added" instead of proposing a duplicate.
#[tauri::command]
pub async fn discover_projects(workspace_dir: String) -> Result<Vec<SavedProject>, String> {
    let found =
        crate::commands::setup_wizard::scan_workspace_for_setup(workspace_dir, None).await?;
    let existing = load_projects_backfilled();

    let entries = found.as_array().cloned().unwrap_or_default();
    let mut out: Vec<SavedProject> = Vec::with_capacity(entries.len());

    for entry in entries {
        let Some(path) = entry.get("path").and_then(|v| v.as_str()) else {
            continue;
        };
        let normalized = normalize_path(path);
        if normalized.is_empty() {
            continue;
        }
        let name = entry
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let manifest = entry
            .get("manifest")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let coarse_type = entry
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Sharpen the manifest-derived type with the framework detector.
        // "unknown" from the detector means "no framework scored" — keep the
        // coarse manifest type in that case rather than degrading it.
        let project_type = match crate::commands::setup_wizard::detect_project_framework_for_setup(
            path.to_string(),
        )
        .await
        {
            Ok(fw) => match fw.get("key").and_then(|v| v.as_str()) {
                Some(key) if key != "unknown" && !key.is_empty() => key.to_string(),
                _ => coarse_type,
            },
            Err(e) => {
                warn!("discover_projects: framework detection failed for {path}: {e}");
                coarse_type
            }
        };

        if let Some(known) = existing
            .iter()
            .find(|p| normalize_path(&p.path) == normalized)
        {
            out.push(known.clone());
            continue;
        }

        out.push(SavedProject {
            id: uuid::Uuid::new_v4().to_string(),
            // Stored as scanned, not normalized — see the note above
            // `normalize_path`'s call sites.
            path: path.to_string(),
            name,
            project_type,
            manifest,
            description: None,
            emoji: None,
            color: None,
            front_page_url: None,
            process_ids: Vec::new(),
            terminal_page_id: None,
            zone_profile: None,
            repo_slug: None,
            notes: None,
            pinned: false,
            last_opened_ms: None,
        });
    }

    info!("discover_projects: {} candidate(s)", out.len());
    Ok(out)
}

/// The joined dashboard view of one project. See
/// [`crate::projects::snapshot::build_snapshot`].
///
/// Process status comes through
/// [`crate::process_capture::commands::get_managed_processes`] rather than
/// straight off the manager, so a secondary runner's proxy-to-primary path is
/// reused rather than re-implemented.
#[tauri::command]
pub async fn project_snapshot(
    terminal_manager: tauri::State<'_, std::sync::Arc<crate::terminal::TerminalManager>>,
    state: tauri::State<'_, std::sync::Arc<crate::commands::AppState>>,
    id: String,
) -> Result<qontinui_types::projects::ProjectSnapshot, String> {
    let projects = load_projects_backfilled();
    let terminals = terminal_manager.list();
    let statuses = crate::process_capture::commands::get_managed_processes(state)
        .await
        .unwrap_or_else(|e| {
            warn!("project_snapshot: process status unavailable: {e}");
            Vec::new()
        });
    crate::projects::snapshot::build_snapshot(&projects, &id, &terminals, &statuses).await
}

/// Set (or clear, with `None`) a project's front-page URL.
#[tauri::command]
pub fn set_project_front_page(id: String, url: Option<String>) -> Result<(), String> {
    let url = url.map(|u| u.trim().to_string()).filter(|u| !u.is_empty());
    if update_project(&id, |p| p.front_page_url = url.clone())? {
        Ok(())
    } else {
        Err(format!("No saved project with id {id}"))
    }
}

/// Pin or unpin a project (plan §5.1 card extras).
///
/// A pinned project sorts first in the grid and earns a sidebar row. Returns
/// the value actually stored so the caller can reconcile its optimistic toggle
/// against the persisted truth rather than assuming its own write won.
#[tauri::command]
pub fn set_project_pinned(id: String, pinned: bool) -> Result<bool, String> {
    if update_project(&id, |p| p.pinned = pinned)? {
        Ok(pinned)
    } else {
        Err(format!("No saved project with id {id}"))
    }
}

/// Bind the set of managed-process config ids this project owns.
///
/// Replaces the existing binding wholesale (the UI edits the whole set), and
/// de-duplicates while preserving the caller's order.
#[tauri::command]
pub fn bind_project_processes(id: String, process_ids: Vec<String>) -> Result<(), String> {
    let mut deduped: Vec<String> = Vec::with_capacity(process_ids.len());
    for pid in process_ids {
        let pid = pid.trim().to_string();
        if !pid.is_empty() && !deduped.contains(&pid) {
            deduped.push(pid);
        }
    }
    if update_project(&id, |p| p.process_ids = deduped.clone())? {
        Ok(())
    } else {
        Err(format!("No saved project with id {id}"))
    }
}

/// Bind (or, with `None`, unbind) the Terminal page this project activates
/// onto.
///
/// This stores a **hint, not a handle** (plan §4). The pages themselves live
/// in the frontend's `instanceStorage` — localStorage, port-namespaced — which
/// Rust cannot read, so an id written here can perfectly legitimately name a
/// page that does not exist in some future window (a cleared WebView2
/// profile, a different runner instance, a page the operator deleted). There
/// is therefore nothing on this side to validate the id against, and this
/// command deliberately does not try: the frontend's `resolveProjectPage`
/// tolerates a dangling id by re-creating the page under it, then calls back
/// here with whatever id it settled on.
#[tauri::command]
pub fn set_project_terminal_page(id: String, page_id: Option<String>) -> Result<(), String> {
    let page_id = page_id
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());
    if update_project(&id, |p| p.terminal_page_id = page_id.clone())? {
        Ok(())
    } else {
        Err(format!("No saved project with id {id}"))
    }
}

/// Build the Tauri plugin that registers this module's command handlers.
pub fn plugin<R: Runtime>() -> TauriPlugin<R> {
    PluginBuilder::new("qontinui_saved_projects")
        .invoke_handler(tauri::generate_handler![
            list_saved_projects,
            save_saved_projects,
            add_saved_project,
            remove_saved_project,
        ])
        .build()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(path: &str) -> SavedProject {
        SavedProject {
            id: uuid::Uuid::new_v4().to_string(),
            path: path.to_string(),
            name: "demo".to_string(),
            project_type: "node".to_string(),
            manifest: "package.json".to_string(),
            description: None,
            emoji: None,
            color: None,
            front_page_url: None,
            process_ids: Vec::new(),
            terminal_page_id: None,
            zone_profile: None,
            repo_slug: None,
            notes: None,
            pinned: false,
            last_opened_ms: None,
        }
    }

    /// The alias table this box actually runs, used as the fixture so the
    /// tests exercise the real shape without depending on the host's
    /// `subst` state.
    fn test_aliases() -> Vec<(String, String)> {
        vec![("D:".to_string(), "C:\\Users\\jspin\\Documents".to_string())]
    }

    fn norm(path: &str) -> String {
        normalize_path_with_aliases(path, &test_aliases())
    }

    /// Platform-appropriate absolute path from segments, so the same
    /// assertion holds on Windows (`E:\a\b`) and Linux (`/a/b`).
    fn tp(segments: &[&str]) -> String {
        let sep = std::path::MAIN_SEPARATOR;
        #[cfg(windows)]
        let mut s = String::from("E:");
        #[cfg(not(windows))]
        let mut s = String::new();
        for segment in segments {
            s.push(sep);
            s.push_str(segment);
        }
        s
    }

    // ---- normalize_path: the five normalizations -------------------------

    #[test]
    fn normalize_strips_trailing_separators() {
        #[cfg(windows)]
        {
            assert_eq!(norm("D:\\foo\\"), norm("D:\\foo"));
            assert_eq!(norm("D:\\foo///"), norm("D:\\foo"));
            assert_eq!(norm("E:\\foo\\"), "e:\\foo");
        }
        #[cfg(not(windows))]
        {
            assert_eq!(norm("/home/me/"), "/home/me");
            assert_eq!(norm("/home/me"), "/home/me");
        }
    }

    #[test]
    #[cfg(windows)]
    fn normalize_unifies_slash_direction() {
        assert_eq!(norm("E:/foo/bar"), "e:\\foo\\bar");
        assert_eq!(norm("E:\\foo/bar"), "e:\\foo\\bar");
    }

    #[test]
    #[cfg(windows)]
    fn normalize_folds_the_whole_path_not_just_the_drive() {
        // The pre-dashboard implementation only uppercased the drive letter,
        // so these two spellings of one directory compared unequal.
        assert_eq!(norm("E:\\Projects\\Pizza"), norm("e:\\projects\\pizza"));
        assert_eq!(norm("E:\\Projects\\Pizza"), "e:\\projects\\pizza");
    }

    #[test]
    #[cfg(windows)]
    fn normalize_rewrites_msys_drive_roots() {
        assert_eq!(norm("/e/foo/bar"), "e:\\foo\\bar");
        assert_eq!(norm("/E/Foo"), "e:\\foo");
        assert_eq!(norm("/e"), "e:");
        // Not a drive root — `/db` is a two-letter first segment.
        assert_eq!(norm("/db/foo"), "\\db\\foo");
        // A single leading slash with no letter is left alone.
        assert_eq!(norm("/"), "");
    }

    #[test]
    #[cfg(windows)]
    fn normalize_resolves_subst_aliases() {
        let expected = "c:\\users\\jspin\\documents\\qontinui-root\\qontinui-web";
        assert_eq!(norm("D:\\qontinui-root\\qontinui-web"), expected);
        assert_eq!(norm("d:/qontinui-root/qontinui-web"), expected);
        assert_eq!(norm("/d/qontinui-root/qontinui-web"), expected);
        assert_eq!(
            norm("C:\\Users\\jspin\\Documents\\qontinui-root\\qontinui-web"),
            expected
        );
    }

    #[test]
    #[cfg(windows)]
    fn normalize_all_four_live_spellings_agree() {
        // The exact finding from the Phase-0 probe: one workspace root with
        // four spellings in `coord.session_touched_files`.
        let spellings = [
            "D:\\qontinui-root\\qontinui-runner\\src\\main.rs",
            "D:/qontinui-root/qontinui-runner/src/main.rs",
            "/d/qontinui-root/qontinui-runner/src/main.rs",
            "C:\\Users\\jspin\\Documents\\qontinui-root\\qontinui-runner\\src\\main.rs",
        ];
        let normalized: Vec<String> = spellings.iter().map(|s| norm(s)).collect();
        assert!(
            normalized.windows(2).all(|w| w[0] == w[1]),
            "all four spellings must normalize identically, got {normalized:?}"
        );
    }

    #[test]
    #[cfg(windows)]
    fn normalize_leaves_non_alias_drives_alone() {
        assert_eq!(norm("E:\\other\\root"), "e:\\other\\root");
    }

    #[test]
    #[cfg(windows)]
    fn normalize_subst_chain_terminates() {
        // X: -> Y:, Y: -> Z:\real. Two hops, must land on the physical path
        // rather than looping.
        let aliases = vec![
            ("X:".to_string(), "Y:\\mid".to_string()),
            ("Y:".to_string(), "Z:\\real".to_string()),
        ];
        assert_eq!(
            normalize_path_with_aliases("X:\\leaf", &aliases),
            "z:\\real\\mid\\leaf"
        );
    }

    #[test]
    #[cfg(windows)]
    fn normalize_self_referential_subst_does_not_spin() {
        // A hand-installed `P: -> P:\loop` must terminate, not hang.
        let aliases = vec![("P:".to_string(), "P:\\loop".to_string())];
        assert_eq!(
            normalize_path_with_aliases("P:\\leaf", &aliases),
            "p:\\leaf"
        );
    }

    #[test]
    fn normalize_empty_and_separator_only_are_empty() {
        assert_eq!(norm(""), "");
        assert_eq!(norm("\\\\"), "");
    }

    // ---- is_under: the prefix boundary -----------------------------------

    #[test]
    fn is_under_matches_self_and_descendants() {
        let root = norm(&tp(&["foo"]));
        assert!(is_under(&root, &root), "a root is under itself");
        assert!(is_under(&root, &norm(&tp(&["foo", "bar"]))));
        assert!(is_under(&root, &norm(&tp(&["foo", "bar", "baz.rs"]))));
    }

    #[test]
    fn is_under_rejects_sibling_with_shared_prefix() {
        // The defect this helper exists to prevent: `D:\foo` must not
        // swallow `D:\foobar`.
        let root = norm(&tp(&["foo"]));
        assert!(!is_under(&root, &norm(&tp(&["foobar"]))));
        assert!(!is_under(&root, &norm(&tp(&["foobar", "baz.rs"]))));
        assert!(!is_under(&root, &norm(&tp(&["fo"]))));
    }

    #[test]
    fn is_under_rejects_unrelated_and_empty_roots() {
        assert!(!is_under(
            &norm(&tp(&["foo"])),
            &norm(&tp(&["other", "bar"]))
        ));
        assert!(!is_under("", &norm(&tp(&["foo"]))));
    }

    #[test]
    #[cfg(windows)]
    fn is_under_handles_a_drive_root_project() {
        let root = norm("E:\\");
        assert_eq!(root, "e:");
        assert!(is_under(&root, &norm("E:\\foo")));
        // A drive root that keeps its separator (constructed, not normalized)
        // must still take the trailing-separator branch.
        assert!(is_under("e:\\", "e:\\foo"));
        // Self-match is TRUE by contract: `is_under` answers "same directory
        // OR descendant" (see its doc comment), and the `candidate == root`
        // early return fires before the trailing-separator branch is reached.
        // This assertion previously expected `false`, which contradicted that
        // contract and failed on Windows only — the test is #[cfg(windows)],
        // so Linux never ran it.
        assert!(is_under("e:\\", "e:\\"));
        // The meaningful negative for a drive root: a DIFFERENT drive is not a
        // descendant, whatever the separators. `strip_prefix` fails outright,
        // so this never reaches the trailing-separator branch.
        assert!(!is_under("e:\\", "f:\\foo"));
    }

    #[test]
    #[cfg(windows)]
    fn is_under_works_across_spellings_after_normalization() {
        let root = norm("D:\\qontinui-root\\qontinui-runner");
        assert!(is_under(
            &root,
            &norm("/d/qontinui-root/qontinui-runner/src/main.rs")
        ));
        assert!(is_under(
            &root,
            &norm("C:\\Users\\jspin\\Documents\\qontinui-root\\qontinui-runner\\Cargo.toml")
        ));
        assert!(!is_under(
            &root,
            &norm("D:\\qontinui-root\\qontinui-runner-wt-projects-dash\\Cargo.toml")
        ));
    }

    #[test]
    fn nested_project_roots_both_match_longest_wins_is_the_callers_job() {
        // `is_under` is deliberately not exclusive: a file under a nested
        // project matches BOTH roots. Longest-prefix attribution is resolved
        // once by the snapshot layer over the whole project list.
        let outer = norm(&tp(&["work"]));
        let inner = norm(&tp(&["work", "pizzeria"]));
        let file = norm(&tp(&["work", "pizzeria", "src", "menu.tsx"]));
        assert!(is_under(&outer, &file));
        assert!(is_under(&inner, &file));
        assert!(inner.len() > outer.len(), "inner root is the longer prefix");
    }

    // ---- registry semantics ----------------------------------------------

    #[test]
    fn add_is_idempotent_on_same_path() {
        // Pure in-memory logic: mimic add_saved_project's dedupe check.
        let existing = [sample(&tp(&["tmp", "proj"]))];
        let incoming = normalize_path(&format!(
            "{}{}",
            tp(&["tmp", "proj"]),
            std::path::MAIN_SEPARATOR
        ));
        let already_present = existing.iter().any(|p| normalize_path(&p.path) == incoming);
        assert!(
            already_present,
            "normalized trailing-separator variant should be treated as present"
        );
    }

    #[test]
    fn remove_by_id_ignores_path() {
        // Removal is id-keyed now: two entries at different paths must be
        // distinguishable, and a path collision must not remove the wrong one.
        let a = sample(&tp(&["tmp", "a"]));
        let b = sample(&tp(&["tmp", "b"]));
        let target = a.id.clone();
        let mut current = vec![a, b.clone()];
        current.retain(|p| p.id != target);
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].id, b.id);
    }

    #[test]
    fn remove_missing_id_is_no_op() {
        let mut current = vec![sample(&tp(&["tmp", "a"])), sample(&tp(&["tmp", "b"]))];
        let before = current.len();
        current.retain(|p| p.id != "not-a-real-id");
        assert_eq!(
            current.len(),
            before,
            "retain must not drop anything when the id is absent"
        );
    }

    #[test]
    fn backfill_mints_ids_only_for_empty_ones() {
        let mut keeper = sample(&tp(&["tmp", "keep"]));
        keeper.id = "stable-id".to_string();
        let mut orphan = sample(&tp(&["tmp", "orphan"]));
        orphan.id = String::new();

        let mut projects = vec![keeper, orphan];
        assert!(backfill_ids(&mut projects), "must report a change");
        assert_eq!(projects[0].id, "stable-id", "existing id is preserved");
        assert!(!projects[1].id.is_empty(), "missing id is minted");
        assert_ne!(projects[0].id, projects[1].id);

        assert!(
            !backfill_ids(&mut projects),
            "a second pass must be a no-op"
        );
    }

    #[test]
    fn backfill_treats_whitespace_id_as_missing() {
        let mut project = sample(&tp(&["tmp", "ws"]));
        project.id = "   ".to_string();
        let mut projects = vec![project];
        assert!(backfill_ids(&mut projects));
        assert!(!projects[0].id.trim().is_empty());
    }

    #[test]
    fn saved_project_round_trips_camel_case_wire() {
        let mut project = sample("E:\\foo");
        project.front_page_url = Some("http://localhost:3000".to_string());
        project.process_ids = vec!["proc-1".to_string()];
        project.pinned = true;
        project.last_opened_ms = Some(1_700_000_000_000);

        let json = serde_json::to_string(&project).expect("serialize");
        assert!(
            json.contains("\"frontPageUrl\""),
            "wire is camelCase: {json}"
        );
        assert!(json.contains("\"processIds\""), "wire is camelCase: {json}");
        assert!(
            json.contains("\"lastOpenedMs\""),
            "wire is camelCase: {json}"
        );

        let back: SavedProject = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.front_page_url, project.front_page_url);
        assert_eq!(back.process_ids, project.process_ids);
        assert!(back.pinned);
    }

    #[test]
    fn saved_project_loads_a_pre_dashboard_settings_entry() {
        // The 4-field shape that exists in every installed settings.json.
        let legacy = r#"{
            "path": "D:\\projects\\pizza",
            "name": "Pizza",
            "projectType": "react",
            "manifest": "package.json"
        }"#;
        let project: SavedProject = serde_json::from_str(legacy).expect("legacy entry must load");
        assert!(
            project.id.is_empty(),
            "id defaults empty so backfill can mint one"
        );
        assert!(project.process_ids.is_empty());
        assert!(!project.pinned);

        let mut projects = vec![project];
        assert!(backfill_ids(&mut projects));
        assert!(!projects[0].id.is_empty());
    }
}
