//! Declaring the sibling a session will EDIT, so the sibling-store override
//! stops hiding its edits.
//!
//! Plan `2026-10-10-a-session-cannot-declare-the-sibling-it-will-edit-so-the-store-override-hides-its-edits`
//! Phases 2 and 3.
//!
//! A coord allocation that carries declared build siblings gets
//! `<agent_root>/.cargo/config.toml`, a `paths` override that points every
//! sibling path dependency at the read-only, SHA-pinned store
//! `.siblings/<repo>@<sha>/` (see [`super::sibling_store`]). That is right for a
//! sibling the session only BUILDS against, and wrong for one it EDITS: cargo
//! keeps resolving the store copy, so the session's edits in
//! `<agent_root>/<sibling>/` never reach its build.
//!
//! Two doors take one sibling off the override, and both leave every OTHER
//! sibling on the store:
//!
//! - **Spawn time** — the terminal spawn surfaces accept `editSiblings`. The
//!   runner sends each one in the allocate's `repos` AND `edit_siblings` (vet
//!   V1: a coord-added requested row the runner did not ask for has no
//!   canonical path and would fail the allocation), drops any store entry for it
//!   from coord's `cargo_config` before writing ([`drop_edit_sibling_entries`]),
//!   and records it in the marker.
//! - **Mid-session** — `POST /agent-worktrees/edit-sibling {path, repo}` runs
//!   [`apply`]: it rewrites the config without that sibling's entries,
//!   atomically and header-preserving, and records it in the marker.
//!
//! The config is filtered TEXTUALLY — only the matching array lines go — and
//! never re-serialised through `toml`, which would drop coord's header comment
//! (vet V10). The result is re-parsed before it is written, and a body that no
//! longer parses is refused rather than written.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use tracing::{info, warn};

use super::canonical_paths;
use super::sibling_store::{override_path_entries, SIBLING_STORE_DIRNAME};
use crate::worktree::run_git_command;

/// The edit-sibling marker: `<agent_root>/.cargo/edit-siblings`, one bare repo
/// name per line, each line a sibling this allocation EDITS rather than builds
/// from the store.
///
/// Shared contract with qontinui-claude-config's `scripts/allocate-worktree.sh`
/// (its `--edit-sibling` flag and mid-session mode write the same file, in the
/// same format), exactly as [`super::sibling_store::COMPLETE_MARKER`] is. A
/// census reads it to tell a deliberate opt-out from a hand-disabled config:
/// a sibling named here is EXPECTED to build per-allocation.
pub(crate) const EDIT_SIBLINGS_MARKER: &str = "edit-siblings";

/// The directory holding the cargo config and the marker, under the agent root.
const CARGO_DIRNAME: &str = ".cargo";

/// The cargo config file name coord's override is written to.
const CARGO_CONFIG_FILENAME: &str = "config.toml";

/// The machine-readable code every edit-sibling input refusal carries.
pub(crate) const INVALID_EDIT_SIBLING: &str = "invalid_edit_sibling";

/// `<agent_root>/.cargo/edit-siblings`.
pub(crate) fn marker_path(agent_root: &Path) -> PathBuf {
    agent_root.join(CARGO_DIRNAME).join(EDIT_SIBLINGS_MARKER)
}

/// `<agent_root>/.cargo/config.toml`.
pub(crate) fn config_path(agent_root: &Path) -> PathBuf {
    agent_root.join(CARGO_DIRNAME).join(CARGO_CONFIG_FILENAME)
}

/// Whether `s` is a non-empty run of `[A-Za-z0-9._-]` that is not `.` or `..`.
fn is_safe_name_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Validate an edit-sibling name and reduce it to its bare repo name.
///
/// Accepts `name` or `owner/name`. Each part must match `^[A-Za-z0-9._-]+$`
/// and must not be `.` or `..` (vet V8: the plan's first class,
/// `^[A-Za-z0-9._/-]+$`, admitted `..`, and the bare name becomes a directory
/// segment under the agent root). Surrounding whitespace is ignored.
pub(crate) fn bare_repo_name(repo: &str) -> Result<String, String> {
    let trimmed = repo.trim();
    let parts: Vec<&str> = trimmed.split('/').collect();
    let bare = match parts.as_slice() {
        [name] => *name,
        [owner, name] if is_safe_name_segment(owner) => *name,
        _ => {
            return Err(format!(
                "edit sibling {repo:?} must be a bare repo name or `owner/name`"
            ))
        }
    };
    if !is_safe_name_segment(bare) {
        return Err(format!(
            "edit sibling {repo:?}: the repo name must match ^[A-Za-z0-9._-]+$ and must not be \
             `.` or `..`"
        ));
    }
    Ok(bare.to_string())
}

/// A spawn surface's refusal of its `editSiblings` input — answered BEFORE any
/// allocate, as a typed [`INVALID_EDIT_SIBLING`] 400 on the HTTP surfaces and
/// an `Err` on the Tauri command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSiblingRefusal {
    pub message: String,
}

impl EditSiblingRefusal {
    pub fn code(&self) -> &'static str {
        INVALID_EDIT_SIBLING
    }
}

impl std::fmt::Display for EditSiblingRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", INVALID_EDIT_SIBLING, self.message)
    }
}

/// Validate a spawn's `editSiblings` before anything is allocated (vet V8, V9).
///
/// Every element must pass [`bare_repo_name`] AND resolve to a checkout through
/// [`canonical_paths::worktree_source_checkout`] — the same door the allocate
/// path cuts a worktree from. Without the resolution check a typo is not loud:
/// the allocate fails, and a terminal's fail-open policy turns that into a
/// `warn!` and a pane in the shared checkout (vet V9).
///
/// A non-empty list with no `intent_repo` is refused too: there is no
/// allocation for the declaration to apply to, and silently ignoring it would
/// leave the caller believing its sibling is editable.
///
/// Returns the trimmed entries, deduplicated by bare name (case-insensitive),
/// in request order.
pub fn validate_spawn_edit_siblings(
    intent_repo: Option<&str>,
    edit_siblings: &[String],
) -> Result<Vec<String>, EditSiblingRefusal> {
    validate_spawn_edit_siblings_with(
        intent_repo,
        edit_siblings,
        canonical_paths::worktree_source_checkout,
    )
}

