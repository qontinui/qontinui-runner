//! `/sessions/<id>/snapshots` and `/sessions/<id>/rewind` HTTP endpoints
//! — Phase 4 of the productivity stack. Backs the `/rewind-session` slash
//! command (see `D:/qontinui-root/.claude/commands/rewind-session.md`).
//!
//! `GET /sessions/<id>/snapshots` lists every `session_file_snapshots`
//! row for the given session — exposed so an external caller can inspect
//! the rollback set before triggering a restore.
//!
//! `GET /sessions/<id>/file-changes` pairs every file the session touched
//! with its pre-edit snapshot text and the file's CURRENT text, so a reader
//! (the Terminal grid's `WorkerSessionCell`) can render a true
//! snapshot-vs-now diff whatever tool made the edit. The diff itself is
//! computed client-side; this route only reads and reports, and it reports a
//! read failure per file rather than dropping the file. It is BOUNDED on three
//! axes — [`FILE_CHANGE_TEXT_CAP_BYTES`] per side (structurally, through a
//! `take`-bounded read rather than a stat that is stale by the time it is
//! used), [`FILE_CHANGE_MAX_FILES`] paths per report, and
//! [`FILE_CHANGE_TOTAL_TEXT_BUDGET_BYTES`] across the whole response — so an
//! unauthenticated loopback caller cannot make the runner allocate a worker's
//! whole touched set. The first two bound each file and the file count; the
//! third bounds their PRODUCT, which is the number that actually reaches the
//! allocator.
//!
//! `POST /sessions/<id>/rewind` performs the actual restore: for each
//! pre-edit snapshot, verify the on-disk blob's sha256 matches the
//! recorded `blob_sha256`, then copy the blob over the original
//! `file_path`. This avoids the slash command needing to orchestrate
//! `cp` calls inside the LLM tool-call context.

use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use tracing::{info, warn};

use crate::database::pg::session_file_snapshots::SnapshotRow;
use crate::mcp::types::ApiState;

/// Per-side byte cap for the text returned by `GET /sessions/<id>/file-changes`.
/// A side above the cap is reported by size only (`truncated: true`, text
/// `None`) — a diff of a truncated file would be a lie, so none is offered.
///
/// The cap is STRUCTURAL, not advisory: [`FileProbe`] offers no unbounded read
/// at all, so a side is read through `File::take(cap + 1)` and a result of
/// `cap + 1` bytes IS the over-cap verdict. A stat-then-read would have decided
/// on a length that is stale the moment it is returned — a worker actively
/// appending to a generated artifact or a log between the two syscalls gets the
/// whole thing read in — and the runner is a tier-0 process whose loss destroys
/// every live session on the box, over a route reachable by anything on
/// loopback (the `:9876` router has permissive CORS and no auth layer). So a
/// worker that touched a multi-GB generated artifact must not be able to make
/// it allocate one, whatever that file is doing while we look at it.
pub const FILE_CHANGE_TEXT_CAP_BYTES: usize = 256 * 1024;

/// Aggregate byte budget for ALL the text one `GET /sessions/<id>/file-changes`
/// response holds.
///
/// [`FILE_CHANGE_TEXT_CAP_BYTES`] bounds one side and [`FILE_CHANGE_MAX_FILES`]
/// bounds the count, but until this budget existed nothing bounded their
/// PRODUCT: 400 files × 2 sides × 256 KiB is ~200 MiB resident, which `Json(…)`
/// then serialises into a second buffer of comparable size — ~400 MiB peak for
/// one request, with no concurrency limit in front of it. The realistic case is
/// worse than the adversarial one is rare: a worker that touched 400 files
/// averaging 50 KiB is ~80 MiB per request, and the page issues one per visible
/// cell per `commit-state-changed` burst.
///
/// Once the budget is spent, the remaining candidates are still REPORTED — with
/// sizes, digests, status and `truncated: true` — so the cut is visible rather
/// than silent. A `detail` naming the budget distinguishes it from a
/// genuinely over-cap file, which the UI renders instead of "too large to
/// diff" (`noDiffReason` in `workerFileChanges.ts`).
pub const FILE_CHANGE_TOTAL_TEXT_BUDGET_BYTES: usize = 4 * 1024 * 1024;

/// Maximum number of candidate paths one `GET /sessions/<id>/file-changes`
/// examines. A session that touched more has the remainder reported as
/// `omittedFiles` with `filesTruncated: true` rather than silently cut — and,
/// more importantly, the route's cost is bounded by this constant rather than
/// by how many files a worker happened to touch.
pub const FILE_CHANGE_MAX_FILES: usize = 400;

/// Buffer size for the streaming digest of an over-cap side.
const SHA_STREAM_CHUNK_BYTES: usize = 64 * 1024;

/// One file a session touched, paired with what it looked like BEFORE the
/// session's first edit and what it looks like NOW.
///
/// `status` is one of:
/// - `modified` — snapshot and current text differ;
/// - `unchanged` — same sha on both sides;
/// - `deleted` — a snapshot exists but the file is gone;
/// - `created` — the session touched a path that had no pre-edit snapshot
///   (the file did not exist when it was first edited) and exists now;
/// - `binary` — at least one side is not valid UTF-8;
/// - `unreadable` — a side could not be read; `detail` names why.
///
/// Both text sides are `None` whenever they cannot honestly be diffed
/// (`binary`, `unreadable`, or a side over [`FILE_CHANGE_TEXT_CAP_BYTES`]).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionFileChange {
    pub file_path: String,
    pub status: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub before_bytes: Option<usize>,
    pub after_bytes: Option<usize>,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub truncated: bool,
    /// `taken_at` of the pre-edit snapshot; `None` for a `created` entry.
    pub taken_at: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionFileChangesResponse {
    pub session_id: String,
    pub files: Vec<SessionFileChange>,
    /// True when the session touched more paths than [`FILE_CHANGE_MAX_FILES`]
    /// and the list below is therefore a prefix — the UI says so rather than
    /// presenting a cut list as the whole truth.
    pub files_truncated: bool,
    /// How many candidate paths were dropped by that cap (`0` when none were).
    pub omitted_files: usize,
    /// Epoch millis the report was assembled, so a reader can label its age.
    pub read_at_ms: i64,
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// sha256 AND byte length of a reader's remaining bytes, computed through a
/// fixed-size buffer so the contents are never held in memory.
///
/// The length comes from the same pass as the digest rather than from a
/// separate `metadata()` call, so the two describe the same bytes even if the
/// file is being written while we read it.
fn sha256_stream(mut reader: impl Read) -> std::io::Result<(String, u64)> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; SHA_STREAM_CHUNK_BYTES];
    let mut len: u64 = 0;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        len += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((format!("{:x}", hasher.finalize()), len))
}

