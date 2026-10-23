//! Carrying one Claude Code session from one account's config dir to another's.
//!
//! A session lives in two places under `<config>/projects/<enc>/`: the main
//! transcript `<sid>.jsonl`, and the sibling side-car directory `<sid>/`
//! (`subagents/agent-<id>.jsonl` + `.meta.json`, `tool-results/`,
//! `custom-title.json`). Claude Code resolves a `--resume` and a `SendMessage`
//! against the ACTIVE account's config dir, so a hop that carries only the
//! `.jsonl` strands every subagent spawned before it ("No transcript found for
//! agent ID").
//!
//! - [`copy_transcript`] is the one carry: the main transcript, then `<sid>/`
//!   file by file, with a per-file record of what both sides looked like
//!   afterwards and the list of subagents the destination now holds. Used by
//!   the account migration ([`super::account_migration::migrate_session`]) and
//!   the cross-machine respawn receiver (`crate::session::respawn`).
//! - [`delta_sync`] tops the destination's `<sid>/` up once the OLD writer has
//!   exited, for files the old pane wrote between the carry and its close. It
//!   never touches `<sid>.jsonl` and never overwrites a file the resumed
//!   process has written since the carry.
//! - [`sync_after_writer_exit`] is the post-close driver: it waits (bounded)
//!   for the old pane's `claude` to be gone from the whole process table, then
//!   runs [`delta_sync`] and the tail rescue ([`tail::rescue_tail`], Phase 2b).
//!   On timeout it skips; it never reads or copies while the old writer may be
//!   live.
//!
//! Plans `2026-10-12-account-migration-leaves-subagent-transcripts-behind`
//! (the carry) and
//! `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`
//! Phase 2 (this module, `delta_sync`, the subagent list).

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tracing::{info, warn};

use crate::terminal::graceful_exit::ProcIdentity;

pub(crate) mod tail;

#[cfg(test)]
mod tests;

/// What [`copy_transcript`] carried across accounts.
#[derive(Debug, Clone)]
pub(crate) struct TranscriptCopy {
    /// The session transcript (`<sid>.jsonl`) under the destination account.
    pub path: PathBuf,
    /// How far into the SOURCE `<sid>.jsonl` the destination is known to
    /// reach: the bytes this carry wrote, or the source's size when the
    /// destination already held at least that much. Everything the old writer
    /// appends past it after the carry is the tail
    /// [`tail::rescue_tail`] rescues (Phase 2b).
    pub transcript_offset: u64,
    /// The session's sibling directory (`<sid>/`), carried best-effort.
    pub session_dir: SessionDirCarry,
    /// Every regular file under `<sid>/` the carry looked at, with the source's
    /// stamp as the carry read it and the destination's as the carry left it —
    /// the baseline [`delta_sync`] compares against.
    pub files: Vec<CarriedFile>,
    /// Subagents whose transcripts the destination holds after the carry,
    /// sorted by id.
    pub subagents: Vec<CarriedSubagent>,
}

/// Tally of the best-effort `<sid>/` directory carry, counted per file (a
/// directory cut off by [`SESSION_DIR_MAX_DEPTH`] counts once).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SessionDirCarry {
    /// Files written to the destination.
    pub copied: usize,
    /// Destination already held the file (an append-only `.jsonl` at least as
    /// large, or any other file with identical bytes) — left untouched.
    pub skipped_present: usize,
    /// Never followed: a symlink or junction (including `<sid>/` itself), a
    /// leftover `*.partial`, a directory past the depth bound, or a session id
    /// that is not one plain path component.
    pub skipped_other: usize,
    /// Read or write errors (including an unreadable `<sid>/`); each is logged.
    pub failed: usize,
}

