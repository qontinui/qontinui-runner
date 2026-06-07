//! Ξ_Worktree census collector (Phase 1, runner side).
//!
//! coord cannot see the operator's Windows disk — it has no host
//! filesystem access. The runner is the only component that can
//! enumerate the on-disk git worktrees, measure their footprint, detect
//! junctioned `node_modules`/`target` dirs (so a 165 GB junctioned
//! `target` costs ~0 to "size" and is attributed to the canonical tree,
//! not the worktree), and read the volume's free space. This module
//! periodically collects that census and POSTs it to coord's
//! `POST /coord/worktree-census/{device_id}` (anonymous, device-keyed — same
//! machine-wide posture as `/coord/trees/upsert`).
//!
//! ## Mirrors the machine-wide pollers, not the per-agent ones
//!
//! Unlike [`crate::dirty_poller`] (per-agent, JWT-gated, one task per
//! allocated agent), the census is **machine-wide**: one task per
//! runner process, keyed by the device's identity from
//! `~/.qontinui/machine.json`. The closest precedent is
//! [`crate::fleet::spawn_tree_publisher`] — same identity source
//! (`device_id` from machine.json), same coord-base resolver
//! (`COORD_HTTP_URL` env → active profile `coord_url`, ws→http), same
//! `tokio::time::interval` + `MissedTickBehavior::Skip` posture, same
//! best-effort "warn and retry next tick, never panic" contract.
//!
//! ## Enumeration
//!
//! For each governed repo root under [`qontinui_root`] (the same parent
//! dir `fleet::tree_publisher` walks), the census finds worktrees three
//! ways and dedups by canonical path:
//!
//! 1. `git -C <canonical> worktree list --porcelain` — git-registered
//!    worktrees (incl. the canonical tree itself).
//! 2. Sibling `<repo>-wt-*` directories in the parent dir — agent /
//!    operator worktrees that may not be registered with the main repo
//!    (e.g. a `git worktree add` from a different checkout, or a manual
//!    clone).
//! 3. Per-repo `.claude/worktrees/*` directories.
//!
//! ## Sizing
//!
//! `node_modules` and the build `target` dir (`target` for cargo repos,
//! `src-tauri/target` for the Tauri runner) are measured with a
//! recursive walk that **skips any reparse point** (junction) — it
//! reports 0 bytes for a junctioned dir and never traverses it. This is
//! the load-bearing safety property: junctioned build dirs (the runner
//! junctions `node_modules`/`dist`/`target` into worktrees to avoid
//! re-downloading/re-compiling) are shared with the canonical tree, so
//! attributing their bytes to the worktree would massively over-count.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Serialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::canonical_paths::default_canonical_path;

/// Default census cadence — 300s (5 min). The census is a heavy-ish
/// walk (it stats every real file in `node_modules`/`target`), so it
/// runs an order of magnitude slower than the 5s dirty poller and the
/// 30s fleet heartbeat. Override via `QONTINUI_WORKTREE_CENSUS_INTERVAL_SECS`.
const DEFAULT_CENSUS_INTERVAL_SECS: u64 = 300;

/// Windows reparse-point attribute bit (`FILE_ATTRIBUTE_REPARSE_POINT`).
/// A junction (and a symlink) sets this in the file attributes returned
/// by `symlink_metadata`. Defined locally so the check needs no winapi
/// binding — `std::os::windows::fs::MetadataExt::file_attributes`
/// surfaces the raw DWORD.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

// ---------------------------------------------------------------------------
// Wire types — coord deserializes these. Field names/shape are the
// contract documented in the Phase 1 plan.
// ---------------------------------------------------------------------------

/// Free-space report for one volume (drive letter on Windows).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct VolumeReport {
    /// Drive letter with trailing colon, e.g. `"D:"`.
    pub volume: String,
    pub total_bytes: u64,
    pub free_bytes: u64,
}

