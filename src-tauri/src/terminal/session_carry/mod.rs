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
//! - [`carry_session`] is the one carry: the main transcript, then `<sid>/`
//!   file by file, with a per-file record of what both sides looked like
//!   afterwards and the list of subagents the destination now holds. Two doors
//!   reach it: [`copy_transcript`] (the account migration's destructive
//!   [`MainTranscript::LiveSourceWins`] carry, visible only inside
//!   `crate::terminal`, whose one caller holds the migration's in-flight
//!   guard — [`super::account_migration::migrate_session`]) and
//!   [`carry_for_respawn`] (the cross-machine respawn receiver,
//!   `crate::session::respawn`, which can name only the two non-destructive
//!   modes, [`RespawnCarry`]).
//! - [`delta_sync`] tops the destination's `<sid>/` up once the OLD writer has
//!   exited, for files the old pane wrote between the carry and its close. It
//!   never touches `<sid>.jsonl`, and backs out if the destination changed
//!   since the carry; best-effort — see [`delta_sync`].
//! - Outside an account migration no carry replaces an existing `.jsonl`
//!   (main or subagent) unless the destination's bytes are a prefix of the
//!   source's: a destination that diverged is a fork with its own tail, and is
//!   left alone. On an account migration ([`MainTranscript::LiveSourceWins`])
//!   the live conversation must be what resumes, so every `.jsonl` — the main
//!   `<sid>.jsonl` and each one under `<sid>/` alike — whose destination is
//!   neither byte-identical to the source nor a prefix of it, whatever its
//!   size, is renamed aside to `<file>.diverged-<unix_ms>` beside it (never
//!   deleted, never over an earlier backup) and the source is copied in its
//!   place. A subagent transcript moved aside keeps a copy of its fork's
//!   `agent-<id>.meta.json` beside it, under the same suffix
//!   (`agent-<id>.meta.json.diverged-<unix_ms>`), best-effort, and a main
//!   transcript moved aside does the same for the fork's `custom-title.json`
//!   when it differs from the live one. A `*.diverged-*` backup is never
//!   carried on to another account. When the live side has no
//!   `custom-title.json` at all, the fork's is backed up the same way and then
//!   removed from the destination, so the live conversation does not inherit
//!   the fork's title. Both title backups are taken whenever the fork is left
//!   at its backup — also by a carry that then fails.
//! - A destination stat that fails for any reason other than "not found" is
//!   never read as "absent": the file is recorded [`CarryWrite::Failed`] with
//!   its stamp UNKNOWN ([`CarriedFile::dst_unknown`]) and nothing is written
//!   over it — by the carry or by [`delta_sync`].
//! - In EVERY mode, a subagent's `agent-<id>.meta.json` at the destination is
//!   replaced — by the carry or later by [`delta_sync`] — only when, after the
//!   carry, its `agent-<id>.jsonl` there is known to hold the source's bytes or
//!   a prefix-extension of them ([`meta_may_follow_source`]). Otherwise the
//!   destination's meta stays with whatever transcript is there — including a
//!   destination `agent-<id>.jsonl` the carry has no record of at all (the
//!   walk skipped or failed its source).
//! - [`spawn_delta_sync_after_exit`] is the detached driver: it waits (bounded)
//!   for the old pane's `claude` to be gone from the whole process table, then
//!   runs [`delta_sync`]. On timeout it skips; it never copies while the old
//!   writer may be live. Its outcome gates the account migration's follow-up
//!   (`account_migration::followup`), which then rescues the main
//!   transcript's TAIL ([`tail::rescue_tail`], Phase 2b) — the lines the old
//!   writer appended after the carry — only once the writer is proven gone.
//!
//! Plans `2026-10-12-account-migration-leaves-subagent-transcripts-behind`
//! (the carry) and
//! `2026-10-21-account-migration-strands-subagent-transcripts-and-forks-live-sessions`
//! Phase 2 (this module, `delta_sync`, the subagent list) and Phase 2b (the
//! tail rescue).

