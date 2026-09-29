//! The SHA-keyed sibling store: `<agent-worktree root>/.siblings/<repo>@<sha>/`.
//!
//! Plan `2026-09-28-shared-cargo-target-holds-one-build-copy-per-sibling-checkout-path`
//! Phase 2. Cargo hashes a path dependency that lives OUTSIDE the consumer's
//! workspace by its absolute path, so every allocation that materialised its own
//! `agent-worktrees/<agent>/qontinui-schemas` minted a fresh `qontinui-types`
//! unit hash — and a fresh copy of every crate above it — in the shared target.
//!
//! Coord now emits a `cargo_config` whose body is a relative `paths` override
//! (`paths = ["../.siblings/<repo>@<sha>/<crate dir>", …]`), resolved against
//! `<agent_root>` (the directory holding `.cargo/`), but ONLY to a caller that
//! sends `materializes_sibling_store: true` on the allocate request. This module
//! is the runner half of that contract:
//!
//! - [`materialize`] extracts `git archive <sha>` of the sibling's canonical
//!   checkout into a temp dir under `.siblings/`, marks its files read-only and
//!   renames it into place atomically. A target that already exists — a reuse,
//!   or a concurrent materialiser that won the rename race — is success. The
//!   SHA is the pin, so an entry never changes once it exists.
//! - [`missing_override_entries`] is the render gate: the caller writes the
//!   cargo config only when every `paths` entry it names exists, because a
//!   `paths` entry pointing at nothing fails EVERY build in the allocation,
//!   while an unwritten config only costs the old (duplicating) behaviour.
//!
//! The per-allocation sibling worktree is unaffected; it still serves Poetry and
//! co-development (deleting `<agent_root>/.cargo/config.toml` is the opt-out).

use std::path::{Path, PathBuf};
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::worktree::run_git_command;

/// The store's directory name under the agent-worktree root.
pub(crate) const SIBLING_STORE_DIRNAME: &str = ".siblings";

/// The completeness marker: an empty file named `.qontinui-sibling-complete` at
/// an entry's root. Shared contract with qontinui-claude-config's
/// `scripts/allocate-worktree.sh`, which writes and requires the same file:
/// - it is written only after a fully successful extraction, inside the temp
///   dir and BEFORE the atomic rename, so it lands with the entry or not at all;
/// - it is required for adoption: an entry without it is never adopted and
///   never backs a cargo config (see [`adopt_existing`] and
///   [`missing_override_entries`]).
pub(crate) const COMPLETE_MARKER: &str = ".qontinui-sibling-complete";

/// `git archive` of a sibling repo is tens of MB; bounded like every other
/// worktree git call so a wedged git cannot hang an allocation.
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(300);

/// `<agent_worktree_root>/.siblings`.
pub(crate) fn store_root(agent_worktree_root: &Path) -> PathBuf {
    agent_worktree_root.join(SIBLING_STORE_DIRNAME)
}