/// Size and modification time of one file — enough to tell "unchanged since"
/// from "written since" without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl FileStamp {
    fn from_meta(meta: &std::fs::Metadata) -> Self {
        Self {
            len: meta.len(),
            modified: meta.modified().ok(),
        }
    }

    /// The stamp of `path`, `None` when nothing is there.
    fn of(path: &Path) -> Option<Self> {
        std::fs::metadata(path).ok().map(|m| Self::from_meta(&m))
    }
}

/// One regular file under `<sid>/` as the carry saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CarriedFile {
    /// Path relative to `<sid>/`.
    pub rel: PathBuf,
    /// The source as the carry read it (before copying).
    pub src: FileStamp,
    /// The destination as the carry left it; `None` when it is absent (a
    /// failed copy).
    pub dst: Option<FileStamp>,
}

/// One subagent whose transcript the destination holds after a carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CarriedSubagent {
    /// The id `SendMessage` addresses (`agent-<id>.jsonl`).
    pub agent_id: String,
    /// `description` from the sibling `agent-<id>.meta.json`, when present.
    pub description: Option<String>,
}

/// What one [`delta_sync`] did, per file.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DeltaSync {
    /// Files re-copied because the old writer moved them on after the carry.
    pub copied: usize,
    /// Source unchanged since the carry (or not grown, for a `.jsonl`).
    pub unchanged: usize,
    /// The destination changed since the carry (the resumed process wrote it,
    /// or a file appeared there the carry did not put there) — never touched.
    pub refused_destination_changed: usize,
    /// Not followed (same rules as the carry).
    pub skipped_other: usize,
    /// Read or write errors; each is logged.
    pub failed: usize,
}

/// Nesting bound for the `<sid>/` carry. Measured 2026-10-12, Claude Code
/// writes one level of directories under `<sid>/` (`subagents/`,
/// `tool-results/`) plus top-level files such as `custom-title.json`; the bound
/// only stops a pathological tree.
const SESSION_DIR_MAX_DEPTH: usize = 8;

/// Suffix of the temporary file a copy writes before renaming it into place.
const PARTIAL_SUFFIX: &str = ".partial";

/// How long [`sync_after_writer_exit`] waits for the old
/// pane's `claude` to leave the process table before it gives up.
pub(crate) const DELTA_SYNC_EXIT_WAIT: Duration = Duration::from_secs(30);

/// The id names files and a directory we copy recursively; a Claude session
/// id is a uuid, and anything else is refused before any path is built.
/// Strict: the hyphenated form only (`parse_str` also takes the simple,
/// braced and `urn:uuid:` spellings).
fn require_session_uuid(claude_session_id: &str) -> Result<(), String> {
    let is_hyphenated_uuid = uuid::Uuid::try_parse(claude_session_id).is_ok_and(|u| {
        u.hyphenated()
            .to_string()
            .eq_ignore_ascii_case(claude_session_id)
    });
    if is_hyphenated_uuid {
        Ok(())
    } else {
        Err(format!(
            "not a Claude session id (expected a uuid): {claude_session_id:?}"
        ))
    }
}