/// Filesystem access for [`assemble_file_changes`], narrow enough that a test
/// can supply an in-memory tree.
///
/// **There is deliberately no unbounded read here.** The trait offers exactly
/// two operations, and neither can pull an arbitrary file into memory:
/// [`FileProbe::read_capped`] stops after `cap + 1` bytes, and
/// [`FileProbe::digest`] streams through a fixed buffer. An earlier shape had a
/// `size` stat plus an unbounded `read`, which made the cap a decision taken on
/// a length that the very next syscall could invalidate (TOCTOU); the bound is
/// now a property of the read itself.
pub trait FileProbe {
    /// Read at most `cap + 1` bytes of `path`.
    ///
    /// The extra byte is the verdict: a result of exactly `cap + 1` bytes means
    /// the file is OVER the cap and must not be diffed. Anything shorter is the
    /// whole file.
    fn read_capped(&self, path: &str, cap: usize) -> std::io::Result<Vec<u8>>;
    /// sha256 AND byte length of `path`, computed in ONE streaming pass so the
    /// contents are never held and the two agree with each other.
    fn digest(&self, path: &str) -> std::io::Result<(String, u64)>;
}

/// The real filesystem.
pub struct DiskFiles;

impl FileProbe for DiskFiles {
    fn read_capped(&self, path: &str, cap: usize) -> std::io::Result<Vec<u8>> {
        // Saturating, not `+ 1`: `FileProbe` is public and the tests already
        // hand it `usize::MAX`. A wrapping `cap + 1` there is a debug panic,
        // and in release a `take(0)` — which reports the file as EMPTY TEXT,
        // the one answer that is both wrong and confident.
        let limit = (cap as u64).saturating_add(1);
        let mut buf = Vec::new();
        std::fs::File::open(path)?
            .take(limit)
            .read_to_end(&mut buf)?;
        Ok(buf)
    }
    fn digest(&self, path: &str) -> std::io::Result<(String, u64)> {
        sha256_stream(std::fs::File::open(path)?)
    }
}

/// One side of a file change as observed on disk.
enum Side {
    Missing,
    Unreadable(String),
    /// At or below the cap, so the bytes are held and can be diffed.
    Text(Vec<u8>),
    /// Over the cap: size and digest only, never buffered.
    Oversize {
        bytes: usize,
        sha256: String,
        /// The cap this side's read was ACTUALLY handed — recorded at the read
        /// that refused it, so the attribution below cannot be moved by a
        /// concurrent writer. `bytes` comes from the second (digest) pass and
        /// is therefore a different observation of the same file: a file that
        /// SHRINKS between the two passes lands under the per-side cap and
        /// would otherwise be attributed to the report's budget when the
        /// budget was never touched.
        handed_cap: usize,
    },
}

impl Side {
    /// Bytes this side is holding resident. `0` for every variant that is not
    /// buffered text — which is the point of the other variants.
    fn buffered_len(&self) -> usize {
        match self {
            Side::Text(bytes) => bytes.len(),
            _ => 0,
        }
    }
}

/// Read one side, bounded at `cap` bytes.
///
/// The read itself carries the bound (`take(cap + 1)`), so nothing decided here
/// can be invalidated by a concurrent writer: a file that grows past `cap`
/// between two syscalls simply comes back as `cap + 1` bytes and is classified
/// [`Side::Oversize`]. That is the whole TOCTOU fix — there is no stat to race.
///
/// `cap` is the smaller of [`FILE_CHANGE_TEXT_CAP_BYTES`] and whatever is left
/// of the report's aggregate budget, so a `0` cap (budget spent) makes every
/// non-empty file oversize, which is exactly the "sizes and digests only"
/// behaviour that budget wants.
fn read_side(files: &dyn FileProbe, path: &str, cap: usize) -> Side {
    let bytes = match files.read_capped(path, cap) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Side::Missing,
        Err(e) => return Side::Unreadable(e.to_string()),
    };
    if bytes.len() <= cap {
        return Side::Text(bytes);
    }
    // Over the cap. Drop the probe bytes before the streaming pass so the two
    // are never resident together, then describe the file by size and digest.
    drop(bytes);
    match files.digest(path) {
        Ok((sha256, len)) => Side::Oversize {
            bytes: usize::try_from(len).unwrap_or(usize::MAX),
            sha256,
            handed_cap: cap,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Side::Missing,
        Err(e) => Side::Unreadable(e.to_string()),
    }
}

/// Running aggregate cap on the text one report holds
/// ([`FILE_CHANGE_TOTAL_TEXT_BUDGET_BYTES`]).
///
/// It works by SHRINKING the per-side cap handed to [`read_side`], so an
/// over-budget side is never read into memory in the first place — the budget
/// is enforced at the same place and in the same way as the per-side cap,
/// rather than by trimming an already-allocated list afterwards.
struct TextBudget {
    total: usize,
    remaining: usize,
}

impl TextBudget {
    fn new(total: usize) -> Self {
        Self {
            total,
            remaining: total,
        }
    }
    /// The cap for the next side: the per-side cap, or what is left of the
    /// aggregate budget, whichever is smaller.
    fn cap(&self) -> usize {
        FILE_CHANGE_TEXT_CAP_BYTES.min(self.remaining)
    }
    /// The cap an UNSPENT budget hands out. A side handed less than this was
    /// refused because EARLIER reads had spent the budget down — which is the
    /// exact claim the truncation `detail` makes, and the only reading under
    /// which it is true for a total budget smaller than the per-side cap (a
    /// configuration the route never uses, but `assemble_file_changes` allows
    /// and the tests exercise).
    fn unspent_cap(&self) -> usize {
        FILE_CHANGE_TEXT_CAP_BYTES.min(self.total)
    }
    fn spend(&mut self, n: usize) {
        self.remaining = self.remaining.saturating_sub(n);
    }
    /// Give back bytes that were read but NOT kept in the response (a side
    /// dropped as binary, or discarded because its partner was oversize). They
    /// were resident for one iteration, but they are not in `out`.
    fn refund(&mut self, n: usize) {
        self.remaining = self.remaining.saturating_add(n);
    }
}

/// What a side contributes once its size, digest and (maybe) bytes are known.
struct SideFacts {
    bytes: usize,
    sha256: String,
    /// `None` for an over-cap side — present but not diffable.
    text: Option<Vec<u8>>,
    /// `Some(cap)` iff the read refused to buffer this side, carrying the cap
    /// it was handed. `None` for a side that fits. See [`Side::Oversize`].
    handed_cap: Option<usize>,
}

enum SideOutcome {
    Absent,
    Failed(String),
    Present(SideFacts),
}

fn classify(side: Side) -> SideOutcome {
    match side {
        Side::Missing => SideOutcome::Absent,
        Side::Unreadable(why) => SideOutcome::Failed(why),
        Side::Text(bytes) => SideOutcome::Present(SideFacts {
            bytes: bytes.len(),
            sha256: sha256_hex(&bytes),
            text: Some(bytes),
            handed_cap: None,
        }),
        Side::Oversize {
            bytes,
            sha256,
            handed_cap,
        } => SideOutcome::Present(SideFacts {
            bytes,
            sha256,
            text: None,
            handed_cap: Some(handed_cap),
        }),
    }
}

/// The bounded result of [`assemble_file_changes`].
pub struct AssembledFileChanges {
    pub files: Vec<SessionFileChange>,
    /// Candidate paths dropped because the report hit its `max_files` bound.
    pub omitted_files: usize,
}