/// [`validate_spawn_edit_siblings`] with the checkout resolver injected.
pub(crate) fn validate_spawn_edit_siblings_with(
    intent_repo: Option<&str>,
    edit_siblings: &[String],
    resolve_checkout: impl Fn(&str) -> Result<PathBuf, String>,
) -> Result<Vec<String>, EditSiblingRefusal> {
    if edit_siblings.is_empty() {
        return Ok(Vec::new());
    }
    let Some(intent) = intent_repo.map(str::trim).filter(|r| !r.is_empty()) else {
        return Err(EditSiblingRefusal {
            message: "editSiblings needs an intentRepo: with no allocation there is no sibling \
                      override to take a sibling off"
                .to_string(),
        });
    };
    let intent_key = match_key(intent);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::with_capacity(edit_siblings.len());
    for raw in edit_siblings {
        let bare = bare_repo_name(raw).map_err(|message| EditSiblingRefusal { message })?;
        let trimmed = raw.trim().to_string();
        // The intent repo is the allocation's primary, never a sibling of
        // itself; naming it would record a bogus opt-out in the marker.
        if intent_key.as_deref() == Some(bare.to_ascii_lowercase().as_str()) {
            return Err(EditSiblingRefusal {
                message: format!(
                    "edit sibling {trimmed:?} is the intent repo itself — it is the allocation's \
                     primary and is never redirected"
                ),
            });
        }
        match resolve_checkout(&trimmed) {
            Ok(path) if path.join(".git").exists() => {}
            Ok(path) => {
                return Err(EditSiblingRefusal {
                    message: format!(
                        "edit sibling {trimmed:?} resolves to {}, which is not a git checkout on \
                         this device",
                        path.display()
                    ),
                })
            }
            Err(e) => {
                return Err(EditSiblingRefusal {
                    message: format!(
                        "edit sibling {trimmed:?} resolves to no checkout on this device: {e}"
                    ),
                })
            }
        }
        if seen.insert(bare.to_ascii_lowercase()) {
            out.push(trimmed);
        }
    }
    Ok(out)
}

/// The bare name of `repo`, lowercased, for matching; `None` when `repo` is not
/// a valid name (such an entry never matches anything).
fn match_key(repo: &str) -> Option<String> {
    bare_repo_name(repo).ok().map(|b| b.to_ascii_lowercase())
}

/// The `repos` an allocate sends: `repos` in order, then every edit sibling not
/// already present (matched by bare name, case-insensitive), in request order.
/// The primary stays index 0.
///
/// Vet V1: the fleet callers must request an edit sibling THEMSELVES. A coord
/// that turns `edit_siblings` into a `requested` row hands back a row the runner
/// resolved no canonical path for, and `plan_worktree_rows` fails the whole
/// allocation on it.
pub(crate) fn merge_edit_siblings_into_repos(
    repos: &[String],
    edit_siblings: &[String],
) -> Vec<String> {
    let mut out: Vec<String> = repos.to_vec();
    let mut seen: BTreeSet<String> = repos.iter().filter_map(|r| match_key(r)).collect();
    for e in edit_siblings {
        let Some(key) = match_key(e) else {
            continue;
        };
        if seen.insert(key) {
            out.push(e.trim().to_string());
        }
    }
    out
}

/// The edit siblings an AI-session resume passes: every NON-PRIMARY repo it
/// re-requests (plan D7, narrowed by vet V5). Each of those repos had its own
/// worktree in the original session, so it is plausibly edited; no git state is
/// probed. The primary (index 0) is never included.
pub(crate) fn resume_edit_siblings(repos: &[String]) -> Vec<String> {
    repos.iter().skip(1).cloned().collect()
}

/// The store-path prefixes (`../.siblings/<bare>@`, lowercased) for every valid
/// edit sibling.
fn store_prefixes(edit_siblings: &[String]) -> Vec<String> {
    edit_siblings
        .iter()
        .filter_map(|e| match_key(e))
        .map(|bare| format!("../{SIBLING_STORE_DIRNAME}/{bare}@"))
        .collect()
}

/// Whether a `paths` entry points into the store entry of an edit sibling.
fn entry_is_edit_sibling(entry: &str, prefixes: &[String]) -> bool {
    let norm = entry.trim().replace('\\', "/").to_ascii_lowercase();
    prefixes.iter().any(|p| norm.starts_with(p.as_str()))
}

/// The entry an array line carries when the line is EXACTLY one basic-string
/// literal — `"…"`, optionally followed by `,` — and nothing else. Coord renders
/// every entry that way, one per line, with nothing to escape (its segments are
/// checked). Any other shape answers `None`, so a line holding two entries is
/// never dropped whole (which would lose its other entry); the post-filter
/// re-parse then refuses the body instead.
fn sole_string_literal(line: &str) -> Option<&str> {
    let body = line.trim();
    let body = body.strip_suffix(',').unwrap_or(body).trim_end();
    let inner = body.strip_prefix('"')?.strip_suffix('"')?;
    (!inner.contains('"')).then_some(inner)
}

/// Remove every `paths` entry that points into an edit sibling's store entry —
/// `../.siblings/<bare>@<sha>` itself or any sub-directory of it (a crate dir
/// such as `rust`).
///
/// TEXTUAL: only the array lines carrying such an entry are dropped; every other
/// byte, coord's header comment included, survives unchanged. Returns the new
/// body and the removed entries (in file order).
///
/// `Err` when the body is not a parseable `paths` override, or when the
/// filtered body no longer parses or still names an edit sibling's store (an
/// entry sharing a line with another, which a line filter cannot separate). The
/// caller must then write nothing.
pub(crate) fn drop_edit_sibling_entries(
    contents: &str,
    edit_siblings: &[String],
) -> Result<(String, Vec<String>), String> {
    if override_path_entries(contents).is_none() {
        return Err("the cargo config is not a parseable `paths` override".to_string());
    }
    let prefixes = store_prefixes(edit_siblings);
    if prefixes.is_empty() {
        return Ok((contents.to_string(), Vec::new()));
    }
    let mut out = String::with_capacity(contents.len());
    let mut removed = Vec::new();
    for line in contents.split_inclusive('\n') {
        match sole_string_literal(line) {
            Some(entry) if entry_is_edit_sibling(entry, &prefixes) => {
                removed.push(entry.to_string());
            }
            _ => out.push_str(line),
        }
    }
    let remaining = override_path_entries(&out)
        .ok_or_else(|| "the filtered cargo config no longer parses".to_string())?;
    if let Some(stuck) = remaining
        .iter()
        .find(|e| entry_is_edit_sibling(e, &prefixes))
    {
        return Err(format!(
            "the cargo config still names {stuck:?} after filtering — it shares a line with \
             another entry, which a line filter cannot separate"
        ));
    }
    Ok((out, removed))
}