/// Copy the session transcript from the source account's project dir into
/// the target account's matching project dir. The source file is never
/// modified or removed. Skips the copy when the destination already exists
/// and is at least as large (idempotent retry).
///
/// Then carries the session's sibling directory `<sid>/` file by file. Claude
/// Code keeps each subagent's transcript there
/// (`<sid>/subagents/agent-<id>.jsonl` + `.meta.json`) and resolves a
/// `SendMessage` resume against the ACTIVE account's config dir, so copying
/// `<sid>.jsonl` alone left every subagent spawned before the migration
/// unresumable — "No transcript found for agent ID" — which is exactly the
/// state a usage-limit kill leaves a fan-out in. Plan
/// `2026-10-12-account-migration-leaves-subagent-transcripts-behind`.
///
/// The directory carry never fails the call: a session that spawned no
/// subagent has no `<sid>/`, and a per-file error is logged and counted rather
/// than stranding the whole session on an exhausted account. It runs even when
/// the main transcript was already present, so a retry completes a partial
/// carry. Every write goes to `<name>.partial` and is renamed into place, so a
/// copy cut short by a killed runner never looks finished to the size rule.
pub(crate) fn copy_transcript(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
) -> Result<TranscriptCopy, String> {
    require_session_uuid(claude_session_id)?;
    let src = super::transcript::session_transcript_path(
        Path::new(src_config_dir),
        working_dir,
        claude_session_id,
    );
    let dst = super::transcript::session_transcript_path(
        Path::new(dst_config_dir),
        working_dir,
        claude_session_id,
    );
    let started = std::time::Instant::now();
    let src_meta = std::fs::metadata(&src)
        .map_err(|e| format!("source transcript missing at {}: {e}", src.display()))?;
    let dst_len = std::fs::metadata(&dst).map(|dst_meta| dst_meta.len()).ok();
    // Which side ran the session last, decided once from the append-only main
    // transcript: the larger one. Sidecars that are rewritten in place (not
    // appended) follow it -- on a migration the source is newer, but on a
    // respawn that tops up from several holders the TARGET may be.
    let source_is_newer = dst_len.is_none_or(|len| src_meta.len() >= len);
    // Already migrated (retry / repeated hint).
    let already_present = dst_len.is_some_and(|len| len >= src_meta.len());
    let mut transcript_offset = src_meta.len();
    if !already_present {
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {} failed: {e}", parent.display()))?;
        }
        copy_via_partial(&src, &dst)
            .map_err(|e| format!("copy {} -> {} failed: {e}", src.display(), dst.display()))?;
        // The old writer may have appended between the stat above and the
        // copy: the copied length, not the stat, is where the tail starts.
        // Nothing else writes `dst` yet — the respawn comes after the carry.
        if let Ok(meta) = std::fs::metadata(&dst) {
            transcript_offset = meta.len();
        }
    }
    let (session_dir, files) = carry_session_dir(&src, &dst, claude_session_id, source_is_newer);
    let subagents = carried_subagents(
        &super::transcript::session_sidecar_dir(
            Path::new(src_config_dir),
            working_dir,
            claude_session_id,
        ),
        &super::transcript::session_sidecar_dir(
            Path::new(dst_config_dir),
            working_dir,
            claude_session_id,
        ),
    );
    // Times the main copy and the `<sid>/` carry together.
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if session_dir.failed > 0 {
        warn!(
            session = %claude_session_id,
            copied = session_dir.copied,
            skipped_present = session_dir.skipped_present,
            skipped_other = session_dir.skipped_other,
            failed = session_dir.failed,
            subagents = subagents.len(),
            elapsed_ms,
            "session transcript carry: <sid>/ carried PARTIALLY — some subagent transcripts may not resume on this account"
        );
    } else if session_dir != SessionDirCarry::default() {
        info!(
            session = %claude_session_id,
            copied = session_dir.copied,
            skipped_present = session_dir.skipped_present,
            skipped_other = session_dir.skipped_other,
            subagents = subagents.len(),
            elapsed_ms,
            "session transcript carry: carried the session directory (subagent transcripts)"
        );
    }
    Ok(TranscriptCopy {
        path: dst,
        transcript_offset,
        session_dir,
        files,
        subagents,
    })
}

/// Copy `from` to a temporary `<to>.<pid>.<seq>.partial` and return its path.
/// The name is unique per call, so two writers racing on one destination (a
/// manual migrate click against the automatic hint, or a migration against a
/// respawn) never rename each other's half-written file into place.
fn write_partial(from: &Path, to: &Path) -> std::io::Result<PathBuf> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut partial_name = to
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or_else(|| std::io::Error::other("destination has no file name"))?;
    partial_name.push(format!(".{}.{seq}{PARTIAL_SUFFIX}", std::process::id()));
    let partial = to.with_file_name(partial_name);
    if let Err(e) = std::fs::copy(from, &partial) {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    Ok(partial)
}