/// A full, lowercase, 40-hex commit id — the only key the store accepts, so an
/// abbreviated or symbolic ref can never name an entry whose content moves.
fn is_full_sha(sha: &str) -> bool {
    sha.len() == 40 && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The entry name `<repo>@<sha>`, or `None` when either half is unusable.
pub(crate) fn entry_name(repo_name: &str, sha: &str) -> Option<String> {
    let ok_repo = !repo_name.is_empty()
        && !repo_name.contains(['/', '\\', '@'])
        && repo_name != "."
        && repo_name != "..";
    (ok_repo && is_full_sha(sha)).then(|| format!("{repo_name}@{sha}"))
}

/// Materialise `<store_root>/<repo_name>@<sha>/` from `canonical` (a checkout of
/// that repo). Idempotent: an existing entry is adopted (its mtime re-stamped)
/// rather than rebuilt.
pub(crate) fn materialize(
    store_root: &Path,
    canonical: &Path,
    repo_name: &str,
    sha: &str,
) -> Result<PathBuf, String> {
    let name = entry_name(repo_name, sha)
        .ok_or_else(|| format!("unusable sibling store key repo={repo_name:?} sha={sha:?}"))?;
    let target = store_root.join(&name);
    if adopt_existing(&target)? {
        debug!("sibling store: reuse {}", target.display());
        return Ok(target);
    }
    std::fs::create_dir_all(store_root)
        .map_err(|e| format!("create {}: {e}", store_root.display()))?;

    ensure_commit(canonical, sha)?;

    // The temp name starts with `.tmp-` and carries a pid and random suffix, so
    // it can never match the `<repo>@<40-hex>` shape a store reaper is allowed
    // to take as an entry, even if this process dies before the rename.
    let tmp = tempfile::Builder::new()
        .prefix(&format!(".tmp-{name}-{}-", std::process::id()))
        .tempdir_in(store_root)
        .map_err(|e| format!("temp dir under {}: {e}", store_root.display()))?;
    extract_archive(store_root, &name, canonical, sha, tmp.path())?;
    set_files_readonly(tmp.path())?;
    // Last, and only after everything above succeeded: the marker is what says
    // "this tree is whole".
    std::fs::write(tmp.path().join(COMPLETE_MARKER), b"")
        .map_err(|e| format!("write completeness marker in {}: {e}", tmp.path().display()))?;

    // `keep()` hands the directory over so a successful rename is not undone by
    // the TempDir drop; on any failure below we remove it ourselves.
    let tmp_path = tmp.keep();
    match std::fs::rename(&tmp_path, &target) {
        Ok(()) => {
            if adopt_existing(&target)? {
                info!("sibling store: materialised {}", target.display());
                Ok(target)
            } else {
                Err(format!(
                    "{} vanished right after its rename",
                    target.display()
                ))
            }
        }
        // Lost the race to a concurrent materialiser (same SHA ⇒ same content).
        Err(_) if adopt_existing(&target)? => {
            remove_tree(&tmp_path);
            debug!(
                "sibling store: {} appeared concurrently — reusing",
                target.display()
            );
            Ok(target)
        }
        Err(e) => {
            remove_tree(&tmp_path);
            Err(format!(
                "rename {} -> {}: {e}",
                tmp_path.display(),
                target.display()
            ))
        }
    }
}

/// Adopt the entry at `target` if it is a real, COMPLETE directory: stamp its
/// mtime FIRST, then re-test it, so an entry a reaper takes in between reads as
/// absent rather than adopted. Never creates anything. `Ok(false)` means
/// "absent, materialise it"; to make that true, whatever else sits at the path
/// is moved or removed first:
/// - a directory without [`COMPLETE_MARKER`] is corrupt (a partial tree). It is
///   never deleted in place — it is renamed to `.trash-<name>-<pid>-<n>` for a
///   reaper, and a failed rename is an error (the caller then skips the config);
/// - a non-directory (a stray file or symlink) is removed.
fn adopt_existing(target: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(target) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("stat {}: {e}", target.display())),
        Ok(meta) if meta.is_dir() && !target.join(COMPLETE_MARKER).is_file() => {
            let trash = trash_path(target);
            warn!(
                "sibling store: {} has no {COMPLETE_MARKER} (partial tree) — moving it to {}",
                target.display(),
                trash.display()
            );
            match std::fs::rename(target, &trash) {
                Ok(()) => {
                    // The marker was absent when checked, but a writer may have
                    // completed the entry between that check and the rename.
                    if restore_if_completed(target, &trash) {
                        touch_entry(target);
                        Ok(target.join(COMPLETE_MARKER).is_file())
                    } else {
                        Ok(false)
                    }
                }
                Err(_) if !target.exists() => Ok(false),
                Err(e) => Err(format!(
                    "incomplete entry {} could not be set aside: {e}",
                    target.display()
                )),
            }
        }
        Ok(meta) if meta.is_dir() => {
            touch_entry(target);
            Ok(std::fs::symlink_metadata(target).is_ok_and(|m| m.is_dir())
                && target.join(COMPLETE_MARKER).is_file())
        }
        Ok(meta) => {
            warn!(
                "sibling store: a non-directory sits at entry {} — removing it",
                target.display()
            );
            if meta.is_file() {
                clear_file_readonly(target, meta.permissions());
            }
            std::fs::remove_file(target)
                .map(|()| false)
                .map_err(|e| format!("remove non-directory {}: {e}", target.display()))
        }
    }
}