/// Build the change list for a session from its snapshot rows and touched
/// paths. Pure over `files` so the pairing/status logic is unit-testable
/// without a filesystem.
///
/// - The FIRST `captured_before` snapshot per path is the "before" side
///   (the same rule `rewind_session_handler` applies); later rows are
///   ignored.
/// - A touched path with no snapshot is reported as `created` when it
///   exists now, and omitted when it does not (nothing to show on either
///   side — the session never left a file there).
/// - Order: snapshot rows in `taken_at` order, then snapshot-less touched
///   paths in touch order.
/// - At most `max_files` CANDIDATE paths are examined, in that order. The
///   bound is on candidates rather than emitted rows so it also bounds the
///   number of read syscalls; a candidate that turns out not to be a change
///   still consumes its slot. Whatever is left over is counted into
///   [`AssembledFileChanges::omitted_files`] and never silently dropped.
/// - At most `total_text_budget` bytes of TEXT are held across the whole
///   report. Once it is spent every remaining candidate is still emitted —
///   status, sizes, digests, `truncated: true` and a `detail` naming the
///   budget — so the operator sees which files changed and only loses the
///   ability to diff them inline. Files are not dropped to stay in budget;
///   their bodies are.
pub fn assemble_file_changes(
    snapshots: &[SnapshotRow],
    touched: &[String],
    files: &dyn FileProbe,
    max_files: usize,
    total_text_budget: usize,
) -> AssembledFileChanges {
    // Resolve the candidate set FIRST, so the bound is applied before any
    // filesystem work rather than after it.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut candidates: Vec<(&str, Option<&SnapshotRow>)> = Vec::new();
    for snap in snapshots.iter().filter(|s| s.captured_before) {
        if seen.insert(snap.file_path.as_str()) {
            candidates.push((snap.file_path.as_str(), Some(snap)));
        }
    }
    for path in touched {
        if seen.insert(path.as_str()) {
            candidates.push((path.as_str(), None));
        }
    }
    let omitted_files = candidates.len().saturating_sub(max_files);
    candidates.truncate(max_files);

    let mut out: Vec<SessionFileChange> = Vec::with_capacity(candidates.len());
    let mut budget = TextBudget::new(total_text_budget);
    for (path, snapshot) in candidates {
        let (before, after) = match snapshot {
            Some(snap) => {
                // Sequential caps, not one cap used twice: the second side's
                // bound already accounts for what the first side took, so a
                // single pair can never hold 2× the remaining budget.
                let before = read_side(files, &snap.snapshot_blob_path, budget.cap());
                budget.spend(before.buffered_len());
                let after = read_side(files, path, budget.cap());
                budget.spend(after.buffered_len());
                (before, after)
            }
            None => {
                let after = read_side(files, path, budget.cap());
                if matches!(after, Side::Missing) {
                    continue;
                }
                budget.spend(after.buffered_len());
                (Side::Missing, after)
            }
        };
        let spent = before.buffered_len() + after.buffered_len();
        // Sampled from the budget's STARTING total, not its remainder, so it is
        // the same value for every entry — the per-side `handed_cap` recorded
        // at each read is what varies, and comparing the two is what decides
        // which bound refused a side.
        let unspent_cap = budget.unspent_cap();
        let change = match snapshot {
            Some(snap) => pair_sides(
                path,
                Some(snap.taken_at.clone()),
                Some(snap.blob_sha256.as_str()),
                before,
                after,
                true,
                unspent_cap,
            ),
            None => pair_sides(path, None, None, before, after, false, unspent_cap),
        };
        // Bytes read but not kept (a binary side, or one discarded because its
        // partner was oversize) were resident for this iteration only — they
        // are not in `out`, so they do not count against the report's budget.
        let kept = change.before.as_ref().map_or(0, |s| s.len())
            + change.after.as_ref().map_or(0, |s| s.len());
        budget.refund(spent.saturating_sub(kept));
        out.push(change);
    }

    AssembledFileChanges {
        files: out,
        omitted_files,
    }
}

/// Pair the two sides into one reported entry.
fn pair_sides(
    file_path: &str,
    taken_at: Option<String>,
    recorded_before_sha: Option<&str>,
    before: Side,
    after: Side,
    had_snapshot: bool,
    unspent_cap: usize,
) -> SessionFileChange {
    let mut change = SessionFileChange {
        file_path: file_path.to_string(),
        status: String::new(),
        before: None,
        after: None,
        before_bytes: None,
        after_bytes: None,
        before_sha256: None,
        after_sha256: None,
        truncated: false,
        taken_at,
        detail: None,
    };

    let before = match classify(before) {
        SideOutcome::Failed(why) => {
            change.status = "unreadable".to_string();
            change.detail = Some(format!("pre-edit snapshot unreadable: {why}"));
            return change;
        }
        SideOutcome::Absent if had_snapshot => {
            change.status = "unreadable".to_string();
            change.detail = Some("pre-edit snapshot blob is missing on disk".to_string());
            return change;
        }
        SideOutcome::Absent => None,
        SideOutcome::Present(facts) => Some(facts),
    };
    if let Some(facts) = &before {
        if let Some(recorded) = recorded_before_sha {
            if recorded != facts.sha256 {
                change.status = "unreadable".to_string();
                change.detail = Some(format!(
                    "pre-edit snapshot blob sha256 mismatch: recorded={recorded}, actual={}",
                    facts.sha256
                ));
                return change;
            }
        }
        change.before_bytes = Some(facts.bytes);
        change.before_sha256 = Some(facts.sha256.clone());
    }

    let after = match classify(after) {
        SideOutcome::Failed(why) => {
            change.status = "unreadable".to_string();
            change.detail = Some(format!("current file unreadable: {why}"));
            return change;
        }
        SideOutcome::Absent => None,
        SideOutcome::Present(facts) => Some(facts),
    };
    if let Some(facts) = &after {
        change.after_bytes = Some(facts.bytes);
        change.after_sha256 = Some(facts.sha256.clone());
    }

    change.status = match (&before, &after) {
        (Some(_), None) => "deleted",
        (None, Some(_)) => "created",
        (Some(_), Some(_)) if change.before_sha256 == change.after_sha256 => "unchanged",
        (Some(_), Some(_)) => "modified",
        (None, None) => "unreadable",
    }
    .to_string();
    if change.status == "unreadable" {
        change.detail = Some("neither side exists".to_string());
        return change;
    }

    let before_text = before
        .as_ref()
        .and_then(|f| f.text.as_deref())
        .map(std::str::from_utf8);
    let after_text = after
        .as_ref()
        .and_then(|f| f.text.as_deref())
        .map(std::str::from_utf8);
    if matches!(before_text, Some(Err(_))) || matches!(after_text, Some(Err(_))) {
        change.status = "binary".to_string();
        return change;
    }
    // A present side with no bytes is one `read_side` refused to buffer. WHICH
    // bound refused it is decided PER SIDE from the cap that side's read was
    // actually handed, recorded at the read itself — not from any flag sampled
    // before the reads (the aggregate budget shrinks between the two sides of
    // one entry, so such a flag is wrong for the second side exactly at the
    // boundary), and not from the side's reported length either. That length
    // comes from the digest pass, a SECOND observation of the same file: a file
    // that shrinks between the two passes reports a length under the per-side
    // cap and would be blamed on a budget that was never touched. The handed
    // cap is immune in both directions because it is not a property of the file
    // at all.
    let budget_limited_sides: Vec<bool> = [before.as_ref(), after.as_ref()]
        .into_iter()
        .flatten()
        .filter(|f| f.text.is_none())
        .map(|f| f.handed_cap.is_some_and(|cap| cap < unspent_cap))
        .collect();
    if !budget_limited_sides.is_empty() {
        change.truncated = true;
        // A side handed less than an unspent budget's cap can only have been
        // refused by what earlier reads had already spent. Honesty: a 2 KiB
        // file whose neighbours ate the report's budget is not "too large to
        // diff", which is what the UI says for a plain over-cap entry, so name
        // the bound that actually applied. If EITHER side was handed the full
        // cap and still refused, "too large" is the true statement about this
        // entry and the UI's default says it.
        if budget_limited_sides.iter().all(|by_budget| *by_budget) {
            change.detail = Some(
                "the report's text budget was spent on earlier files — size and digest only"
                    .to_string(),
            );
        }
        return change;
    }
    change.before = before_text.and_then(|r| r.ok()).map(str::to_string);
    change.after = after_text.and_then(|r| r.ok()).map(str::to_string);
    change
}

