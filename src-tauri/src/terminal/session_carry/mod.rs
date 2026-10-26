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
//!   never touches `<sid>.jsonl`, and backs out if the destination changed
//!   since the carry; best-effort — see [`delta_sync`].
//! - No carry replaces an existing `.jsonl` (main or subagent) unless the
//!   destination's bytes are a prefix of the source's: a destination that
//!   diverged is a fork with its own tail, and is left alone. The one
//!   exception is the main `<sid>.jsonl` on an account migration
//!   ([`MainTranscript::LiveSourceWins`]): the live conversation must be what
//!   resumes, so the fork is renamed aside to `<sid>.jsonl.diverged-<unix_ms>`
//!   (never deleted) and the source is copied in its place.
//! - [`spawn_delta_sync_after_exit`] is the detached driver: it waits (bounded)
//!   for the old pane's `claude` to be gone from the whole process table, then
//!   runs [`delta_sync`]. On timeout it skips; it never copies while the old
//!   writer may be live.
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

#[cfg(test)]
mod tests;

/// What [`copy_transcript`] carried across accounts.
#[derive(Debug, Clone)]
pub(crate) struct TranscriptCopy {
    /// The session transcript (`<sid>.jsonl`) under the destination account.
    pub path: PathBuf,
    /// What the carry did with the main `<sid>.jsonl`.
    pub main: MainCarry,
    /// The source's `<sid>/` side-car dir as the carry resolved it (beside the
    /// source transcript), whether or not it exists. [`delta_sync`] reads it
    /// from here rather than re-deriving it.
    pub src_session_dir: PathBuf,
    /// The destination's `<sid>/` side-car dir the carry wrote into (beside
    /// [`Self::path`] — for a long cwd, the source's `<prefix>-<hash>` name).
    pub dst_session_dir: PathBuf,
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
    /// that is not one plain path component. Also an existing `.jsonl` under
    /// `<sid>/` whose bytes are not a prefix of the source's: a diverged fork,
    /// never overwritten. The main `<sid>.jsonl` is NOT counted here — its
    /// outcome is [`TranscriptCopy::main`].
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
    /// failed copy of a file the destination did not have).
    pub dst: Option<FileStamp>,
    /// What the carry did to the destination.
    pub wrote: CarryWrite,
    /// A file rewritten in place (not a `.jsonl`) whose destination copy the
    /// carry KEPT because the destination side ran the session last.
    /// [`delta_sync`] never overwrites it either.
    pub kept_destination_newer: bool,
}

/// What [`carry_session`] did with the main `<sid>.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MainCarry {
    /// The destination had none; the source was copied.
    Copied,
    /// The destination held an older byte-prefix of the source; replaced.
    ExtendedPrefix,
    /// The destination was already at least as large; left untouched.
    Present,
    /// [`MainTranscript::LiveSourceWins`] only: the destination had diverged
    /// from the source (a fork with a tail of its own). It was renamed aside to
    /// `backup`, in the same dir, and the source copied in its place.
    DivergedPreserved { backup: PathBuf },
    /// The destination's smaller copy was kept although the source's differs:
    /// a [`MainTranscript::KeepExisting`] target, or a diverged fork under
    /// [`MainTranscript::ExtendPrefix`].
    KeptExisting,
}

impl MainCarry {
    /// The `mainTranscript` value of the `session-account-migrated` event.
    pub(crate) fn event_name(&self) -> &'static str {
        match self {
            Self::Copied => "copied",
            Self::ExtendedPrefix => "extended",
            Self::Present => "present",
            Self::DivergedPreserved { .. } => "diverged_preserved",
            Self::KeptExisting => "kept_existing",
        }
    }

    /// Where the diverged fork was preserved, when it was.
    pub(crate) fn backup(&self) -> Option<&Path> {
        match self {
            Self::DivergedPreserved { backup } => Some(backup),
            _ => None,
        }
    }
}

/// What the carry did to one destination file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CarryWrite {
    /// Written now.
    Copied,
    /// Already current — left untouched.
    Present,
    /// An existing `.jsonl` whose bytes are not a prefix of the source's — a
    /// fork with its own tail, left untouched.
    Diverged,
    /// The write failed. `dst` is what was there afterwards: absent, or an
    /// existing file the failed overwrite left in place — [`delta_sync`]
    /// retries it while it still has that stamp.
    Failed,
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
    /// Not followed (same rules as the carry, including a diverged `.jsonl`).
    pub skipped_other: usize,
    /// An in-place file the carry kept as the destination's (the destination
    /// side ran the session last) — never overwritten by the sync either.
    pub kept_destination_newer: usize,
    /// Read or write errors; each is logged.
    pub failed: usize,
}

