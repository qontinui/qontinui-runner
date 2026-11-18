//! Which account root holds the NEWEST copy of a session's transcript: the
//! guard in front of the account migration's carry ([`super::copy_transcript`]).
//!
//! A migration takes its source account from the session's record, and a
//! record can name the wrong one. On 2026-11-15 a boot reconcile rebound a
//! migrated session to the first root, in roster order, that held its id. The
//! next migration then carried that root's copy, two days stale, and the live
//! copy in another root was never carried. The carry never deletes its source,
//! so after any migration several roots hold `<sid>.jsonl`. This module picks
//! the copy the conversation actually continued in, whatever caused the wrong
//! source. Plan
//! `2026-11-16-boot-reconcile-reverts-a-migrated-sessions-config-dir-so-the-next-migration-carries-a-stale-transcript`,
//! D3.
//!
//! ## How the copies are ranked
//!
//! - **Byte prefix first.** A transcript is append-only, so a copy whose bytes
//!   are a strict prefix of another copy's is older ([`super::prefix_relation`]).
//!   Byte-identical copies count as one copy.
//! - **Then the newest TURN**, for copies that diverged (each holds a tail the
//!   other lacks). That is the `timestamp` of the last real `user`/`assistant`
//!   line ([`newest_turn_ms`]), the same answer
//!   [`crate::session::transcript_last_turn`] gives
//!   `agent_worktree::custody::SessionDirectory` for the same question. Not the
//!   last record of any type: a superseded copy whose tail gained a
//!   `queue-operation`, an `attachment` or an `isMeta` line would then outrank
//!   the copy holding the newest turn. The strictly later turn wins.
//! - **Never the mtime.** A carry gives its copy a fresh mtime while the copy
//!   holds only old records. The transcript tailer's mtime rule
//!   (`session_transcript_tailer::locate_session_jsonl`) bound exactly such a
//!   stale copy in the incident this guards against.
//! - **UNKNOWN never wins.** The full ranking fails when a copy cannot be
//!   stat'ed, opened or compared, when a diverged copy has no turn in reach or
//!   cannot be read, or when two diverged copies tie. The verdict then falls
//!   back to what the byte prefix order alone proves among the copies that
//!   could be read ([`prefix_fallback`]): a copy PROVEN to strictly extend the
//!   source is newer than it whatever else is unknown, so the greatest such
//!   copy is carried (or the destination kept, when it is that copy), with
//!   `unranked` naming why the full ranking failed. Only when no readable copy
//!   is proven to extend the source is the verdict [`Newest::Unknown`], and the
//!   carry does what it did before this guard: it reads the migration's source.
//!
//! The destination is ranked too. A migration moves a diverged destination
//! aside and copies the source over it, so a destination that already holds
//! the newest copy would be buried at a `.diverged-*` backup.
//! [`keep_destination`] keeps it in place instead.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::warn;

use super::{
    carried_subagents, is_jsonl, prefix_relation, walk_session_dir, CarriedFile, CarryWrite,
    FileStamp, MainCarry, PrefixRelation, SessionDirCarry, TranscriptCopy, WalkEvent,
};

/// Where the newest copy of a session's transcript is, relative to the
/// migration's two roots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Newest {
    /// The migration's source holds it, alone or byte-identical with other
    /// copies. The carry runs exactly as it did before this guard.
    Source,
    /// The destination holds it and the source does not. Carrying the source
    /// over it would bury the newest copy at a `.diverged-*` backup.
    /// `unranked` is `Some(reason)` when the full ranking failed for that
    /// reason and the byte prefix order alone chose the destination
    /// ([`prefix_fallback`]).
    Destination { unranked: Option<String> },
    /// Another root holds it, and neither the source nor the destination
    /// holds the same bytes. `unranked` as for [`Self::Destination`].
    Other {
        root: PathBuf,
        unranked: Option<String>,
    },
    /// The ranking could not be established, for this reason.
    Unknown(String),
}