use std::collections::{HashMap, HashSet};
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
    /// What the carry did with the main `<sid>.jsonl`.
    pub main: MainCarry,
    /// The source's `<sid>/` side-car dir as the carry resolved it (beside the
    /// source transcript), whether or not it exists. [`delta_sync`] reads it
    /// from here rather than re-deriving it.
    pub src_session_dir: PathBuf,
    /// The destination's `<sid>/` side-car dir the carry wrote into (beside
    /// [`Self::path`] — for a long cwd, the source's `<prefix>-<hash>` name).
    pub dst_session_dir: PathBuf,
    /// Where in the SOURCE `<sid>.jsonl` the tail starts: the line boundary
    /// [`settle_jsonl_end`] left the copy on. After a TORN final line, that is
    /// just past the last `\n` copied (the fragment was cut, so the line the
    /// old writer was half-way through appending is rescued WHOLE rather than
    /// as an unparseable fragment — 2026-10-23 review fix 7); with no complete
    /// line, 0. After a WHOLE final record the writer had not yet terminated,
    /// it is the copied length: the copy was completed with its own `\n`, and
    /// the writer's later `\n` reads as a blank first tail line, which the
    /// rescue skips. Everything past it is the tail [`tail::rescue_tail`]
    /// rescues (Phase 2b). Recorded whenever this carry wrote the destination
    /// ([`MainCarry::Copied`], [`MainCarry::ExtendedPrefix`], and
    /// [`MainCarry::DivergedPreserved`]), after which the destination holds the
    /// source's bytes up to that boundary (plus that `\n`, in the whole-record
    /// case) — except with no complete line and a torn fragment, where the
    /// offset is 0 and the destination still holds that fragment, so
    /// `dst == src[..offset]` is not an invariant to splice on.
    ///
    /// `None` when this carry copied nothing because the destination already
    /// held the transcript ([`MainCarry::Present`], [`MainCarry::KeptExisting`]
    /// — a retry, or a respawn topping up): the carry read no source bytes, so
    /// which lines are new is UNKNOWN and nothing is rescued (fix 8).
    pub transcript_offset: Option<u64>,
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
    /// Destination already held the file — left untouched. What "held" means
    /// depends on the mode and the file:
    /// - a `.jsonl` under [`MainTranscript::LiveSourceWins`] (a migration):
    ///   only when byte-identical to the source's;
    /// - a `.jsonl` in every other mode: when it is at least as large as the
    ///   source's (append-only, so the newer copy);
    /// - any other file: when its bytes are identical, OR — whatever its bytes
    ///   — when the destination side ran the session last and its copy was
    ///   kept as the newer one ([`CarriedFile::kept_destination_newer`];
    ///   never on a migration, where the live source always ran last);
    /// - in any mode, the `<stem>.meta.json` of a subagent whose `<stem>.jsonl`
    ///   at the destination is NOT known, after the carry, to hold the source's
    ///   bytes or a prefix-extension of them ([`meta_may_follow_source`] —
    ///   a fork left diverged in place, a write that failed with a file still
    ///   there, or a `.jsonl` judged current by its size alone): that
    ///   transcript's own meta, kept with it whatever its bytes (or its
    ///   absence) ([`CarriedFile::kept_destination_newer`]). Likewise the meta
    ///   of a `<stem>.jsonl` that exists at the destination but that the carry
    ///   has no record of (the walk skipped or failed its source).
    pub skipped_present: usize,
    /// Never followed: a symlink or junction (including `<sid>/` itself), a
    /// leftover `*.partial`, an earlier `*.diverged-*` backup, a directory
    /// past the depth bound, or a session id that is not one plain path
    /// component. The main `<sid>.jsonl` is NOT counted in this tally at all —
    /// its outcome is [`TranscriptCopy::main`].
    pub skipped_other: usize,
    /// [`MainTranscript::LiveSourceWins`] only: an existing `.jsonl` under
    /// `<sid>/` that diverged from the source's was renamed aside to
    /// `<file>.diverged-<unix_ms>` and the source copied in its place. Not
    /// also counted in `copied`.
    pub diverged_preserved: usize,
    /// Every other mode: an existing `.jsonl` under `<sid>/` whose bytes are
    /// not a prefix of the source's — a diverged fork, kept, never overwritten.
    pub diverged_kept: usize,
    /// Read or write errors (including an unreadable `<sid>/`, and a diverged
    /// `.jsonl` a migration could not move aside); each is logged.
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

    /// The stamp of `path`: `Ok(None)` only on a positive "not found"; any
    /// other stat error is returned, never read as "absent".
    fn of(path: &Path) -> std::io::Result<Option<Self>> {
        Self::of_with(path, &mut |p| std::fs::metadata(p))
    }

    /// [`Self::of`] over an injected `stat` ([`DestinationReads::stat`]).
    fn of_with(
        path: &Path,
        stat: &mut dyn FnMut(&Path) -> std::io::Result<std::fs::Metadata>,
    ) -> std::io::Result<Option<Self>> {
        match stat(path) {
            Ok(meta) => Ok(Some(Self::from_meta(&meta))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
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
    /// failed copy of a file the destination did not have) — or, when
    /// [`Self::dst_unknown`] is set, when it could not be stat'ed.
    pub dst: Option<FileStamp>,
    /// The destination's stat failed with something other than "not found",
    /// so what is there is UNKNOWN: `dst` is `None` but does not mean absent.
    /// Such a file is never written over — not by the carry, and not by
    /// [`delta_sync`], which cannot prove it unchanged since the carry — and a
    /// `.jsonl` recorded this way keeps its destination meta
    /// ([`meta_may_follow_source`]).
    pub dst_unknown: bool,
    /// What the carry did to the destination.
    pub wrote: CarryWrite,
    /// A file rewritten in place (not a `.jsonl`) whose destination copy the
    /// carry KEPT: because the destination side ran the session last, or — in
    /// any mode — because it is the `.meta.json` of a subagent whose
    /// destination `.jsonl` is not known to hold the source's bytes or a
    /// prefix-extension of them ([`meta_may_follow_source`]), so the meta
    /// stays with the transcript that is there. [`delta_sync`] never
    /// overwrites it either.
    pub kept_destination_newer: bool,
}

/// What [`carry_session`] did with the main `<sid>.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MainCarry {
    /// The destination had none; the source was copied.
    Copied,
    /// The destination held an older byte-prefix of the source; replaced.
    ExtendedPrefix,
    /// The destination was already current; left untouched. Under
    /// [`MainTranscript::LiveSourceWins`] that means byte-identical to the
    /// source; otherwise at least as large.
    Present,
    /// [`MainTranscript::LiveSourceWins`] only: the destination was neither
    /// byte-identical to the source nor a strict byte-prefix of it (a fork —
    /// smaller, the same size with other bytes, or larger). It was renamed
    /// aside to `backup`, in the same dir, and the source copied in its place.
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
    /// Already current — left untouched. For a `.jsonl` this is only ever a
    /// migration's verdict, and means byte-identical to the source.
    Present,
    /// A `.jsonl` outside a migration left untouched because it is at least as
    /// large as the source's (append-only, so the newer copy). Judged by size
    /// alone: its bytes were never compared, so it is NOT known to hold the
    /// source's ([`meta_may_follow_source`]).
    PresentBySize,
    /// An existing `.jsonl` whose bytes are not a prefix of the source's — a
    /// fork with its own tail, left in place: kept by design outside a
    /// migration, or, on a migration, because it could not be moved aside.
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
    /// side ran the session last, or the meta of a subagent fork left in
    /// place) — never overwritten by the sync either.
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

/// What a backup name carries between the file's own name and its stamp
/// (`<file>.diverged-<unix_ms>`, [`claim_diverged_backup_at`]).
const DIVERGED_MARKER: &str = ".diverged-";

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
/// Private: outside this module the destructive
/// [`MainTranscript::LiveSourceWins`] is reachable only through
/// [`copy_transcript`], and the respawn receiver names its modes with
/// [`RespawnCarry`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MainTranscript {
    /// Replace it when it is a strict byte-prefix of the source's (an
    /// append-only extension); keep it otherwise.
    ExtendPrefix,
    /// The account migration: the SOURCE is the live conversation and is what
    /// the respawn resumes, whatever the sizes. A byte-identical destination is
    /// left alone ([`MainCarry::Present`]) and a strict byte-prefix is extended
    /// ([`MainCarry::ExtendedPrefix`]); any other destination — smaller and
    /// diverged, the same size with other bytes, or larger — is renamed aside
    /// to `<sid>.jsonl.diverged-<unix_ms>` (never deleted) and the source
    /// copied in its place ([`MainCarry::DivergedPreserved`]). If the
    /// rename-aside fails the carry fails, so the migration leaves the old pane
    /// untouched. Every `.jsonl` under `<sid>/` (subagent transcripts) follows
    /// the same three rules, except that a subagent file that cannot be moved
    /// aside is skipped (counted `failed`, left out of
    /// [`TranscriptCopy::subagents`]) rather than failing the carry.
    ///
    /// Files under `<sid>/` that are rewritten in place rather than appended
    /// to follow the live side and are overwritten. When the main transcript
    /// was moved aside (and whenever it is left at its backup, even by a
    /// failed carry), the fork's `<sid>/custom-title.json` is first copied to
    /// `custom-title.json.diverged-<same suffix>` if it differs from the
    /// source's — and, when the source has none, then removed from the
    /// destination ([`preserve_fork_custom_title`]) — and a subagent's
    /// `.meta.json` follows its transcript ([`meta_may_follow_source`]). Any
    /// other in-place file the fork rewrote is overwritten by the live one
    /// with no backup.
    LiveSourceWins,
    /// Never replace an existing one (only an absent one is written). The
    /// respawn receiver uses this for a target that already held the
    /// transcript: that copy is what the target resumes, even when a holder's
    /// is larger.
    KeepExisting,
}

/// [`carry_session`] with [`MainTranscript::LiveSourceWins`] — the account
/// migration's carry, which moves a diverged destination aside and copies the
/// live source over it. Visible only inside `crate::terminal`: its one caller
/// ([`super::account_migration::migrate_session`]) holds the session's
/// in-flight guard across it, and nothing outside this module tree can name
/// [`MainTranscript`] to reach the same carry another way
/// ([`carry_for_respawn`] takes [`RespawnCarry`], which has no destructive
/// mode).
pub(super) fn copy_transcript(
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

/// The two carry modes the cross-machine respawn receiver may ask for — the
/// non-destructive [`MainTranscript`]s, and no other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RespawnCarry {
    /// [`MainTranscript::ExtendPrefix`].
    ExtendPrefix,
    /// [`MainTranscript::KeepExisting`].
    KeepExisting,
}

/// [`carry_session`] for the respawn receiver (`crate::session::respawn`), in
/// one of the [`RespawnCarry`] modes. It cannot move a destination aside or
/// replace one that is not a byte-prefix of the source's.
pub(crate) fn carry_for_respawn(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
    mode: RespawnCarry,
) -> Result<TranscriptCopy, String> {
    carry_session(
        src_config_dir,
        dst_config_dir,
        working_dir,
        claude_session_id,
        match mode {
            RespawnCarry::ExtendPrefix => MainTranscript::ExtendPrefix,
            RespawnCarry::KeepExisting => MainTranscript::KeepExisting,
        },
    )
}

/// Copy the session transcript from the source account's project dir into
/// the target account's matching project dir. The source file is never
/// modified or removed. What happens to an existing destination depends on
/// `main`:
///
/// - [`MainTranscript::ExtendPrefix`]: skipped when it is at least as large
///   (idempotent retry); replaced when it is a smaller strict byte-prefix of
///   the source's; otherwise kept (a fork; [`MainCarry::KeptExisting`],
///   warned).
/// - [`MainTranscript::KeepExisting`]: never replaced — skipped when at least
///   as large, and kept when smaller ([`MainCarry::KeptExisting`]), whether or
///   not its bytes are a prefix of the source's.
/// - [`MainTranscript::LiveSourceWins`] (a migration): skipped only when
///   byte-identical, extended when a strict byte-prefix, and otherwise renamed
///   aside with the source copied in its place
///   ([`MainCarry::DivergedPreserved`]).
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
fn carry_session(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
    main: MainTranscript,
) -> Result<TranscriptCopy, String> {
    carry_session_with_rename(
        src_config_dir,
        dst_config_dir,
        working_dir,
        claude_session_id,
        main,
        |from, to| std::fs::rename(from, to),
    )
}

/// [`carry_session`] with the primitive `rename` injected. The rename-aside of
/// a diverged `.jsonl` is that rename retried once on `PermissionDenied`
/// ([`rename_with_retry_using`]) — the same transient Windows filter hold the
/// carry's own copies already ride out. Retrying is safe there: the first
/// attempt failed, so the fork is still at the destination and the claimed
/// backup name is still the empty placeholder
/// [`claim_diverged_backup_at`] created, which the retry renames over.
fn carry_session_with_rename<R>(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
    main: MainTranscript,
    mut rename: R,
) -> Result<TranscriptCopy, String>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    carry_session_with(
        src_config_dir,
        dst_config_dir,
        working_dir,
        claude_session_id,
        main,
        |from, to| rename_with_retry_using(from, to, &mut rename),
        || {},
    )
}