async fn file_changes_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<SessionFileChangesResponse>, (StatusCode, String)> {
    let pg = &state.app_state.pg_db;
    let snapshots = pg
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let touched = pg
        .get_files_touched(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    // Disk reads are blocking; keep them off the async executor.
    let assembled = tokio::task::spawn_blocking(move || {
        assemble_file_changes(
            &snapshots,
            &touched,
            &DiskFiles,
            FILE_CHANGE_MAX_FILES,
            FILE_CHANGE_TOTAL_TEXT_BUDGET_BYTES,
        )
    })
    .await
    .map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("file-changes assembly panicked: {e}"),
        )
    })?;

    if assembled.omitted_files > 0 {
        warn!(
            "file-changes: session={} touched more than {} paths; {} omitted from the report",
            session_id, FILE_CHANGE_MAX_FILES, assembled.omitted_files
        );
    }

    Ok(Json(SessionFileChangesResponse {
        session_id,
        files: assembled.files,
        files_truncated: assembled.omitted_files > 0,
        omitted_files: assembled.omitted_files,
        read_at_ms: chrono::Utc::now().timestamp_millis(),
    }))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetSnapshotsResponse {
    pub session_id: String,
    pub snapshots: Vec<SnapshotRow>,
}

async fn get_snapshots_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
) -> Result<Json<GetSnapshotsResponse>, (StatusCode, String)> {
    let snapshots = state
        .app_state
        .pg_db
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(GetSnapshotsResponse {
        session_id,
        snapshots,
    }))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct RewindSessionRequest {
    /// Reserved for future use (e.g. dry-run, scope-by-path). The body
    /// is currently empty `{}` per the slash command's contract; we
    /// accept extra fields liberally.
    #[serde(default)]
    pub _placeholder: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindError {
    pub file_path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewindSessionResponse {
    pub session_id: String,
    pub files_restored: usize,
    pub files_skipped: usize,
    pub errors: Vec<RewindError>,
}

/// Compute the sha256 of `path`'s contents as a lowercase hex string.
/// Returns `None` if the file cannot be read. Streamed, so a large snapshot
/// blob costs a fixed buffer rather than its own size.
fn sha256_of_file(path: &Path) -> Option<String> {
    // `sha256_stream` also reports the length it hashed; the rewind path only
    // verifies the digest, so the length is dropped here.
    sha256_stream(std::fs::File::open(path).ok()?)
        .ok()
        .map(|(sha, _len)| sha)
}

async fn rewind_session_handler(
    State(state): State<Arc<ApiState>>,
    AxumPath(session_id): AxumPath<String>,
    body: Option<Json<RewindSessionRequest>>,
) -> Result<Json<RewindSessionResponse>, (StatusCode, String)> {
    let _ = body; // body fields are reserved for future use

    let snapshots = state
        .app_state
        .pg_db
        .get_snapshots_for_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let mut files_restored = 0usize;
    let mut files_skipped = 0usize;
    let mut errors: Vec<RewindError> = Vec::new();

    // Only the FIRST captured-before snapshot per file_path is the
    // rollback target. Subsequent rows are informational. Walk in
    // taken_at ASC order (the SELECT already does this) and skip a
    // file_path once we've handled it.
    let mut handled: std::collections::HashSet<String> = std::collections::HashSet::new();

    for snap in &snapshots {
        if !snap.captured_before {
            continue;
        }
        if !handled.insert(snap.file_path.clone()) {
            // already restored from the first snapshot for this path
            files_skipped += 1;
            continue;
        }

        let blob_path = Path::new(&snap.snapshot_blob_path);
        if !blob_path.exists() {
            errors.push(RewindError {
                file_path: snap.file_path.clone(),
                reason: format!("blob missing: {}", snap.snapshot_blob_path),
            });
            continue;
        }

        let actual_sha = match sha256_of_file(blob_path) {
            Some(h) => h,
            None => {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("blob unreadable: {}", snap.snapshot_blob_path),
                });
                continue;
            }
        };
        if actual_sha != snap.blob_sha256 {
            errors.push(RewindError {
                file_path: snap.file_path.clone(),
                reason: format!(
                    "blob sha256 mismatch: stored={}, actual={}",
                    snap.blob_sha256, actual_sha
                ),
            });
            continue;
        }

        // Ensure the destination's parent dir exists (the file may
        // have been deleted by the failed worker).
        if let Some(parent) = Path::new(&snap.file_path).parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("create parent dir failed: {}", e),
                });
                continue;
            }
        }

        match std::fs::copy(blob_path, Path::new(&snap.file_path)) {
            Ok(_) => {
                files_restored += 1;
                info!(
                    "rewind_session: restored {} from blob {} (session={})",
                    snap.file_path, snap.snapshot_blob_path, session_id
                );
            }
            Err(e) => {
                errors.push(RewindError {
                    file_path: snap.file_path.clone(),
                    reason: format!("copy failed: {}", e),
                });
            }
        }
    }

    if !errors.is_empty() {
        warn!(
            "rewind_session: {} restored, {} errored, session={}",
            files_restored,
            errors.len(),
            session_id
        );
    }

    Ok(Json(RewindSessionResponse {
        session_id,
        files_restored,
        files_skipped,
        errors,
    }))
}

pub fn routes() -> Router<Arc<ApiState>> {
    Router::new()
        // Avoid the v0.7-syntax false positive on `{name}` captures, mirroring
        // the workaround in `mcp/ai_session.rs::routes`.
        .without_v07_checks()
        .route(
            "/sessions/{session_id}/snapshots",
            get(get_snapshots_handler),
        )
        .route(
            "/sessions/{session_id}/rewind",
            post(rewind_session_handler),
        )
        .route(
            "/sessions/{session_id}/file-changes",
            get(file_changes_handler),
        )
}

#[cfg(test)]
mod file_changes_tests {
    use super::*;
    use std::collections::HashMap;