/// Copy `from` over `to` through [`write_partial`] + rename.
fn copy_via_partial(from: &Path, to: &Path) -> std::io::Result<()> {
    let partial = write_partial(from, to)?;
    let renamed = rename_with_retry(&partial, to);
    if renamed.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    renamed
}

/// `rename`, retried once after a short pause on `PermissionDenied` -- on
/// Windows an antivirus or indexer filter can briefly hold a file that was just
/// written. Callers run on the blocking pool (see
/// [`super::account_migration::migrate_session_off_runtime`]).
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            std::thread::sleep(std::time::Duration::from_millis(200));
            std::fs::rename(from, to)
        }
        other => other,
    }
}

/// Carry `<sid>/` from beside `src_transcript` to beside `dst_transcript`.
/// Only "no such directory" is silent; every other way of carrying nothing is
/// counted and logged, so a stranded fan-out never reads as "no subagents".
fn carry_session_dir(
    src_transcript: &Path,
    dst_transcript: &Path,
    claude_session_id: &str,
    source_is_newer: bool,
) -> (SessionDirCarry, Vec<CarriedFile>) {
    let mut carry = SessionDirCarry::default();
    let mut files = Vec::new();
    // The id names a directory we copy recursively, so it must be exactly one
    // ordinary path component — never `..`, a separator, or a drive prefix.
    let mut components = Path::new(claude_session_id).components();
    let single_normal = matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    );
    if !single_normal {
        warn!(
            session = %claude_session_id,
            "session transcript carry: session id is not one plain path component — <sid>/ not carried"
        );
        carry.skipped_other += 1;
        return (carry, files);
    }
    let (Some(src_parent), Some(dst_parent)) = (src_transcript.parent(), dst_transcript.parent())
    else {
        return (carry, files);
    };
    let src_dir = src_parent.join(claude_session_id);
    match std::fs::symlink_metadata(&src_dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            warn!(
                dir = %src_dir.display(),
                "session transcript carry: <sid> is not a plain directory (symlink/junction/file) — not followed"
            );
            carry.skipped_other += 1;
            return (carry, files);
        }
        // No subagents, no sidecars: the common case, and the only silent one.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (carry, files),
        Err(e) => {
            warn!(dir = %src_dir.display(), error = %e, "session transcript carry: <sid>/ unreadable");
            carry.failed += 1;
            return (carry, files);
        }
    }
    let dst_dir = dst_parent.join(claude_session_id);
    walk_session_dir(&src_dir, &mut |event| match event {
        WalkEvent::Skipped => carry.skipped_other += 1,
        WalkEvent::Failed => carry.failed += 1,
        WalkEvent::File { from, rel, src } => {
            let to = dst_dir.join(rel);
            if destination_is_current(from, &to, src.len, source_is_newer) {
                carry.skipped_present += 1;
            } else {
                let written = to
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| copy_via_partial(from, &to));
                match written {
                    Ok(()) => carry.copied += 1,
                    Err(e) => {
                        warn!(
                            from = %from.display(),
                            to = %to.display(),
                            error = %e,
                            "session transcript carry: file copy failed"
                        );
                        carry.failed += 1;
                    }
                }
            }
            files.push(CarriedFile {
                rel: rel.to_path_buf(),
                src,
                dst: FileStamp::of(&to),
            });
        }
    });
    (carry, files)
}

/// Whether the destination already holds what `from` should leave there. A
/// `.jsonl` transcript is append-only, so a destination at least as large is
/// the newer copy (a repeated hint, or a migration back) and must never be
/// truncated. Anything else (e.g. `custom-title.json`, a JSON file overwritten
/// in place rather than appended to) keeps the destination's copy when the
/// destination side ran the session last, and otherwise is current only when
/// its bytes are identical.
fn destination_is_current(from: &Path, to: &Path, from_len: u64, source_is_newer: bool) -> bool {
    let Ok(to_meta) = std::fs::metadata(to) else {
        return false;
    };
    if is_jsonl(from) {
        return to_meta.len() >= from_len;
    }
    !source_is_newer
        || (to_meta.len() == from_len
            && matches!(
                (std::fs::read(from), std::fs::read(to)),
                (Ok(a), Ok(b)) if a == b
            ))
}