/// Census row for one worktree.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeCensus {
    /// Basename of the main repo (e.g. `qontinui-runner`).
    pub repo: String,
    /// Absolute path to the worktree dir.
    pub path: String,
    pub branch: Option<String>,
    pub head_sha: Option<String>,
    /// `now - committer-time-of-HEAD`, in seconds. `None` on an unborn
    /// HEAD or when `git log` fails.
    pub head_age_secs: Option<i64>,
    pub is_dirty: bool,

    pub nm_present: bool,
    pub nm_is_junction: bool,
    /// Real (non-junction) bytes of `node_modules`. 0 when junctioned or
    /// absent.
    pub nm_bytes: u64,

    pub target_present: bool,
    pub target_is_junction: bool,
    /// Real (non-junction) bytes of the build `target` dir. 0 when
    /// junctioned or absent.
    pub target_bytes: u64,

    /// RFC3339 mtime of the worktree dir itself, `None` if unreadable.
    pub last_access_mtime: Option<String>,
    /// Sum of the non-junction real bytes attributable to this worktree
    /// (`nm_bytes + target_bytes`). A junctioned dir contributes 0.
    pub attributable_bytes: u64,

    /// G2 "work landed" — whether this worktree's HEAD is already
    /// represented on `origin/main`, computed cheaply from local git only:
    ///
    /// * `Some(true)`  — HEAD is an ancestor of `origin/main` (true merge /
    ///   fast-forward), OR every commit unique to HEAD has a patch-id
    ///   equivalent already on `origin/main` (rebase / cherry-pick).
    /// * `Some(false)` — HEAD has commits not represented on `origin/main`.
    /// * `None`        — couldn't determine (no `origin/main` ref, detached
    ///   oddity, git failure). Coord's gate treats `None` as NOT landed.
    ///
    /// **Squash merges are NOT detectable here** — a squash rewrites the
    /// commits into a single new commit with a fresh patch-id, so neither
    /// the ancestry nor the `git cherry` patch-id test sees it. Coord
    /// covers squashes independently via the PR `close_cause='merged'`
    /// signal; this field is only the ancestry/patch-id half of G2.
    pub landed_in_main: Option<bool>,

    /// G6 shadow-mode probe — whether this worktree is currently building
    /// per [`super::reclaim::worktree_is_building`] (cargo `.cargo-lock`
    /// exclusive-open probe + recent-activity mtime window). Reported every
    /// census tick regardless of reclaim arming, so coord can gauge
    /// "instructions that WOULD have been G6-skipped" while arming is still
    /// OFF — the passive prove-out feed for the Q1 rejunction graduation.
    /// `Some(_)` is the live probe result; old runners omit the field and
    /// coord reads NULL (honest unknown).
    pub building: Option<bool>,

    /// Ξ_Worktree Phase 7.3 — canonical-checkout state, the input coord
    /// needs to prove SharedBranch's P1/P2 preconditions safe (§3.2/§3.3).
    ///
    /// These describe the **canonical repo checkout** (`<root>/<repo>/`),
    /// not this worktree row's path — canonical state is per-repo, so every
    /// worktree row of a repo carries the same values (coord reads one row
    /// per repo). All three are `None` when the canonical path can't be
    /// resolved or git fails; coord treats `None` as unsafe → falls through
    /// to an isolated Worktree (the fail-safe staging idiom). Inert until
    /// coord ingests them (7.3 consumer) and Rule 2 reads them (7.2).
    ///
    /// Current branch of the canonical checkout
    /// (`git symbolic-ref --short HEAD`). `None` on detached HEAD or git
    /// failure.
    pub canonical_current_branch: Option<String>,

    /// Whether the canonical checkout has uncommitted changes
    /// (`git status --porcelain` non-empty). `Some(true)` dirty,
    /// `Some(false)` clean, `None` on git failure (coord treats as unsafe).
    /// This is the P1-clean precondition input for SharedBranch.
    pub canonical_is_dirty: Option<bool>,

    /// Advisory base-divergence summary for the canonical checkout, e.g.
    /// `"on:main"` when parked on main, else
    /// `"on:<branch>;<behind>\t<ahead>"` from
    /// `git rev-list --count --left-right origin/main...HEAD`. Best-effort:
    /// tolerates a missing `origin/main` (just the branch name) and never
    /// errors the census. Human-readable context for the P2 base check.
    pub canonical_base_divergence: Option<String>,
}

/// Full census body POSTed to coord.
#[derive(Debug, Clone, Serialize)]
pub struct WorktreeCensusReq {
    pub device_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub volumes: Vec<VolumeReport>,
    pub worktrees: Vec<WorktreeCensus>,
}

// ---------------------------------------------------------------------------
// Identity + coord-base resolution (mirrors fleet.rs).
// ---------------------------------------------------------------------------

/// `~/.qontinui/machine.json` device identity — `device_id` (serde-
/// aliased to the legacy `machine_id`). Mirrors `fleet::DeviceFile` but
/// kept local so this module is self-contained.
#[derive(Debug, Clone, serde::Deserialize)]
struct DeviceFile {
    #[serde(alias = "machine_id")]
    device_id: String,
}

fn load_device_id() -> Option<Uuid> {
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(path).ok()?;
    let device: DeviceFile = serde_json::from_slice(&bytes).ok()?;
    Uuid::parse_str(device.device_id.trim()).ok()
}

/// Crate-visible alias of [`load_device_id`] so sibling modules (the
/// Phase 4 reclaim poller) reuse the SAME identity resolution rather than
/// duplicating the machine.json parse.
pub(crate) fn load_device_id_pub() -> Option<Uuid> {
    load_device_id()
}

/// Resolve the coord HTTP base. `COORD_HTTP_URL` env → active profile
/// `coord_url` (ws→http, `/ws` suffix stripped). Returns `None` (the
/// tick cleanly skips) when nothing is configured, matching
/// `fleet::coord_http_base`.
fn coord_http_base() -> Option<String> {
    if let Ok(v) = std::env::var("COORD_HTTP_URL") {
        let t = v.trim();
        if !t.is_empty() {
            return Some(t.trim_end_matches('/').to_string());
        }
    }
    let coord_url = qontinui_runner_lib::profiles::load_strict()
        .ok()?
        .coord_url?;
    let trimmed = coord_url.trim_end_matches('/').trim_end_matches("/ws");
    let with_http = trimmed
        .strip_prefix("wss://")
        .map(|rest| format!("https://{rest}"))
        .or_else(|| {
            trimmed
                .strip_prefix("ws://")
                .map(|rest| format!("http://{rest}"))
        })
        .unwrap_or_else(|| trimmed.to_string());
    Some(with_http.trim_end_matches('/').to_string())
}

/// Crate-visible alias of [`coord_http_base`] so the Phase 4 reclaim
/// poller reuses the identical coord-base resolution (env → profile,
/// ws→http, `/ws`-suffix strip).
pub(crate) fn coord_http_base_pub() -> Option<String> {
    coord_http_base()
}