/// Whether a (filtered) config holds NOTHING worth keeping: it parses, and
/// apart from an empty `paths = []` it has no key or table at all. Only then
/// may the file be deleted (or, at spawn time, not written). A config that also
/// carries anything else — a hand-added `[build]` or `[env]` table — is kept
/// with its emptied `paths`, and one that does not parse is never disposable,
/// so a caller never deletes a file it could not read.
pub(crate) fn is_disposable_config(contents: &str) -> bool {
    let Ok(mut table) = contents.parse::<toml::Table>() else {
        return false;
    };
    if table
        .get("paths")
        .and_then(toml::Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        table.remove("paths");
    }
    table.is_empty()
}

/// Serialises every read-modify-write of an allocation's `.cargo/config.toml`
/// and marker within this process, so two concurrent edit-sibling calls cannot
/// each read the old file and the later writer undo the earlier one's change.
/// Process-wide rather than per agent root: the critical section is a few small
/// file operations, never git.
static EDIT_SIBLING_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn write_lock() -> std::sync::MutexGuard<'static, ()> {
    EDIT_SIBLING_WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Write `contents` to `target` atomically: a temp file in the same directory,
/// then a rename over the target, so a reader never sees a half-written file.
fn write_atomically(target: &Path, contents: &str) -> std::io::Result<()> {
    let dir = target.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} has no parent directory", target.display()),
        )
    })?;
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".edit-sibling-")
        .suffix(".tmp")
        .tempfile_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(target).map_err(|e| e.error)?;
    Ok(())
}

/// The bare names already recorded in the marker (lowercased keys and the
/// original spelling), in file order. A missing marker is an empty list.
fn read_marker(agent_root: &Path) -> std::io::Result<Vec<String>> {
    match std::fs::read_to_string(marker_path(agent_root)) {
        Ok(s) => Ok(s
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// Record every edit sibling's bare name in `<agent_root>/.cargo/edit-siblings`,
/// appending only names not already there (idempotent). Returns the marker's
/// full contents after the write. Written atomically.
pub(crate) fn record_edit_siblings(
    agent_root: &Path,
    edit_siblings: &[String],
) -> std::io::Result<Vec<String>> {
    let _guard = write_lock();
    record_edit_siblings_unlocked(agent_root, edit_siblings)
}

/// [`record_edit_siblings`] for a caller already holding [`write_lock`].
fn record_edit_siblings_unlocked(
    agent_root: &Path,
    edit_siblings: &[String],
) -> std::io::Result<Vec<String>> {
    let mut lines = read_marker(agent_root)?;
    let mut seen: BTreeSet<String> = lines.iter().map(|l| l.to_ascii_lowercase()).collect();
    let mut changed = false;
    for e in edit_siblings {
        let Ok(bare) = bare_repo_name(e) else {
            continue;
        };
        if seen.insert(bare.to_ascii_lowercase()) {
            lines.push(bare);
            changed = true;
        }
    }
    if changed {
        let mut body = lines.join("\n");
        body.push('\n');
        write_atomically(&marker_path(agent_root), &body)?;
    }
    Ok(lines)
}

/// Spawn-time half (Phase 2): drop the edit siblings' entries from coord's
/// re-rooted `cargo_config` body before it is gated and written.
///
/// Returns `Some(filtered body)` to write, or `None` to write NO config — every
/// entry was an edit sibling's, or the body could not be filtered safely (a
/// body that cannot be filtered must not be written: it would redirect the very
/// sibling the session declared it edits). Each dropped entry is logged.
pub(crate) fn filter_spawn_cargo_config(
    contents: &str,
    edit_siblings: &[String],
) -> Option<String> {
    if edit_siblings.is_empty() {
        return Some(contents.to_string());
    }
    match drop_edit_sibling_entries(contents, edit_siblings) {
        Ok((filtered, removed)) => {
            if !removed.is_empty() {
                warn!(
                    "phase5 cargo override: dropped {} store entr{} for declared edit sibling(s) \
                     {:?}: {}",
                    removed.len(),
                    if removed.len() == 1 { "y" } else { "ies" },
                    edit_siblings,
                    removed.join(", ")
                );
            }
            if !is_disposable_config(&filtered) {
                Some(filtered)
            } else {
                info!(
                    "phase5 cargo override: every entry belonged to an edit sibling — no config \
                     written"
                );
                None
            }
        }
        Err(e) => {
            warn!(
                "phase5 cargo override NOT written: could not take edit sibling(s) {:?} off it: \
                 {e}",
                edit_siblings
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Mid-session door (Phase 3)
// ---------------------------------------------------------------------------

/// Why [`apply`] refused. Every arm but [`EditSiblingError::Io`] leaves both
/// the config and the marker untouched; an `Io` failure while rewriting the
/// config may leave the marker already recorded (harmless — a repeat call
/// converges), never the reverse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditSiblingError {
    /// `repo` is not a valid bare or `owner/name` repo name (vet V8).
    InvalidRepo(String),
    /// `path` is inside no live agent allocation.
    NotAnAllocation(String),
    /// `<agent_root>/<repo>` is not a git worktree.
    NoSuchSiblingWorktree(String),
    /// The existing `.cargo/config.toml` could not be filtered safely.
    ConfigUnparseable(String),
    /// A filesystem write failed.
    Io(String),
}

impl EditSiblingError {
    /// The stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            EditSiblingError::InvalidRepo(_) => INVALID_EDIT_SIBLING,
            EditSiblingError::NotAnAllocation(_) => "not_an_allocation",
            EditSiblingError::NoSuchSiblingWorktree(_) => "no_such_sibling_worktree",
            EditSiblingError::ConfigUnparseable(_) => "config_unparseable",
            EditSiblingError::Io(_) => "io_error",
        }
    }

    /// The human-readable detail.
    pub fn message(&self) -> &str {
        match self {
            EditSiblingError::InvalidRepo(m)
            | EditSiblingError::NotAnAllocation(m)
            | EditSiblingError::NoSuchSiblingWorktree(m)
            | EditSiblingError::ConfigUnparseable(m)
            | EditSiblingError::Io(m) => m,
        }
    }
}

impl std::fmt::Display for EditSiblingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.message())
    }
}

/// What [`apply`] did.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EditSiblingOutcome {
    /// The bare repo name taken off the override.
    pub repo: String,
    /// The allocation's agent root (the directory holding `.cargo/`).
    pub agent_root: String,
    /// The `paths` entries removed by THIS call. Empty on a repeat call.
    pub removed: Vec<String>,
    /// `<agent_root>/.cargo/config.toml`.
    pub config_path: String,
    /// Whether the config exists after the call (it is removed when nothing is
    /// left in it, and may never have existed).
    pub config_present: bool,
    /// `<agent_root>/.cargo/edit-siblings`.
    pub marker_path: String,
    /// The marker's full contents after the call.
    pub edit_siblings: Vec<String>,
    /// `<agent_root>/<repo>`, the worktree cargo now resolves.
    pub sibling_worktree: String,
    /// The sibling worktree's HEAD commit, when git answered.
    pub head: Option<String>,
    /// The sibling worktree's branch (`HEAD` when detached), when git answered.
    pub branch: Option<String>,
    /// The sibling's default branch, as `origin/<default>` names it.
    pub default_branch: Option<String>,
    /// Commits on `origin/<default>` not in HEAD, when git answered — counted
    /// against the LOCAL `origin/<default>` as of its last fetch; this door
    /// never fetches.
    pub behind_default: Option<u64>,
    /// How to catch the sibling up, when it is behind AND a fast-forward can do
    /// it (HEAD is an ancestor of `origin/<default>`).
    pub hint: Option<String>,
}