fn is_jsonl(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "jsonl")
}

/// One thing [`walk_session_dir`] met.
enum WalkEvent<'a> {
    /// A regular file, never a `*.partial`.
    File {
        from: &'a Path,
        /// Relative to the walked root.
        rel: &'a Path,
        src: FileStamp,
    },
    /// Not followed: a symlink or junction, a leftover `*.partial`, or a
    /// directory past [`SESSION_DIR_MAX_DEPTH`]. Logged where it is decided.
    Skipped,
    /// An unreadable directory, entry or stat. Logged where it happened.
    Failed,
}

/// Walk `<sid>/` without following links — the ONE walk the carry and
/// [`delta_sync`] share, so the two cannot disagree about what is carryable.
fn walk_session_dir(root: &Path, visit: &mut dyn FnMut(WalkEvent<'_>)) {
    walk_dir(root, Path::new(""), 0, visit);
}

fn walk_dir(dir: &Path, rel: &Path, depth: usize, visit: &mut dyn FnMut(WalkEvent<'_>)) {
    if depth >= SESSION_DIR_MAX_DEPTH {
        warn!(dir = %dir.display(), "session transcript carry: depth bound reached — not followed");
        visit(WalkEvent::Skipped);
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            warn!(dir = %dir.display(), error = %e, "session transcript carry: dir unreadable");
            visit(WalkEvent::Failed);
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                warn!(dir = %dir.display(), error = %e, "session transcript carry: unreadable entry");
                visit(WalkEvent::Failed);
                continue;
            }
        };
        let from = entry.path();
        let child_rel = rel.join(entry.file_name());
        let meta = match std::fs::symlink_metadata(&from) {
            Ok(meta) => meta,
            Err(e) => {
                warn!(path = %from.display(), error = %e, "session transcript carry: stat failed");
                visit(WalkEvent::Failed);
                continue;
            }
        };
        if meta.is_dir() {
            walk_dir(&from, &child_rel, depth + 1, visit);
            continue;
        }
        let is_partial = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(PARTIAL_SUFFIX));
        if !meta.is_file() || is_partial {
            visit(WalkEvent::Skipped);
            continue;
        }
        visit(WalkEvent::File {
            from: &from,
            rel: &child_rel,
            src: FileStamp::from_meta(&meta),
        });
    }
}

/// The subagents the destination can resume after a carry: every
/// `subagents/agent-<id>.jsonl` regular file in the SOURCE's `<sid>/` whose
/// counterpart is a regular file under the destination's `<sid>/`, with the
/// `description` from `agent-<id>.meta.json` (destination copy first, then the
/// source's). Ids that are not plain `[A-Za-z0-9_-]` are left out — they are
/// typed into the resumed session's prompt.
fn carried_subagents(src_sidecar: &Path, dst_sidecar: &Path) -> Vec<CarriedSubagent> {
    let (src_sub, dst_sub) = (src_sidecar.join("subagents"), dst_sidecar.join("subagents"));
    let Ok(entries) = std::fs::read_dir(&src_sub) else {
        return Vec::new();
    };
    let mut out: Vec<CarriedSubagent> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let agent_id = name.strip_prefix("agent-")?.strip_suffix(".jsonl")?;
            let plain = !agent_id.is_empty()
                && agent_id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            let is_file = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
            if !plain || !is_file(&entry.path()) || !is_file(&dst_sub.join(&name)) {
                return None;
            }
            let meta_name = format!("agent-{agent_id}.meta.json");
            let description = meta_description(&dst_sub.join(&meta_name))
                .or_else(|| meta_description(&src_sub.join(&meta_name)));
            Some(CarriedSubagent {
                agent_id: agent_id.to_string(),
                description,
            })
        })
        .collect();
    out.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    out
}