    fn snap(path: &str, blob: &str, sha: &str, before: bool) -> SnapshotRow {
        SnapshotRow {
            id: format!("id-{path}"),
            session_id: "s1".to_string(),
            file_path: path.to_string(),
            snapshot_blob_path: blob.to_string(),
            blob_sha256: sha.to_string(),
            captured_before: before,
            taken_at: "2026-09-15T00:00:00Z".to_string(),
        }
    }

    /// In-memory tree that RECORDS every bounded read — the path, the cap it
    /// was asked for, and how many bytes it handed back — so a test can assert
    /// what the production code actually pulled into memory rather than trust
    /// it. `read_capped` honours the cap exactly as `DiskFiles` does.
    struct FakeFs {
        files: HashMap<String, Vec<u8>>,
        reads: std::cell::RefCell<Vec<(String, usize, usize)>>,
    }

    impl FakeFs {
        fn new(entries: &[(&str, &[u8])]) -> Self {
            Self {
                files: entries
                    .iter()
                    .map(|(p, b)| (p.to_string(), b.to_vec()))
                    .collect(),
                reads: std::cell::RefCell::new(Vec::new()),
            }
        }
        fn get(&self, path: &str) -> std::io::Result<&Vec<u8>> {
            match self.files.get(path) {
                Some(b) => Ok(b),
                None if path.starts_with("EIO:") => Err(std::io::Error::other("disk on fire")),
                None => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            }
        }
        /// Paths whose read came back WITHIN its cap — the only case the
        /// production code buffers as text.
        ///
        /// Deliberately not "n == the file's length": a file of exactly
        /// `cap + 1` bytes returns its whole length and is still over the cap,
        /// so length equality would call the very file the bound exists to stop
        /// "fully read".
        fn fully_buffered(&self) -> Vec<String> {
            self.reads
                .borrow()
                .iter()
                .filter(|(_, cap, n)| n <= cap)
                .map(|(p, _, _)| p.clone())
                .collect()
        }
        /// The largest number of bytes any single read handed back. The whole
        /// point of the `take`-bounded read is that this stays tiny however
        /// large the tree is.
        fn largest_read(&self) -> usize {
            self.reads
                .borrow()
                .iter()
                .map(|(_, _, n)| *n)
                .max()
                .unwrap_or(0)
        }
        /// Every read honoured its cap: no call returned more than `cap + 1`.
        fn every_read_respected_its_cap(&self) -> bool {
            self.reads.borrow().iter().all(|(_, cap, n)| *n <= cap + 1)
        }
    }

    impl FileProbe for FakeFs {
        fn read_capped(&self, path: &str, cap: usize) -> std::io::Result<Vec<u8>> {
            let body = self.get(path)?;
            let take = body.len().min(cap.saturating_add(1));
            let bytes = body[..take].to_vec();
            self.reads
                .borrow_mut()
                .push((path.to_string(), cap, bytes.len()));
            Ok(bytes)
        }
        fn digest(&self, path: &str) -> std::io::Result<(String, u64)> {
            sha256_stream(self.get(path)?.as_slice())
        }
    }

    fn fs(entries: &[(&str, &[u8])]) -> FakeFs {
        FakeFs::new(entries)
    }

    /// The bounds are exercised by their own tests; everywhere else they must
    /// not interfere.
    const NO_FILE_CAP: usize = usize::MAX;
    const NO_TEXT_BUDGET: usize = usize::MAX;

    /// `assemble_file_changes` with both bounds wide open.
    fn assemble(
        snapshots: &[SnapshotRow],
        touched: &[String],
        files: &dyn FileProbe,
    ) -> AssembledFileChanges {
        assemble_file_changes(snapshots, touched, files, NO_FILE_CAP, NO_TEXT_BUDGET)
    }