/// Read `active_tenant_id` from `~/.qontinui/machine.json` (advisory —
/// coord DB-resolves the real tenant, per memory
/// `reference_oauth_tenant_claim_advisory_db_resolved`). `None` for
/// single-tenant operators, which is fine: coord attributes the census
/// to the device's resolved tenant regardless.
fn resolve_tenant_id() -> Option<Uuid> {
    let path = dirs::home_dir()?.join(".qontinui").join("machine.json");
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let raw = value.get("active_tenant_id").and_then(|v| v.as_str())?;
    Uuid::parse_str(raw.trim()).ok()
}

/// The parent dir under which the runner's canonical checkouts +
/// sibling worktrees live. `QONTINUI_ROOT` env → `D:/qontinui-root`
/// (Windows) → `$HOME/qontinui-root`. Mirrors `fleet::qontinui_root`.
///
/// `pub(crate)` so the Phase 5 fs_backstop poller enumerates governed
/// canonical checkouts under the SAME workspace root (env → Windows default →
/// `$HOME`) the census walks — no second root-resolution to drift from.
pub(crate) fn qontinui_root() -> Option<PathBuf> {
    if let Ok(s) = std::env::var("QONTINUI_ROOT") {
        let p = PathBuf::from(s);
        if p.is_dir() {
            return Some(p);
        }
    }
    #[cfg(target_os = "windows")]
    {
        let p = PathBuf::from("D:/qontinui-root");
        if p.is_dir() {
            return Some(p);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let p = home.join("qontinui-root");
        if p.is_dir() {
            return Some(p);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Junction detection + sizing (the cross-platform-safe core).
// ---------------------------------------------------------------------------

/// True iff `path` is a reparse point (junction / symlink) on Windows.
/// Always `false` on non-Windows (the runner ships on Windows; the
/// non-windows arm exists so the crate type-checks + tests run on CI's
/// other targets). Uses `symlink_metadata` so it inspects the link
/// itself, never its target.
pub fn is_junction(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        match std::fs::symlink_metadata(path) {
            Ok(meta) => meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0,
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        // Treat a symlink as the closest analog to a junction so the
        // sizing walk still refuses to traverse it.
        std::fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
    }
}

/// Recursive byte size of `dir`, summing real file sizes. SKIPS any
/// directory that is a reparse point (junction): it contributes 0 and is
/// never traversed. The top-level `dir` is assumed already checked by
/// the caller (we never even call this for a junctioned top-level dir),
/// but nested junctions inside the tree are also skipped defensively so
/// a junction buried under a real dir can't cause a 165 GB traversal.
fn dir_size_skipping_junctions(dir: &Path) -> u64 {
    let mut total: u64 = 0;
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    for entry in read.flatten() {
        let path = entry.path();
        // symlink_metadata: never follow a link/junction.
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            // A symlink (file or dir) — skip, do not follow.
            continue;
        }
        if file_type.is_dir() {
            if is_junction(&path) {
                // Reparse point — never traverse.
                continue;
            }
            total = total.saturating_add(dir_size_skipping_junctions(&path));
        } else if file_type.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// Measure one candidate dir (`node_modules` / `target`):
/// `(present, is_junction, bytes)`. A junction reports
/// `(true, true, 0)` and is never walked.
fn measure_dir(dir: &Path) -> (bool, bool, u64) {
    if !dir.exists() {
        return (false, false, 0);
    }
    if is_junction(dir) {
        return (true, true, 0);
    }
    (true, false, dir_size_skipping_junctions(dir))
}

/// Pick the build `target` dir for a worktree. The Tauri runner's cargo
/// workspace lives under `src-tauri/`, so its target is
/// `src-tauri/target`; everything else uses `target`. We prefer
/// `src-tauri/target` when `src-tauri/` exists, else fall back to the
/// top-level `target`.
fn target_dir_for(worktree: &Path) -> PathBuf {
    let src_tauri = worktree.join("src-tauri");
    if src_tauri.is_dir() {
        let st_target = src_tauri.join("target");
        // Use src-tauri/target if it exists OR if there's no top-level
        // target (the Tauri layout). If the operator happens to have a
        // top-level target too, prefer the one that actually exists.
        if st_target.exists() || !worktree.join("target").exists() {
            return st_target;
        }
    }
    worktree.join("target")
}

// ---------------------------------------------------------------------------
// Volume free space (sysinfo — already a dependency, has the `disk` feature).
// ---------------------------------------------------------------------------

/// Build {volume, total_bytes, free_bytes} for each distinct drive
/// letter among the worktree paths. Uses `sysinfo::Disks` (already a
/// runner dependency with the `disk` feature) rather than a raw
/// `GetDiskFreeSpaceExW` binding — it gives total + available per mount
/// portably. We map each worktree's drive letter to the sysinfo disk
/// whose mount point covers it.
fn collect_volumes(worktree_paths: &[PathBuf]) -> Vec<VolumeReport> {
    use sysinfo::Disks;

    // Distinct drive letters (uppercased, with colon) among the paths.
    let mut wanted: HashSet<String> = HashSet::new();
    for p in worktree_paths {
        if let Some(vol) = drive_letter_of(p) {
            wanted.insert(vol);
        }
    }
    if wanted.is_empty() {
        return Vec::new();
    }

    let disks = Disks::new_with_refreshed_list();
    let mut out: BTreeMap<String, VolumeReport> = BTreeMap::new();
    for d in disks.list() {
        let mount = d.mount_point();
        if let Some(vol) = drive_letter_of(mount) {
            if wanted.contains(&vol) && !out.contains_key(&vol) {
                out.insert(
                    vol.clone(),
                    VolumeReport {
                        volume: vol,
                        total_bytes: d.total_space(),
                        free_bytes: d.available_space(),
                    },
                );
            }
        }
    }
    out.into_values().collect()
}

/// Extract the `"D:"`-style drive letter from a path. `None` for paths
/// without a Windows drive prefix (e.g. POSIX paths in CI tests).
fn drive_letter_of(path: &Path) -> Option<String> {
    let s = path.to_string_lossy();
    let bytes = s.as_bytes();
    // Windows: `D:\...` / `D:/...` / `D:`.
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Some(format!("{}:", (bytes[0] as char).to_ascii_uppercase()));
    }
    None
}

// ---------------------------------------------------------------------------
// Worktree enumeration.
// ---------------------------------------------------------------------------

/// Canonicalize for dedup; fall back to the raw path when canonicalize
/// fails (e.g. the dir was just removed). Lower-cases on Windows so
/// `D:\x` and `d:\x` dedup.
fn dedup_key(path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let s = canonical.to_string_lossy().to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// The emitted `path` field, separator-normalized to forward slashes. Worktrees
/// found via the sibling-dir scan come back as `PathBuf::join` results (Windows
/// `\`), while git-listed ones use `/`; without this a single worktree could be
/// reported with mixed separators (`D:/qontinui-root\qontinui-runner-wt-x`),
/// inflating the coord-side `DISTINCT ON (device, repo, path)` set and confusing
/// path-string matching. Cosmetic on Windows (the APIs accept both) but keeps
/// the twin's data clean + stable.
fn normalize_path_str(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// True iff `name` is a CANONICAL repo checkout dir (a repo root), not a
/// worktree of one. A worktree dir is named `<repo>-wt-<slug>` (infix `-wt-`)
/// or carries a `-wt` suffix (e.g. a cross-repo `qontinui-runner-xrepo-wt`);
/// excluding both prevents a stray worktree dir with its own `.git` from being
/// mis-treated as a repo root and re-listing a shared worktree under a phantom
/// repo (which the coord `DISTINCT ON (device, repo, path)` read then keeps as a
/// duplicate row).
pub(crate) fn is_canonical_repo_dir(name: &str) -> bool {
    name.starts_with("qontinui-") && !name.contains("-wt-") && !name.ends_with("-wt")
}

/// Run `git -C <canonical> worktree list --porcelain` and return the
/// `worktree <path>` lines as absolute paths. Best-effort: a non-git
/// dir or git failure yields an empty list.
fn git_registered_worktrees(canonical: &Path) -> Vec<PathBuf> {
    let canonical_str = match canonical.to_str() {
        Some(s) => s,
        None => return Vec::new(),
    };
    let out = match Command::new("git")
        .args(["-C", canonical_str, "worktree", "list", "--porcelain"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|p| PathBuf::from(p.trim()))
        .collect()
}

/// Sibling `<repo>-wt-*` dirs in the parent dir, plus per-repo
/// `.claude/worktrees/*` dirs. These catch worktrees not registered with
/// the canonical repo's `git worktree list`.
fn sibling_and_claude_worktrees(root: &Path, canonical: &Path, repo: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();

    // Sibling `<repo>-wt-*` in the qontinui-root parent dir.
    let prefix = format!("{repo}-wt-");
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with(&prefix) {
                    out.push(path);
                }
            }
        }
    }

    // Per-repo `.claude/worktrees/*`.
    let claude_wts = canonical.join(".claude").join("worktrees");
    if let Ok(entries) = std::fs::read_dir(&claude_wts) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.push(path);
            }
        }
    }

    out
}

/// Run a git query against a worktree, returning trimmed stdout on
/// success.
fn git_capture(worktree: &Path, args: &[&str]) -> Option<String> {
    let wt = worktree.to_str()?;
    let mut full: Vec<&str> = vec!["-C", wt];
    full.extend_from_slice(args);
    let out = Command::new("git").args(&full).output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

/// Compute G2 `landed_in_main` for a worktree using local git only.
///
/// 1. `git merge-base --is-ancestor HEAD origin/main` exit 0 → `Some(true)`
///    (HEAD is on origin/main via a true merge / fast-forward).
/// 2. Else `git cherry origin/main HEAD`: if it emits ≥1 line and EVERY
///    line starts with `-`, every HEAD-unique commit has a patch-id
///    equivalent already on origin/main (rebase / cherry-pick) → `Some(true)`.
/// 3. Else `Some(false)` — there is genuinely-unlanded work.
/// 4. Any git failure / missing `origin/main` / detached oddity → `None`
///    (honest unknown; coord's gate treats `None` as not-landed).
///
/// Squash merges are deliberately NOT covered (the patch-id changes) —
/// coord handles those via the PR `close_cause='merged'` signal.
fn compute_landed_in_main(worktree: &Path) -> Option<bool> {
    // Require a resolvable origin/main ref; without it we can't answer.
    git_capture(
        worktree,
        &["rev-parse", "--verify", "--quiet", "origin/main"],
    )?;

    let wt = worktree.to_str()?;

    // (1) Ancestry test — exit 0 means HEAD is an ancestor of origin/main.
    if let Ok(status) = Command::new("git")
        .args([
            "-C",
            wt,
            "merge-base",
            "--is-ancestor",
            "HEAD",
            "origin/main",
        ])
        .status()
    {
        if status.success() {
            return Some(true);
        }
    } else {
        // git itself failed to spawn / run — honest unknown.
        return None;
    }

    // (2) Patch-id test via `git cherry`. Lines starting `-` are commits
    // whose patch-id already exists on the upstream; `+` lines are
    // genuinely-unlanded. ALL lines must be `-` (and there must be ≥1).
    let cherry = git_capture(worktree, &["cherry", "origin/main", "HEAD"])?;
    let mut saw_line = false;
    for line in cherry.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        saw_line = true;
        if !line.starts_with('-') {
            // A `+` (unlanded) line → not fully landed.
            return Some(false);
        }
    }
    if saw_line {
        // Every line was `-` → all HEAD-unique commits already upstream.
        return Some(true);
    }

    // (3) No cherry lines + not an ancestor: HEAD == origin/main tip would
    // have been caught by the ancestry test, so this is the no-info case —
    // treat as not landed (there is nothing showing it landed).
    Some(false)
}

/// Ξ_Worktree P7.3 — current branch of the canonical checkout
/// (`git symbolic-ref --short HEAD`). `None` on detached HEAD (the command
/// errors), an empty result, or any git failure.
pub(crate) fn compute_canonical_branch(canonical: &Path) -> Option<String> {
    git_capture(canonical, &["symbolic-ref", "--short", "HEAD"]).filter(|s| !s.is_empty())
}

/// Ξ_Worktree P7.3 — dirty bit of the canonical checkout
/// (`git status --porcelain` non-empty). `Some(true)` when there are
/// uncommitted changes, `Some(false)` when clean, `None` on a git failure
/// (fail-OPEN to `None` is fine: coord reads `None` as unsafe). This is the
/// P1-clean precondition input for SharedBranch.
pub(crate) fn compute_canonical_is_dirty(canonical: &Path) -> Option<bool> {
    git_capture(canonical, &["status", "--porcelain"]).map(|s| !s.trim().is_empty())
}

/// Ξ_Worktree P7.3 — advisory base-divergence summary for the canonical
/// checkout. Never errors the census; falls back gracefully:
///
/// * On `main` → `Some("on:main")`.
/// * Otherwise, with `origin/main` resolvable →
///   `Some("on:<branch>;<behind>\t<ahead>")` from
///   `git rev-list --count --left-right origin/main...HEAD`.
/// * Otherwise (missing `origin/main`, detached HEAD, git failure) → just
///   the branch name `Some("on:<branch>")`, or `None` if even the branch is
///   unresolvable.
fn compute_canonical_base_divergence(canonical: &Path) -> Option<String> {
    let branch = compute_canonical_branch(canonical)?;
    if branch == "main" {
        return Some("on:main".to_string());
    }
    // Best-effort ahead/behind vs origin/main. `--left-right` on the
    // symmetric-difference `A...B` prints `<behind>\t<ahead>`. A missing
    // origin/main makes this fail → fall back to just the branch name.
    match git_capture(
        canonical,
        &["rev-list", "--count", "--left-right", "origin/main...HEAD"],
    ) {
        Some(lr) if !lr.is_empty() => Some(format!("on:{branch};{lr}")),
        _ => Some(format!("on:{branch}")),
    }
}

/// Build the census row for a single worktree dir.
fn capture_worktree(repo: &str, worktree: &Path) -> WorktreeCensus {
    let branch =
        git_capture(worktree, &["symbolic-ref", "--short", "HEAD"]).filter(|s| !s.is_empty());
    let head_sha = git_capture(worktree, &["rev-parse", "HEAD"]).filter(|s| !s.is_empty());

    let head_age_secs = git_capture(worktree, &["log", "-1", "--format=%ct"])
        .and_then(|s| s.parse::<i64>().ok())
        .map(|committed| chrono::Utc::now().timestamp().saturating_sub(committed));

    // is_dirty — non-empty `git status --porcelain`.
    let is_dirty = git_capture(worktree, &["status", "--porcelain"])
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    let (nm_present, nm_is_junction, nm_bytes) = measure_dir(&worktree.join("node_modules"));
    let (target_present, target_is_junction, target_bytes) = measure_dir(&target_dir_for(worktree));

    let last_access_mtime = std::fs::metadata(worktree)
        .ok()
        .and_then(|m| m.modified().ok())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339());

    let attributable_bytes = nm_bytes.saturating_add(target_bytes);

    // G2: ancestry/patch-id "landed in origin/main" — local git only,
    // None when undeterminable (no origin/main, git failure).
    let landed_in_main = compute_landed_in_main(worktree);

    // G6 shadow-mode: the same build probe the reclaim executor uses,
    // reported every tick regardless of arming (the passive prove-out feed).
    let building = Some(super::reclaim::probe_building(worktree));

    // Ξ_Worktree P7.3 — canonical-checkout state (per-REPO, not per-worktree;
    // it's fine that every worktree row of a repo carries the same values —
    // coord reads one row per repo). Resolve the canonical path for this
    // repo; a missing/unresolvable canonical → all three facts are None
    // (`.and_then` off the canonical Option), which coord reads as unsafe.
    let canonical = super::canonical_paths::default_canonical_path(repo).ok();
    let canonical_current_branch = canonical.as_deref().and_then(compute_canonical_branch);
    let canonical_is_dirty = canonical.as_deref().and_then(compute_canonical_is_dirty);
    let canonical_base_divergence = canonical
        .as_deref()
        .and_then(compute_canonical_base_divergence);

    WorktreeCensus {
        repo: repo.to_string(),
        path: normalize_path_str(worktree),
        branch,
        head_sha,
        head_age_secs,
        is_dirty,
        nm_present,
        nm_is_junction,
        nm_bytes,
        target_present,
        target_is_junction,
        target_bytes,
        last_access_mtime,
        attributable_bytes,
        landed_in_main,
        building,
        canonical_current_branch,
        canonical_is_dirty,
        canonical_base_divergence,
    }
}