/// `description` out of an `agent-<id>.meta.json`, or `None`.
fn meta_description(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    value
        .get("description")?
        .as_str()
        .map(str::to_string)
        .filter(|d| !d.trim().is_empty())
}

/// Top the destination's `<sid>/` up from the source after the OLD writer has
/// exited, against the baseline the carry recorded in `since`.
///
/// Per regular file under the source's `<sid>/` (same walk as the carry):
/// - the destination must be EXACTLY as the carry left it (same size and
///   mtime; absent stays absent). Otherwise the resumed process — or someone —
///   has written it, and it is never touched (`refused_destination_changed`);
/// - the source must have moved on since the carry: a `.jsonl` must have GROWN
///   past both its carried size and the destination's (append-only, never
///   truncate); any other file must differ in size or mtime. A file that is new
///   since the carry counts as moved on when the destination is still absent,
///   and so does a file the carry failed to write.
///
/// The copy re-checks the destination immediately before its rename (and
/// again before its one retry) and backs out if it changed while the bytes
/// were being written. That narrows the race with the resumed process to the
/// gap between that check and the rename; it does not close it — Windows has
/// no rename-if-unchanged — so this is best-effort, not a guarantee.
///
/// `<sid>.jsonl` is never touched: from the moment of respawn the resumed
/// process owns the destination's main transcript.
pub(crate) fn delta_sync(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
    since: &TranscriptCopy,
) -> Result<DeltaSync, String> {
    require_session_uuid(claude_session_id)?;
    let src_dir = super::transcript::session_sidecar_dir(
        Path::new(src_config_dir),
        working_dir,
        claude_session_id,
    );
    let dst_dir = super::transcript::session_sidecar_dir(
        Path::new(dst_config_dir),
        working_dir,
        claude_session_id,
    );
    let mut sync = DeltaSync::default();
    match std::fs::symlink_metadata(&src_dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            sync.skipped_other += 1;
            return Ok(sync);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(sync),
        Err(e) => return Err(format!("{} unreadable: {e}", src_dir.display())),
    }
    let baseline: HashMap<&Path, &CarriedFile> =
        since.files.iter().map(|f| (f.rel.as_path(), f)).collect();
    walk_session_dir(&src_dir, &mut |event| match event {
        WalkEvent::Skipped => sync.skipped_other += 1,
        WalkEvent::Failed => sync.failed += 1,
        WalkEvent::File { from, rel, src } => {
            let to = dst_dir.join(rel);
            let dst_now = FileStamp::of(&to);
            let recorded = baseline.get(rel).copied();
            if dst_now != recorded.and_then(|r| r.dst) {
                sync.refused_destination_changed += 1;
                return;
            }
            // A file the carry failed to write (destination still absent) is
            // retried even when its source did not move on: there is nothing
            // at the destination to overwrite.
            let moved_on = recorded
                .is_none_or(|r| r.dst.is_none() || source_moved_on(rel, r.src, src, dst_now));
            if !moved_on {
                sync.unchanged += 1;
                return;
            }
            match copy_if_destination_is(from, &to, dst_now) {
                Ok(true) => sync.copied += 1,
                Ok(false) => sync.refused_destination_changed += 1,
                Err(e) => {
                    warn!(
                        from = %from.display(),
                        to = %to.display(),
                        error = %e,
                        "session delta sync: file copy failed"
                    );
                    sync.failed += 1;
                }
            }
        }
    });
    Ok(sync)
}