    #[test]
    fn modified_file_carries_both_sides_and_shas() {
        let before = b"a\nb\n";
        let sha = sha256_hex(before);
        let read = fs(&[("/blob/1", before), ("/src/x.rs", b"a\nc\n")]);
        let out = assemble(&[snap("/src/x.rs", "/blob/1", &sha, true)], &[], &read).files;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].status, "modified");
        assert_eq!(out[0].before.as_deref(), Some("a\nb\n"));
        assert_eq!(out[0].after.as_deref(), Some("a\nc\n"));
        assert_eq!(out[0].before_sha256.as_deref(), Some(sha.as_str()));
        assert!(!out[0].truncated);
        assert_eq!(out[0].taken_at.as_deref(), Some("2026-09-15T00:00:00Z"));
    }

    #[test]
    fn unchanged_deleted_and_created_are_told_apart() {
        let text = b"same\n";
        let sha = sha256_hex(text);
        let read = fs(&[
            ("/blob/same", text),
            ("/src/same.rs", text),
            ("/blob/gone", text),
            ("/src/new.rs", b"fresh\n"),
        ]);
        let snaps = [
            snap("/src/same.rs", "/blob/same", &sha, true),
            snap("/src/gone.rs", "/blob/gone", &sha, true),
        ];
        let touched = [
            "/src/same.rs".to_string(),
            "/src/new.rs".to_string(),
            "/src/never-landed.rs".to_string(),
        ];
        let out = assemble(&snaps, &touched, &read).files;
        let by_path: HashMap<_, _> = out.iter().map(|c| (c.file_path.as_str(), c)).collect();
        assert_eq!(by_path["/src/same.rs"].status, "unchanged");
        assert_eq!(by_path["/src/gone.rs"].status, "deleted");
        assert_eq!(by_path["/src/gone.rs"].after, None);
        assert_eq!(by_path["/src/new.rs"].status, "created");
        assert_eq!(by_path["/src/new.rs"].before, None);
        assert_eq!(by_path["/src/new.rs"].after.as_deref(), Some("fresh\n"));
        // A touched path that exists on neither side is not a change.
        assert!(!by_path.contains_key("/src/never-landed.rs"));
        // Snapshot rows come first, in row order; touched-only paths follow.
        assert_eq!(out[0].file_path, "/src/same.rs");
        assert_eq!(out[1].file_path, "/src/gone.rs");
        assert_eq!(out[2].file_path, "/src/new.rs");
    }

    #[test]
    fn a_failed_read_is_reported_not_dropped() {
        let sha = sha256_hex(b"x");
        let read = fs(&[("/blob/ok", b"x"), ("/blob/x", b"x")]);
        let snaps = [
            // blob missing on disk
            snap("/src/a.rs", "/blob/missing", &sha, true),
            // current file unreadable (not ENOENT)
            snap("EIO:/src/b.rs", "/blob/ok", &sha, true),
            // recorded sha disagrees with the blob's bytes
            snap("/src/c.rs", "/blob/x", "deadbeef", true),
        ];
        let out = assemble(&snaps, &[], &read).files;
        assert_eq!(out.len(), 3);
        for c in &out {
            assert_eq!(c.status, "unreadable", "{c:?}");
            assert!(c.detail.is_some(), "{c:?}");
            assert_eq!(c.before, None);
            assert_eq!(c.after, None);
        }
        assert!(out[0].detail.as_deref().unwrap().contains("missing"));
        assert!(out[1].detail.as_deref().unwrap().contains("disk on fire"));
        assert!(out[2].detail.as_deref().unwrap().contains("mismatch"));
    }

    #[test]
    fn binary_and_oversized_sides_carry_no_text() {
        let bin = [0xff_u8, 0xfe, 0x00];
        let sha_bin = sha256_hex(&bin);
        let big = vec![b'a'; FILE_CHANGE_TEXT_CAP_BYTES + 1];
        let sha_big = sha256_hex(&big);
        let read = fs(&[
            ("/blob/bin", &bin),
            ("/src/bin", b"text now"),
            ("/blob/big", &big),
            ("/src/big", b"small now"),
        ]);
        let snaps = [
            snap("/src/bin", "/blob/bin", &sha_bin, true),
            snap("/src/big", "/blob/big", &sha_big, true),
        ];
        let out = assemble(&snaps, &[], &read).files;
        assert_eq!(out[0].status, "binary");
        assert_eq!(out[0].before, None);
        assert_eq!(out[1].status, "modified");
        assert!(out[1].truncated);
        assert_eq!(out[1].before, None);
        assert_eq!(out[1].before_bytes, Some(FILE_CHANGE_TEXT_CAP_BYTES + 1));
    }

    /// The availability fix: an over-cap side is never pulled into memory.
    /// Before this, every side was read whole and the cap only suppressed the
    /// text afterwards — so one multi-GB generated artifact in a worker's
    /// touched set could OOM the runner, a tier-0 process, through an
    /// unauthenticated loopback route.
    #[test]
    fn an_over_cap_side_is_never_read_into_memory() {
        let big = vec![b'a'; FILE_CHANGE_TEXT_CAP_BYTES + 1];
        let sha_big = sha256_hex(&big);
        let small = b"small now";
        let read = fs(&[("/blob/big", &big), ("/src/big", small)]);
        let out = assemble(&[snap("/src/big", "/blob/big", &sha_big, true)], &[], &read).files;

        // Only the small side came back WHOLE; the oversize blob was probed to
        // `cap + 1` bytes and no further.
        assert_eq!(read.fully_buffered(), vec!["/src/big".to_string()]);
        assert!(read.largest_read() <= FILE_CHANGE_TEXT_CAP_BYTES + 1);
        // It is still fully described: size, digest, and an honest `truncated`.
        assert_eq!(out[0].before_bytes, Some(FILE_CHANGE_TEXT_CAP_BYTES + 1));
        assert_eq!(out[0].before_sha256.as_deref(), Some(sha_big.as_str()));
        assert_eq!(out[0].after_bytes, Some(small.len()));
        assert!(out[0].truncated);
        assert_eq!(out[0].before, None);
        assert_eq!(out[0].after, None);
        assert_eq!(out[0].status, "modified");
        // Cut by its own size, so no budget claim.
        assert_eq!(out[0].detail, None);
    }

    /// The TOCTOU fix: the cap is enforced by the READ, not by a stat taken
    /// before it.
    ///
    /// The old `read_side` stat'd, decided the file was under the cap, then
    /// called `std::fs::read` — the whole file, at whatever size it had by
    /// then. A worker appending to a generated artifact or a log between those
    /// two syscalls got that file read whole into a tier-0 process. Here the
    /// tree holds a body 16× the cap: whatever any stat might have said, the
    /// bound is what `read_capped` hands back, so the entry is `truncated`
    /// with a streamed digest and NOTHING near the body's size is ever
    /// resident.
    ///
    /// Note the bound is now structural as well as tested: [`FileProbe`] has no
    /// unbounded read to call, so restoring the old stat-then-read shape does
    /// not fail this assertion — it fails to compile.
    #[test]
    fn a_side_far_over_the_cap_is_bounded_by_the_read_itself() {
        let huge = vec![b'a'; FILE_CHANGE_TEXT_CAP_BYTES * 16];
        let sha_huge = sha256_hex(&huge);
        let read = fs(&[("/blob/huge", &huge), ("/src/huge", b"now\n")]);
        let out = assemble(
            &[snap("/src/huge", "/blob/huge", &sha_huge, true)],
            &[],
            &read,
        )
        .files;

        assert!(read.every_read_respected_its_cap());
        assert!(
            read.largest_read() <= FILE_CHANGE_TEXT_CAP_BYTES + 1,
            "largest read was {} bytes for a {}-byte file",
            read.largest_read(),
            huge.len()
        );
        assert!(out[0].truncated);
        assert_eq!(out[0].before, None);
        assert_eq!(out[0].before_bytes, Some(huge.len()));
        assert_eq!(out[0].before_sha256.as_deref(), Some(sha_huge.as_str()));
    }

    /// The streamed digest of an over-cap side is still checked against the
    /// recorded one, so a corrupt huge blob is `unreadable`, not `modified`.
    #[test]
    fn an_over_cap_snapshot_blob_still_fails_its_sha_check() {
        let big = vec![b'a'; FILE_CHANGE_TEXT_CAP_BYTES + 1];
        let read = fs(&[("/blob/big", &big), ("/src/big", b"now")]);
        let out = assemble(
            &[snap("/src/big", "/blob/big", "deadbeef", true)],
            &[],
            &read,
        )
        .files;
        assert_eq!(out[0].status, "unreadable");
        assert!(out[0].detail.as_deref().unwrap().contains("mismatch"));
        // The digest that failed the check was STREAMED: the huge blob's body
        // was never pulled in whole. (The under-cap current file is.)
        assert!(!read.fully_buffered().contains(&"/blob/big".to_string()));
        assert!(read.largest_read() <= FILE_CHANGE_TEXT_CAP_BYTES + 1);
    }

    /// The file-count bound: the report stops at `max_files` candidates, says
    /// how many it dropped, and does no filesystem work for them at all.
    #[test]
    fn the_file_count_is_capped_and_the_cut_is_reported() {
        let entries: Vec<(String, Vec<u8>)> = (0..10)
            .map(|i| (format!("/src/f{i}"), b"body".to_vec()))
            .collect();
        let borrowed: Vec<(&str, &[u8])> = entries
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let read = fs(&borrowed);
        let touched: Vec<String> = entries.iter().map(|(p, _)| p.clone()).collect();

        let assembled = assemble_file_changes(&[], &touched, &read, 3, NO_TEXT_BUDGET);
        assert_eq!(assembled.files.len(), 3);
        assert_eq!(assembled.omitted_files, 7);
        // Order is preserved: the cut is a suffix, not an arbitrary subset.
        assert_eq!(assembled.files[0].file_path, "/src/f0");
        assert_eq!(assembled.files[2].file_path, "/src/f2");
        // Nothing beyond the bound was even opened.
        assert_eq!(read.fully_buffered().len(), 3);

        // Under the bound, nothing is reported as omitted.
        let all = assemble_file_changes(&[], &touched, &fs(&borrowed), 10, NO_TEXT_BUDGET);
        assert_eq!(all.files.len(), 10);
        assert_eq!(all.omitted_files, 0);
    }

    /// The aggregate-bytes fix. The per-side cap and the file-count cap each
    /// bound one axis; nothing bounded their PRODUCT, so 400 files just under
    /// the per-side cap was ~200 MiB resident plus a comparable serialisation
    /// buffer — for one unauthenticated loopback request, with no concurrency
    /// limit. The realistic shape is the one tested here: many ordinary files,
    /// each individually fine.
    ///
    /// The budget does not DROP files. Every candidate is still reported, with
    /// status, sizes and digests; only the bodies stop.
    #[test]
    fn the_total_text_budget_bounds_the_whole_report() {
        // 20 files of 1 KiB each = 20 KiB of text, against a 4 KiB budget.
        let bodies: Vec<(String, Vec<u8>)> = (0..20)
            .map(|i| (format!("/src/f{i:02}"), vec![b'x'; 1024]))
            .collect();
        let borrowed: Vec<(&str, &[u8])> = bodies
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let read = fs(&borrowed);
        let touched: Vec<String> = bodies.iter().map(|(p, _)| p.clone()).collect();

        let assembled = assemble_file_changes(&[], &touched, &read, NO_FILE_CAP, 4 * 1024);

        // Nothing was dropped: the file-count bound is a different bound.
        assert_eq!(assembled.files.len(), 20);
        assert_eq!(assembled.omitted_files, 0);

        // The text the response holds is inside the budget.
        let held: usize = assembled
            .files
            .iter()
            .map(|c| {
                c.before.as_ref().map_or(0, |s| s.len()) + c.after.as_ref().map_or(0, |s| s.len())
            })
            .sum();
        assert!(
            held <= 4 * 1024,
            "held {held} bytes against a 4096-byte budget"
        );

        // The first few carry text; the rest are truncated but fully described.
        assert!(assembled.files[0].after.is_some());
        assert!(!assembled.files[0].truncated);
        let tail = &assembled.files[19];
        assert!(tail.truncated);
        assert_eq!(tail.after, None);
        assert_eq!(tail.after_bytes, Some(1024));
        assert!(tail.after_sha256.is_some());
        assert_eq!(tail.status, "created");
        // Honesty: a 1 KiB file is not "too large to diff". The entry names the
        // bound that actually applied, and the UI renders that `detail`.
        assert!(
            tail.detail.as_deref().unwrap().contains("budget"),
            "{:?}",
            tail.detail
        );
        // And it is the BUDGET, not the per-side cap, so the per-file detail
        // must not appear on an entry read while the budget was still wide.
        assert_eq!(assembled.files[0].detail, None);
    }

    /// The budget shrinks the cap handed to the read, so an over-budget side is
    /// never buffered in the first place — the bound is not a post-hoc trim of
    /// an already-allocated list.
    #[test]
    fn an_over_budget_side_is_not_read_into_memory_and_then_discarded() {
        let bodies: Vec<(String, Vec<u8>)> = (0..6)
            .map(|i| (format!("/src/g{i}"), vec![b'y'; 1024]))
            .collect();
        let borrowed: Vec<(&str, &[u8])> = bodies
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_slice()))
            .collect();
        let read = fs(&borrowed);
        let touched: Vec<String> = bodies.iter().map(|(p, _)| p.clone()).collect();

        let assembled = assemble_file_changes(&[], &touched, &read, NO_FILE_CAP, 2048);

        // Two files fit; the remaining four were probed to their (zero) cap and
        // no further, so only two whole bodies were ever resident.
        assert_eq!(read.fully_buffered().len(), 2);
        assert!(read.every_read_respected_its_cap());
        assert_eq!(assembled.files.iter().filter(|c| c.truncated).count(), 4);
    }

    /// The truncation REASON is decided per side from that side's own length,
    /// not from a flag sampled before the entry's reads.
    ///
    /// The budget shrinks between the two sides of one entry, so a per-entry
    /// flag is wrong exactly at the boundary — which is the entry most likely
    /// to be truncated. Here the `before` side (256 KiB, within its own cap)
    /// leaves too little budget for a 60 KiB `after`: the UI must be told the
    /// BUDGET cut it, or `noDiffReason` renders "too large to diff" about a
    /// 60 KiB file against a 256 KiB cap.
    #[test]
    fn a_side_cut_at_the_budget_boundary_names_the_budget_not_its_size() {
        let before = vec![b'b'; FILE_CHANGE_TEXT_CAP_BYTES];
        let after = vec![b'a'; 60 * 1024];
        let sha_before = sha256_hex(&before);
        let read = fs(&[("/blob/x", &before), ("/src/x", &after)]);

        let out = assemble_file_changes(
            &[snap("/src/x", "/blob/x", &sha_before, true)],
            &[],
            &read,
            NO_FILE_CAP,
            300 * 1024, // wide open at the top of the loop, spent by the before side
        )
        .files;

        assert!(out[0].truncated);
        assert_eq!(out[0].after_bytes, Some(60 * 1024));
        assert!(
            out[0].detail.as_deref().unwrap_or("").contains("budget"),
            "a 60 KiB side cut by the budget claimed the per-side cap: {:?}",
            out[0].detail
        );
    }

    /// The converse: a genuinely over-cap side says nothing about the budget,
    /// even when the budget happens to be low. "Too large to diff" is the true
    /// statement about that entry and the UI supplies it.
    #[test]
    fn a_genuinely_over_cap_side_never_blames_the_budget() {
        let huge = vec![b'h'; FILE_CHANGE_TEXT_CAP_BYTES * 4];
        let sha_huge = sha256_hex(&huge);
        let read = fs(&[("/blob/h", &huge), ("/src/h", b"now\n")]);

        // Budget deliberately smaller than the per-side cap, which is the case
        // an entry-level flag got wrong on the very FIRST candidate.
        let out = assemble_file_changes(
            &[snap("/src/h", "/blob/h", &sha_huge, true)],
            &[],
            &read,
            NO_FILE_CAP,
            4 * 1024,
        )
        .files;

        assert!(out[0].truncated);
        assert_eq!(out[0].before_bytes, Some(FILE_CHANGE_TEXT_CAP_BYTES * 4));
        assert_eq!(
            out[0].detail, None,
            "blamed the budget for an over-cap side"
        );
    }

    /// A probe whose file is OVER the cap at the bounded read and SMALLER at
    /// the digest pass — the concurrent-writer case in the shrinking
    /// direction. It also records every cap it was handed.
    struct ShrinkingFile {
        handed_caps: std::cell::RefCell<Vec<usize>>,
        shrunk_to: Vec<u8>,
    }

    impl FileProbe for ShrinkingFile {
        fn read_capped(&self, _path: &str, cap: usize) -> std::io::Result<Vec<u8>> {
            self.handed_caps.borrow_mut().push(cap);
            // Always over whatever cap it was handed: the read is what decides.
            Ok(vec![b'w'; cap.saturating_add(1)])
        }
        fn digest(&self, _path: &str) -> std::io::Result<(String, u64)> {
            Ok((sha256_hex(&self.shrunk_to), self.shrunk_to.len() as u64))
        }
    }

    /// Attribution reads the cap handed to the READ, not the length reported by
    /// the digest pass.
    ///
    /// The two passes are separate observations of the same file. A file that
    /// shrinks between them comes back under the per-side cap, so a rule keyed
    /// on that length blames the report's aggregate budget — here on the FIRST
    /// candidate, with the budget wide open and nothing yet spent. The `detail`
    /// exists to stop false statements; that would be one.
    #[test]
    fn a_side_that_shrinks_between_the_two_passes_does_not_blame_an_untouched_budget() {
        let probe = ShrinkingFile {
            handed_caps: std::cell::RefCell::new(Vec::new()),
            shrunk_to: vec![b'w'; 2048],
        };

        let out = assemble_file_changes(
            &[],
            &["/src/shrinks".to_string()],
            &probe,
            NO_FILE_CAP,
            NO_TEXT_BUDGET,
        )
        .files;

        // The read really was handed the full per-side cap: nothing had been
        // spent, so the budget cannot be what refused it.
        assert_eq!(
            probe.handed_caps.borrow().as_slice(),
            &[FILE_CHANGE_TEXT_CAP_BYTES]
        );
        assert!(out[0].truncated);
        assert_eq!(out[0].after_bytes, Some(2048));
        assert_eq!(
            out[0].detail, None,
            "blamed an untouched budget for a file that shrank between the bounded read and the digest pass"
        );
    }

    /// Every other test in this module drives [`FakeFs`], whose `read_capped`
    /// honours the cap BY CONSTRUCTION — so they pin what
    /// `assemble_file_changes` asks for, not what the production probe does.
    /// Dropping `.take(..)` from [`DiskFiles::read_capped`] compiles, satisfies
    /// the trait and fails none of them. These three exercise the real
    /// filesystem, which is where the bound actually has to hold.
    #[test]
    fn disk_files_read_capped_stops_at_the_cap_on_a_real_filesystem() {
        const CAP: usize = 64;
        let dir = tempfile::tempdir().expect("tempdir");
        let write = |name: &str, len: usize| -> String {
            let path = dir.path().join(name);
            std::fs::write(&path, vec![b'q'; len]).expect("write");
            path.to_string_lossy().into_owned()
        };

        // Under the cap and exactly AT it are whole files: the returned length
        // is the file's own, and `read_side` classifies them as text.
        assert_eq!(
            DiskFiles
                .read_capped(&write("under", CAP - 1), CAP)
                .unwrap()
                .len(),
            CAP - 1
        );
        assert_eq!(
            DiskFiles
                .read_capped(&write("exact", CAP), CAP)
                .unwrap()
                .len(),
            CAP
        );
        // `cap + 1` is the VERDICT, and it is the ONLY over-cap signal — which
        // is why the assertion is on the returned LENGTH. A file one byte over
        // and a file forty times the cap must be indistinguishable here, or the
        // bound is not a property of the read.
        assert_eq!(
            DiskFiles
                .read_capped(&write("over", CAP + 1), CAP)
                .unwrap()
                .len(),
            CAP + 1
        );
        assert_eq!(
            DiskFiles
                .read_capped(&write("huge", CAP * 40), CAP)
                .unwrap()
                .len(),
            CAP + 1
        );
        // A zero cap (aggregate budget spent) still probes exactly one byte, so
        // a non-empty file is oversize and an empty one is text.
        assert_eq!(DiskFiles.read_capped(&write("z", 10), 0).unwrap().len(), 1);
        assert_eq!(
            DiskFiles.read_capped(&write("empty", 0), 0).unwrap().len(),
            0
        );
        // The error kind `read_side` branches on to report `Missing`.
        let missing = dir.path().join("nope").to_string_lossy().into_owned();
        assert_eq!(
            DiskFiles.read_capped(&missing, CAP).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    #[test]
    fn disk_files_digest_reports_the_files_own_length_and_sha() {
        let dir = tempfile::tempdir().expect("tempdir");
        let body = vec![b'd'; 5000];
        let path = dir.path().join("body");
        std::fs::write(&path, &body).expect("write");
        let path = path.to_string_lossy().into_owned();

        // The two halves must agree with each other AND with the bytes on
        // disk: `Side::Oversize` reports a file it never buffered, so this pair
        // is the only description the operator gets of it.
        let (sha, len) = DiskFiles.digest(&path).expect("digest");
        assert_eq!(len, body.len() as u64);
        assert_eq!(sha, sha256_hex(&body));

        let empty = dir.path().join("empty");
        std::fs::write(&empty, b"").expect("write");
        let (sha, len) = DiskFiles.digest(&empty.to_string_lossy()).expect("digest");
        assert_eq!(len, 0);
        assert_eq!(sha, sha256_hex(b""));

        let missing = dir.path().join("nope").to_string_lossy().into_owned();
        assert_eq!(
            DiskFiles.digest(&missing).unwrap_err().kind(),
            std::io::ErrorKind::NotFound
        );
    }

    /// `FileProbe` is public and `usize::MAX` is a cap a caller can hand it —
    /// the tests above pass it as a budget. A wrapping `cap as u64 + 1` is a
    /// debug panic, and in release a `take(0)`: the file comes back empty and
    /// `bytes.len() <= cap` classifies it as EMPTY TEXT. Confidently wrong is
    /// the one answer this route must not give.
    #[test]
    fn disk_files_read_capped_survives_a_usize_max_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("whole");
        std::fs::write(&path, b"whole file\n").expect("write");
        let got = DiskFiles
            .read_capped(&path.to_string_lossy(), usize::MAX)
            .expect("read");
        assert_eq!(got, b"whole file\n");
    }

    /// Bytes read but NOT kept are refunded: a binary file's bytes are dropped
    /// by `pair_sides`, so they must not eat the budget the text files need.
    #[test]
    fn bytes_that_never_reach_the_response_do_not_spend_the_budget() {
        let bin = vec![0xff_u8; 1024];
        let text = vec![b'z'; 1024];
        let read = fs(&[("/src/a.bin", &bin), ("/src/b.txt", &text)]);
        let touched = ["/src/a.bin".to_string(), "/src/b.txt".to_string()];

        // 1200 bytes: enough for ONE 1 KiB body. The binary one is read first
        // and discarded, so the text one must still fit.
        let assembled = assemble_file_changes(&[], &touched, &read, NO_FILE_CAP, 1200);
        assert_eq!(assembled.files[0].status, "binary");
        assert_eq!(assembled.files[0].after, None);
        assert_eq!(assembled.files[1].status, "created");
        assert!(
            assembled.files[1].after.is_some(),
            "the binary file's discarded bytes spent the budget: {:?}",
            assembled.files[1]
        );
    }

    #[test]
    fn only_the_first_pre_edit_snapshot_per_path_counts() {
        let first = b"first\n";
        let second = b"second\n";
        let read = fs(&[
            ("/blob/first", first),
            ("/blob/second", second),
            ("/src/x", b"now\n"),
        ]);
        let snaps = [
            snap("/src/x", "/blob/first", &sha256_hex(first), true),
            snap("/src/x", "/blob/second", &sha256_hex(second), true),
            snap("/src/x", "/blob/second", &sha256_hex(second), false),
        ];
        let out = assemble(&snaps, &["/src/x".to_string()], &read).files;
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].before.as_deref(), Some("first\n"));
    }
}