/// Nesting bound for the `<sid>/` carry. The session directories seen so far
/// hold one level of directories under `<sid>/` (`subagents/`,
/// `tool-results/`) plus top-level files such as `custom-title.json`; the bound
/// only stops a pathological tree.
const SESSION_DIR_MAX_DEPTH: usize = 8;

/// Suffix of the temporary file a copy writes before renaming it into place.
const PARTIAL_SUFFIX: &str = ".partial";

/// How long the detached [`spawn_delta_sync_after_exit`] waits for the old
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

/// What [`carry_session`] may do to an existing destination `<sid>.jsonl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MainTranscript {
    /// Replace it when it is a strict byte-prefix of the source's (an
    /// append-only extension); keep it otherwise.
    ExtendPrefix,
    /// The account migration: the SOURCE is the live conversation and is what
    /// the respawn resumes. As [`Self::ExtendPrefix`], except that a smaller
    /// destination that DIVERGED is renamed aside to
    /// `<sid>.jsonl.diverged-<unix_ms>` (never deleted) and the source copied
    /// in its place ([`MainCarry::DivergedPreserved`]). If the rename-aside
    /// fails the carry fails, so the migration leaves the old pane untouched.
    LiveSourceWins,
    /// Never replace an existing one (only an absent one is written). The
    /// respawn receiver uses this for a target that already held the
    /// transcript: that copy is what the target resumes, even when a holder's
    /// is larger.
    KeepExisting,
}

/// [`carry_session`] with [`MainTranscript::LiveSourceWins`] — the account
/// migration's carry.
pub(crate) fn copy_transcript(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
) -> Result<TranscriptCopy, String> {
    carry_session(
        src_config_dir,
        dst_config_dir,
        working_dir,
        claude_session_id,
        MainTranscript::LiveSourceWins,
    )
}

/// Copy the session transcript from the source account's project dir into
/// the target account's matching project dir. The source file is never
/// modified or removed. Skips the copy when the destination already exists
/// and is at least as large (idempotent retry). When it is smaller but its
/// bytes are not a prefix of the source's (a fork), `main` decides: keep it
/// ([`MainCarry::KeptExisting`], warned), or — on a migration — rename it
/// aside and copy the source ([`MainCarry::DivergedPreserved`]). `main` can
/// also forbid replacing an existing destination at all.
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
pub(crate) fn carry_session(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
    main: MainTranscript,
) -> Result<TranscriptCopy, String> {
    carry_session_with(
        src_config_dir,
        dst_config_dir,
        working_dir,
        claude_session_id,
        main,
        |from, to| std::fs::rename(from, to),
    )
}