/// Take ONE sibling off an allocation's store override, mid-session.
///
/// 1. Resolve the allocation from `path` (any directory inside it) through
///    [`canonical_paths::allocated_worktree_for_path`]; its parent is the agent
///    root. Anything else is [`EditSiblingError::NotAnAllocation`].
/// 2. Validate `repo` ([`bare_repo_name`]) and require `<agent_root>/<repo>` to
///    be a git worktree ([`EditSiblingError::NoSuchSiblingWorktree`]).
/// 3. Rewrite `.cargo/config.toml` without that sibling's entries, atomically
///    and header-preserving; remove it when nothing is left. A config that
///    cannot be filtered is [`EditSiblingError::ConfigUnparseable`] and stays
///    byte-identical.
/// 4. Record the sibling in the marker (idempotent).
/// 5. Report the removed entries and the sibling worktree's HEAD, branch and
///    whether it is behind `origin/<default>`, with a fast-forward hint.
///
/// Never touches git state. A repeat call removes nothing and succeeds.
pub fn apply(path: &Path, repo: &str) -> Result<EditSiblingOutcome, EditSiblingError> {
    apply_with(path, repo, canonical_paths::allocated_worktree_for_path)
}

/// [`apply`] with the allocation resolver injected, so a scratch layout can
/// stand in for the runner's worktree root.
pub(crate) fn apply_with(
    path: &Path,
    repo: &str,
    resolve_allocation: impl Fn(&Path) -> Option<PathBuf>,
) -> Result<EditSiblingOutcome, EditSiblingError> {
    let bare = bare_repo_name(repo).map_err(EditSiblingError::InvalidRepo)?;
    // A caller-named path with `.` or `..` segments could walk the allocation
    // probe out of the worktree root (`<root>/../<checkout>` "is" an allocation
    // whose parent is the workspace root), and this door WRITES under the
    // resolved root. Refuse it before resolving.
    // Checked on the raw string as well as the components: `Path::components`
    // silently drops an interior `.`, while the allocation probe splits the
    // string itself.
    let has_dot_segment = path
        .to_string_lossy()
        .split(['/', '\\'])
        .any(|s| s == "." || s == "..");
    if has_dot_segment
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(EditSiblingError::NotAnAllocation(format!(
            "{} contains `.` or `..` segments — name a directory inside the allocation directly",
            path.display()
        )));
    }
    let allocation = resolve_allocation(path).ok_or_else(|| {
        EditSiblingError::NotAnAllocation(format!(
            "{} is not inside a live agent-worktree allocation",
            path.display()
        ))
    })?;
    let agent_root = allocation.parent().map(Path::to_path_buf).ok_or_else(|| {
        EditSiblingError::NotAnAllocation(format!(
            "allocation {} has no agent root",
            allocation.display()
        ))
    })?;
    let sibling = agent_root.join(&bare);
    if !sibling.join(".git").exists() {
        return Err(EditSiblingError::NoSuchSiblingWorktree(format!(
            "{} is not a git worktree — this allocation holds no {bare} checkout to edit",
            sibling.display()
        )));
    }

    let config = config_path(&agent_root);
    let edit = [bare.clone()];
    // One read-modify-write of the config and the marker at a time.
    let guard = write_lock();
    // 1. Read and filter the config WITHOUT writing: a refusal here leaves
    //    both the config and the marker untouched.
    let rewrite: Option<(String, Vec<String>)> = match std::fs::read_to_string(&config) {
        Ok(contents) => {
            let (filtered, removed) = drop_edit_sibling_entries(&contents, &edit).map_err(|e| {
                EditSiblingError::ConfigUnparseable(format!("{}: {e}", config.display()))
            })?;
            (!removed.is_empty()).then_some((filtered, removed))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(EditSiblingError::Io(format!(
                "read {}: {e}",
                config.display()
            )))
        }
    };

    // 2. Record the marker BEFORE touching the config. A marker naming a
    //    sibling whose entry is still present (the config write below failed)
    //    is harmless and a repeat call converges; the reverse order could leave
    //    the override gone with no marker and a 500.
    let marker = record_edit_siblings_unlocked(&agent_root, &edit).map_err(|e| {
        EditSiblingError::Io(format!(
            "record {}: {e}",
            marker_path(&agent_root).display()
        ))
    })?;

    // 3. Rewrite (or remove) the config.
    let removed = match rewrite {
        Some((filtered, removed)) => {
            if !is_disposable_config(&filtered) {
                write_atomically(&config, &filtered).map_err(|e| {
                    EditSiblingError::Io(format!("rewrite {}: {e}", config.display()))
                })?;
            } else {
                std::fs::remove_file(&config).map_err(|e| {
                    EditSiblingError::Io(format!("remove {}: {e}", config.display()))
                })?;
            }
            info!(
                "edit-sibling: took {bare} off {} ({} entr{} removed)",
                config.display(),
                removed.len(),
                if removed.len() == 1 { "y" } else { "ies" }
            );
            removed
        }
        None => Vec::new(),
    };
    drop(guard);

    let git = SiblingGitState::read(&sibling);
    // `merge --ff-only` only works when HEAD is an ancestor of the default
    // branch; an agent branch with its own commits gets no hint.
    let hint = match (git.behind_default, &git.default_branch, git.ff_possible) {
        (Some(n), Some(default), true) if n > 0 => Some(format!(
            "git -C {} merge --ff-only origin/{default}",
            sibling.display()
        )),
        _ => None,
    };

    Ok(EditSiblingOutcome {
        repo: bare,
        agent_root: agent_root.display().to_string(),
        removed,
        config_path: config.display().to_string(),
        config_present: config.exists(),
        marker_path: marker_path(&agent_root).display().to_string(),
        edit_siblings: marker,
        sibling_worktree: sibling.display().to_string(),
        head: git.head,
        branch: git.branch,
        default_branch: git.default_branch,
        behind_default: git.behind_default,
        hint,
    })
}