/// Whether the source moved on since the carry recorded `before`.
fn source_moved_on(
    rel: &Path,
    before: FileStamp,
    now: FileStamp,
    dst_now: Option<FileStamp>,
) -> bool {
    if now == before {
        return false;
    }
    if is_jsonl(rel) {
        return now.len > before.len && dst_now.is_none_or(|d| now.len > d.len);
    }
    true
}

/// Copy `from` over `to` only while `to` still has stamp `expected` (`None` =
/// absent): write the partial, re-stamp the destination, and rename only when
/// it is unchanged. `Ok(false)` when it changed in between (nothing written).
fn copy_if_destination_is(
    from: &Path,
    to: &Path,
    expected: Option<FileStamp>,
) -> std::io::Result<bool> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = write_partial(from, to)?;
    // Not `rename_with_retry`: a `PermissionDenied` here is most likely the
    // resumed `claude` holding the destination open while it writes, so the
    // stamp is checked again before the one retry, never assumed unchanged.
    let mut attempt = 0;
    let outcome = loop {
        if FileStamp::of(to) != expected {
            break Ok(false);
        }
        match std::fs::rename(&partial, to) {
            Ok(()) => break Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied && attempt == 0 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => break Err(e),
        }
    };
    if !matches!(outcome, Ok(true)) {
        let _ = std::fs::remove_file(&partial);
    }
    outcome
}

/// One look at whether the old writer is still alive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WriterProbe {
    Alive,
    Gone,
    /// The process table could not be read; says nothing either way.
    Unreadable,
}

/// Probe `writer` against a fresh snapshot of the whole process table — the
/// absence test [`crate::terminal::graceful_exit`] uses
/// ([`crate::terminal::graceful_exit::alive_in`]: pid plus start time, so a
/// reused pid is not mistaken for it).
async fn probe_writer(writer: ProcIdentity) -> WriterProbe {
    let snapshot = crate::process_capture::process_tree::snapshot_process_table_public().await;
    if snapshot.parent_map.is_empty() {
        return WriterProbe::Unreadable;
    }
    if crate::terminal::graceful_exit::alive_in(&snapshot, writer) {
        WriterProbe::Alive
    } else {
        WriterProbe::Gone
    }
}

/// Probe until the writer has been [`WriterProbe::Gone`] on
/// [`crate::terminal::graceful_exit::GONE_PROBES_REQUIRED`] consecutive looks
/// (`true`), or `deadline` passes (`false`). An unreadable look breaks the
/// streak: it is not evidence of absence.
pub(crate) async fn wait_for_writer_exit<P, F>(
    mut probe: P,
    deadline: Duration,
    poll: Duration,
) -> bool
where
    P: FnMut() -> F,
    F: Future<Output = WriterProbe>,
{
    let started = tokio::time::Instant::now();
    let mut gone_streak = 0;
    loop {
        if probe().await == WriterProbe::Gone {
            gone_streak += 1;
            if gone_streak >= crate::terminal::graceful_exit::GONE_PROBES_REQUIRED {
                return true;
            }
        } else {
            gone_streak = 0;
        }
        if started.elapsed() >= deadline {
            return false;
        }
        tokio::time::sleep(poll).await;
    }
}

/// What the detached post-exit step ended with. `T` is what the step returns
/// once the writer is gone ([`DeltaSync`] alone, or with the tail rescue).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeltaSyncOutcome<T = DeltaSync> {
    /// The old writer was not proven gone within the deadline; nothing copied.
    WriterNotGone,
    Synced(T),
    Failed(String),
}

/// Wait for the old writer to exit, then run `sync`. Never runs `sync` while
/// the writer may be live.
pub(crate) async fn run_delta_sync_after_exit<P, F, S, SF, T>(
    probe: P,
    deadline: Duration,
    poll: Duration,
    sync: S,
) -> DeltaSyncOutcome<T>
where
    P: FnMut() -> F,
    F: Future<Output = WriterProbe>,
    S: FnOnce() -> SF,
    SF: Future<Output = Result<T, String>>,
{
    if !wait_for_writer_exit(probe, deadline, poll).await {
        return DeltaSyncOutcome::WriterNotGone;
    }
    match sync().await {
        Ok(done) => DeltaSyncOutcome::Synced(done),
        Err(e) => DeltaSyncOutcome::Failed(e),
    }
}