/// Enumerate every worktree under `root`, dedup by canonical path, and
/// build a census row for each.
fn enumerate_worktrees(root: &Path) -> Vec<WorktreeCensus> {
    // Discover the governed repos: every top-level `qontinui-*` dir with
    // a `.git` (matches fleet::tree_publisher's notion of a governed
    // repo, but here it's the canonical checkout we anchor the worktree
    // search on).
    let mut repo_roots: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n.to_string(),
                None => continue,
            };
            // Canonical checkout: `qontinui-*` with a `.git`. Skip worktree
            // dirs here — they're discovered as worktrees OF their repo, not
            // as repos themselves.
            if !is_canonical_repo_dir(&name) {
                continue;
            }
            if path.join(".git").exists() {
                repo_roots.push((name, path));
            }
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut rows: Vec<WorktreeCensus> = Vec::new();

    for (repo, canonical) in &repo_roots {
        // Also try the canonical-path resolver so a repo whose dir name
        // differs from its slug still anchors correctly. (For the flat
        // layout the two agree.)
        let _ = default_canonical_path(repo);

        let mut candidates: Vec<PathBuf> = Vec::new();
        candidates.extend(git_registered_worktrees(canonical));
        candidates.extend(sibling_and_claude_worktrees(root, canonical, repo));
        // The canonical tree itself is reported too (it's a worktree of
        // the repo and its node_modules/target footprint matters).
        candidates.push(canonical.clone());

        for wt in candidates {
            if !wt.is_dir() {
                continue;
            }
            let key = dedup_key(&wt);
            if !seen.insert(key) {
                continue;
            }
            rows.push(capture_worktree(repo, &wt));
        }
    }

    rows
}