/// The read-only git facts [`apply`] reports about a sibling worktree. Every
/// field is best-effort: a git failure leaves it `None` rather than failing a
/// door whose real work (the config rewrite) already succeeded.
struct SiblingGitState {
    head: Option<String>,
    branch: Option<String>,
    default_branch: Option<String>,
    behind_default: Option<u64>,
    /// Whether HEAD is an ancestor of `origin/<default>` (a fast-forward can
    /// catch it up). `false` when git could not tell.
    ff_possible: bool,
}

impl SiblingGitState {
    fn read(sibling: &Path) -> Self {
        let git = |args: &[&str]| {
            run_git_command(sibling, args)
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };
        let head = git(&["rev-parse", "HEAD"]);
        let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"]);
        let default_branch = crate::git_trunk::resolve_trunk_branch(sibling);
        let behind_default = default_branch.as_deref().and_then(|d| {
            git(&["rev-list", "--count", &format!("HEAD..origin/{d}")])
                .and_then(|n| n.parse::<u64>().ok())
        });
        // `merge-base --is-ancestor` answers by exit status alone, which
        // `run_git_command` maps to Ok / Err.
        let ff_possible = default_branch.as_deref().is_some_and(|d| {
            run_git_command(
                sibling,
                &[
                    "merge-base",
                    "--is-ancestor",
                    "HEAD",
                    &format!("origin/{d}"),
                ],
            )
            .is_ok()
        });
        Self {
            head,
            branch,
            default_branch,
            behind_default,
            ff_possible,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "02fdbb97aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// The header coord's `sibling_store_override::render` emits, verbatim
    /// enough that a header-dropping filter would be caught.
    const HEADER: &str =
        "# Generated by qontinui-coord (POST /agents/allocate, materializes_sibling_store).\n\
         # Points this allocation's sibling path dependencies at the shared, read-only,\n\
         # SHA-keyed sibling store. Not in any git repo: never commit it.\n\n";

    fn config(entries: &[String]) -> String {
        let mut s = String::from(HEADER);
        s.push_str("paths = [\n");
        for e in entries {
            s.push_str(&format!("    \"{e}\",\n"));
        }
        s.push_str("]\n");
        s
    }

    fn three_siblings() -> Vec<String> {
        vec![
            format!("../.siblings/qontinui-schemas@{SHA_A}/code-graph"),
            format!("../.siblings/qontinui-schemas@{SHA_A}/rust"),
            format!("../.siblings/qontinui-types@{SHA_B}"),
            format!("../.siblings/ui-bridge@{SHA_B}/crates/core"),
        ]
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    // ---- name validation (V8) ----

    #[test]
    fn bare_repo_name_accepts_bare_and_owner_slug() {
        assert_eq!(
            bare_repo_name("qontinui-schemas").unwrap(),
            "qontinui-schemas"
        );
        assert_eq!(
            bare_repo_name("qontinui/qontinui-schemas").unwrap(),
            "qontinui-schemas"
        );
        assert_eq!(bare_repo_name("  ui.bridge_2 ").unwrap(), "ui.bridge_2");
    }

    #[test]
    fn bare_repo_name_refuses_dot_dot_and_path_shapes() {
        for bad in [
            "",
            ".",
            "..",
            "../x",
            "a/b/c",
            "qontinui/..",
            "../..",
            "/x",
            "x/",
            "a b",
            "x\\y",
            "x;y",
            "..\\x",
        ] {
            assert!(bare_repo_name(bad).is_err(), "{bad:?} must be refused");
        }
    }

    // ---- spawn validation (V9) ----

    fn checkout_resolver(root: &Path) -> impl Fn(&str) -> Result<PathBuf, String> + '_ {
        move |repo: &str| {
            let bare = bare_repo_name(repo)?;
            Ok(root.join(bare))
        }
    }

    #[test]
    fn spawn_validation_resolves_each_sibling_and_dedups() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws.path().join("qontinui-schemas/.git")).unwrap();
        let out = validate_spawn_edit_siblings_with(
            Some("qontinui-coord"),
            &s(&[
                "qontinui-schemas",
                "qontinui/qontinui-schemas",
                " QONTINUI-SCHEMAS",
            ]),
            checkout_resolver(ws.path()),
        )
        .unwrap();
        assert_eq!(out, s(&["qontinui-schemas"]));
    }