/// Best-effort, awaited by the account migration's detached follow-up task
/// (`account_migration::followup`): once the old pane's `claude` (`writer`,
/// captured BEFORE the pane was closed — `TerminalManager::close` does not
/// wait for it) is gone from the whole process table, re-copy the `<sid>/`
/// files it wrote between the carry and its exit, then rescue the main
/// transcript's tail ([`tail::rescue_tail`]). Bounded by
/// [`DELTA_SYNC_EXIT_WAIT`]; on timeout, or with no writer identity to watch,
/// it logs and skips both — neither ever reads while the writer may be live.
/// A failure is logged and never reaches the migration.
///
/// Returns the tail rescue's verdict, `None` when it did not run (or failed).
pub(crate) async fn sync_after_writer_exit(
    writer: Option<ProcIdentity>,
    src_config_dir: String,
    dst_config_dir: String,
    working_dir: String,
    claude_session_id: String,
    since: TranscriptCopy,
) -> Option<tail::TailRescue> {
    let Some(writer) = writer else {
        info!(
            session = %claude_session_id,
            "session delta sync + tail rescue: skipped — the old pane's claude was not resolved before the close, so its exit cannot be proven"
        );
        return None;
    };
    let sid = claude_session_id.clone();
    let outcome = run_delta_sync_after_exit(
        || probe_writer(writer),
        DELTA_SYNC_EXIT_WAIT,
        crate::terminal::graceful_exit::POLL_INTERVAL,
        // Small (side-car files written in a ~seconds window) but still
        // disk I/O: keep it off the async workers.
        move || async move {
            qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
                let delta = delta_sync(
                    &src_config_dir,
                    &dst_config_dir,
                    &working_dir,
                    &claude_session_id,
                    &since,
                );
                let tail = tail::rescue_tail(
                    &src_config_dir,
                    &dst_config_dir,
                    &working_dir,
                    &claude_session_id,
                    &since,
                    chrono::Utc::now().timestamp_millis(),
                );
                Ok::<_, String>((delta, tail))
            })
            .await
            .map_err(|e| format!("delta sync task did not complete: {e}"))?
        },
    )
    .await;
    let (delta, tail) = match outcome {
        DeltaSyncOutcome::Synced(both) => both,
        DeltaSyncOutcome::WriterNotGone => {
            warn!(
                session = %sid,
                pid = writer.pid,
                wait_s = DELTA_SYNC_EXIT_WAIT.as_secs(),
                "session delta sync + tail rescue: skipped — the old pane's claude was still in the process table at the deadline"
            );
            return None;
        }
        DeltaSyncOutcome::Failed(e) => {
            warn!(
                session = %sid,
                error = %e,
                "session delta sync + tail rescue: failed (the migration itself is unaffected)"
            );
            return None;
        }
    };
    match delta {
        Ok(done) if done == DeltaSync::default() => {}
        Ok(done) => info!(
            session = %sid,
            copied = done.copied,
            unchanged = done.unchanged,
            refused_destination_changed = done.refused_destination_changed,
            skipped_other = done.skipped_other,
            failed = done.failed,
            "session delta sync: topped up the destination's <sid>/ after the old writer exited"
        ),
        Err(e) => warn!(
            session = %sid,
            error = %e,
            "session delta sync: failed (the migration itself is unaffected)"
        ),
    }
    match tail {
        Ok(rescue) => Some(rescue),
        Err(e) => {
            warn!(
                session = %sid,
                error = %e,
                "session tail rescue: failed (the migration itself is unaffected)"
            );
            None
        }
    }
}