/// [`carry_session`] with two test seams: the rename-aside of a diverged
/// `.jsonl` (main or subagent), so its failure paths are testable, and
/// `after_stat`, run once right after both main transcripts were stat'ed, so a
/// test can grow the source between that stat and the classification.
fn carry_session_with<R, A>(
    src_config_dir: &str,
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
    main: MainTranscript,
    mut rename_aside: R,
    after_stat: A,
) -> Result<TranscriptCopy, String>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()>,
    A: FnOnce(),
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
    // `try_exists`, not `exists`: a stat error is not "absent" here either.
    let dst_present = dst
        .try_exists()
        .map_err(|e| format!("stat {} failed: {e}", dst.display()))?;
    let dst = match (dst_present, src.parent().and_then(Path::file_name)) {
        (false, Some(src_dir_name)) => Path::new(dst_config_dir)
            .join("projects")
            .join(src_dir_name)
            .join(format!("{claude_session_id}.jsonl")),
        _ => dst,
    };
    let src_meta = std::fs::metadata(&src)
        .map_err(|e| format!("source transcript missing at {}: {e}", src.display()))?;
    let src_len = src_meta.len();
    // Only a positive "not found" is an absent destination. Any other stat
    // error leaves what is there unknown, so the carry fails rather than
    // write over it.
    let dst_len = match std::fs::metadata(&dst) {
        Ok(dst_meta) => Some(dst_meta.len()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return Err(format!(
                "stat {} failed: {e} — not read as absent, nothing written",
                dst.display()
            ))
        }
    };
    after_stat();
    let main_verdict = match dst_len {
        None => Verdict::Write,
        // A migration resumes the LIVE source, whatever the sizes: only a
        // byte-identical destination is current, a strict byte-prefix is
        // extended, and anything else — the same size with other bytes, or
        // larger — is a fork to move aside. Before this, a larger stale fork
        // read as current, was resumed, and the live pane was closed —
        // dropping the live conversation.
        //
        // Prefix FIRST, with both lengths taken from the same open handles as
        // the comparison ([`prefix_relation`]), never from the stats above:
        // the source is the live transcript and may grow between that stat and
        // this read. Sized against a stale `src_len`, a destination identical
        // to the source as it was a moment ago read as "same size, other
        // bytes" and was moved aside as a fork; read prefix-first it is a
        // strict prefix of the grown source and is extended.
        Some(_) if main == MainTranscript::LiveSourceWins => live_jsonl_verdict(&dst, &src)
            .map_err(|e| {
                format!(
                    "compare {} with {} failed: {e}",
                    dst.display(),
                    src.display()
                )
            })?,
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
    // `settled` is `Some` exactly when this carry wrote `dst`: the outcome of
    // settling the copy's end onto a line boundary ([`settle_jsonl_end`]),
    // done on the `.partial` BEFORE its rename so the file that appears at
    // `dst` already ends on one, and nothing reopens `dst` after the rename
    // (round-13 review S2: a concurrent carry could have replaced it by then).
    let (main_carry, settled) = match (main_verdict, main) {
        (Verdict::Write, _) => {
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("create {} failed: {e}", parent.display()))?;
            }
            let settled = copy_via_partial_with(&src, &dst, settle_jsonl_end)
                .map_err(|e| format!("copy {} -> {} failed: {e}", src.display(), dst.display()))?;
            let carry = if dst_len.is_some() {
                MainCarry::ExtendedPrefix
            } else {
                MainCarry::Copied
            };
            (carry, Some(settled))
        }
        // Byte-identical by `live_jsonl_verdict` — not re-judged by the stale
        // stats.
        (Verdict::Current, MainTranscript::LiveSourceWins) => (MainCarry::Present, None),
        (Verdict::Current, _) if dst_len.is_some_and(|len| len >= src_len) => {
            (MainCarry::Present, None)
        }
        (Verdict::Current, _) => (MainCarry::KeptExisting, None),
        (Verdict::Diverged, MainTranscript::LiveSourceWins) => {
            let (carry, settled) = preserve_diverged_main(
                &src,
                &dst,
                (
                    &src.with_file_name(claude_session_id),
                    &dst.with_file_name(claude_session_id),
                ),
                &mut rename_aside,
            )?;
            (carry, Some(settled))
        }
        (Verdict::Diverged, _) => {
            warn!(
                from = %src.display(),
                to = %dst.display(),
                "session transcript carry: the destination <sid>.jsonl is not a prefix of the source's (a fork) — kept, not overwritten"
            );
            (MainCarry::KeptExisting, None)
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
    } else if main == MainTranscript::LiveSourceWins {
        // A migration that got here left the destination holding the live
        // source's bytes (copied, extended, preserved-aside-and-copied, or
        // already identical), so the live side's sidecars follow it.
        true
    } else if main_verdict == Verdict::Diverged {
        // A fork kept in place ran on its own and keeps its sidecars.
        false
    } else {
        match dst_len {
            None => true,
            Some(len) if len < src_len => true,
            // The same size by the stat: current by its size alone
            // ([`MainCarry::Present`]), its bytes never compared. Only the
            // source's own bytes (or a prefix of a source that grew since the
            // stat) say the source ran last; a same-size destination with other
            // bytes is a fork that keeps its in-place sidecars — and so does
            // one whose compare fails.
            Some(len) if len == src_len => {
                matches!(is_byte_prefix(&dst, &src), Ok(true))
            }
            Some(_) => false,
        }
    };
    // Where the tail starts. When this carry wrote `dst`, the old writer may
    // have appended between the stat above and the copy, possibly part of a
    // line: the copied bytes, not the stat, say where the tail starts — at
    // the line boundary [`settle_jsonl_end`] left them on. A destination the
    // carry did not write, or a copy whose end could not be settled, leaves
    // the offset UNKNOWN.
    let transcript_offset = match settled {
        Some(Ok(offset)) => Some(offset),
        Some(Err(e)) => {
            warn!(
                session = %claude_session_id,
                dst = %dst.display(),
                error = %e,
                "session transcript carry: could not settle the copy's end on a line boundary — the tail rescue will report UNKNOWN, and the resumed session's first append may join a torn line"
            );
            None
        }
        None => None,
    };
    let live_source_wins = main == MainTranscript::LiveSourceWins;
    // A main transcript moved aside already kept the fork's
    // `custom-title.json` beside it ([`preserve_diverged_main`]).
    let (session_dir, files) = carry_session_dir(
        &src,
        &dst,
        claude_session_id,
        source_is_newer,
        live_source_wins,
        &mut rename_aside,
    );
    // Beside the two transcripts actually used above, so a long-cwd target
    // resolves to the same `<prefix>-<hash>` dir the main copy landed in.
    // `carry_session_dir` derives the same two paths (`parent()/<sid>`).
    let (src_session_dir, dst_session_dir) = (
        src.with_file_name(claude_session_id),
        dst.with_file_name(claude_session_id),
    );
    // On a migration the destination must hold the LIVE subagent for the
    // nudge to call it resumable: a file left diverged (it could not be moved
    // aside) or whose write failed does not.
    let not_live: HashSet<&Path> = if live_source_wins {
        files
            .iter()
            .filter(|f| matches!(f.wrote, CarryWrite::Diverged | CarryWrite::Failed))
            .map(|f| f.rel.as_path())
            .collect()
    } else {
        HashSet::new()
    };
    let subagents = carried_subagents(&src_session_dir, &dst_session_dir, &not_live);
    // Times the main copy and the `<sid>/` carry together.
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    if session_dir.failed > 0 {
        warn!(
            session = %claude_session_id,
            main = main_carry.event_name(),
            copied = session_dir.copied,
            skipped_present = session_dir.skipped_present,
            skipped_other = session_dir.skipped_other,
            diverged_preserved = session_dir.diverged_preserved,
            diverged_kept = session_dir.diverged_kept,
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
            diverged_preserved = session_dir.diverged_preserved,
            diverged_kept = session_dir.diverged_kept,
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
        transcript_offset,
        session_dir,
        files,
        subagents,
    })
}

/// Why [`preserve_diverged_and_copy`] did not leave the source in place.
#[derive(Debug)]
enum PreserveError {
    /// Nothing was moved: the fork is still at the destination. `backup` is
    /// the name that was claimed for it, when one was.
    NotMovedAside {
        backup: Option<PathBuf>,
        error: std::io::Error,
    },
    /// The fork was moved aside to `backup`, then the copy failed. `restored`:
    /// the fork was moved back to the destination.
    CopyFailed {
        backup: PathBuf,
        restored: bool,
        error: std::io::Error,
    },
}