/// [`carry_session`] with the diverged main transcript's rename-aside
/// injected, so its failure path is testable.
fn carry_session_with<R>(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
    main: MainTranscript,
    rename_aside: R,
) -> Result<TranscriptCopy, String>
where
    R: FnOnce(&Path, &Path) -> std::io::Result<()>,
{
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
    // Before the first copy the target has no project dir to resolve, and for
    // a cwd over 200 encoded chars `session_transcript_path` then answers the
    // UNTRUNCATED name, which is not where `claude --resume` looks. Reuse the
    // source's on-disk dir name instead: Claude Code wrote it, `-<hash>`
    // suffix included, and derives the same name from the same cwd.
    let dst = match (dst.exists(), src.parent().and_then(Path::file_name)) {
        (false, Some(src_dir_name)) => Path::new(dst_config_dir)
            .join("projects")
            .join(src_dir_name)
            .join(format!("{claude_session_id}.jsonl")),
        _ => dst,
    };
    let src_meta = std::fs::metadata(&src)
        .map_err(|e| format!("source transcript missing at {}: {e}", src.display()))?;
    let src_len = src_meta.len();
    let dst_len = std::fs::metadata(&dst).map(|dst_meta| dst_meta.len()).ok();
    let main_verdict = match dst_len {
        None => Verdict::Write,
        // Already migrated (retry / repeated hint), or the newer copy.
        Some(len) if len >= src_len => Verdict::Current,
        // Smaller: an older prefix of the source, or a fork that wrote its
        // own tail. Only the first may be extended in place.
        Some(_) => match is_byte_prefix(&dst, &src) {
            Ok(true) if main == MainTranscript::KeepExisting => Verdict::Current,
            Ok(true) => Verdict::Write,
            Ok(false) => Verdict::Diverged,
            Err(e) => {
                return Err(format!(
                    "compare {} with {} failed: {e}",
                    dst.display(),
                    src.display()
                ))
            }
        },
    };
    let main_carry = match (main_verdict, main) {
        (Verdict::Write, _) => {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create {} failed: {e}", parent.display()))?;
            }
            copy_via_partial(&src, &dst)
                .map_err(|e| format!("copy {} -> {} failed: {e}", src.display(), dst.display()))?;
            if dst_len.is_some() {
                MainCarry::ExtendedPrefix
            } else {
                MainCarry::Copied
            }
        }
        (Verdict::Current, _) if dst_len.is_some_and(|len| len >= src_len) => MainCarry::Present,
        (Verdict::Current, _) => MainCarry::KeptExisting,
        (Verdict::Diverged, MainTranscript::LiveSourceWins) => {
            preserve_diverged_and_copy(&src, &dst, rename_aside)?
        }
        (Verdict::Diverged, _) => {
            warn!(
                from = %src.display(),
                to = %dst.display(),
                "session transcript carry: the destination <sid>.jsonl is not a prefix of the source's (a fork) — kept, not overwritten"
            );
            MainCarry::KeptExisting
        }
    };
    // Which side ran the session last, decided once from the append-only main
    // transcript. Sidecars that are rewritten in place (not appended) follow
    // it -- on a migration the source is newer, but on a respawn that tops up
    // from several holders the TARGET may be.
    let source_is_newer = if main == MainTranscript::KeepExisting && dst_len.is_some() {
        // A target that already held the transcript resumes ITS copy, so its
        // in-place sidecars stay with it, whatever a holder has.
        false
    } else if main_verdict == Verdict::Diverged {
        // A fork kept in place ran on its own and keeps its sidecars; a fork
        // moved aside was replaced by the live source, whose sidecars follow.
        matches!(main_carry, MainCarry::DivergedPreserved { .. })
    } else {
        dst_len.is_none_or(|len| src_len >= len)
    };
    let (session_dir, files) = carry_session_dir(&src, &dst, claude_session_id, source_is_newer);
    // Beside the two transcripts actually used above, so a long-cwd target
    // resolves to the same `<prefix>-<hash>` dir the main copy landed in.
    // `carry_session_dir` derives the same two paths (`parent()/<sid>`).
    let (src_session_dir, dst_session_dir) = (
        src.with_file_name(claude_session_id),
        dst.with_file_name(claude_session_id),
    );
    let subagents = carried_subagents(&src_session_dir, &dst_session_dir);
    // Times the main copy and the `<sid>/` carry together.
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if session_dir.failed > 0 {
        warn!(
            session = %claude_session_id,
            main = main_carry.event_name(),
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
            main = main_carry.event_name(),
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
        main: main_carry,
        src_session_dir,
        dst_session_dir,
        session_dir,
        files,
        subagents,
    })
}

/// [`MainTranscript::LiveSourceWins`] on a diverged destination: move the
/// fork aside to [`diverged_backup_path`] with `rename_aside`, then copy the
/// source in its place. Errors — so the carry, and the migration's step 1,
/// fail — when the fork cannot be moved aside (nothing is overwritten then),
/// or when the copy fails afterwards, in which case the fork is moved back if
/// the destination path is still free.
fn preserve_diverged_and_copy<R>(
    src: &Path,
    dst: &Path,
    rename_aside: R,
) -> Result<MainCarry, String>
where
    R: FnOnce(&Path, &Path) -> std::io::Result<()>,
{
    let backup = diverged_backup_path(dst);
    rename_aside(dst, &backup).map_err(|e| {
        format!(
            "the destination {} diverged from the live transcript and could not be moved aside to {}: {e} — nothing overwritten",
            dst.display(),
            backup.display()
        )
    })?;
    warn!(
        from = %src.display(),
        to = %dst.display(),
        backup = %backup.display(),
        "session transcript carry: the destination <sid>.jsonl diverged from the live transcript — the fork was moved aside and the live transcript copied in its place"
    );
    if let Err(e) = copy_via_partial(src, dst) {
        let restored = !dst.exists() && std::fs::rename(&backup, dst).is_ok();
        let fork = if restored {
            "the fork was moved back".to_string()
        } else {
            format!("the fork is preserved at {}", backup.display())
        };
        return Err(format!(
            "copy {} -> {} failed after the diverged destination was moved aside: {e} — {fork}",
            src.display(),
            dst.display(),
        ));
    }
    Ok(MainCarry::DivergedPreserved { backup })
}