/// Which of the migration's roots a copy was found under. The source and the
/// destination are listed first, so a root that appears again in the roster
/// keeps its role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Source,
    Destination,
    Other,
}

/// One root's copy of `<sid>.jsonl`.
#[derive(Debug)]
struct RootCopy {
    root: PathBuf,
    path: PathBuf,
    role: Role,
}

/// How one copy's bytes relate to another's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Order {
    /// Byte-identical.
    Same,
    /// A strict prefix of the other copy: older.
    Older,
    /// The other copy is a strict prefix of this one: newer.
    Newer,
    /// Each holds a tail the other lacks.
    Diverged,
}

impl Order {
    /// How the other copy relates to this one.
    fn reverse(self) -> Order {
        match self {
            Order::Same => Order::Same,
            Order::Older => Order::Newer,
            Order::Newer => Order::Older,
            Order::Diverged => Order::Diverged,
        }
    }
}

/// Rank every copy of `claude_session_id`'s transcript across `src_root`,
/// `dst_root` and `roots`. `roots` is every account root on the machine; the
/// source and the destination are ranked whether or not they are listed in
/// it. A copy is `session_transcript_path(root, working_dir, sid)`, the
/// enumeration `DiskTranscriptIndex::transcript_config_dirs` uses. A root
/// listed twice, or two roots that resolve to the same file, are one copy.
///
/// When any copy cannot be ranked, the verdict falls back to what the byte
/// prefix order alone proves among the copies that could be read
/// ([`prefix_fallback`]), and `unranked` carries why the full ranking failed.
pub(super) fn rank(
    src_root: &Path,
    dst_root: &Path,
    roots: &[PathBuf],
    working_dir: &str,
    claude_session_id: &str,
) -> Newest {
    let (copies, mut failures) =
        copies_of(src_root, dst_root, roots, working_dir, claude_session_id);
    let classes = classify(&copies);
    failures.extend(classes.failures.iter().cloned());
    let ranked = if failures.is_empty() {
        newest_head(&copies, &classes)
    } else {
        Err(failures.join("; "))
    };
    match ranked {
        Ok(class) => verdict_for(&copies, &classes, class, None),
        Err(reason) => match prefix_fallback(&copies, &classes) {
            Some(class) => verdict_for(&copies, &classes, class, Some(reason)),
            None => Newest::Unknown(reason),
        },
    }
}

/// The verdict naming `class`'s holder: the source when it is in the class,
/// else the destination, else the first other root in listing order.
fn verdict_for(
    copies: &[RootCopy],
    classes: &Classes,
    class: usize,
    unranked: Option<String>,
) -> Newest {
    let members: Vec<&RootCopy> = copies
        .iter()
        .zip(&classes.class_of)
        .filter(|&(_, &c)| c == class)
        .map(|(copy, _)| copy)
        .collect();
    let held_by = |role: Role| members.iter().find(|copy| copy.role == role);
    if held_by(Role::Source).is_some() {
        Newest::Source
    } else if held_by(Role::Destination).is_some() {
        Newest::Destination { unranked }
    } else if let Some(copy) = held_by(Role::Other) {
        Newest::Other {
            root: copy.root.clone(),
            unranked,
        }
    } else {
        Newest::Unknown("no copy ranked newest".to_string())
    }
}