/// [`MainTranscript::LiveSourceWins`] on a `.jsonl` destination that is
/// neither byte-identical to the source nor a prefix of it: move the fork
/// aside to a freshly claimed [`claim_diverged_backup`] name with
/// `rename_aside`, then copy the source in its place, running `settle` on the
/// copy before it is renamed into place ([`copy_via_partial_with`]). Returns
/// the backup path and what `settle` returned.
/// When the copy fails after the move, the fork is moved back if the
/// destination path is still free.
fn preserve_diverged_and_copy<R, S, T>(
    src: &Path,
    dst: &Path,
    rename_aside: &mut R,
    settle: S,
) -> Result<(PathBuf, T), PreserveError>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()> + ?Sized,
    S: FnOnce(&Path) -> T,
{
    let backup = claim_diverged_backup(dst).map_err(|error| PreserveError::NotMovedAside {
        backup: None,
        error,
    })?;
    if let Err(error) = rename_aside(dst, &backup) {
        // Drop the empty placeholder the claim created — only while it is
        // still empty, so a rename that moved the fork and then reported an
        // error never costs its bytes.
        if std::fs::metadata(&backup).is_ok_and(|m| m.len() == 0) {
            let _ = std::fs::remove_file(&backup);
        }
        return Err(PreserveError::NotMovedAside {
            backup: Some(backup),
            error,
        });
    }
    let settled = match copy_via_partial_with(src, dst, settle) {
        Ok(settled) => settled,
        Err(error) => {
            // Check-then-rename, not atomic. The migration's per-session
            // in-flight guard (`account_migration::begin_migration`), held across
            // the whole carry, excludes only a CONCURRENT MIGRATION of this
            // session; the resumed pane that will own the destination is not
            // spawned yet. It does NOT exclude an earlier hop's detached step-4b
            // delta sync, which runs outside the guard by design (the
            // `account_migration` module doc's exception): after a fast hop back
            // to an account the session left moments ago, that sync may still be
            // writing into this same `<sid>/`. It writes only a destination still
            // exactly as its own carry left it, so it backs off a file this carry
            // moved aside — best-effort, not a lock. A write that lands before the
            // check below leaves the fork at `backup` (`restored: false`); one
            // that lands between the check and the rename is replaced by the fork.
            // `path_is_free`, not `!exists()`: a stat error is not a free path,
            // and a rename back over something there would replace it.
            let restored = path_is_free(dst) && std::fs::rename(&backup, dst).is_ok();
            return Err(PreserveError::CopyFailed {
                backup,
                restored,
                error,
            });
        }
    };
    Ok((backup, settled))
}

/// [`preserve_diverged_and_copy`] for the main `<sid>.jsonl`. Errors — so the
/// carry, and the migration's step 1, fail — when the fork cannot be moved
/// aside (nothing is overwritten then), or when the copy fails afterwards.
///
/// `session_dirs` is the `(source, destination)` pair of `<sid>/` dirs. Every
/// arm that leaves the fork at its backup — moved and copied over, moved and
/// then errored, or a copy that failed without moving it back — keeps the
/// fork's `custom-title.json` beside it first ([`preserve_fork_custom_title`]).
/// The error arms need it as much as the success arm: a retry finds the slot
/// empty, copies the live transcript in as [`MainCarry::Copied`], and would
/// otherwise carry the live title over the fork's with no backup.
fn preserve_diverged_main<R>(
    src: &Path,
    dst: &Path,
    session_dirs: (&Path, &Path),
    rename_aside: &mut R,
) -> Result<(MainCarry, std::io::Result<u64>), String>
where
    R: FnMut(&Path, &Path) -> std::io::Result<()>,
{
    let (src_session_dir, dst_session_dir) = session_dirs;
    let keep_title_with =
        |backup: &Path| preserve_fork_custom_title(src_session_dir, dst_session_dir, dst, backup);
    match preserve_diverged_and_copy(src, dst, rename_aside, settle_jsonl_end) {
        Ok((backup, settled)) => {
            warn!(
                from = %src.display(),
                to = %dst.display(),
                backup = %backup.display(),
                "session transcript carry: the destination <sid>.jsonl diverged from the live transcript — the fork was moved aside and the live transcript copied in its place"
            );
            // Before `carry_session_dir` copies the live `custom-title.json`
            // over the fork's.
            keep_title_with(&backup);
            Ok((MainCarry::DivergedPreserved { backup }, settled))
        }
        // A rename that moved the fork and then reported an error: the fork is
        // at its backup, not at the destination. Same test as the subagent
        // arm in `preserve_diverged_file`.
        Err(PreserveError::NotMovedAside {
            backup: Some(backup),
            error,
        }) if std::fs::metadata(&backup).is_ok_and(|m| m.len() > 0) || path_is_free(dst) => {
            keep_title_with(&backup);
            Err(format!(
                "the rename-aside of the diverged destination {} reported an error but moved it: {error} — the fork is preserved at {}, and the live transcript was not copied",
                dst.display(),
                backup.display(),
            ))
        }
        Err(PreserveError::NotMovedAside { backup, error }) => Err(format!(
            "the destination {} diverged from the live transcript and could not be moved aside{}: {error} — nothing overwritten",
            dst.display(),
            backup.map_or_else(String::new, |b| format!(" to {}", b.display())),
        )),
        Err(PreserveError::CopyFailed {
            backup,
            restored,
            error,
        }) => {
            let fork = if restored {
                "the fork was moved back".to_string()
            } else {
                keep_title_with(&backup);
                format!("the fork is preserved at {}", backup.display())
            };
            Err(format!(
                "copy {} -> {} failed after the diverged destination was moved aside: {error} — {fork}",
                src.display(),
                dst.display(),
            ))
        }
    }
}

/// Whether nothing at all is at `path` — the test that a rename-aside which
/// reported an error nevertheless MOVED the fork. Only a positive "not found"
/// counts: any other stat error, and a link whose target is gone, leave the
/// fork's whereabouts unknown, so they read as "still there" (`false`), never
/// as moved. `Path::exists` answers `false` for both and is not used.
fn path_is_free(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Claim `<file>.diverged-<unix_ms>` beside `dst` for a fork about to be
/// moved aside. See [`claim_diverged_backup_at`].
fn claim_diverged_backup(dst: &Path) -> std::io::Result<PathBuf> {
    let now_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    claim_diverged_backup_at(dst, now_ms)
}

/// Claim `<file>.diverged-<stamp>` beside `dst` by CREATING it as an empty
/// placeholder with `create_new`, bumping `stamp` past every name that already
/// exists; the caller then renames the fork over its own placeholder. The
/// create is atomic, so two carries backing up in the same millisecond — or a
/// name that appears between a probe and a rename — never share a name, and an
/// earlier backup is never replaced.
fn claim_diverged_backup_at(dst: &Path, mut stamp: u128) -> std::io::Result<PathBuf> {
    let file_name = dst
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or_else(|| std::io::Error::other("destination has no file name"))?;
    loop {
        let mut name = file_name.clone();
        name.push(format!("{DIVERGED_MARKER}{stamp}"));
        let candidate = dst.with_file_name(name);
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(_) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => stamp += 1,
            Err(e) => return Err(e),
        }
    }
}

/// How far back from the end [`line_boundary_offset`] reads per step.
const LINE_BOUNDARY_CHUNK: u64 = 64 * 1024;

/// The offset just past the LAST `\n` in `path` (0 when it holds none) —
/// read backwards from the end in [`LINE_BOUNDARY_CHUNK`] steps, so a
/// transcript of any size costs one small read in the usual case (it ends in
/// `\n`).
pub(crate) fn line_boundary_offset(path: &Path) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let mut end = file.metadata()?.len();
    let mut buf = Vec::new();
    while end > 0 {
        let start = end.saturating_sub(LINE_BOUNDARY_CHUNK);
        buf.resize(usize::try_from(end - start).unwrap_or(0), 0);
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf)?;
        if let Some(i) = buf.iter().rposition(|b| *b == b'\n') {
            return Ok(start + i as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

/// Settle the end of a copied `.jsonl` onto a line boundary and return the
/// SOURCE offset the tail starts at. Run on the `.partial` before its rename
/// ([`copy_via_partial_with`]), so the file that appears at the destination
/// already ends on a boundary.
///
/// The bytes past the last `\n` ([`line_boundary_offset`]) are either:
/// - a WHOLE record (they parse as a JSON object) whose `\n` the writer had
///   not appended yet — it is kept and completed with a `\n`, and the offset
///   is the copied length: the `\n` is the destination's own, so the source's
///   tail starts where the copied bytes end (a `\n` the writer adds there
///   reads as a blank line, which the rescue skips). Cutting it would drop a
///   complete record from the resumed transcript (round-13 review S1);
/// - a TORN line — cut back to the boundary, which is the offset: the tail
///   rescue reads that line whole from the source, and left in place the
///   resumed CLI's first append would glue a new record onto the fragment
///   (round-12 review S3). A copy holding NO complete line (boundary 0) is
///   kept as it is rather than emptied, since the resume needs a transcript.
///
/// The appended `\n` makes the destination NOT a byte-prefix of a source that
/// has not yet written its own `\n`, so a `LiveSourceWins` retry inside that
/// window reads `Diverged` and moves the byte-equal-plus-newline copy aside:
/// that `.diverged-*` is not a real fork (round-14 review S1).
pub(crate) fn settle_jsonl_end(path: &Path) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let boundary = line_boundary_offset(path)?;
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let len = file.metadata()?.len();
    if boundary >= len {
        return Ok(len);
    }
    let mut fragment = Vec::new();
    file.seek(SeekFrom::Start(boundary))?;
    file.read_to_end(&mut fragment)?;
    if is_whole_json_object(&fragment) {
        file.seek(SeekFrom::End(0))?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        return Ok(len);
    }
    if boundary == 0 {
        return Ok(0);
    }
    file.set_len(boundary)?;
    file.sync_all()?;
    Ok(boundary)
}

/// [`settle_jsonl_end`] for a `.jsonl` under `<sid>/` (a subagent transcript),
/// run on the `.partial` before its rename: a copy taken mid-append is cut back
/// to its last line boundary (or its whole final record completed), so the
/// resumed subagent's first append starts a fresh line (round-14 review S3).
/// No tail offset is recorded for these, so a failure is logged, not fatal.
fn settle_subagent_jsonl(partial: &Path) {
    if let Err(e) = settle_jsonl_end(partial) {
        warn!(
            path = %partial.display(),
            error = %e,
            "session transcript carry: could not settle a subagent transcript copy on a line boundary — its first resumed append may join a torn line"
        );
    }
}

/// Whether `bytes` are one complete JSON object (a transcript record).
fn is_whole_json_object(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes).is_ok_and(|v| v.is_object())
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
    copy_via_partial_with(from, to, |_| ())
}