    #[test]
    fn create_terminal_request_refuses_dotdot_and_unresolvable_edit_sibling() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws.path().join("qontinui-schemas/.git")).unwrap();
        let resolve = checkout_resolver(ws.path());
        let dotdot =
            validate_spawn_edit_siblings_with(Some("qontinui-coord"), &s(&[".."]), &resolve)
                .unwrap_err();
        assert_eq!(dotdot.code(), INVALID_EDIT_SIBLING);
        let typo = validate_spawn_edit_siblings_with(
            Some("qontinui-coord"),
            &s(&["qontinui-schemsa"]),
            &resolve,
        )
        .unwrap_err();
        assert!(
            typo.message.contains("qontinui-schemsa"),
            "{}",
            typo.message
        );
        assert!(
            typo.message.contains("not a git checkout"),
            "{}",
            typo.message
        );
        let unresolvable = validate_spawn_edit_siblings_with(
            Some("qontinui-coord"),
            &s(&["qontinui-schemas"]),
            |_: &str| Err("no checkout configured".to_string()),
        )
        .unwrap_err();
        assert!(unresolvable.message.contains("no checkout configured"));
    }

    #[test]
    fn spawn_validation_needs_an_intent_repo_only_when_siblings_are_named() {
        let never = |_: &str| -> Result<PathBuf, String> { panic!("must not resolve") };
        assert_eq!(
            validate_spawn_edit_siblings_with(None, &[], never).unwrap(),
            Vec::<String>::new()
        );
        let refused =
            validate_spawn_edit_siblings_with(None, &s(&["qontinui-schemas"]), never).unwrap_err();
        assert!(refused.message.contains("intentRepo"));
        assert!(
            validate_spawn_edit_siblings_with(Some("  "), &s(&["qontinui-schemas"]), never)
                .is_err()
        );
    }

    // ---- repos merge (V1) and resume (D7/V5) ----

    #[test]
    fn acquire_appends_edit_siblings_to_repos() {
        let merged = merge_edit_siblings_into_repos(
            &s(&["qontinui-coord"]),
            &s(&[
                "qontinui-schemas",
                "qontinui/qontinui-schemas",
                "QONTINUI-COORD",
                "ui-bridge",
            ]),
        );
        assert_eq!(
            merged,
            s(&["qontinui-coord", "qontinui-schemas", "ui-bridge"])
        );
        assert_eq!(merged[0], "qontinui-coord", "the primary stays index 0");

        // An edit sibling already requested (owner slug vs bare) is not doubled.
        let merged = merge_edit_siblings_into_repos(
            &s(&["qontinui-coord", "qontinui/qontinui-schemas"]),
            &s(&["qontinui-schemas"]),
        );
        assert_eq!(merged, s(&["qontinui-coord", "qontinui/qontinui-schemas"]));

        // No edit siblings → repos unchanged.
        assert_eq!(
            merge_edit_siblings_into_repos(&s(&["qontinui-coord"]), &[]),
            s(&["qontinui-coord"])
        );
    }

    #[test]
    fn resume_passes_non_primary_repos_as_edit_siblings() {
        assert_eq!(
            resume_edit_siblings(&s(&["qontinui-coord", "qontinui-schemas", "ui-bridge"])),
            s(&["qontinui-schemas", "ui-bridge"])
        );
        assert!(resume_edit_siblings(&s(&["qontinui-coord"])).is_empty());
        assert!(resume_edit_siblings(&[]).is_empty());
    }

    // ---- the textual filter ----

    #[test]
    fn drop_edit_sibling_entries_removes_only_the_named_repo_including_sub_dirs() {
        let body = config(&three_siblings());
        let (out, removed) = drop_edit_sibling_entries(&body, &s(&["qontinui-schemas"])).unwrap();
        assert_eq!(
            removed,
            vec![
                format!("../.siblings/qontinui-schemas@{SHA_A}/code-graph"),
                format!("../.siblings/qontinui-schemas@{SHA_A}/rust"),
            ]
        );
        let left = override_path_entries(&out).unwrap();
        assert_eq!(
            left,
            vec![
                format!("../.siblings/qontinui-types@{SHA_B}"),
                format!("../.siblings/ui-bridge@{SHA_B}/crates/core"),
            ]
        );
    }

    #[test]
    fn drop_edit_sibling_entries_keeps_every_other_sibling() {
        // `qontinui-types` must not match a `qontinui-types-extra` store entry,
        // nor `qontinui` match `qontinui-types` — the `@` anchors the name.
        let entries = vec![
            format!("../.siblings/qontinui-types@{SHA_B}"),
            format!("../.siblings/qontinui-types-extra@{SHA_B}"),
        ];
        let body = config(&entries);
        let (out, removed) = drop_edit_sibling_entries(&body, &s(&["qontinui"])).unwrap();
        assert!(removed.is_empty());
        assert_eq!(out, body);
        let (out, removed) = drop_edit_sibling_entries(&body, &s(&["qontinui-types"])).unwrap();
        assert_eq!(
            removed,
            vec![format!("../.siblings/qontinui-types@{SHA_B}")]
        );
        assert_eq!(
            override_path_entries(&out).unwrap(),
            vec![format!("../.siblings/qontinui-types-extra@{SHA_B}")]
        );
    }

    #[test]
    fn drop_edit_sibling_entries_empty_result_means_no_config() {
        let body = config(&[format!("../.siblings/qontinui-schemas@{SHA_A}/rust")]);
        let (out, removed) = drop_edit_sibling_entries(&body, &s(&["qontinui-schemas"])).unwrap();
        assert_eq!(removed.len(), 1);
        assert!(is_disposable_config(&out));
        assert_eq!(
            filter_spawn_cargo_config(&body, &s(&["qontinui-schemas"])),
            None,
            "an emptied override is not written"
        );
    }

    #[test]
    fn drop_edit_sibling_entries_owner_slug_and_bare_name_are_equivalent() {
        let body = config(&three_siblings());
        let bare = drop_edit_sibling_entries(&body, &s(&["qontinui-schemas"])).unwrap();
        let slug = drop_edit_sibling_entries(&body, &s(&["qontinui/qontinui-schemas"])).unwrap();
        let upper = drop_edit_sibling_entries(&body, &s(&["QONTINUI-SCHEMAS"])).unwrap();
        assert_eq!(bare, slug);
        assert_eq!(bare, upper);
    }

    #[test]
    fn drop_edit_sibling_entries_keeps_the_header() {
        let body = config(&three_siblings());
        let (out, _) = drop_edit_sibling_entries(&body, &s(&["qontinui-schemas"])).unwrap();
        assert!(
            out.starts_with(HEADER),
            "header must survive byte-for-byte:\n{out}"
        );
        // A comment line that merely MENTIONS a store path is never dropped.
        let commented = format!(
            "# was \"../.siblings/qontinui-schemas@{SHA_A}/rust\"\n{}",
            config(&three_siblings())
        );
        let (out, _) = drop_edit_sibling_entries(&commented, &s(&["qontinui-schemas"])).unwrap();
        assert!(out.starts_with("# was "), "{out}");
    }

    #[test]
    fn drop_edit_sibling_entries_refuses_what_it_cannot_filter() {
        assert!(drop_edit_sibling_entries("paths = [", &s(&["qontinui-schemas"])).is_err());
        assert!(drop_edit_sibling_entries("paths = 3\n", &s(&["qontinui-schemas"])).is_err());
        // Two entries on one line: a line filter cannot separate them, and the
        // edit sibling must not survive silently.
        let one_line = format!(
            "paths = [\"../.siblings/qontinui-types@{SHA_B}\", \"../.siblings/qontinui-schemas@{SHA_A}/rust\"]\n"
        );
        assert!(drop_edit_sibling_entries(&one_line, &s(&["qontinui-schemas"])).is_err());
        // ...in either order: the edit sibling FIRST on the line must not take
        // the other entry down with it.
        let reversed = format!(
            "paths = [\"../.siblings/qontinui-schemas@{SHA_A}/rust\", \"../.siblings/qontinui-types@{SHA_B}\"]\n"
        );
        assert!(drop_edit_sibling_entries(&reversed, &s(&["qontinui-schemas"])).is_err());
        assert_eq!(
            filter_spawn_cargo_config(&one_line, &s(&["qontinui-schemas"])),
            None,
            "a body that cannot be filtered is not written"
        );
    }

    #[test]
    fn filter_spawn_cargo_config_is_identity_without_edit_siblings() {
        let body = config(&three_siblings());
        assert_eq!(filter_spawn_cargo_config(&body, &[]), Some(body.clone()));
        assert_eq!(
            filter_spawn_cargo_config(&body, &s(&["ui-bridge"])),
            Some(config(&three_siblings()[..3]))
        );
    }

    // ---- marker ----

    #[test]
    fn the_marker_accumulates_without_duplicates() {
        let root = tempfile::tempdir().unwrap();
        let first = record_edit_siblings(root.path(), &s(&["qontinui-schemas"])).unwrap();
        assert_eq!(first, s(&["qontinui-schemas"]));
        let second = record_edit_siblings(
            root.path(),
            &s(&["qontinui/qontinui-schemas", "ui-bridge", "UI-BRIDGE", ".."]),
        )
        .unwrap();
        assert_eq!(second, s(&["qontinui-schemas", "ui-bridge"]));
        assert_eq!(
            std::fs::read_to_string(marker_path(root.path())).unwrap(),
            "qontinui-schemas\nui-bridge\n"
        );
        // Recording nothing new writes nothing new.
        record_edit_siblings(root.path(), &s(&["ui-bridge"])).unwrap();
        assert_eq!(
            std::fs::read_to_string(marker_path(root.path())).unwrap(),
            "qontinui-schemas\nui-bridge\n"
        );
    }

    // ---- apply (Phase 3) on a scratch layout ----

    /// `<tmp>/wt/<agent>/{qontinui-coord,qontinui-schemas,qontinui-types,ui-bridge}`
    /// each with a `.git` marker, and the config naming three siblings.
    struct Layout {
        _tmp: tempfile::TempDir,
        worktree_root: PathBuf,
        agent_root: PathBuf,
    }

    fn layout(config_body: Option<&str>) -> Layout {
        let tmp = tempfile::tempdir().unwrap();
        let worktree_root = tmp.path().join("wt");
        let agent_root = worktree_root.join("AGENT");
        for repo in [
            "qontinui-coord",
            "qontinui-schemas",
            "qontinui-types",
            "ui-bridge",
        ] {
            let dir = agent_root.join(repo);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(".git"), "gitdir: elsewhere\n").unwrap();
        }
        if let Some(body) = config_body {
            std::fs::create_dir_all(agent_root.join(".cargo")).unwrap();
            std::fs::write(config_path(&agent_root), body).unwrap();
        }
        Layout {
            _tmp: tmp,
            worktree_root,
            agent_root,
        }
    }

    impl Layout {
        /// The production probe's shape: `<root>/<agent>/<repo>` with `.git`.
        fn resolver(&self) -> impl Fn(&Path) -> Option<PathBuf> + '_ {
            move |p: &Path| {
                let rel = p.strip_prefix(&self.worktree_root).ok()?;
                let mut comps = rel.components();
                let agent = comps.next()?;
                let repo = comps.next()?;
                let candidate = self.worktree_root.join(agent).join(repo);
                candidate.join(".git").exists().then_some(candidate)
            }
        }

        fn coord(&self) -> PathBuf {
            self.agent_root.join("qontinui-coord")
        }
    }

    #[test]
    fn apply_removes_one_of_three_siblings_and_keeps_the_header() {
        let l = layout(Some(&config(&three_siblings())));
        let out = apply_with(&l.coord().join("src"), "qontinui-schemas", l.resolver()).unwrap();
        assert_eq!(out.repo, "qontinui-schemas");
        assert_eq!(out.removed.len(), 2);
        assert!(out.config_present);
        let written = std::fs::read_to_string(config_path(&l.agent_root)).unwrap();
        assert!(written.starts_with(HEADER), "{written}");
        assert_eq!(
            override_path_entries(&written).unwrap(),
            vec![
                format!("../.siblings/qontinui-types@{SHA_B}"),
                format!("../.siblings/ui-bridge@{SHA_B}/crates/core"),
            ]
        );
        assert_eq!(out.edit_siblings, s(&["qontinui-schemas"]));
        assert!(out.sibling_worktree.ends_with("qontinui-schemas"));
        // No temp file is left beside the config.
        let leftovers: Vec<_> = std::fs::read_dir(l.agent_root.join(".cargo"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn apply_is_idempotent() {
        let l = layout(Some(&config(&three_siblings())));
        apply_with(&l.coord(), "qontinui-schemas", l.resolver()).unwrap();
        let after_first = std::fs::read(config_path(&l.agent_root)).unwrap();
        let again = apply_with(&l.coord(), "qontinui/qontinui-schemas", l.resolver()).unwrap();
        assert!(again.removed.is_empty());
        assert_eq!(
            std::fs::read(config_path(&l.agent_root)).unwrap(),
            after_first
        );
        assert_eq!(again.edit_siblings, s(&["qontinui-schemas"]));
    }

    #[test]
    fn apply_removes_the_config_when_nothing_is_left() {
        let l = layout(Some(&config(&[format!(
            "../.siblings/qontinui-schemas@{SHA_A}/rust"
        )])));
        let out = apply_with(&l.coord(), "qontinui-schemas", l.resolver()).unwrap();
        assert_eq!(out.removed.len(), 1);
        assert!(!out.config_present);
        assert!(!config_path(&l.agent_root).exists());
        // The marker is still written, so a census sees the opt-out.
        assert_eq!(
            std::fs::read_to_string(marker_path(&l.agent_root)).unwrap(),
            "qontinui-schemas\n"
        );
    }

    #[test]
    fn apply_without_a_config_only_records_the_marker() {
        let l = layout(None);
        let out = apply_with(&l.coord(), "qontinui-schemas", l.resolver()).unwrap();
        assert!(out.removed.is_empty());
        assert!(!out.config_present);
        assert_eq!(out.edit_siblings, s(&["qontinui-schemas"]));
    }

    #[test]
    fn apply_refuses_an_unparseable_config_and_leaves_it_byte_identical() {
        let garbage = "paths = [\n    \"../.siblings/qontinui-schemas@x/rust\",\n";
        let l = layout(Some(garbage));
        let err = apply_with(&l.coord(), "qontinui-schemas", l.resolver()).unwrap_err();
        assert_eq!(err.code(), "config_unparseable");
        assert_eq!(
            std::fs::read_to_string(config_path(&l.agent_root)).unwrap(),
            garbage
        );
        assert!(
            !marker_path(&l.agent_root).exists(),
            "a refusal records nothing"
        );
    }

    #[test]
    fn apply_refuses_a_path_outside_an_allocation() {
        let l = layout(Some(&config(&three_siblings())));
        let outside = tempfile::tempdir().unwrap();
        let err = apply_with(outside.path(), "qontinui-schemas", l.resolver()).unwrap_err();
        assert_eq!(err.code(), "not_an_allocation");
    }

    #[test]
    fn apply_refuses_dot_dot_and_a_missing_sibling_worktree() {
        let l = layout(Some(&config(&three_siblings())));
        let before = std::fs::read(config_path(&l.agent_root)).unwrap();
        let err = apply_with(&l.coord(), "..", l.resolver()).unwrap_err();
        assert_eq!(err.code(), INVALID_EDIT_SIBLING);
        let err = apply_with(&l.coord(), "qontinui-web", l.resolver()).unwrap_err();
        assert_eq!(err.code(), "no_such_sibling_worktree");
        assert_eq!(std::fs::read(config_path(&l.agent_root)).unwrap(), before);
    }

    #[test]
    fn apply_marker_accumulates_across_siblings() {
        let l = layout(Some(&config(&three_siblings())));
        apply_with(&l.coord(), "qontinui-schemas", l.resolver()).unwrap();
        let out = apply_with(&l.coord(), "ui-bridge", l.resolver()).unwrap();
        assert_eq!(out.edit_siblings, s(&["qontinui-schemas", "ui-bridge"]));
        assert_eq!(
            override_path_entries(&std::fs::read_to_string(config_path(&l.agent_root)).unwrap())
                .unwrap(),
            vec![format!("../.siblings/qontinui-types@{SHA_B}")]
        );
    }

    #[test]
    fn error_codes_are_stable() {
        assert_eq!(
            EditSiblingError::InvalidRepo(String::new()).code(),
            "invalid_edit_sibling"
        );
        assert_eq!(
            EditSiblingError::NotAnAllocation(String::new()).code(),
            "not_an_allocation"
        );
        assert_eq!(
            EditSiblingError::NoSuchSiblingWorktree(String::new()).code(),
            "no_such_sibling_worktree"
        );
        assert_eq!(
            EditSiblingError::ConfigUnparseable(String::new()).code(),
            "config_unparseable"
        );
    }

    #[test]
    fn apply_refuses_a_path_with_dot_dot_segments() {
        let l = layout(Some(&config(&three_siblings())));
        // `<root>/AGENT/qontinui-coord/../../AGENT/qontinui-coord` would resolve
        // under a lexical probe; any `..` / `.` segment is refused outright.
        let sneaky = l
            .coord()
            .join("..")
            .join("..")
            .join("AGENT")
            .join("qontinui-coord");
        let err = apply_with(&sneaky, "qontinui-schemas", |_: &Path| {
            Some(PathBuf::from("/never/consulted"))
        })
        .unwrap_err();
        assert_eq!(err.code(), "not_an_allocation");
        let dot = l.coord().join(".");
        assert_eq!(
            apply_with(&dot, "qontinui-schemas", l.resolver())
                .unwrap_err()
                .code(),
            "not_an_allocation"
        );
        assert!(!marker_path(&l.agent_root).exists());
    }

    #[test]
    fn apply_keeps_a_config_that_holds_more_than_paths() {
        let body = format!(
            "{}[build]\njobs = 4\n",
            config(&[format!("../.siblings/qontinui-schemas@{SHA_A}/rust")])
        );
        let l = layout(Some(&body));
        let out = apply_with(&l.coord(), "qontinui-schemas", l.resolver()).unwrap();
        assert_eq!(out.removed.len(), 1);
        assert!(
            out.config_present,
            "a hand-added [build] table is not deleted"
        );
        let written = std::fs::read_to_string(config_path(&l.agent_root)).unwrap();
        assert!(
            written.contains("[build]") && written.contains("jobs = 4"),
            "{written}"
        );
        assert_eq!(
            override_path_entries(&written).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn is_disposable_config_only_for_an_empty_override() {
        assert!(is_disposable_config("# header\npaths = [\n]\n"));
        assert!(is_disposable_config("# only a comment\n"));
        assert!(!is_disposable_config("paths = [\"../x\"]\n"));
        assert!(!is_disposable_config("paths = []\n[env]\nX = \"1\"\n"));
        assert!(
            !is_disposable_config("paths = ["),
            "unparseable is never disposable"
        );
    }

    #[test]
    fn spawn_validation_refuses_the_intent_repo_as_its_own_sibling() {
        let ws = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(ws.path().join("qontinui-coord/.git")).unwrap();
        let err = validate_spawn_edit_siblings_with(
            Some("qontinui/qontinui-coord"),
            &s(&["QONTINUI-COORD"]),
            checkout_resolver(ws.path()),
        )
        .unwrap_err();
        assert!(err.message.contains("intent repo"), "{}", err.message);
    }
}