// ---------------------------------------------------------------------------
// Tick + spawn.
// ---------------------------------------------------------------------------

/// Build the census body. Public so an integration test can drive the
/// collection without the spawn machinery (mirrors
/// `dirty_poller::tick_once`). Returns `None` when identity / root is
/// unresolvable (the tick cleanly skips).
pub fn build_census() -> Option<WorktreeCensusReq> {
    let device_id = match load_device_id() {
        Some(id) => id,
        None => {
            debug!(
                "worktree_census: ~/.qontinui/machine.json missing or device_id unparseable — skipping"
            );
            return None;
        }
    };
    let root = match qontinui_root() {
        Some(r) => r,
        None => {
            debug!("worktree_census: no qontinui-root dir resolved — skipping");
            return None;
        }
    };

    let worktrees = enumerate_worktrees(&root);
    let paths: Vec<PathBuf> = worktrees.iter().map(|w| PathBuf::from(&w.path)).collect();
    let volumes = collect_volumes(&paths);

    Some(WorktreeCensusReq {
        device_id,
        tenant_id: resolve_tenant_id(),
        volumes,
        worktrees,
    })
}

/// One census cycle: collect + POST. Returns `Ok(())` on a clean skip or
/// a successful POST; `Err` only on a built-payload transport / non-2xx
/// failure (the caller logs + retries next tick).
pub async fn tick_once() -> Result<(), String> {
    // build_census stats real files under every worktree's node_modules/
    // target and shells out to git — a synchronous multi-second disk walk.
    // Run it on the blocking pool so the shared fleet-publishers runtime's
    // async worker isn't pinned for the duration (the starvation class
    // PR #391 isolated the heartbeat from).
    let req = match tokio::task::spawn_blocking(build_census)
        .await
        .map_err(|e| format!("census walk panicked: {e}"))?
    {
        Some(r) => r,
        None => return Ok(()),
    };
    let base = match coord_http_base() {
        Some(b) => b,
        None => {
            debug!("worktree_census: no coord_url configured — skipping POST");
            return Ok(());
        }
    };

    let url = format!(
        "{}/coord/worktree-census/{}",
        base.trim_end_matches('/'),
        req.device_id
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("build census http client: {e}"))?;
    let resp = client
        .post(&url)
        .json(&req)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let excerpt: String = body.chars().take(200).collect();
        return Err(format!("coord returned {status} for POST {url}: {excerpt}"));
    }
    debug!(
        "worktree_census: posted device_id={} worktrees={} volumes={}",
        req.device_id,
        req.worktrees.len(),
        req.volumes.len()
    );
    Ok(())
}