/// [`copy_via_partial`], running `settle` on the `.partial` BEFORE the atomic
/// rename and returning what it returned — so whatever `settle` does to the
/// copy is already true of the file that appears at `to`, and nothing reopens
/// `to` afterwards, where a concurrent carry may have replaced it.
fn copy_via_partial_with<S, T>(from: &Path, to: &Path, settle: S) -> std::io::Result<T>
where
    S: FnOnce(&Path) -> T,
{
    let partial = write_partial(from, to)?;
    let settled = settle(&partial);
    let renamed = rename_with_retry(&partial, to);
    if renamed.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    renamed.map(|()| settled)
}

/// `rename`, retried once after a short pause on `PermissionDenied` -- on
/// Windows an antivirus or indexer filter can briefly hold a file that was just
/// written. Callers run on the blocking pool (see
/// [`super::account_migration::migrate_session_off_runtime`]).
fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    rename_with_retry_using(from, to, &mut |a, b| std::fs::rename(a, b))
}

/// [`rename_with_retry`] over an injected `rename`, so the retry is testable.
fn rename_with_retry_using(
    from: &Path,
    to: &Path,
    rename: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    match rename(from, to) {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            std::thread::sleep(std::time::Duration::from_millis(200));
            rename(from, to)
        }
        other => other,
    }
}

/// Carry `<sid>/` from beside `src_transcript` to beside `dst_transcript`.
/// Only "no such directory" is silent; every other way of carrying nothing is
/// counted and logged, so a stranded fan-out never reads as "no subagents".
///
/// `live_source_wins` ([`MainTranscript::LiveSourceWins`]): a diverged `.jsonl`
/// is moved aside with `rename_aside` and the source copied in its place; when
/// it cannot be moved aside the file is skipped (`failed`, recorded
/// [`CarryWrite::Diverged`]) and the carry goes on.
fn carry_session_dir(
    src_transcript: &Path,
    dst_transcript: &Path,
    claude_session_id: &str,
    source_is_newer: bool,
    live_source_wins: bool,
    rename_aside: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
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
    let mut walked: Vec<WalkedFile> = Vec::new();
    walk_session_dir(&src_dir, &mut |event| match event {
        WalkEvent::Skipped => carry.skipped_other += 1,
        WalkEvent::Failed => carry.failed += 1,
        WalkEvent::File { from, rel, src } => {
            walked.push((from.to_path_buf(), rel.to_path_buf(), src));
        }
    });
    let files = carry_walked_files(
        walked,
        &dst_dir,
        source_is_newer,
        live_source_wins,
        rename_aside,
        &mut DestinationReads {
            stat: &mut |dst_file| std::fs::metadata(dst_file),
            compare: &mut |dst_file, src_file| prefix_relation(dst_file, src_file),
        },
        &mut carry,
    );
    (carry, files)
}

/// One regular file the walk of the source's `<sid>/` met: its path, its path
/// relative to `<sid>/`, and its stamp as read then.
type WalkedFile = (PathBuf, PathBuf, FileStamp);

/// The two reads [`carry_walked_files`] makes of a destination file, injected
/// so a test can fail either one.
struct DestinationReads<'a> {
    /// `stat(to)` — [`std::fs::metadata`] in production. Only an error of kind
    /// `NotFound` reads as "absent" ([`FileStamp::of_with`]).
    stat: &'a mut dyn FnMut(&Path) -> std::io::Result<std::fs::Metadata>,
    /// `compare(to, from)` — [`prefix_relation`] in production.
    compare: &'a mut dyn FnMut(&Path, &Path) -> std::io::Result<PrefixRelation>,
}

/// Carry the files [`walk_session_dir`] collected into `dst_dir`, in whatever
/// order they arrive — the walk's order is not trusted, so this sorts them
/// itself (a test hands them in the wrong order to prove it).
///
/// Every `.jsonl` goes before any other file: a diverged subagent transcript
/// moved aside takes its fork's `agent-<id>.meta.json` with it
/// ([`preserve_sibling_meta`]), which must happen before the source's meta is
/// copied over that one, and whether a meta may be replaced at all is decided
/// by its transcript's record ([`meta_may_follow_source`]), which the meta's
/// own turn must already know. `read_dir` promises no order.
///
/// `reads` are [`std::fs::metadata`] and [`prefix_relation`] in production; a
/// test injects a failing one. A destination whose stat fails with anything
/// but "not found" is never written: it is recorded [`CarryWrite::Failed`],
/// and when its stamp after the carry cannot be read either it is recorded
/// [`CarriedFile::dst_unknown`].
///
/// A `<stem>.meta.json` is kept as the destination's, besides the
/// [`meta_may_follow_source`] rule, when the carry has NO record of its
/// `<stem>.jsonl` (the walk skipped or failed that source) while one exists
/// at the destination: nothing says that transcript holds the source's bytes.
fn carry_walked_files(
    mut walked: Vec<WalkedFile>,
    dst_dir: &Path,
    source_is_newer: bool,
    live_source_wins: bool,
    rename_aside: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
    reads: &mut DestinationReads<'_>,
    carry: &mut SessionDirCarry,
) -> Vec<CarriedFile> {
    walked.sort_by_key(|(_, rel, _)| !is_jsonl(rel));
    let mut files = Vec::new();
    // `<stem>.meta.json` (relative to `<sid>/`) of every subagent transcript
    // whose record fails [`meta_may_follow_source`]: the destination's meta
    // stays with the transcript that is there — the source's is not copied
    // over it, and [`delta_sync`] never overwrites it.
    let mut fork_metas: HashSet<PathBuf> = HashSet::new();
    // Every `.jsonl` this carry recorded; the sort above records them all
    // before the first meta's turn.
    let mut recorded_jsonl: HashSet<PathBuf> = HashSet::new();
    for (from, rel, src) in walked {
        let (from, rel) = (from.as_path(), rel.as_path());
        let to = dst_dir.join(rel);
        if fork_metas.contains(rel) || meta_of_unrecorded_jsonl(rel, dst_dir, &recorded_jsonl) {
            carry.skipped_present += 1;
            let (dst, dst_unknown) = destination_stamp(&to, &mut *reads.stat);
            files.push(CarriedFile {
                rel: rel.to_path_buf(),
                src,
                dst,
                dst_unknown,
                wrote: CarryWrite::Present,
                kept_destination_newer: true,
            });
            continue;
        }
        let verdict = destination_verdict(
            from,
            &to,
            src.len,
            source_is_newer,
            live_source_wins,
            &mut *reads,
        );
        let wrote = match verdict {
            // Outside a migration a `.jsonl` is current by its size alone
            // ([`destination_verdict`]); a migration's is byte-identical.
            Ok(Verdict::Current) if is_jsonl(rel) && !live_source_wins => {
                carry.skipped_present += 1;
                CarryWrite::PresentBySize
            }
            Ok(Verdict::Current) => {
                carry.skipped_present += 1;
                CarryWrite::Present
            }
            Ok(Verdict::Diverged) if live_source_wins => {
                preserve_diverged_file(from, &to, &mut *rename_aside, carry)
            }
            Ok(Verdict::Diverged) => {
                warn!(
                    from = %from.display(),
                    to = %to.display(),
                    "session transcript carry: destination .jsonl is not a prefix of the source's (a fork) — kept, not overwritten"
                );
                carry.diverged_kept += 1;
                CarryWrite::Diverged
            }
            Ok(Verdict::Write) => {
                let written = to
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|()| {
                        if is_jsonl(rel) {
                            copy_via_partial_with(from, &to, settle_subagent_jsonl)
                        } else {
                            copy_via_partial(from, &to)
                        }
                    });
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
                    "session transcript carry: could not stat or compare the destination — not written"
                );
                carry.failed += 1;
                CarryWrite::Failed
            }
        };
        let (dst, dst_unknown) = destination_stamp(&to, &mut *reads.stat);
        let record = CarriedFile {
            rel: rel.to_path_buf(),
            src,
            dst,
            dst_unknown,
            wrote,
            kept_destination_newer: wrote == CarryWrite::Present
                && !source_is_newer
                && !is_jsonl(rel),
        };
        if is_jsonl(rel) {
            recorded_jsonl.insert(rel.to_path_buf());
        }
        // Whatever the mode; the `.jsonl` sort above runs this before that
        // meta's own turn.
        if let Some(meta) = protected_meta_rel(&record) {
            fork_metas.insert(meta);
        }
        files.push(record);
    }
    files
}