/// `<sid>.jsonl.diverged-<unix_ms>` beside `dst`. The millisecond is bumped
/// until the name is free, so an earlier backup is never replaced.
fn diverged_backup_path(dst: &Path) -> PathBuf {
    let mut stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    loop {
        let mut name = dst
            .file_name()
            .map(std::ffi::OsStr::to_os_string)
            .unwrap_or_default();
        name.push(format!(".diverged-{stamp}"));
        let candidate = dst.with_file_name(name);
        if std::fs::symlink_metadata(&candidate).is_err() {
            return candidate;
        }
        stamp += 1;
    }
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
            let wrote = match destination_verdict(from, &to, src.len, source_is_newer) {
                Ok(Verdict::Current) => {
                    carry.skipped_present += 1;
                    CarryWrite::Present
                }
                Ok(Verdict::Diverged) => {
                    warn!(
                        from = %from.display(),
                        to = %to.display(),
                        "session transcript carry: destination .jsonl is not a prefix of the source's (a fork) — kept, not overwritten"
                    );
                    carry.skipped_other += 1;
                    CarryWrite::Diverged
                }
                Ok(Verdict::Write) => {
                    let written = to
                        .parent()
                        .map_or(Ok(()), std::fs::create_dir_all)
                        .and_then(|()| copy_via_partial(from, &to));
                    match written {
                        Ok(()) => {
                            carry.copied += 1;
                            CarryWrite::Copied
                        }
                        Err(e) => {
                            warn!(
                                from = %from.display(),
                                to = %to.display(),
                                error = %e,
                                "session transcript carry: file copy failed"
                            );
                            carry.failed += 1;
                            CarryWrite::Failed
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        from = %from.display(),
                        to = %to.display(),
                        error = %e,
                        "session transcript carry: could not compare with the destination"
                    );
                    carry.failed += 1;
                    CarryWrite::Failed
                }
            };
            files.push(CarriedFile {
                rel: rel.to_path_buf(),
                src,
                dst: FileStamp::of(&to),
                wrote,
                kept_destination_newer: wrote == CarryWrite::Present
                    && !source_is_newer
                    && !is_jsonl(rel),
            });
        }
    });
    (carry, files)
}

/// What a carry should do with one destination file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Absent, or an older copy that may be replaced.
    Write,
    /// Already holds what the source would leave there.
    Current,
    /// An existing `.jsonl` that is not a prefix of the source's.
    Diverged,
}

/// What the carry should do with `to`, given `from`. A `.jsonl` transcript is
/// append-only, so a destination at least as large is the newer copy (a
/// repeated hint, or a migration back) and must never be truncated, and a
/// smaller one is replaced only when its bytes are a prefix of the source's —
/// otherwise it is a fork with a tail of its own ([`Verdict::Diverged`]).
/// Anything else (e.g. `custom-title.json`, a JSON file overwritten in place
/// rather than appended to) keeps the destination's copy when the destination
/// side ran the session last, and otherwise is current only when its bytes
/// are identical.
fn destination_verdict(
    from: &Path,
    to: &Path,
    from_len: u64,
    source_is_newer: bool,
) -> std::io::Result<Verdict> {
    let Ok(to_meta) = std::fs::metadata(to) else {
        return Ok(Verdict::Write);
    };
    if is_jsonl(from) {
        return Ok(if to_meta.len() >= from_len {
            Verdict::Current
        } else if is_byte_prefix(to, from)? {
            Verdict::Write
        } else {
            Verdict::Diverged
        });
    }
    let current = !source_is_newer
        || (to_meta.len() == from_len
            && matches!(
                (std::fs::read(from), std::fs::read(to)),
                (Ok(a), Ok(b)) if a == b
            ));
    Ok(if current {
        Verdict::Current
    } else {
        Verdict::Write
    })
}