/// Spawn the periodic census task on the ambient tokio runtime.
///
/// Interval read from `QONTINUI_WORKTREE_CENSUS_INTERVAL_SECS` (default
/// 300s, floored at 30s). `MissedTickBehavior::Skip` matches
/// `fleet::spawn_heartbeat` / `fleet::spawn_tree_publisher` — a system
/// suspend skips catch-up rather than blasting back-to-back ticks.
/// Failures `warn!` and retry on the next tick; the loop never panics.
pub fn spawn_census() {
    let secs: u64 = std::env::var("QONTINUI_WORKTREE_CENSUS_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_CENSUS_INTERVAL_SECS)
        .max(30);

    info!(
        "worktree_census: starting periodic census task, interval={}s",
        secs
    );

    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(secs));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Err(e) = tick_once().await {
                warn!("worktree_census: {e}");
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_repo_dir_excludes_worktree_dirs() {
        // Real repo roots.
        assert!(is_canonical_repo_dir("qontinui-runner"));
        assert!(is_canonical_repo_dir("qontinui-coord"));
        assert!(is_canonical_repo_dir("qontinui-supervisor"));
        // `-wt-` infix worktrees.
        assert!(!is_canonical_repo_dir("qontinui-runner-wt-pnpm"));
        assert!(!is_canonical_repo_dir("qontinui-coord-wt-verify"));
        // `-wt` SUFFIX dirs (the oddity: a stray cross-repo worktree dir with
        // its own .git was mis-treated as a repo root → duplicate census rows).
        assert!(!is_canonical_repo_dir("qontinui-runner-xrepo-wt"));
        // Non-qontinui dirs.
        assert!(!is_canonical_repo_dir("node_modules"));
    }

    #[test]
    fn normalize_path_str_forces_forward_slashes() {
        // A sibling-scan PathBuf can carry Windows backslashes; the emitted
        // census path must be separator-stable so coord's DISTINCT ON
        // (device, repo, path) doesn't keep `\` and `/` variants as two rows.
        let p = Path::new(r"D:\qontinui-root\qontinui-runner-wt-verify");
        assert_eq!(
            normalize_path_str(p),
            "D:/qontinui-root/qontinui-runner-wt-verify"
        );
        // Already-forward paths are unchanged.
        let q = Path::new("D:/qontinui-root/qontinui-coord-wt-cpw");
        assert_eq!(
            normalize_path_str(q),
            "D:/qontinui-root/qontinui-coord-wt-cpw"
        );
    }

    #[test]
    fn normal_dir_is_not_a_junction() {
        // A freshly-created plain directory must read as non-junction on
        // every platform. (We can't portably create a junction in a unit
        // test, so this pins the non-junction arm — the common case.)
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_junction(dir.path()));
    }

    #[test]
    fn missing_dir_measures_as_absent() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("node_modules");
        let (present, is_junction, bytes) = measure_dir(&missing);
        assert!(!present);
        assert!(!is_junction);
        assert_eq!(bytes, 0);
    }

    #[test]
    fn dir_size_sums_real_files_and_skips_nothing_when_no_junctions() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 100]).unwrap();
        std::fs::write(sub.join("b.bin"), vec![0u8; 250]).unwrap();
        let total = dir_size_skipping_junctions(dir.path());
        assert_eq!(total, 350, "should sum nested real files");
    }

    #[test]
    fn measure_present_real_dir_reports_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let nm = dir.path().join("node_modules");
        std::fs::create_dir(&nm).unwrap();
        std::fs::write(nm.join("pkg.json"), vec![0u8; 42]).unwrap();
        let (present, is_junction, bytes) = measure_dir(&nm);
        assert!(present);
        assert!(!is_junction);
        assert_eq!(bytes, 42);
    }

    #[test]
    fn drive_letter_parsing() {
        assert_eq!(
            drive_letter_of(Path::new("D:/qontinui-root/x")),
            Some("D:".to_string())
        );
        assert_eq!(
            drive_letter_of(Path::new("c:\\Users\\foo")),
            Some("C:".to_string())
        );
        assert_eq!(drive_letter_of(Path::new("/home/user/x")), None);
        assert_eq!(drive_letter_of(Path::new("relative/path")), None);
    }

    #[test]
    fn target_dir_prefers_src_tauri_when_present() {
        let dir = tempfile::tempdir().unwrap();
        // No src-tauri → top-level target.
        assert_eq!(target_dir_for(dir.path()), dir.path().join("target"));
        // With src-tauri/ → src-tauri/target.
        std::fs::create_dir(dir.path().join("src-tauri")).unwrap();
        assert_eq!(
            target_dir_for(dir.path()),
            dir.path().join("src-tauri").join("target")
        );
    }

    #[test]
    fn collect_volumes_empty_when_no_drive_letters() {
        // POSIX-style paths have no drive letter → no volume rows (so CI
        // on linux gets a deterministic empty result).
        let paths = vec![PathBuf::from("/tmp/x"), PathBuf::from("/home/y")];
        assert!(collect_volumes(&paths).is_empty());
    }

    #[test]
    fn capture_worktree_on_non_git_dir_is_clean_and_unbranched() {
        // A plain dir (no git) → branch/sha/age None, not dirty, no nm,
        // no target.
        let dir = tempfile::tempdir().unwrap();
        let row = capture_worktree("qontinui-runner", dir.path());
        assert_eq!(row.repo, "qontinui-runner");
        assert!(row.branch.is_none());
        assert!(row.head_sha.is_none());
        assert!(!row.is_dirty);
        assert!(!row.nm_present);
        assert!(!row.target_present);
        assert_eq!(row.attributable_bytes, 0);
        assert!(row.last_access_mtime.is_some(), "dir mtime should read");
        // No git repo → no origin/main ref → landed_in_main is the honest
        // unknown `None` (which coord's gate reads as not-landed).
        assert!(row.landed_in_main.is_none());
        // G6 shadow probe always reports: a freshly-created tempdir has a
        // root mtime inside the activity window → Some(true).
        assert_eq!(row.building, Some(true));
    }

    #[test]
    fn landed_in_main_is_none_without_origin_main() {
        // A real git repo but no `origin/main` ref → undeterminable → None.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .args([&["-C", path.to_str().unwrap()], args].concat())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?} should succeed");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        run(&["add", "a.txt"]);
        run(&["commit", "-q", "-m", "c1"]);
        // No origin/main ref exists → compute returns None.
        assert!(compute_landed_in_main(path).is_none());
    }

    #[test]
    fn landed_in_main_true_when_head_is_ancestor_of_origin_main() {
        // Build a repo, create an `origin/main` ref AT HEAD via a local
        // bare "remote", then verify HEAD-is-ancestor → Some(true).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let wt = path.to_str().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args([&["-C", wt], args].concat())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "c1"]);
        // Point a local origin/main remote-tracking ref straight at HEAD.
        let head = git_capture(path, &["rev-parse", "HEAD"]).unwrap();
        git(&["update-ref", "refs/remotes/origin/main", &head]);
        assert_eq!(compute_landed_in_main(path), Some(true));

        // Add an unlanded commit on top → HEAD no longer ancestor and no
        // patch-id match → Some(false).
        std::fs::write(path.join("b.txt"), b"y").unwrap();
        git(&["add", "b.txt"]);
        git(&["commit", "-q", "-m", "c2"]);
        assert_eq!(compute_landed_in_main(path), Some(false));
    }

    /// Ξ_Worktree P7.3 — canonical-checkout facts on a real temp git repo.
    /// Mirrors the git-tempdir idiom above. Drives the three compute helpers
    /// directly (a tempdir is not a canonical `<root>/<repo>/` checkout, so
    /// `capture_worktree` would resolve canonical to a real on-disk repo or
    /// None — we exercise the helpers against a controlled repo instead).
    #[test]
    fn canonical_checkout_facts_branch_dirty_and_divergence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let wt = path.to_str().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args([&["-C", wt], args].concat())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        // Name the initial branch deterministically (default may be main or
        // master depending on git config) so the assertions are stable.
        git(&["checkout", "-q", "-b", "feature-x"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "c1"]);

        // Branch resolves to the current branch.
        assert_eq!(
            compute_canonical_branch(path),
            Some("feature-x".to_string())
        );

        // Clean tree → Some(false); flips to Some(true) after an uncommitted
        // write.
        assert_eq!(compute_canonical_is_dirty(path), Some(false));
        std::fs::write(path.join("b.txt"), b"y").unwrap();
        assert_eq!(compute_canonical_is_dirty(path), Some(true));

        // Divergence string is non-empty (no origin/main here → falls back to
        // just the branch name, which is still a non-empty advisory string).
        let div = compute_canonical_base_divergence(path).expect("divergence Some");
        assert!(!div.is_empty(), "divergence string should be non-empty");
        assert!(div.starts_with("on:feature-x"), "got: {div}");
    }

    /// Ξ_Worktree P7.3 — `on:main` carve-out + ahead/behind formatting when
    /// `origin/main` resolves.
    #[test]
    fn canonical_checkout_facts_on_main_and_divergence_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        let wt = path.to_str().unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args([&["-C", wt], args].concat())
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["checkout", "-q", "-b", "main"]);
        std::fs::write(path.join("a.txt"), b"x").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "c1"]);

        // On main → exact "on:main" carve-out (no rev-list needed).
        assert_eq!(
            compute_canonical_base_divergence(path),
            Some("on:main".to_string())
        );

        // Point a local origin/main at HEAD, branch off, add one ahead commit
        // → divergence string carries the rev-list left-right counts.
        let head = git_capture(path, &["rev-parse", "HEAD"]).unwrap();
        git(&["update-ref", "refs/remotes/origin/main", &head]);
        git(&["checkout", "-q", "-b", "topic"]);
        std::fs::write(path.join("c.txt"), b"z").unwrap();
        git(&["add", "c.txt"]);
        git(&["commit", "-q", "-m", "c2"]);
        let div = compute_canonical_base_divergence(path).expect("divergence Some");
        // origin/main...HEAD: 0 behind, 1 ahead → "on:topic;0\t1".
        assert!(div.starts_with("on:topic;"), "got: {div}");
        assert!(div.contains('1'), "should report the 1 ahead commit: {div}");
    }

    #[test]
    fn attributable_bytes_sums_nm_and_target() {
        let dir = tempfile::tempdir().unwrap();
        let nm = dir.path().join("node_modules");
        std::fs::create_dir(&nm).unwrap();
        std::fs::write(nm.join("x"), vec![0u8; 10]).unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("y"), vec![0u8; 20]).unwrap();
        let row = capture_worktree("qontinui-coord", dir.path());
        assert_eq!(row.nm_bytes, 10);
        assert_eq!(row.target_bytes, 20);
        assert_eq!(row.attributable_bytes, 30);
    }
}