/// The destination's stamp as a record keeps it: `(stamp, unknown)`, where a
/// stat error other than "not found" is `(None, true)` — logged, never read as
/// absent.
fn destination_stamp(
    to: &Path,
    stat: &mut dyn FnMut(&Path) -> std::io::Result<std::fs::Metadata>,
) -> (Option<FileStamp>, bool) {
    match FileStamp::of_with(to, stat) {
        Ok(stamp) => (stamp, false),
        Err(e) => {
            warn!(
                to = %to.display(),
                error = %e,
                "session transcript carry: could not stat the destination after the carry — recorded as unknown"
            );
            (None, true)
        }
    }
}

/// `<stem>.jsonl` beside the meta `<stem>.meta.json` at `meta_rel`; `None` for
/// any other file.
fn sibling_jsonl_rel(meta_rel: &Path) -> Option<PathBuf> {
    let stem = meta_rel.file_name()?.to_str()?.strip_suffix(".meta.json")?;
    Some(meta_rel.with_file_name(format!("{stem}.jsonl")))
}

/// Whether `rel` is a `<stem>.meta.json` whose `<stem>.jsonl` is NOT in
/// `recorded` while something is at the destination in its place (or the
/// destination cannot tell — only a positive "not found" is free,
/// [`path_is_free`]). Such a meta stays with the transcript that is there.
fn meta_of_unrecorded_jsonl(rel: &Path, dst_dir: &Path, recorded: &HashSet<PathBuf>) -> bool {
    sibling_jsonl_rel(rel)
        .is_some_and(|jsonl| !recorded.contains(&jsonl) && !path_is_free(&dst_dir.join(&jsonl)))
}

/// `<stem>.meta.json` beside the transcript `<stem>.jsonl` at `jsonl_rel`.
fn sibling_meta_rel(jsonl_rel: &Path) -> Option<PathBuf> {
    let stem = jsonl_rel.file_name()?.to_str()?.strip_suffix(".jsonl")?;
    Some(jsonl_rel.with_file_name(format!("{stem}.meta.json")))
}

/// THE rule for a subagent's `<stem>.meta.json`, in every carry mode: the
/// destination's meta may be replaced — by the carry, or later by
/// [`delta_sync`] — only when, after the carry, the destination `<stem>.jsonl`
/// is KNOWN to hold the source's bytes or a prefix-extension of them, judged
/// from that `.jsonl`'s carry record:
/// - [`CarryWrite::Copied`]: written now — it was absent, a byte-prefix
///   extended, or a fork moved aside and the source copied in its place (that
///   fork's meta was copied beside its backup first, [`preserve_sibling_meta`]);
/// - [`CarryWrite::Present`]: compared byte for byte and identical (only a
///   migration records `Present` for a `.jsonl`);
/// - [`CarryWrite::Failed`] with nothing left at the destination (a positive
///   "not found", not [`CarriedFile::dst_unknown`]): the fork was moved aside
///   to its backup (its meta copied beside it) or there never was one. The
///   slot MAY then get the live file from [`delta_sync`] — only on a migration
///   whose old pane's `claude` was identified before the close and left the
///   process table within [`DELTA_SYNC_EXIT_WAIT`]
///   ([`spawn_delta_sync_after_exit`]); the respawn receiver runs no sync at
///   all — and its meta follows that live one whenever it does. Otherwise the
///   slot stays empty until a later carry fills it, and the meta beside it is
///   the live one or none.
///
/// Every other outcome keeps the destination's meta with the transcript that
/// is there: [`CarryWrite::Diverged`] (a fork left in place), `Failed` with a
/// file still at the destination (a compare error, a failed overwrite) or with
/// a destination whose stat failed (unknown, never read as absent), and
/// [`CarryWrite::PresentBySize`] (current by its size, bytes never compared).
fn meta_may_follow_source(jsonl: &CarriedFile) -> bool {
    match jsonl.wrote {
        CarryWrite::Copied | CarryWrite::Present => true,
        CarryWrite::Failed => jsonl.dst.is_none() && !jsonl.dst_unknown,
        CarryWrite::Diverged | CarryWrite::PresentBySize => false,
    }
}

/// The `<stem>.meta.json` that `record` (a `<stem>.jsonl`'s) protects from
/// being replaced ([`meta_may_follow_source`]); `None` for any other file, or
/// a transcript whose meta may follow the source's.
fn protected_meta_rel(record: &CarriedFile) -> Option<PathBuf> {
    if meta_may_follow_source(record) {
        return None;
    }
    sibling_meta_rel(&record.rel)
}

/// A diverged `.jsonl` under `<sid>/` on a migration: move the fork aside
/// and copy the source over it ([`preserve_diverged_and_copy`]). Never fails
/// the carry — a subagent file that cannot be moved aside is skipped, counted
/// `failed` and recorded [`CarryWrite::Diverged`], so it is left out of
/// [`TranscriptCopy::subagents`] and [`delta_sync`] never overwrites it.
///
/// Wherever the fork ends up, its `agent-<id>.meta.json` goes with it: beside
/// the backup when the fork was moved aside (copied, success, a copy that
/// failed and could not move it back, or a rename-aside that reported an error
/// after moving it — those last two are recorded [`CarryWrite::Failed`]), and
/// untouched in place when the fork stayed there (the caller keeps the
/// source's meta off it).
fn preserve_diverged_file(
    from: &Path,
    to: &Path,
    rename_aside: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
    carry: &mut SessionDirCarry,
) -> CarryWrite {
    let settle = |partial: &Path| {
        if is_jsonl(to) {
            settle_subagent_jsonl(partial);
        }
    };
    match preserve_diverged_and_copy(from, to, rename_aside, settle) {
        Ok((backup, ())) => {
            warn!(
                from = %from.display(),
                to = %to.display(),
                backup = %backup.display(),
                "session transcript carry: destination .jsonl diverged from the live source — the fork was moved aside and the live file copied in its place"
            );
            preserve_sibling_meta(to, &backup);
            carry.diverged_preserved += 1;
            CarryWrite::Copied
        }
        // The rename reported an error but MOVED the fork anyway: the claimed
        // backup holds bytes (a diverged fork is never empty), or the
        // destination path is free. That is the stranded-at-its-backup case
        // below (`CopyFailed { restored: false }`), not a fork left in place:
        // its meta goes beside the backup, and the slot is recorded `Failed`
        // so [`delta_sync`] fills it with the live file.
        Err(PreserveError::NotMovedAside {
            backup: Some(backup),
            error,
        }) if std::fs::metadata(&backup).is_ok_and(|m| m.len() > 0) || path_is_free(to) => {
            warn!(
                from = %from.display(),
                to = %to.display(),
                backup = %backup.display(),
                error = %error,
                "session transcript carry: the rename-aside of a diverged destination .jsonl reported an error but moved the fork — it is preserved at its backup, and the live file was not copied"
            );
            carry.failed += 1;
            preserve_sibling_meta(to, &backup);
            CarryWrite::Failed
        }
        Err(PreserveError::NotMovedAside { backup, error }) => {
            warn!(
                from = %from.display(),
                to = %to.display(),
                backup = ?backup,
                error = %error,
                "session transcript carry: destination .jsonl diverged from the live source and could not be moved aside — skipped, not overwritten"
            );
            carry.failed += 1;
            CarryWrite::Diverged
        }
        Err(PreserveError::CopyFailed {
            backup,
            restored,
            error,
        }) => {
            warn!(
                from = %from.display(),
                to = %to.display(),
                backup = %backup.display(),
                restored,
                error = %error,
                "session transcript carry: copy failed after the diverged destination .jsonl was moved aside"
            );
            carry.failed += 1;
            if restored {
                CarryWrite::Diverged
            } else {
                // The fork now lives at `backup`, away from its meta: keep
                // the meta with it, as on the success path.
                preserve_sibling_meta(to, &backup);
                CarryWrite::Failed
            }
        }
    }
}