/// Every root's copy of the transcript, the source's first and the
/// destination's second when they exist, plus why each copy that could not
/// be ranked was left out. A root whose copy is absent is left out silently;
/// one whose path cannot be stat'ed, or is not a regular file, is left out
/// with a reason. The same file reached twice (a root listed again, or
/// spelled another way) is kept once, under the first role it was listed
/// with. Comparing a live file with itself is not free and not even
/// reliable: a write between the two length reads makes it a strict prefix
/// of itself.
fn copies_of(
    src_root: &Path,
    dst_root: &Path,
    roots: &[PathBuf],
    working_dir: &str,
    claude_session_id: &str,
) -> (Vec<RootCopy>, Vec<String>) {
    let listed = [(src_root, Role::Source), (dst_root, Role::Destination)]
        .into_iter()
        .chain(roots.iter().map(|root| (root.as_path(), Role::Other)));
    let mut copies: Vec<RootCopy> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for (root, role) in listed {
        let path = crate::terminal::transcript::session_transcript_path(
            root,
            working_dir,
            claude_session_id,
        );
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_file() => {}
            Ok(_) => {
                failures.push(format!("{} is not a regular file", path.display()));
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                failures.push(format!("stat {} failed: {e}", path.display()));
                continue;
            }
        }
        if !seen.insert(std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone())) {
            continue;
        }
        copies.push(RootCopy {
            root: root.to_path_buf(),
            path,
            role,
        });
    }
    (copies, failures)
}

/// The copies grouped into byte-identical classes, every class compared with
/// every other class once.
#[derive(Debug)]
struct Classes {
    /// The class of each copy, as an index into the classes. Classes are
    /// numbered in the listing order of their first copy.
    class_of: Vec<usize>,
    /// `rel[a][b]`: how class `a`'s bytes relate to class `b`'s. `None` on
    /// the diagonal and where the compare failed.
    rel: Vec<Vec<Option<Order>>>,
    /// Why each failed compare between two classes failed.
    failures: Vec<String>,
}

impl Classes {
    fn count(&self) -> usize {
        self.rel.len()
    }

    /// `a` is PROVEN to be a strict byte prefix of `b`.
    fn older(&self, a: usize, b: usize) -> bool {
        self.rel[a][b] == Some(Order::Older)
    }
}

/// Group `copies` into byte-identical classes. Each copy is compared only
/// with the first copy of each class found so far, and stops at the first it
/// is identical to: it then relates to every other class exactly as that one
/// does, which was already measured. So a pair already known to share a class
/// is never compared, and each pair of classes is compared once.
fn classify(copies: &[RootCopy]) -> Classes {
    let mut heads: Vec<usize> = Vec::new();
    let mut class_of: Vec<usize> = Vec::with_capacity(copies.len());
    let mut rel: Vec<Vec<Option<Order>>> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for copy in copies {
        let mut found: Vec<Option<Order>> = Vec::with_capacity(heads.len() + 1);
        let mut failed: Vec<String> = Vec::new();
        let mut same = None;
        for (class, &head) in heads.iter().enumerate() {
            let other = &copies[head].path;
            match order(&copy.path, other) {
                Ok(Order::Same) => {
                    same = Some(class);
                    break;
                }
                Ok(o) => found.push(Some(o)),
                Err(e) => {
                    failed.push(format!(
                        "compare {} with {} failed: {e}",
                        copy.path.display(),
                        other.display()
                    ));
                    found.push(None);
                }
            }
        }
        if let Some(class) = same {
            class_of.push(class);
            continue;
        }
        // A new class: `found` holds its relation to every earlier one.
        for (row, o) in rel.iter_mut().zip(&found) {
            row.push(o.map(Order::reverse));
        }
        found.push(None);
        rel.push(found);
        class_of.push(heads.len());
        heads.push(class_of.len() - 1);
        failures.extend(failed);
    }
    Classes {
        class_of,
        rel,
        failures,
    }
}

/// The class holding the newest copy, when every copy could be compared.
/// `Err` with the reason whenever that cannot be established.
fn newest_head(copies: &[RootCopy], classes: &Classes) -> Result<usize, String> {
    let n = classes.count();
    if n == 0 {
        return Err("no root holds a copy of the transcript".to_string());
    }
    // The classes no other class strictly extends.
    let tops: Vec<usize> = (0..n)
        .filter(|&a| !(0..n).any(|b| classes.older(a, b)))
        .collect();
    match tops.as_slice() {
        [] => Err("every copy is a prefix of another (the files changed while read)".into()),
        [only] => Ok(*only),
        // Diverged copies: the strictly latest turn wins.
        _ => {
            let mut stamped: Vec<(i64, usize)> = Vec::with_capacity(tops.len());
            for &class in &tops {
                let first = classes
                    .class_of
                    .iter()
                    .position(|&c| c == class)
                    .ok_or("a class has no copy")?;
                stamped.push((newest_turn_ms(&copies[first].path)?, class));
            }
            // Latest first.
            stamped.sort_unstable_by_key(|&(ms, _)| std::cmp::Reverse(ms));
            match stamped.as_slice() {
                [(first, _), (second, _), ..] if first == second => Err(format!(
                    "two diverged copies hold their newest turn at the same instant ({first} ms)"
                )),
                [(_, class), ..] => Ok(*class),
                [] => Err("no diverged copy was stamped".into()),
            }
        }
    }
}