/// The TOCTOU guard for setting a partial entry aside: when the tree just moved
/// to `trash` turns out to carry [`COMPLETE_MARKER`] after all, move it back to
/// `target`. Returns whether it was restored. A failed move back (typically
/// because a peer has published `target` again meanwhile) leaves `trash` for the
/// reaper; the caller then treats `target` as absent and re-reads it.
fn restore_if_completed(target: &Path, trash: &Path) -> bool {
    if !trash.join(COMPLETE_MARKER).is_file() {
        return false;
    }
    match std::fs::rename(trash, target) {
        Ok(()) => {
            info!(
                "sibling store: {} was completed while being set aside — restored",
                target.display()
            );
            true
        }
        Err(e) => {
            warn!(
                "sibling store: completed tree {} could not be moved back to {} ({e}) — \
                 left for the reaper",
                trash.display(),
                target.display()
            );
            false
        }
    }
}

/// A `.trash-<name>-<pid>-<n>` sibling of `target` that does not exist yet.
fn trash_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent = target.parent().unwrap_or(target);
    let pid = std::process::id();
    (0u32..)
        .map(|n| parent.join(format!(".trash-{name}-{pid}-{n}")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| parent.join(format!(".trash-{name}-{pid}")))
}

/// Set the entry directory's mtime to now, so a store reaper's quiet window
/// reads "last adopted by an allocation", not "first materialised". Best-effort:
/// a failure only makes the entry look older than it is.
fn touch_entry(dir: &Path) {
    if let Err(e) = set_dir_mtime(dir, std::time::SystemTime::now()) {
        warn!("sibling store: could not touch {}: {e}", dir.display());
    }
}