/// A subagent transcript `<stem>.jsonl` at `jsonl_dst` was just moved aside to
/// `jsonl_backup` (`<stem>.jsonl.diverged-<stamp>`) — and stayed there, whether
/// the live copy then landed or not: keep its fork's sibling
/// `<stem>.meta.json` (the subagent's description and type) with it, as a copy
/// at `<stem>.meta.json.diverged-<stamp>` — the SAME suffix, so the pair reads
/// as one. Copied, not moved: the destination meta stays for the carry to
/// replace with the source's, or keep when identical, exactly as before. The
/// carry visits every `.jsonl` before any other file, so this runs before the
/// source's meta can overwrite the fork's.
///
/// Best-effort: the `.jsonl` is the part that matters, so a failure here is
/// logged and the carry goes on. Never replaces an existing file (the name is
/// claimed with `create_new`), and a copy cut short is removed rather than
/// left looking like the fork's meta.
fn preserve_sibling_meta(jsonl_dst: &Path, jsonl_backup: &Path) {
    let (Some(dst_name), Some(suffix)) = (
        jsonl_dst.file_name().and_then(|n| n.to_str()),
        diverged_suffix(jsonl_dst, jsonl_backup),
    ) else {
        return;
    };
    let Some(stem) = dst_name.strip_suffix(".jsonl") else {
        return;
    };
    let meta = jsonl_dst.with_file_name(format!("{stem}.meta.json"));
    if !std::fs::symlink_metadata(&meta).is_ok_and(|m| m.is_file()) {
        return;
    }
    let meta_backup = jsonl_dst.with_file_name(format!("{stem}.meta.json{suffix}"));
    if let Err(e) = copy_to_new_file(&meta, &meta_backup) {
        warn!(
            meta = %meta.display(),
            backup = %meta_backup.display(),
            transcript_backup = %jsonl_backup.display(),
            error = %e,
            "session transcript carry: the diverged subagent's .meta.json could not be kept beside its moved-aside transcript — the transcript itself is preserved"
        );
    }
}

/// The `.diverged-<stamp>` part of `backup`, the name `dst` was moved aside to
/// (`<dst name>.diverged-<stamp>`).
fn diverged_suffix<'a>(dst: &Path, backup: &'a Path) -> Option<&'a str> {
    let dst_name = dst.file_name()?.to_str()?;
    backup
        .file_name()?
        .to_str()?
        .strip_prefix(dst_name)
        .filter(|suffix| suffix.starts_with(DIVERGED_MARKER))
}

/// Copy `from` to `to`, which must not exist yet (`create_new`, so an earlier
/// file is never replaced). A copy cut short is removed rather than left
/// looking complete.
fn copy_to_new_file(from: &Path, to: &Path) -> std::io::Result<u64> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)
        .and_then(|mut out| {
            let copied =
                std::fs::File::open(from).and_then(|mut inp| std::io::copy(&mut inp, &mut out));
            if copied.is_err() {
                drop(out);
                let _ = std::fs::remove_file(to);
            }
            copied
        })
}

/// The `<sid>/` file holding the session's custom title, rewritten in place.
const CUSTOM_TITLE: &str = "custom-title.json";