/// What the byte prefix order ALONE proves, for when the full ranking failed
/// (a copy that cannot be stat'ed, read or compared, a diverged copy with no
/// turn in reach, a tie). The copies PROVEN to strictly extend the source's
/// are newer than it, whatever else is unknown:
///
/// - the one that every other such copy is proven a prefix of (or identical
///   to) wins;
/// - failing that, the destination wins when it is one of them, since it is
///   then kept in place rather than buried under an older copy;
/// - otherwise `None`: UNKNOWN, and the carry reads the source.
fn prefix_fallback(copies: &[RootCopy], classes: &Classes) -> Option<usize> {
    let class_with = |role: Role| {
        copies
            .iter()
            .position(|copy| copy.role == role)
            .map(|i| classes.class_of[i])
    };
    let source = class_with(Role::Source)?;
    let extensions: Vec<usize> = (0..classes.count())
        .filter(|&c| classes.older(source, c))
        .collect();
    let greatest = extensions
        .iter()
        .copied()
        .find(|&g| extensions.iter().all(|&e| e == g || classes.older(e, g)));
    greatest.or_else(|| class_with(Role::Destination).filter(|d| extensions.contains(d)))
}

/// How the bytes of `a` relate to those of `b`. Each direction is one
/// [`prefix_relation`] read. The first returns at once without reading any
/// bytes when `a` is the longer, so a pair costs one full compare.
fn order(a: &Path, b: &Path) -> std::io::Result<Order> {
    Ok(match prefix_relation(a, b)? {
        PrefixRelation::Identical => Order::Same,
        PrefixRelation::StrictPrefix => Order::Older,
        PrefixRelation::NotPrefix => match prefix_relation(b, a)? {
            // `a` shrank or `b` grew between the two reads. Neither happens to
            // an append-only transcript at rest; a live one can only grow.
            PrefixRelation::Identical => Order::Same,
            PrefixRelation::StrictPrefix => Order::Newer,
            PrefixRelation::NotPrefix => Order::Diverged,
        },
    })
}

/// The newest turn (unix ms) in the diverged copy at `path`: the timestamp of
/// its last real `user`/`assistant` line, from
/// [`crate::session::transcript_last_turn::last_turn_ms`] with an unbounded
/// read budget (the copies of one session are few). That reads the tail only,
/// skips `isMeta` and non-turn records, and caches per `(len, mtime)`; the
/// mtime is part of the cache key there, never an ordering. `Err` with the
/// reason when the copy has no turn in reach or cannot be read: UNKNOWN, never
/// a loser and never a winner.
fn newest_turn_ms(path: &Path) -> Result<i64, String> {
    use crate::session::transcript_last_turn::{
        last_turn_ms, FileStamp as TurnStamp, ReadBudget, TurnRead,
    };
    let stamp = std::fs::metadata(path)
        .ok()
        .map(|meta| TurnStamp::of(&meta));
    match last_turn_ms(path, stamp, &mut ReadBudget::Unbounded) {
        TurnRead::Read(Some(ms)) => Ok(ms),
        TurnRead::Read(None) => Err(format!(
            "{} diverged from another copy and has no turn in reach",
            path.display()
        )),
        TurnRead::Deferred | TurnRead::Unreadable => Err(format!(
            "{} diverged from another copy and could not be read",
            path.display()
        )),
    }
}