/// Whether the bytes of `prefix` are a prefix of the bytes of `of` (`false`
/// when `prefix` is the longer). Compared in chunks, so a large transcript is
/// never read into memory whole.
fn is_byte_prefix(prefix: &Path, of: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    const CHUNK: usize = 64 * 1024;
    let mut p = std::fs::File::open(prefix)?;
    let mut o = std::fs::File::open(of)?;
    let p_len = p.metadata()?.len();
    if p_len > o.metadata()?.len() {
        return Ok(false);
    }
    let (mut a, mut b) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    let mut remaining = p_len;
    while remaining > 0 {
        let n = usize::try_from(remaining).map_or(CHUNK, |r| r.min(CHUNK));
        p.read_exact(&mut a[..n])?;
        o.read_exact(&mut b[..n])?;
        if a[..n] != b[..n] {
            return Ok(false);
        }
        remaining -= n as u64;
    }
    Ok(true)
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
/// exited, against the baseline the carry recorded in `since` — including the
/// two `<sid>/` dirs the carry actually used, which are never re-derived here
/// (for a long cwd a re-derivation can name a different project dir).
///
/// Per regular file under the source's `<sid>/` (same walk as the carry):
/// - an in-place file the carry kept as the destination's
///   ([`CarriedFile::kept_destination_newer`]) is never overwritten
///   (`kept_destination_newer`);
/// - the destination must be EXACTLY as the carry left it (same size and
///   mtime; absent stays absent). Otherwise the resumed process — or someone —
///   has written it, and it is never touched (`refused_destination_changed`);
/// - the source must have moved on since the carry: a `.jsonl` must have GROWN
///   past both its carried size and the destination's (append-only, never
///   truncate); any other file must differ in size or mtime. A file that is new
///   since the carry counts as moved on when the destination is still absent,
///   and so does a file the carry failed to write ([`CarryWrite::Failed`]),
///   whether that left the destination absent or left an older copy in place;
/// - an existing destination `.jsonl` is replaced only when its bytes are a
///   prefix of the source's; a diverged one counts as `skipped_other`.
///
/// The copy re-checks the destination immediately before its rename (and
/// again before its one retry) and backs out if it changed while the bytes
/// were being written. That narrows the race with the resumed process to the
/// gap between that check and the rename; it does not close it —
/// `std::fs::rename` has no compare-and-swap form — so this is best-effort,
/// not a guarantee.
///
/// `<sid>.jsonl` is never touched: from the moment of respawn the resumed
/// process owns the destination's main transcript.
pub(crate) fn delta_sync(since: &TranscriptCopy) -> Result<DeltaSync, String> {
    // Built by a carry that has already validated the session id.
    let (src_dir, dst_dir) = (&since.src_session_dir, &since.dst_session_dir);
    let mut sync = DeltaSync::default();
    match std::fs::symlink_metadata(src_dir) {
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
    walk_session_dir(src_dir, &mut |event| match event {
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
            // The carry kept the destination's copy as the newer one; the
            // older side rewriting its own copy since does not change that.
            if recorded.is_some_and(|r| r.kept_destination_newer) {
                sync.kept_destination_newer += 1;
                return;
            }
            // A file the carry failed to write is retried even when its source
            // did not move on — the destination is still exactly what the
            // failed write left (checked above): absent, or the older copy it
            // meant to replace. A recorded-absent destination likewise has
            // nothing to overwrite.
            let moved_on = recorded.is_none_or(|r| {
                r.wrote == CarryWrite::Failed
                    || r.dst.is_none()
                    || source_moved_on(rel, r.src, src, dst_now)
            });
            if !moved_on {
                sync.unchanged += 1;
                return;
            }
            if dst_now.is_some() && is_jsonl(rel) {
                match is_byte_prefix(&to, from) {
                    Ok(true) => {}
                    Ok(false) => {
                        warn!(
                            from = %from.display(),
                            to = %to.display(),
                            "session delta sync: destination .jsonl is not a prefix of the source's (a fork) — kept, not overwritten"
                        );
                        sync.skipped_other += 1;
                        return;
                    }
                    Err(e) => {
                        warn!(
                            from = %from.display(),
                            to = %to.display(),
                            error = %e,
                            "session delta sync: could not compare with the destination"
                        );
                        sync.failed += 1;
                        return;
                    }
                }
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
    copy_if_destination_is_with(from, to, expected, |a, b| std::fs::rename(a, b))
}

/// [`copy_if_destination_is`] with the rename injected, so the re-check before
/// the retry is testable.
fn copy_if_destination_is_with<R>(
    from: &Path,
    to: &Path,
    expected: Option<FileStamp>,
    mut rename: R,
) -> std::io::Result<bool>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = write_partial(from, to)?;
    // Not `rename_with_retry`: the stamp is checked again before the one
    // retry, never assumed unchanged across the pause. On Windows the
    // toolchain's `std::fs::rename` calls `MoveFileExW` with
    // `MOVEFILE_REPLACE_EXISTING` and, on `ERROR_ACCESS_DENIED`, retries as a
    // `FileRenameInfoEx` rename with `FILE_RENAME_FLAG_POSIX_SEMANTICS`. A
    // writer holding the destination WITHOUT `FILE_SHARE_DELETE` makes the
    // rename fail; so can a transient filter (antivirus, indexer). The one
    // `PermissionDenied` retry below and the back-out after it handle that —
    // the partial is removed and nothing is replaced — and whatever caused
    // the failure, the destination may have moved on.
    let mut attempt = 0;
    let outcome = loop {
        if FileStamp::of(to) != expected {
            break Ok(false);
        }
        match rename(&partial, to) {
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

/// What the detached delta sync ended with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeltaSyncOutcome {
    /// The old writer was not proven gone within the deadline; nothing copied.
    WriterNotGone,
    Synced(DeltaSync),
    Failed(String),
}

/// Wait for the old writer to exit, then run `sync`. Never runs `sync` while
/// the writer may be live.
pub(crate) async fn run_delta_sync_after_exit<P, F, S, SF>(
    probe: P,
    deadline: Duration,
    poll: Duration,
    sync: S,
) -> DeltaSyncOutcome
where
    P: FnMut() -> F,
    F: Future<Output = WriterProbe>,
    S: FnOnce() -> SF,
    SF: Future<Output = Result<DeltaSync, String>>,
{
    if !wait_for_writer_exit(probe, deadline, poll).await {
        return DeltaSyncOutcome::WriterNotGone;
    }
    match sync().await {
        Ok(done) => DeltaSyncOutcome::Synced(done),
        Err(e) => DeltaSyncOutcome::Failed(e),
    }
}

/// Detached, best-effort: once the old pane's `claude` (`writer`, captured
/// BEFORE the pane was closed — `TerminalManager::close` does not wait for it)
/// is gone from the whole process table, re-copy the `<sid>/` files it wrote
/// between the carry and its exit. Bounded by [`DELTA_SYNC_EXIT_WAIT`]; on
/// timeout, or with no writer identity to watch, it logs and skips. A failure
/// is logged and never reaches the migration.
///
/// Returns a receiver that gets the outcome once the sync has finished or
/// given up, so the continue nudge can wait for it; `None` when no sync runs
/// at all (no writer identity).
pub(crate) fn spawn_delta_sync_after_exit(
    writer: Option<ProcIdentity>,
    claude_session_id: String,
    since: TranscriptCopy,
) -> Option<tokio::sync::oneshot::Receiver<DeltaSyncOutcome>> {
    let Some(writer) = writer else {
        info!(
            session = %claude_session_id,
            "session delta sync: skipped — the old pane's claude was not resolved before the close, so its exit cannot be proven"
        );
        return None;
    };
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    tauri::async_runtime::spawn(async move {
        let sid = claude_session_id;
        let outcome = run_delta_sync_after_exit(
            || probe_writer(writer),
            DELTA_SYNC_EXIT_WAIT,
            crate::terminal::graceful_exit::POLL_INTERVAL,
            // Small (side-car files written in a ~seconds window) but still
            // disk I/O: keep it off the async workers.
            move || async move {
                qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked(move || {
                    delta_sync(&since)
                })
                .await
                .map_err(|e| format!("delta sync task did not complete: {e}"))?
            },
        )
        .await;
        match &outcome {
            DeltaSyncOutcome::Synced(done) if *done == DeltaSync::default() => {}
            DeltaSyncOutcome::Synced(done) => info!(
                session = %sid,
                copied = done.copied,
                unchanged = done.unchanged,
                refused_destination_changed = done.refused_destination_changed,
                skipped_other = done.skipped_other,
                kept_destination_newer = done.kept_destination_newer,
                failed = done.failed,
                "session delta sync: topped up the destination's <sid>/ after the old writer exited"
            ),
            DeltaSyncOutcome::WriterNotGone => warn!(
                session = %sid,
                pid = writer.pid,
                wait_s = DELTA_SYNC_EXIT_WAIT.as_secs(),
                "session delta sync: skipped — the old pane's claude was still in the process table at the deadline"
            ),
            DeltaSyncOutcome::Failed(e) => warn!(
                session = %sid,
                error = %e,
                "session delta sync: failed (the migration itself is unaffected)"
            ),
        }
        // No receiver (auto-continue off) is fine.
        let _ = done_tx.send(outcome);
    });
    Some(done_rx)
}