/// A migration moved the destination's main transcript aside (`main_dst` ->
/// `main_backup`) and the fork stays there — [`MainCarry::DivergedPreserved`],
/// or a [`preserve_diverged_main`] error arm that left it at its backup: keep
/// the fork's `<sid>/custom-title.json` as `custom-title.json.diverged-<stamp>`
/// in the destination's `<sid>/` — the SAME suffix as the transcript's backup,
/// so the two read as one fork.
///
/// - The source has a `custom-title.json` whose bytes are not known to be
///   identical to the destination's: back the fork's up before the carry (now,
///   or on a retry) copies the live one over it.
/// - The source has NONE (a positive "not found"): no carry would ever replace
///   the fork's title, so the live conversation would inherit it. Back it up,
///   then REMOVE it from the destination. When the backup fails it is left in
///   place (warned) — never removed without a copy.
///
/// Best-effort: a failure is logged and the carry goes on. Never replaces an
/// existing file (`create_new`). The walk never carries the backup on
/// (`*.diverged-*`, [`walk_dir`]).
fn preserve_fork_custom_title(
    src_session_dir: &Path,
    dst_session_dir: &Path,
    main_dst: &Path,
    main_backup: &Path,
) {
    let Some(suffix) = diverged_suffix(main_dst, main_backup) else {
        return;
    };
    let (src_title, dst_title) = (
        src_session_dir.join(CUSTOM_TITLE),
        dst_session_dir.join(CUSTOM_TITLE),
    );
    let is_plain_file = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
    if !is_plain_file(&dst_title) {
        return;
    }
    let live_has_none = path_is_free(&src_title);
    if !live_has_none {
        if !is_plain_file(&src_title) {
            return;
        }
        if matches!(
            (std::fs::read(&src_title), std::fs::read(&dst_title)),
            (Ok(a), Ok(b)) if a == b
        ) {
            return;
        }
    }
    let title_backup = dst_session_dir.join(format!("{CUSTOM_TITLE}{suffix}"));
    match copy_to_new_file(&dst_title, &title_backup) {
        Ok(_) if live_has_none => {
            if let Err(e) = std::fs::remove_file(&dst_title) {
                warn!(
                    title = %dst_title.display(),
                    backup = %title_backup.display(),
                    error = %e,
                    "session transcript carry: the diverged session's custom-title.json was kept beside its moved-aside transcript but could not be removed — the live conversation, which has no title, keeps the fork's"
                );
            }
        }
        Ok(_) => {}
        Err(e) => warn!(
            title = %dst_title.display(),
            backup = %title_backup.display(),
            transcript_backup = %main_backup.display(),
            error = %e,
            live_has_title = !live_has_none,
            "session transcript carry: the diverged session's custom-title.json could not be kept beside its moved-aside transcript — left in place when the live session has no title, else the live title replaces it"
        ),
    }
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
///
/// `live_source_wins` (a migration) judges a `.jsonl` exactly like the main
/// transcript instead ([`live_jsonl_verdict`]): byte-identical is current, a
/// strict byte-prefix is replaced, and anything else — larger, or diverged —
/// is [`Verdict::Diverged`], which the migration moves aside.
///
/// Only a stat of `to` that fails with "not found" is an absent destination
/// ([`Verdict::Write`]); any other stat error is returned, so the caller
/// records the file [`CarryWrite::Failed`] and writes nothing over it.
///
/// `reads` are [`std::fs::metadata`] and [`prefix_relation`] in production.
fn destination_verdict(
    from: &Path,
    to: &Path,
    from_len: u64,
    source_is_newer: bool,
    live_source_wins: bool,
    reads: &mut DestinationReads<'_>,
) -> std::io::Result<Verdict> {
    let to_meta = match (reads.stat)(to) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Verdict::Write),
        Err(e) => return Err(e),
    };
    if is_jsonl(from) {
        if live_source_wins {
            return Ok(live_verdict((reads.compare)(to, from)?));
        }
        return Ok(if to_meta.len() >= from_len {
            Verdict::Current
        } else if (reads.compare)(to, from)? != PrefixRelation::NotPrefix {
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

/// How the bytes of one file relate to another's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrefixRelation {
    /// Same length, same bytes.
    Identical,
    /// Shorter, and every byte matches the other's start.
    StrictPrefix,
    /// Longer, or some byte differs.
    NotPrefix,
}

/// How the bytes of `prefix` relate to those of `of`. Both lengths come from
/// the two open handles, read once just before the comparison, so they
/// describe the same moment as the bytes compared (as nearly as an unlocked
/// read can). Compared in chunks, so a large transcript is never read into
/// memory whole; never a size or mtime shortcut for "identical".
fn prefix_relation(prefix: &Path, of: &Path) -> std::io::Result<PrefixRelation> {
    use std::io::Read;
    const CHUNK: usize = 64 * 1024;
    let mut p = std::fs::File::open(prefix)?;
    let mut o = std::fs::File::open(of)?;
    let (p_len, o_len) = (p.metadata()?.len(), o.metadata()?.len());
    if p_len > o_len {
        return Ok(PrefixRelation::NotPrefix);
    }
    let (mut a, mut b) = (vec![0u8; CHUNK], vec![0u8; CHUNK]);
    let mut remaining = p_len;
    while remaining > 0 {
        let n = usize::try_from(remaining).map_or(CHUNK, |r| r.min(CHUNK));
        p.read_exact(&mut a[..n])?;
        o.read_exact(&mut b[..n])?;
        if a[..n] != b[..n] {
            return Ok(PrefixRelation::NotPrefix);
        }
        remaining -= n as u64;
    }
    Ok(if p_len == o_len {
        PrefixRelation::Identical
    } else {
        PrefixRelation::StrictPrefix
    })
}

/// Whether the bytes of `prefix` are a prefix of the bytes of `of` (equal
/// files included; `false` when `prefix` is the longer).
fn is_byte_prefix(prefix: &Path, of: &Path) -> std::io::Result<bool> {
    Ok(prefix_relation(prefix, of)? != PrefixRelation::NotPrefix)
}

/// [`MainTranscript::LiveSourceWins`] for one existing `.jsonl` destination,
/// classified prefix-first: byte-identical to the source is current, a strict
/// byte-prefix is extended, anything else is a fork to move aside. A source
/// that grows between an earlier stat and this read is therefore seen as what
/// it is — the destination a prefix of it — never as a same-size fork.
fn live_jsonl_verdict(dst: &Path, src: &Path) -> std::io::Result<Verdict> {
    Ok(live_verdict(prefix_relation(dst, src)?))
}

/// [`live_jsonl_verdict`] for a relation already read.
fn live_verdict(relation: PrefixRelation) -> Verdict {
    match relation {
        PrefixRelation::Identical => Verdict::Current,
        PrefixRelation::StrictPrefix => Verdict::Write,
        PrefixRelation::NotPrefix => Verdict::Diverged,
    }
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
    /// Not followed: a symlink or junction, a leftover `*.partial`, an earlier
    /// `*.diverged-*` backup, or a directory past [`SESSION_DIR_MAX_DEPTH`].
    /// Logged where it is decided.
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
        // A leftover `*.partial`, or a `*.diverged-*` backup an earlier
        // migration made in this account: the backup belongs to the account
        // that forked, and carrying it on would copy it into every later one.
        let is_leftover = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.ends_with(PARTIAL_SUFFIX) || name.contains(DIVERGED_MARKER));
        if !meta.is_file() || is_leftover {
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
/// typed into the resumed session's prompt — and so is every file whose path
/// relative to `<sid>/` is in `not_carried` (a destination the carry knows
/// does not hold the source's transcript).
fn carried_subagents(
    src_sidecar: &Path,
    dst_sidecar: &Path,
    not_carried: &HashSet<&Path>,
) -> Vec<CarriedSubagent> {
    try_carried_subagents(src_sidecar, dst_sidecar, not_carried).unwrap_or_default()
}

/// [`carried_subagents`], with an unreadable source `subagents/` reported as
/// the error it is. A source with no `subagents/` has no subagents (`Ok`,
/// empty).
fn try_carried_subagents(
    src_sidecar: &Path,
    dst_sidecar: &Path,
    not_carried: &HashSet<&Path>,
) -> std::io::Result<Vec<CarriedSubagent>> {
    let (src_sub, dst_sub) = (src_sidecar.join("subagents"), dst_sidecar.join("subagents"));
    let entries = match std::fs::read_dir(&src_sub) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
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
            let rel = Path::new("subagents").join(&name);
            if !plain
                || not_carried.contains(rel.as_path())
                || !is_file(&entry.path())
                || !is_file(&dst_sub.join(&name))
            {
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
    Ok(out)
}

/// The subagents a MIGRATION's destination can resume now — recomputed after
/// the delta sync reported (or was given up on), for the continue nudge.
/// [`carried_subagents`] over the two `<sid>/` dirs the carry recorded in
/// `since`, so a subagent the delta sync first copied is named, and one still
/// missing at the destination is not. Left out besides, by the carry's own
/// record:
/// - a file the carry left [`CarryWrite::Diverged`] (it could not be moved
///   aside) — the delta sync never overwrites one, so it is still the fork;
/// - a file whose carry write [`CarryWrite::Failed`] and whose destination is
///   still exactly as that failure left it — the sync did not replace it.
///
/// Written for [`MainTranscript::LiveSourceWins`], the only mode that nudges.
/// `Err` when the source's `subagents/` cannot be read; the caller then falls
/// back to the carry-time [`TranscriptCopy::subagents`].
pub(crate) fn resumable_subagents_now(
    since: &TranscriptCopy,
) -> std::io::Result<Vec<CarriedSubagent>> {
    let still_not_live: HashSet<&Path> = since
        .files
        .iter()
        .filter(|f| match f.wrote {
            CarryWrite::Diverged => true,
            // Unknown at the carry, or unreadable now: not shown to be replaced.
            CarryWrite::Failed => {
                f.dst_unknown
                    || match FileStamp::of(&since.dst_session_dir.join(&f.rel)) {
                        Ok(now) => now == f.dst,
                        Err(_) => true,
                    }
            }
            CarryWrite::Copied | CarryWrite::Present | CarryWrite::PresentBySize => false,
        })
        .map(|f| f.rel.as_path())
        .collect();
    try_carried_subagents(
        &since.src_session_dir,
        &since.dst_session_dir,
        &still_not_live,
    )
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
///   (`kept_destination_newer`), and neither is the `<stem>.meta.json` of any
///   `.jsonl` whose carry record fails [`meta_may_follow_source`], even one
///   whose source meta first appeared after the carry (also
///   `kept_destination_newer`), nor the meta of a `.jsonl` that is at the
///   destination with no carry record behind it (the carry's walk skipped or
///   failed its source) unless this sync itself just wrote that `.jsonl`;
/// - the destination must be EXACTLY as the carry left it (same size and
///   mtime; absent stays absent). Otherwise the resumed process — or someone —
///   has written it, and it is never touched (`refused_destination_changed`).
///   A destination the carry recorded as UNKNOWN
///   ([`CarriedFile::dst_unknown`]) cannot be shown unchanged and is refused
///   the same way; one whose stat fails now (anything but "not found") is
///   counted `failed` and not written;
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
    // The `<stem>.meta.json` of every `.jsonl` whose carry record fails
    // [`meta_may_follow_source`] — the same predicate the carry used: the
    // sync never writes one there, whether or not the carry saw a source meta
    // (one that first appears after the carry has no baseline record to say
    // so).
    let fork_metas: HashSet<PathBuf> = since.files.iter().filter_map(protected_meta_rel).collect();
    let recorded_jsonl: HashSet<PathBuf> = since
        .files
        .iter()
        .filter(|f| is_jsonl(&f.rel))
        .map(|f| f.rel.clone())
        .collect();
    let mut walked: Vec<WalkedFile> = Vec::new();
    walk_session_dir(src_dir, &mut |event| match event {
        WalkEvent::Skipped => sync.skipped_other += 1,
        WalkEvent::Failed => sync.failed += 1,
        WalkEvent::File { from, rel, src } => {
            walked.push((from.to_path_buf(), rel.to_path_buf(), src));
        }
    });
    // Transcripts first, as in the carry, so a meta's turn knows whether THIS
    // sync just wrote its `.jsonl`.
    walked.sort_by_key(|(_, rel, _)| !is_jsonl(rel));
    // `.jsonl`s this sync wrote: the destination holds the source's bytes, so
    // their metas may follow even with no carry record behind them.
    let mut synced_jsonl: HashSet<PathBuf> = HashSet::new();
    for (from, rel, src) in walked {
        let (from, rel) = (from.as_path(), rel.as_path());
        let to = dst_dir.join(rel);
        let dst_now = match FileStamp::of(&to) {
            Ok(stamp) => stamp,
            Err(e) => {
                warn!(
                    to = %to.display(),
                    error = %e,
                    "session delta sync: could not stat the destination — not read as absent, not written"
                );
                sync.failed += 1;
                continue;
            }
        };
        let recorded = baseline.get(rel).copied();
        // A carry that could not stat what it left cannot vouch that the
        // destination is still exactly that.
        if recorded.is_some_and(|r| r.dst_unknown) || dst_now != recorded.and_then(|r| r.dst) {
            sync.refused_destination_changed += 1;
            continue;
        }
        // The carry kept the destination's copy as the newer one; the
        // older side rewriting its own copy since does not change that. And a
        // meta whose `.jsonl` is at the destination with no record behind it
        // — not the carry's, not this sync's — stays with that transcript.
        if recorded.is_some_and(|r| r.kept_destination_newer)
            || fork_metas.contains(rel)
            || (meta_of_unrecorded_jsonl(rel, dst_dir, &recorded_jsonl)
                && !sibling_jsonl_rel(rel).is_some_and(|j| synced_jsonl.contains(&j)))
        {
            sync.kept_destination_newer += 1;
            continue;
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
            continue;
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
                    continue;
                }
                Err(e) => {
                    warn!(
                        from = %from.display(),
                        to = %to.display(),
                        error = %e,
                        "session delta sync: could not compare with the destination"
                    );
                    sync.failed += 1;
                    continue;
                }
            }
        }
        match copy_if_destination_is(from, &to, dst_now) {
            Ok(true) => {
                sync.copied += 1;
                if is_jsonl(rel) {
                    synced_jsonl.insert(rel.to_path_buf());
                }
            }
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
        match FileStamp::of(to) {
            Ok(now) if now == expected => {}
            Ok(_) => break Ok(false),
            // Unknown is not "unchanged": never rename over it.
            Err(e) => break Err(e),
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
///
/// Only looks that COMPLETE by the deadline count: no look starts after it,
/// the pause between looks is cut short so the last one lands on it, and a
/// look that returns after it is discarded (`false`) whatever it saw. So a
/// `true` always means the writer was proven gone by `deadline`, and the
/// caller's follow-up work starts no later than that — which is what lets the
/// continue nudge bound its own wait for the sync
/// (`account_migration::NUDGE_SYNC_WAIT`).
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
        if started.elapsed() > deadline {
            return false;
        }
        let look = probe().await;
        if started.elapsed() > deadline {
            return false;
        }
        if look == WriterProbe::Gone {
            gone_streak += 1;
            if gone_streak >= crate::terminal::graceful_exit::GONE_PROBES_REQUIRED {
                return true;
            }
        } else {
            gone_streak = 0;
        }
        let remaining = deadline.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return false;
        }
        tokio::time::sleep(poll.min(remaining)).await;
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