/// The carry's answer when the destination already holds the newest copy
/// ([`Newest::Destination`]). It writes nothing. The destination's
/// `<sid>.jsonl` and `<sid>/` stay exactly as they are, and the returned
/// [`TranscriptCopy`] describes them as carried from the destination itself:
///
/// - [`MainCarry::Present`], and [`TranscriptCopy::carried_from`] is
///   `dst_config_dir`;
/// - [`TranscriptCopy::src_session_dir`] is the destination's own `<sid>/`, so
///   a later [`super::delta_sync`] compares the destination with itself. Every
///   file is recorded as the destination holds it now, and an in-place file
///   (not a `.jsonl`) as [`CarriedFile::kept_destination_newer`], because the
///   destination ran the session last. The sync then counts a `.jsonl` still
///   unchanged as `unchanged`, an in-place file as `kept_destination_newer`,
///   and a file written since as `refused_destination_changed`. It copies
///   nothing;
/// - [`TranscriptCopy::subagents`] lists the destination's own subagents.
///
/// The destination holding the newest copy usually means the session already
/// runs there: the old pane may still be appending to this same file, and the
/// migration then resumes a second `claude` on it. That is the migration's to
/// settle. `carried_from` equal to the destination records the case, but no
/// caller reads it yet (the account migration does not); acting on it is a
/// follow-up.
///
/// This is not a carry from the destination to itself. That carry is not
/// write-free: a destination a live writer extends between the two length
/// reads of [`prefix_relation`] reads as a strict prefix of itself. It would
/// then be copied over itself and renamed into place, and the writer's later
/// appends would go to the unlinked file.
pub(super) fn keep_destination(
    dst_config_dir: &str,
    working_dir: &str,
    claude_session_id: &str,
) -> Result<TranscriptCopy, String> {
    let path = crate::terminal::transcript::session_transcript_path(
        Path::new(dst_config_dir),
        working_dir,
        claude_session_id,
    );
    match FileStamp::of(&path) {
        Ok(Some(_)) => {}
        Ok(None) => {
            return Err(format!(
                "the destination's newest copy {} is gone — nothing carried",
                path.display()
            ))
        }
        Err(e) => {
            return Err(format!(
                "stat {} failed: {e} — not read as absent, nothing carried",
                path.display()
            ))
        }
    }
    let session_dir_path = path.with_file_name(claude_session_id);
    let mut session_dir = SessionDirCarry::default();
    let mut files: Vec<CarriedFile> = Vec::new();
    match std::fs::symlink_metadata(&session_dir_path) {
        Ok(meta) if meta.is_dir() => {
            walk_session_dir(&session_dir_path, &mut |event| match event {
                WalkEvent::Skipped => session_dir.skipped_other += 1,
                WalkEvent::Failed => session_dir.failed += 1,
                WalkEvent::File { rel, src, .. } => {
                    session_dir.skipped_present += 1;
                    files.push(CarriedFile {
                        rel: rel.to_path_buf(),
                        src,
                        dst: Some(src),
                        dst_unknown: false,
                        wrote: CarryWrite::Present,
                        kept_destination_newer: !is_jsonl(rel),
                    });
                }
            })
        }
        // Not followed, as in the carry.
        Ok(_) => session_dir.skipped_other += 1,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            warn!(dir = %session_dir_path.display(), error = %e, "session transcript carry: the destination's <sid>/ is unreadable");
            session_dir.failed += 1;
        }
    }
    let subagents = carried_subagents(&session_dir_path, &session_dir_path, &HashSet::new());
    Ok(TranscriptCopy {
        path,
        main: MainCarry::Present,
        src_session_dir: session_dir_path.clone(),
        dst_session_dir: session_dir_path,
        session_dir,
        files,
        subagents,
        carried_from: PathBuf::from(dst_config_dir),
    })
}

#[cfg(test)]
mod tests;