fn set_dir_mtime(dir: &Path, when: std::time::SystemTime) -> std::io::Result<()> {
    // Windows opens a directory handle only with FILE_FLAG_BACKUP_SEMANTICS, and
    // setting its times needs FILE_WRITE_ATTRIBUTES rather than read access.
    #[cfg(windows)]
    let handle = {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        std::fs::OpenOptions::new()
            .access_mode(FILE_WRITE_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(dir)?
    };
    #[cfg(not(windows))]
    let handle = std::fs::File::open(dir)?;
    handle.set_modified(when)
}

/// Make sure `sha` is a commit in `canonical`'s object store, fetching once when
/// it is not (the requested row's locality fetch usually already brought it in).
fn ensure_commit(canonical: &Path, sha: &str) -> Result<(), String> {
    let spec = format!("{sha}^{{commit}}");
    if run_git_command(canonical, &["cat-file", "-e", &spec]).is_ok() {
        return Ok(());
    }
    if let Err(e) = run_git_command(canonical, &["fetch", "origin", sha]) {
        warn!(
            "sibling store: fetch of {sha} in {} failed: {e}",
            canonical.display()
        );
    }
    run_git_command(canonical, &["cat-file", "-e", &spec])
        .map(|_| ())
        .map_err(|e| format!("commit {sha} absent from {}: {e}", canonical.display()))
}

/// `git archive --format=tar -o <file> <sha>`, then unpacked from that file into
/// `dest`.
///
/// The archive goes to a FILE, never through captured stdout:
/// `output_with_timeout` caps captured output at `MAX_CAPTURED_BYTES` (4 MiB)
/// and still reports success, which truncated a 7 MB schemas archive — usually
/// into an unpack error, but when the cut fell on a tar header boundary into a
/// silently PARTIAL tree. git's exit status is checked, the timeout watchdog is
/// kept, and the tar file sits beside the extraction dir (not inside it) under a
/// `.tmp-` name a reaper never takes as an entry.
fn extract_archive(
    store_root: &Path,
    name: &str,
    canonical: &Path,
    sha: &str,
    dest: &Path,
) -> Result<(), String> {
    let tar_file = tempfile::Builder::new()
        .prefix(&format!(".tmp-{name}-{}-", std::process::id()))
        .suffix(".tar")
        .tempfile_in(store_root)
        .map_err(|e| format!("temp archive under {}: {e}", store_root.display()))?;
    let tar_path = tar_file.path().to_string_lossy().to_string();
    let mut cmd = crate::process_helpers::no_window("git");
    cmd.args(["archive", "--format=tar", "-o", &tar_path, sha])
        .current_dir(canonical);
    let out = crate::process_helpers::output_with_timeout(cmd, ARCHIVE_TIMEOUT)
        .map_err(|e| format!("git archive {sha}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git archive {sha} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    unpack_file(tar_file.path(), dest).map_err(|e| format!("archive of {sha}: {e}"))
}

/// Unpack the tar at `tar_path` into `dest`.
fn unpack_file(tar_path: &Path, dest: &Path) -> Result<(), String> {
    let file =
        std::fs::File::open(tar_path).map_err(|e| format!("open {}: {e}", tar_path.display()))?;
    tar::Archive::new(std::io::BufReader::new(file))
        .unpack(dest)
        .map_err(|e| format!("unpack into {}: {e}", dest.display()))
}

/// Mark every regular file under `dir` read-only, on every platform. Directories
/// are left writable deliberately: on Windows a read-only directory attribute
/// does not protect its contents anyway, and on every platform a writable
/// directory keeps the entry removable by a reaper without a chmod pass. What
/// matters — that an edit to a shared, SHA-pinned file fails loudly — holds.
fn set_files_readonly(dir: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("read {}: {e}", dir.display()))?;
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path)
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
        if meta.is_dir() {
            set_files_readonly(&path)?;
        } else if meta.is_file() {
            let mut perms = meta.permissions();
            perms.set_readonly(true);
            std::fs::set_permissions(&path, perms)
                .map_err(|e| format!("chmod {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Best-effort removal of an abandoned temp extraction. Clears the read-only
/// bit first because Windows refuses to delete a read-only file.
fn remove_tree(dir: &Path) {
    clear_readonly(dir);
    if let Err(e) = std::fs::remove_dir_all(dir) {
        warn!(
            "sibling store: could not remove temp {}: {e}",
            dir.display()
        );
    }
}

fn clear_readonly(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            clear_readonly(&path);
        } else if meta.is_file() {
            clear_file_readonly(&path, meta.permissions());
        }
    }
}

/// Clear one file's read-only bit (Windows refuses to delete a read-only file).
#[expect(
    clippy::permissions_set_readonly_false,
    reason = "clearing our own read-only bit only to delete the file"
)]
fn clear_file_readonly(path: &Path, mut perms: std::fs::Permissions) {
    perms.set_readonly(false);
    let _ = std::fs::set_permissions(path, perms);
}

/// The `paths` entries of a cargo config body. `None` when the body is not TOML
/// or `paths` is present but not an array of strings — unverifiable, so the
/// caller must not write it. `Some(vec![])` when there is no `paths` key.
pub(crate) fn override_path_entries(contents: &str) -> Option<Vec<String>> {
    let table: toml::Table = contents.parse().ok()?;
    match table.get("paths") {
        None => Some(Vec::new()),
        Some(toml::Value::Array(items)) => items
            .iter()
            .map(|v| v.as_str().map(str::to_string))
            .collect(),
        Some(_) => None,
    }
}

/// The `paths` entries that are not crate directories — no `<entry>/Cargo.toml`
/// file — each resolved the way cargo resolves them: relative to `agent_root`,
/// the directory that holds `.cargo/config.toml` (an absolute entry replaces it).
/// Every entry is checked, inside `.siblings/` or not; `Path` normalises `//`
/// and `/./`, and accepts both `/` and `\` separators on Windows. `Err` when the
/// body cannot be parsed.
pub(crate) fn missing_override_entries(
    agent_root: &Path,
    contents: &str,
) -> Result<Vec<PathBuf>, String> {
    let entries = override_path_entries(contents)
        .ok_or_else(|| "cargo_config contents are not a parseable `paths` override".to_string())?;
    Ok(entries
        .iter()
        .map(|e| agent_root.join(e))
        .filter(|p| !p.join("Cargo.toml").is_file() || !store_entry_is_complete(p))
        .collect())
}

/// For a path inside the store (`…/.siblings/<entry>/…`), whether `<entry>`
/// carries [`COMPLETE_MARKER`]. A path outside the store has no entry to check
/// and passes.
fn store_entry_is_complete(path: &Path) -> bool {
    let mut root = PathBuf::new();
    let mut comps = path.components();
    while let Some(c) = comps.next() {
        root.push(c);
        if c.as_os_str() == SIBLING_STORE_DIRNAME {
            return match comps.next() {
                Some(entry) => root.join(entry).join(COMPLETE_MARKER).is_file(),
                None => false,
            };
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const SHA_A: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn entry_name_requires_a_full_sha_and_a_bare_repo() {
        assert_eq!(
            entry_name("qontinui-schemas", SHA_A).as_deref(),
            Some("qontinui-schemas@0123456789abcdef0123456789abcdef01234567")
        );
        assert_eq!(entry_name("qontinui-schemas", "0123abc"), None);
        assert_eq!(entry_name("qontinui-schemas", &SHA_A.to_uppercase()), None);
        assert_eq!(entry_name("qontinui/qontinui-schemas", SHA_A), None);
        assert_eq!(entry_name("..", SHA_A), None);
        assert_eq!(entry_name("", SHA_A), None);
    }

    #[test]
    fn override_paths_are_parsed_from_the_contract_body() {
        let body = format!(
            "paths = [\"../.siblings/qontinui-schemas@{SHA_A}/rust\", \
             \"../.siblings/qontinui-schemas@{SHA_A}/code-graph\"]\n"
        );
        assert_eq!(
            override_path_entries(&body).unwrap(),
            vec![
                format!("../.siblings/qontinui-schemas@{SHA_A}/rust"),
                format!("../.siblings/qontinui-schemas@{SHA_A}/code-graph"),
            ]
        );
        assert_eq!(
            override_path_entries("[net]\noffline = true\n"),
            Some(vec![])
        );
        assert_eq!(override_path_entries("paths = \"x\"\n"), None);
        assert_eq!(override_path_entries("paths = [1]\n"), None);
        assert_eq!(override_path_entries("paths = [\n"), None);
    }

    #[test]
    fn render_gate_writes_only_when_every_entry_exists() {
        let ws = tempfile::tempdir().unwrap();
        let agent_root = ws.path().join("agent-a");
        std::fs::create_dir_all(&agent_root).unwrap();
        let entry = ws
            .path()
            .join(format!(".siblings/qontinui-schemas@{SHA_A}"));
        std::fs::create_dir_all(entry.join("rust")).unwrap();
        std::fs::write(entry.join(COMPLETE_MARKER), "").unwrap();
        let body = format!(
            "paths = [\"../.siblings/qontinui-schemas@{SHA_A}/rust\", \
             \"../.siblings/qontinui-schemas@{SHA_A}/code-graph\"]\n"
        );

        std::fs::write(entry.join("rust/Cargo.toml"), "").unwrap();

        // One entry missing → gate refuses and names it.
        let missing = missing_override_entries(&agent_root, &body).unwrap();
        assert_eq!(missing.len(), 1);
        assert!(missing[0].ends_with("code-graph"));

        // The directory alone is not a crate: no Cargo.toml → still refused.
        std::fs::create_dir_all(entry.join("code-graph")).unwrap();
        assert_eq!(
            missing_override_entries(&agent_root, &body).unwrap().len(),
            1
        );

        // All present → nothing missing, caller writes.
        std::fs::write(entry.join("code-graph/Cargo.toml"), "").unwrap();
        assert!(missing_override_entries(&agent_root, &body)
            .unwrap()
            .is_empty());

        // Every entry is gated, not only `.siblings/` ones; `//` and `/./`
        // spellings resolve to the same directory.
        let other = format!(
            "paths = [\"..//.siblings/./qontinui-schemas@{SHA_A}/rust\", \"../elsewhere/crate\"]\n"
        );
        let missing = missing_override_entries(&agent_root, &other).unwrap();
        assert_eq!(missing.len(), 1);
        assert!(missing[0].ends_with("crate"));

        // Unparseable → Err, caller skips.
        assert!(missing_override_entries(&agent_root, "paths = [").is_err());
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// A one-commit repo with `rust/Cargo.toml`; returns (repo dir, sha).
    fn fixture_repo(root: &Path) -> (PathBuf, String) {
        let repo = root.join("qontinui-schemas");
        std::fs::create_dir_all(repo.join("rust")).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("rust/Cargo.toml"), "[package]\nname = \"t\"\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "--no-verify", "-m", "init"]);
        let sha = git(&repo, &["rev-parse", "HEAD"]);
        (repo, sha)
    }

    #[test]
    fn materialize_is_idempotent_and_read_only() {
        let ws = tempfile::tempdir().unwrap();
        let (repo, sha) = fixture_repo(ws.path());
        let store = store_root(&ws.path().join("agent-worktrees"));

        let first = materialize(&store, &repo, "qontinui-schemas", &sha).unwrap();
        assert_eq!(first, store.join(format!("qontinui-schemas@{sha}")));
        let manifest = first.join("rust/Cargo.toml");
        assert!(manifest.is_file());
        assert!(first.join(COMPLETE_MARKER).is_file());
        assert!(std::fs::metadata(&manifest)
            .unwrap()
            .permissions()
            .readonly());
        // No temp extraction or temp archive left behind.
        let leftovers: Vec<_> = std::fs::read_dir(&store)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());

        // Second call reuses the entry, and re-stamps its mtime as "adopted".
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        set_dir_mtime(&first, old).unwrap();
        let second = materialize(&store, &repo, "qontinui-schemas", &sha).unwrap();
        assert_eq!(first, second);
        let mtime = std::fs::metadata(&second).unwrap().modified().unwrap();
        assert!(mtime > old + std::time::Duration::from_secs(1800));
    }

    #[test]
    fn an_entry_that_already_exists_is_success_without_git() {
        // A losing racer / a reuse: the target is there, so no git runs — the
        // canonical path below does not even exist.
        let ws = tempfile::tempdir().unwrap();
        let store = ws.path().join(".siblings");
        let pre = store.join(format!("qontinui-schemas@{SHA_A}"));
        std::fs::create_dir_all(&pre).unwrap();
        std::fs::write(pre.join(COMPLETE_MARKER), "").unwrap();
        let got =
            materialize(&store, &ws.path().join("absent"), "qontinui-schemas", SHA_A).unwrap();
        assert_eq!(got, pre);
    }

    /// B1 regression: an archive larger than `MAX_CAPTURED_BYTES` (4 MiB) must
    /// round-trip completely — the captured-stdout path truncated it, sometimes
    /// into a silently partial tree.
    #[test]
    fn an_archive_over_four_mib_round_trips_completely() {
        let ws = tempfile::tempdir().unwrap();
        let repo = ws.path().join("qontinui-schemas");
        std::fs::create_dir_all(repo.join("rust")).unwrap();
        git(&repo, &["init", "-q"]);
        // Pseudo-random bytes so nothing along the way can shrink them.
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let big: Vec<u8> = (0..6 * 1024 * 1024)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x.to_le_bytes()[0]
            })
            .collect();
        std::fs::write(repo.join("a_big.bin"), &big).unwrap();
        std::fs::write(repo.join("rust/Cargo.toml"), "[package]\nname = \"t\"\n").unwrap();
        std::fs::write(repo.join("zz_last.txt"), "tail").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "--no-verify", "-m", "big"]);
        let sha = git(&repo, &["rev-parse", "HEAD"]);

        let store = ws.path().join(".siblings");
        let got = materialize(&store, &repo, "qontinui-schemas", &sha).unwrap();
        assert_eq!(std::fs::read(got.join("a_big.bin")).unwrap(), big);
        assert_eq!(
            std::fs::read_to_string(got.join("zz_last.txt")).unwrap(),
            "tail"
        );
        assert!(got.join("rust/Cargo.toml").is_file());
        assert!(got.join(COMPLETE_MARKER).is_file());
    }

    /// B2: a directory at the entry path without the completeness marker is a
    /// partial tree. It is not adopted, not deleted in place (set aside as
    /// `.trash-*`), and a complete entry is published in its place.
    #[test]
    fn a_partial_entry_is_set_aside_and_rematerialised() {
        let ws = tempfile::tempdir().unwrap();
        let (repo, sha) = fixture_repo(ws.path());
        let store = ws.path().join(".siblings");
        let entry = store.join(format!("qontinui-schemas@{sha}"));
        std::fs::create_dir_all(entry.join("rust")).unwrap();
        std::fs::write(entry.join("rust/half.rs"), "partial").unwrap();

        assert!(!adopt_existing(&entry).unwrap());
        assert!(
            !entry.exists(),
            "the partial tree was moved aside, not adopted"
        );
        let trash: Vec<_> = std::fs::read_dir(&store)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".trash-"))
            .collect();
        assert_eq!(trash.len(), 1);
        assert!(trash[0].path().join("rust/half.rs").is_file());

        std::fs::create_dir_all(entry.join("rust")).unwrap();
        let got = materialize(&store, &repo, "qontinui-schemas", &sha).unwrap();
        assert!(got.join(COMPLETE_MARKER).is_file());
        assert!(got.join("rust/Cargo.toml").is_file());
        assert!(!got.join("rust/half.rs").exists());
    }

    #[test]
    fn a_tree_completed_while_being_set_aside_is_restored() {
        let ws = tempfile::tempdir().unwrap();
        let target = ws.path().join(format!("qontinui-schemas@{SHA_A}"));
        let trash = trash_path(&target);

        // No marker in the trash → nothing moves back.
        std::fs::create_dir_all(&trash).unwrap();
        assert!(!restore_if_completed(&target, &trash));
        assert!(trash.is_dir() && !target.exists());

        // The marker appeared between the check and the rename → restored.
        std::fs::write(trash.join(COMPLETE_MARKER), "").unwrap();
        assert!(restore_if_completed(&target, &trash));
        assert!(target.join(COMPLETE_MARKER).is_file());
        assert!(!trash.exists());
    }

    #[test]
    fn a_completed_trash_stays_put_when_the_entry_was_republished() {
        let ws = tempfile::tempdir().unwrap();
        let target = ws.path().join(format!("qontinui-schemas@{SHA_A}"));
        std::fs::create_dir_all(target.join("rust")).unwrap();
        std::fs::write(target.join(COMPLETE_MARKER), "").unwrap();
        let trash = trash_path(&target);
        std::fs::create_dir_all(trash.join("rust")).unwrap();
        std::fs::write(trash.join(COMPLETE_MARKER), "").unwrap();

        assert!(!restore_if_completed(&target, &trash));
        assert!(trash.join(COMPLETE_MARKER).is_file(), "left for the reaper");
        assert!(target.join(COMPLETE_MARKER).is_file());
    }

    /// A failure AFTER the temp extraction dir and temp archive exist (here
    /// `git archive` failing on a missing blob, past `ensure_commit`) must leave
    /// neither behind, and publish no entry.
    #[test]
    fn a_failure_after_temp_creation_leaves_no_temp_behind() {
        let ws = tempfile::tempdir().unwrap();
        let (repo, sha) = fixture_repo(ws.path());
        let blob = git(&repo, &["rev-parse", &format!("{sha}:rust/Cargo.toml")]);
        let (dir, file) = blob.split_at(2);
        std::fs::remove_file(repo.join(".git/objects").join(dir).join(file)).unwrap();

        let store = ws.path().join(".siblings");
        let err = materialize(&store, &repo, "qontinui-schemas", &sha).unwrap_err();
        assert!(err.contains("git archive"), "{err}");
        let left: Vec<_> = std::fs::read_dir(&store)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(left.is_empty(), "left behind: {left:?}");
    }

    #[test]
    fn a_corrupt_archive_fails_to_unpack_cleanly() {
        let ws = tempfile::tempdir().unwrap();
        let tar = ws.path().join("bad.tar");
        std::fs::write(&tar, vec![0xAB; 1536]).unwrap();
        let dest = ws.path().join("dest");
        std::fs::create_dir_all(&dest).unwrap();
        assert!(unpack_file(&tar, &dest).is_err());
    }

    #[test]
    fn the_gate_refuses_an_entry_without_the_marker() {
        let ws = tempfile::tempdir().unwrap();
        let agent_root = ws.path().join("agent-a");
        std::fs::create_dir_all(&agent_root).unwrap();
        let entry = ws
            .path()
            .join(format!(".siblings/qontinui-schemas@{SHA_A}"));
        std::fs::create_dir_all(entry.join("rust")).unwrap();
        std::fs::write(entry.join("rust/Cargo.toml"), "").unwrap();
        let body = format!("paths = [\"../.siblings/qontinui-schemas@{SHA_A}/rust\"]\n");
        assert_eq!(
            missing_override_entries(&agent_root, &body).unwrap().len(),
            1
        );
        std::fs::write(entry.join(COMPLETE_MARKER), "").unwrap();
        assert!(missing_override_entries(&agent_root, &body)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_regular_file_at_the_entry_path_is_replaced_by_a_real_entry() {
        let ws = tempfile::tempdir().unwrap();
        let (repo, sha) = fixture_repo(ws.path());
        let store = ws.path().join(".siblings");
        std::fs::create_dir_all(&store).unwrap();
        let squat = store.join(format!("qontinui-schemas@{sha}"));
        std::fs::write(&squat, "not a directory").unwrap();
        let mut perms = std::fs::metadata(&squat).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&squat, perms).unwrap();

        let got = materialize(&store, &repo, "qontinui-schemas", &sha).unwrap();
        assert_eq!(got, squat);
        assert!(got.is_dir());
        assert!(got.join("rust/Cargo.toml").is_file());
    }

    #[test]
    fn adopting_never_creates_an_absent_entry() {
        let ws = tempfile::tempdir().unwrap();
        let absent = ws.path().join(format!("qontinui-schemas@{SHA_A}"));
        assert!(!adopt_existing(&absent).unwrap());
        assert!(!absent.exists());
    }

    #[test]
    fn a_missing_commit_fails_without_leaving_an_entry() {
        let ws = tempfile::tempdir().unwrap();
        let (repo, _) = fixture_repo(ws.path());
        let store = ws.path().join(".siblings");
        let err = materialize(&store, &repo, "qontinui-schemas", SHA_A).unwrap_err();
        assert!(err.contains("absent"), "{err}");
        assert!(!store.join(format!("qontinui-schemas@{SHA_A}")).exists());
    }
}
