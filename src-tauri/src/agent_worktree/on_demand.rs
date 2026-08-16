//! On-demand agent-worktree reclaim (plan
//! `2026-07-19-worktree-cleanup-lifecycle-tracking`, Phase 4 — the EXECUTE +
//! TRIGGER halves).
//!
//! ## The through-line
//!
//! **coord DECIDES → the runner EXECUTES → a human button or an agent
//! TRIGGERS.** coord owns reap eligibility (its five numbered gates G1–G5 plus
//! the Phase-2 retention pin); the runner owns the disk and the WIP-safe
//! removal path; this module is the *consented, visible* trigger in front of
//! it.
//!
//! ## Why this exists when a reclaim poller already runs
//!
//! [`super::reclaim::spawn_reclaim`] is invoked UNCONDITIONALLY at boot
//! (`main.rs`) and polls coord every 300s on every runner today. What is
//! default-OFF is coord's **willingness to emit an armed `remove`**
//! (coord-side `COORD_WORKTREE_RECLAIM_ENABLED`). So the accurate framing is:
//! *the executor already runs silently everywhere; it is simply never handed
//! anything destructive to do.* That silent path has **no visibility and no
//! per-item consent**, which is exactly why it is kept disarmed.
//!
//! This module adds the opposite posture — the operator (or an agent acting
//! for them) SEES the exact set that would be removed, with a reason for every
//! refusal, and then presses a button. It therefore does NOT require arming
//! the silent path, and must never be used to arm it.
//!
//! ## Guard layering (two independent layers, both kept)
//!
//! | Guard | Owner | Fact | `reason` |
//! |---|---|---|---|
//! | **G1** dirty | coord (inviolable) **+** runner re-check | uncommitted WIP | `dirty` |
//! | **G2** not landed | coord | HEAD not on `origin/main`, PR unmerged | `not-landed` |
//! | **G3** session-live | coord | another live claim/session on the path | `session-live` |
//! | **G4** main-merge | coord | active `MainMerge` claim on the repo | `main-merge` |
//! | **G5** grace | coord | per-signal grace not elapsed | `grace` |
//! | pin | coord (Phase 2) | `retention='pinned'` | `pinned` |
//! | **G6** in-flight build | **runner only** | held `.cargo-lock` / recent activity | `building` |
//!
//! G6 is runner-local disk truth coord cannot see, so coord's cleared set
//! deliberately excludes it — this module applies it and returns the item in
//! `skipped[]`. The runner NEVER trusts coord's dirty verdict alone: it
//! re-runs `git status --porcelain` at execution time
//! ([`super::reclaim::worktree_is_dirty`]) and `plan_reclaim`'s own
//! "Defense in depth #1" `is_dirty` check still fires inside the plan.
//!
//! **Every refusal carries a specific reason — never a silent drop.**

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use super::canonical_paths::default_canonical_path;
use super::census::{self, CensusSnapshot, WorktreeCensus};
use super::reclaim::{self, ReclaimAction, ReclaimInstruction, ReclaimPull, ReclaimStep};

// ---------------------------------------------------------------------------
// Bounded-wait policy — the endpoint must ALWAYS answer.
// ---------------------------------------------------------------------------

/// How long the survey will wait for a census snapshot before answering
/// anyway. Small on purpose: a census WALK takes minutes on a real machine, so
/// waiting for one is never the plan — this only smooths the case where a walk
/// is about to finish.
const DEFAULT_CENSUS_WAIT_SECS: u64 = 2;

/// Hard ceiling on the caller-supplied `?waitSecs=`. The endpoint answers
/// within this plus coord's own bounded (15s) pull, no matter what.
const MAX_CENSUS_WAIT_SECS: u64 = 10;

/// A snapshot older than this is labelled `stale` rather than `fresh`. Twice
/// the 300s census cadence, so a single missed tick does not cry wolf.
const CENSUS_STALE_AFTER_SECS: u64 = 600;

// ---------------------------------------------------------------------------
// Skip reasons — the refusal vocabulary. One token per guard.
// ---------------------------------------------------------------------------

/// Why an item was NOT reclaimed. Serialized kebab-case; every `skipped[]`
/// entry carries exactly one, so a refusal is never silent or unexplained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    /// **G1** — uncommitted work in the tree. Inviolable, checked BOTH by
    /// coord's census verdict and by the runner's own execution-time
    /// `git status --porcelain`.
    Dirty,
    /// Phase-2 `retention='pinned'` — kept for follow-up work.
    Pinned,
    /// **G3** — another live session/claim references this path.
    SessionLive,
    /// **G6** — a build is in flight (runner-local: held `.cargo-lock`, or
    /// `target`/`node_modules`/root touched inside the activity window).
    Building,
    /// **G2** — the work has not landed on `origin/main` (and no merged PR).
    NotLanded,
    /// **G4** — an active `MainMerge` claim is mid-op on this repo.
    MainMerge,
    /// **G5** — the per-signal reclaim grace has not elapsed yet.
    Grace,
    /// coord has no trigger signal for this worktree — it is simply still in
    /// use / not a reclaim candidate.
    NotACandidate,
    /// coord did not clear this worktree and gave no typed reason (the
    /// pre-Phase-1 coord ships only its cleared set). Honest UNKNOWN, not a
    /// claim that it is safe.
    NotCleared,
    /// coord is unreachable — with no decision there is no eligibility.
    /// Fail-safe: nothing is reclaimable while this holds.
    CoordUnreachable,
    /// The path vanished from disk between the survey and the execution.
    Absent,
    /// The caller named an id that is not in the current reapable set.
    NotReapable,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::Dirty => "dirty",
            SkipReason::Pinned => "pinned",
            SkipReason::SessionLive => "session-live",
            SkipReason::Building => "building",
            SkipReason::NotLanded => "not-landed",
            SkipReason::MainMerge => "main-merge",
            SkipReason::Grace => "grace",
            SkipReason::NotACandidate => "not-a-candidate",
            SkipReason::NotCleared => "not-cleared",
            SkipReason::CoordUnreachable => "coord-unreachable",
            SkipReason::Absent => "absent",
            SkipReason::NotReapable => "not-reapable",
        }
    }

    /// A one-sentence operator-facing explanation. This is what the UI panel
    /// renders under each blocked row — the "you see exactly why" half of the
    /// consent story.
    pub fn detail(self) -> &'static str {
        match self {
            SkipReason::Dirty => "Uncommitted work in this tree (G1) — commit or stash it first.",
            SkipReason::Pinned => {
                "Pinned for follow-up work (retention='pinned') — unpin to reclaim."
            }
            SkipReason::SessionLive => {
                "Another live session or claim still references this path (G3)."
            }
            SkipReason::Building => {
                "A build is in flight here (G6) — cargo lock held or recent activity."
            }
            SkipReason::NotLanded => "The work has not landed on origin/main yet (G2).",
            SkipReason::MainMerge => "A main-merge is mid-flight on this repo (G4).",
            SkipReason::Grace => "Still inside the reclaim grace window (G5).",
            SkipReason::NotACandidate => {
                "Not a reclaim candidate — coord has no trigger signal for it."
            }
            SkipReason::NotCleared => "coord has not cleared this worktree for removal.",
            SkipReason::CoordUnreachable => {
                "coord is unreachable — nothing can be reclaimed without its decision."
            }
            SkipReason::Absent => "No longer present on disk.",
            SkipReason::NotReapable => "Not in the reapable set at execution time.",
        }
    }

    /// Map coord's `DeferReason` snake_case token onto our vocabulary.
    /// An unrecognized token degrades to [`SkipReason::NotCleared`] (honest
    /// unknown) rather than being dropped.
    fn from_coord_token(token: &str) -> SkipReason {
        match token {
            "dirty" => SkipReason::Dirty,
            "not_landed" => SkipReason::NotLanded,
            "other_live_reference" => SkipReason::SessionLive,
            "serialize_claim_active" => SkipReason::MainMerge,
            "grace_pending" => SkipReason::Grace,
            "not_a_candidate" => SkipReason::NotACandidate,
            "pinned" => SkipReason::Pinned,
            _ => SkipReason::NotCleared,
        }
    }
}

// ---------------------------------------------------------------------------
// The pure guard — the load-bearing decision.
// ---------------------------------------------------------------------------

/// Every fact needed to decide ONE worktree, pre-resolved by the caller.
/// PURE input: no DB, no disk, no clock — so the guard is exhaustively
/// unit-testable (mirrors coord's own `CandidateInput`/`gate` shape).
#[derive(Debug, Clone, Default)]
pub struct CandidateFacts {
    /// coord emitted an `action: "remove"` instruction for this path — i.e.
    /// its G1–G5 (+ Phase-2 pin) gate CLEARED it.
    pub coord_reapable: bool,
    /// coord's typed reason when it did NOT clear the path.
    pub coord_block_reason: Option<SkipReason>,
    /// G1 — dirty per the census AND/OR the runner's own `git status`.
    pub is_dirty: bool,
    /// Phase-2 retention pin.
    pub pinned: bool,
    /// G3 — another live session/claim on the path.
    pub session_live: bool,
    /// G6 — runner-local in-flight build.
    pub building: bool,
    /// The worktree root exists on disk right now.
    pub root_exists: bool,
    /// coord could not be reached — no decision, therefore no eligibility.
    pub coord_unreachable: bool,
}

/// The verdict for one worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Safe to remove on demand.
    Reap,
    /// Refused, with the specific reason.
    Skip(SkipReason),
}

/// The guard. Order is the safety contract and is deliberately
/// **runner-guards-first**: the runner's own disk truth (absent / dirty /
/// pinned / session-live / building) can refuse an item coord cleared, but
/// coord's clearance can NEVER override a runner refusal. Only when every
/// local guard passes AND coord cleared the path do we return
/// [`Decision::Reap`].
pub fn classify(f: &CandidateFacts) -> Decision {
    if !f.root_exists {
        return Decision::Skip(SkipReason::Absent);
    }
    // G1 — inviolable, first, always.
    if f.is_dirty {
        return Decision::Skip(SkipReason::Dirty);
    }
    // Phase-2 pin — an explicit keep-for-follow-up intent outranks candidacy.
    if f.pinned {
        return Decision::Skip(SkipReason::Pinned);
    }
    // G3 — someone else is still on it.
    if f.session_live {
        return Decision::Skip(SkipReason::SessionLive);
    }
    // G6 — runner-only; coord's cleared set cannot account for it.
    if f.building {
        return Decision::Skip(SkipReason::Building);
    }
    if f.coord_unreachable {
        return Decision::Skip(SkipReason::CoordUnreachable);
    }
    if f.coord_reapable {
        return Decision::Reap;
    }
    Decision::Skip(f.coord_block_reason.unwrap_or(SkipReason::NotCleared))
}

// ---------------------------------------------------------------------------
// Wire types — `GET /agent-worktrees/reclaimable`.
// ---------------------------------------------------------------------------

/// One surveyed worktree as the panel / agent sees it.
#[derive(Debug, Clone, Serialize)]
pub struct SurveyItem {
    /// Stable handle used by `POST /agent-worktrees/reclaim {ids}` — the
    /// normalized worktree path (lowercased, forward slashes).
    pub id: String,
    /// The path as it appears on disk.
    pub worktree_path: String,
    pub repo: String,
    pub branch: Option<String>,
    /// `"reapable"` | `"blocked"`.
    pub status: &'static str,
    /// `None` for a reapable item; the refusal token otherwise.
    pub reason: Option<&'static str>,
    /// Operator-facing sentence for `reason`.
    pub reason_detail: Option<&'static str>,
    pub is_dirty: bool,
    pub building: bool,
    pub pinned: bool,
    pub landed_in_main: Option<bool>,
    /// Real (non-junctioned) bytes attributable to this worktree — what the
    /// operator actually gets back.
    pub attributable_bytes: u64,
    /// Junction reparse points that must be unlinked BEFORE any recursive
    /// delete (INV-W4). Rendered so the operator sees they are links.
    pub junctioned_paths: Vec<String>,
    /// coord's own reason string for the instruction (e.g.
    /// `worktree:lifecycle:pr_merged`) — provenance for a reapable item.
    pub coord_reason: Option<String>,
}

/// Aggregate counts for the panel header, plus the machine's free-space
/// headroom.
///
/// ## Why the disk half lives here (INV-D1)
///
/// "How much room is left?" and "what can I safely reclaim?" are the same
/// question to a user staring at a full disk, but they have completely
/// different availability: the reclaim half depends on a census walk that can
/// take hours and on coord's decision, while the free-space half depends on
/// neither. Carrying both here lets the panel answer the disk question on the
/// cold-start path, mid-walk, and with coord unreachable — the conditions
/// under which the old PowerShell preview refused to answer at all.
///
/// The volume fields come from the decoupled 60 s publisher
/// ([`census::spawn_volume_publisher`]), NOT from the census snapshot's
/// `volumes` (which is as old as the walk). When no sample exists yet,
/// `volumes` is empty AND `volumes_status` is `pending` — the pair is the
/// honesty contract: empty-with-`pending` is UNKNOWN, never "0 bytes free".
#[derive(Debug, Clone, Serialize, Default)]
pub struct SurveySummary {
    pub reapable: usize,
    pub blocked: usize,
    /// Bytes that pressing "Clean up safe worktrees" would free.
    pub reclaimable_bytes: u64,

    // --- Volume headroom (disk-monitoring Phase 1, step 3) ---
    /// Free/total bytes per mounted volume. EMPTY while `volumes_status` is
    /// `pending` — read the status before reading this.
    pub volumes: Vec<census::VolumeReport>,
    /// `pending` (never sampled — UNKNOWN) | `fresh` | `stale`.
    pub volumes_status: CensusStatus,
    /// RFC3339 instant of the sample. `None` while `pending`.
    pub volumes_observed_at: Option<String>,
    /// Age of the sample in seconds — what the panel renders as "as of Nm
    /// ago". `None` while `pending`.
    pub volumes_age_secs: Option<u64>,
    /// Total free bytes across every sampled volume. `None` — NOT `0` —
    /// while `pending`.
    pub free_bytes_total: Option<u64>,
    /// Total capacity across every sampled volume. `None` while `pending`.
    pub total_bytes_total: Option<u64>,
    /// One operator-facing sentence describing the reading's provenance.
    /// Always present, and explicitly says "not known" rather than implying
    /// an empty or full disk.
    pub volumes_note: String,
}

/// A volume sample older than this is flagged `stale` — 5× the publisher's
/// 60 s default cadence, so a single missed tick does not cry wolf while a
/// dead publisher is surfaced within minutes.
const VOLUME_STALE_AFTER_SECS: u64 = 300;

impl SurveySummary {
    /// Fill the volume half from the publisher's latest sample.
    ///
    /// Called on EVERY survey path — including the cold-start arm where the
    /// census is still `pending` — because the disk question is answerable
    /// even when the worktree question is not. That asymmetry is the whole
    /// point of INV-D1.
    fn with_volume_sample(
        mut self,
        sample: Option<&census::VolumeSample>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let Some(sample) = sample else {
            self.volumes_status = CensusStatus::Pending;
            self.volumes_note = "Free space has not been sampled yet this session — this is \
                                 UNKNOWN, not zero. The volume publisher samples every mounted \
                                 volume once a minute."
                .to_string();
            return self;
        };
        let age_secs = (now - sample.taken_at).num_seconds().max(0) as u64;
        let status = if age_secs <= VOLUME_STALE_AFTER_SECS {
            CensusStatus::Fresh
        } else {
            CensusStatus::Stale
        };
        self.free_bytes_total = Some(
            sample
                .volumes
                .iter()
                .fold(0u64, |acc, v| acc.saturating_add(v.free_bytes)),
        );
        self.total_bytes_total = Some(
            sample
                .volumes
                .iter()
                .fold(0u64, |acc, v| acc.saturating_add(v.total_bytes)),
        );
        self.volumes_note = format!(
            "Free space as of {} ago, across {} volume{}{}.",
            humanize_secs(age_secs),
            sample.volumes.len(),
            if sample.volumes.len() == 1 { "" } else { "s" },
            if status == CensusStatus::Stale {
                " (stale — the volume publisher is overdue)"
            } else {
                ""
            },
        );
        self.volumes = sample.volumes.clone();
        self.volumes_status = status;
        self.volumes_observed_at = Some(sample.taken_at.to_rfc3339());
        self.volumes_age_secs = Some(age_secs);
        self
    }
}

/// Freshness of the disk census the survey is derived from. This is the
/// UX-honesty contract: the panel MUST be able to say "as of Nm ago", and MUST
/// never render a pending census as an authoritative "nothing to clean".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum CensusStatus {
    /// No census walk has completed yet this boot — `items` is empty because
    /// we do not KNOW, not because there is nothing.
    #[default]
    Pending,
    /// Snapshot within [`CENSUS_STALE_AFTER_SECS`].
    Fresh,
    /// Snapshot older than that — still shown, but flagged.
    Stale,
}

impl CensusStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            CensusStatus::Pending => "pending",
            CensusStatus::Fresh => "fresh",
            CensusStatus::Stale => "stale",
        }
    }
}

/// Query string of `GET /agent-worktrees/reclaimable`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SurveyQuery {
    /// `?refresh=1` — kick a fresh census walk in the BACKGROUND. Not the
    /// default path, and never awaited beyond [`SurveyQuery::wait`]: a walk
    /// takes minutes, so the response still comes from the cached snapshot
    /// with `census_refreshing: true`.
    #[serde(default)]
    pub refresh: Option<String>,
    /// Bounded override of the snapshot wait, capped at
    /// [`MAX_CENSUS_WAIT_SECS`].
    #[serde(default)]
    pub wait_secs: Option<u64>,
}

impl SurveyQuery {
    pub fn refresh_requested(&self) -> bool {
        matches!(
            self.refresh.as_deref(),
            Some("1" | "true" | "yes" | "on" | "")
        )
    }

    pub fn wait(&self) -> Duration {
        Duration::from_secs(
            self.wait_secs
                .unwrap_or(DEFAULT_CENSUS_WAIT_SECS)
                .min(MAX_CENSUS_WAIT_SECS),
        )
    }
}

/// Body of `GET /agent-worktrees/reclaimable`.
#[derive(Debug, Clone, Serialize)]
pub struct Survey {
    pub device_id: Option<Uuid>,
    /// `false` when coord could not be reached — the panel says so instead of
    /// implying "nothing to clean".
    ///
    /// SCOPE: this describes the pull THIS REQUEST just made. It says nothing
    /// about the background poller, which is the only thing that can actually
    /// remove a worktree. Read [`Survey::poller`] for that. The distinction is
    /// not academic — from 2026-07-28 to 08-01 this field read `true` on every
    /// request while the poller failed 100 % of its pulls for five days, and a
    /// reader who took `coord_reachable` as evidence the reclaim pipeline was
    /// alive would have been wrong for the whole window.
    pub coord_reachable: bool,
    /// Present when `coord_reachable == false`.
    pub coord_error: Option<String>,
    /// Health of the BACKGROUND POLLER — the executor. A subsystem's health
    /// signal must report on its OUTPUT, not on the inputs of whoever asked.
    pub poller: reclaim::PollerHealth,
    /// Echo of coord's arming flags — informational only. The on-demand path
    /// deliberately does NOT depend on them.
    pub remove_armed: bool,
    pub rejunction_armed: bool,
    /// Canonical repo checkouts filtered out of the list (never reclaimable).
    /// Reported as a count so the omission is visible, not silent.
    pub canonical_excluded: usize,
    pub items: Vec<SurveyItem>,
    pub summary: SurveySummary,

    // --- Census freshness (UX honesty — never present stale data as live) ---
    /// `pending` | `fresh` | `stale`.
    pub census_status: CensusStatus,
    /// RFC3339 wall-clock time the underlying disk census was taken.
    /// `None` while `census_status == pending`.
    pub census_taken_at: Option<String>,
    /// Age of that snapshot in seconds — what the panel renders as
    /// "as of Nm ago".
    pub census_age_secs: Option<u64>,
    /// How long the snapshot's walk took to build (ms). Explains the cadence
    /// to the operator: on a real machine this is minutes.
    pub census_build_ms: Option<u64>,
    /// A census walk is running right now (periodic tick or `?refresh=1`).
    pub census_refreshing: bool,
    /// One operator-facing sentence describing the data's provenance. Always
    /// present, and explicitly says "not ready" rather than implying empty.
    pub census_note: String,
}

// ---------------------------------------------------------------------------
// Wire types — `POST /agent-worktrees/reclaim`.
// ---------------------------------------------------------------------------

/// Body of `POST /agent-worktrees/reclaim`.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ReclaimRequest {
    /// Ids (from the survey) to reclaim. `None`/absent = every currently
    /// reapable item.
    #[serde(default)]
    pub ids: Option<Vec<String>>,
    /// Re-run every guard and report the verdict WITHOUT touching disk.
    #[serde(default)]
    pub dry_run: bool,
}

/// One successfully removed worktree.
#[derive(Debug, Clone, Serialize)]
pub struct RemovedItem {
    pub id: String,
    pub worktree_path: String,
    pub repo: String,
    /// Real bytes reclaimed (from the survey's attribution).
    pub freed_bytes: u64,
    /// `true` when `dryRun` was set — nothing was actually removed.
    pub dry_run: bool,
}

/// One refused worktree, with the guard that refused it.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedItem {
    pub id: String,
    pub worktree_path: String,
    pub reason: &'static str,
    pub detail: String,
}

/// Body of `POST /agent-worktrees/reclaim`.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ReclaimOutcome {
    pub removed: Vec<RemovedItem>,
    pub skipped: Vec<SkippedItem>,
    pub dry_run: bool,
    /// Freshness of the census the CANDIDATE SET was derived from. The guards
    /// themselves are always re-evaluated against live disk immediately before
    /// removal — this only tells the caller how current the *candidate list*
    /// was, so `pending` (nothing surveyed yet) is never mistaken for
    /// "nothing was reclaimable".
    pub census_status: CensusStatus,
    pub census_age_secs: Option<u64>,
}

// ---------------------------------------------------------------------------
// Path normalization — one id shape, case/separator insensitive on Windows.
// ---------------------------------------------------------------------------

/// Normalize a worktree path into the stable `id` used on the wire:
/// backslashes → forward slashes, lowercased, no trailing slash.
pub fn norm_path(p: &str) -> String {
    let s = p.replace('\\', "/");
    let s = s.trim_end_matches('/');
    s.to_lowercase()
}

// ---------------------------------------------------------------------------
// Survey — GET /agent-worktrees/reclaimable
// ---------------------------------------------------------------------------

/// Facts about one worktree assembled from the census + coord's decision,
/// before classification.
struct Assembled {
    census: WorktreeCensus,
    facts: CandidateFacts,
    junctioned_paths: Vec<String>,
    coord_reason: Option<String>,
}

/// Build the survey: coord's decision ⋈ this device's most recent on-disk
/// census snapshot, with the runner-local G6 guard applied on top.
///
/// ## Why this reads a CACHED census
///
/// The census walk ([`census::build_and_publish`]) can take hours (thousands
/// of worktrees under the workspace root, ~10 `git` spawns per row plus
/// recursive sizing of every non-junctioned `node_modules`/`target`). Running
/// it inline per request made this endpoint hang for the whole walk. It now
/// reads the snapshot the census task publishes — upsert-merged per chunk
/// while a walk is in flight, replaced on walk completion — and REPORTS ITS
/// AGE, never presenting a stale snapshot as live. Removal still re-checks
/// live disk ([`execute_targets`]), so a stale snapshot can never authorize a
/// deletion.
///
/// ## Always answers
///
/// Cold start (no snapshot yet) waits at most [`SurveyQuery::wait`] and then
/// returns `census_status: "pending"` with an empty `items` and an explicit
/// note — an honest "not ready", never a hang and never a misleading
/// "nothing to clean".
///
/// Never errors on a coord failure either — it degrades to
/// `coord_reachable: false` with every item blocked as `coord-unreachable`,
/// the honest fail-safe (no decision ⇒ no eligibility).
pub async fn survey(query: SurveyQuery) -> Result<Survey, String> {
    let previous = census::latest_census();

    // `?refresh=1` — kick a walk in the BACKGROUND. Deliberately not the
    // default path, and never awaited past the bounded wait below.
    if query.refresh_requested() && census::spawn_census_rebuild() {
        info!("agent_worktrees: survey requested an explicit census refresh (background)");
    }

    // coord's decision and the bounded snapshot wait run CONCURRENTLY, so the
    // worst case is max(coord 15s timeout, wait ≤10s), not their sum.
    let wait_for = if query.refresh_requested() {
        // Wait for something strictly NEWER than what we already had.
        previous.as_ref().map(|s| s.taken_at)
    } else {
        // Any snapshot at all; returns instantly when one already exists.
        None
    };
    let (pull, waited) = tokio::join!(
        reclaim::fetch_pull(),
        census::wait_for_census_after(wait_for, query.wait())
    );

    let (device_id, pull, coord_error) = match pull {
        Ok(Some((id, p))) => (Some(id), Some(p), None),
        Ok(None) => (
            None,
            None,
            Some("no device_id / coord_url configured".to_string()),
        ),
        Err(e) => {
            warn!("agent_worktrees: coord reclaim pull failed: {e}");
            (None, None, Some(e))
        }
    };

    // `waited` is the newer snapshot when one arrived in time; otherwise fall
    // back to whatever we already had (possibly still `None` on cold start).
    let snapshot = waited.or_else(census::latest_census).or(previous);

    // The free-space half is read from the 60s volume publisher, NOT from the
    // census snapshot — that is the whole point of decoupling them, and it is
    // why this line has no `?`, no await on coord and no dependency on the
    // walk: `None` here is an honest UNKNOWN, rendered as such.
    let volume_sample = census::latest_volume_sample();

    Ok(assemble_survey(
        snapshot.as_ref(),
        pull.as_ref(),
        device_id,
        coord_error,
        census::census_build_active(),
        volume_sample.as_ref(),
        chrono::Utc::now(),
    ))
}

/// PURE assembly of the survey response from an (optional) census snapshot and
/// an (optional) coord pull. Split out from [`survey`] so the cold-start /
/// freshness behaviour is unit-testable without a disk walk or coord.
fn assemble_survey(
    snapshot: Option<&CensusSnapshot>,
    pull: Option<&ReclaimPull>,
    coord_device_id: Option<Uuid>,
    coord_error: Option<String>,
    census_refreshing: bool,
    volume_sample: Option<&census::VolumeSample>,
    now: chrono::DateTime<chrono::Utc>,
) -> Survey {
    let coord_reachable = pull.is_some();
    let remove_armed = pull.is_some_and(|p| p.remove_armed);
    let rejunction_armed = pull.is_some_and(|p| p.rejunction_armed);

    let Some(snapshot) = snapshot else {
        // Cold start: we do NOT know what is on disk, and we say so.
        return Survey {
            device_id: coord_device_id,
            coord_reachable,
            coord_error,
            poller: reclaim::poller_health(),
            remove_armed,
            rejunction_armed,
            canonical_excluded: 0,
            items: Vec::new(),
            // INV-D1: the census being pending says NOTHING about free space.
            // The disk answer is filled in here exactly as it is on the warm
            // path — a cold start that renders "unknown free space" because
            // an unrelated walk has not finished is the failure this plan
            // exists to remove.
            summary: SurveySummary::default().with_volume_sample(volume_sample, now),
            census_status: CensusStatus::Pending,
            census_taken_at: None,
            census_age_secs: None,
            census_build_ms: None,
            census_refreshing,
            census_note: if census_refreshing {
                "No disk snapshot yet — the first census walk of this session is still \
                 running. This list is not 'nothing to clean up', it is 'not known yet'."
                    .to_string()
            } else {
                "No disk snapshot yet — the census has not completed a walk this session. \
                 This list is not 'nothing to clean up', it is 'not known yet'."
                    .to_string()
            },
        };
    };

    let age_secs = (now - snapshot.taken_at).num_seconds().max(0) as u64;
    let status = if age_secs <= CENSUS_STALE_AFTER_SECS {
        CensusStatus::Fresh
    } else {
        CensusStatus::Stale
    };

    let (items, canonical_excluded) = build_survey_items(&snapshot.req.worktrees, pull);
    let summary = SurveySummary {
        reapable: items.iter().filter(|i| i.status == "reapable").count(),
        blocked: items.iter().filter(|i| i.status == "blocked").count(),
        reclaimable_bytes: items
            .iter()
            .filter(|i| i.status == "reapable")
            .map(|i| i.attributable_bytes)
            .sum(),
        ..Default::default()
    }
    .with_volume_sample(volume_sample, now);

    Survey {
        device_id: Some(snapshot.req.device_id),
        coord_reachable,
        coord_error,
        poller: reclaim::poller_health(),
        remove_armed,
        rejunction_armed,
        canonical_excluded,
        items,
        summary,
        census_status: status,
        census_taken_at: Some(snapshot.taken_at.to_rfc3339()),
        census_age_secs: Some(age_secs),
        census_build_ms: Some(snapshot.build_ms),
        census_refreshing,
        census_note: format!(
            "Disk state as of {} ago{}{}. Removal always re-checks live disk before \
             deleting anything.",
            humanize_secs(age_secs),
            if status == CensusStatus::Stale {
                " (stale — the census walk is overdue)"
            } else {
                ""
            },
            if census_refreshing {
                "; a fresh walk is running now"
            } else {
                ""
            }
        ),
    }
}

/// `95` → `"1m 35s"`. Small, allocation-cheap, and only used for the note.
fn humanize_secs(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{}m {}s", mins, secs % 60);
    }
    format!("{}h {}m", mins / 60, mins % 60)
}

/// True when `path` IS the canonical checkout of `repo` (not a worktree of
/// it). The census enumerates the canonical tree too; it is never a reclaim
/// candidate, so it is filtered out (and counted, so the omission is visible).
///
/// **Structural first, root-derived second — because the root can now move.**
/// Before slice 1 the root was effectively immutable: the hardcoded
/// `D:/qontinui-root` literal answered on every boot, so comparing against
/// [`default_canonical_path`] was as good as comparing against the truth. The
/// root is now resolved (`$QONTINUI_ROOT` → `$QONTINUI_WORKSPACE_ROOT` → the
/// `paths.workspace_root` setting → the executable's ancestry), which opens two
/// ways for a *genuine* canonical checkout to slip past a purely root-derived
/// comparison and into the reclaim candidate list:
///
/// 1. the root does not resolve at all, so there is nothing to compare against;
/// 2. the root resolves *differently* than it did when the census row was taken
///    — and [`survey`] reads a **cached** census, so rows routinely outlive the
///    resolution that produced them.
///
/// [`is_canonical_repo_root`](census::is_canonical_repo_root) settles both: a
/// `.git` **directory** (not a worktree's `.git` file) under a `qontinui-*` name
/// is a canonical clone no matter which root resolved. The root-derived check is
/// kept as a second chance for a canonical checkout that is not on disk right
/// now, and the `Err` arm answers `true` — "cannot rule this out" — because this
/// filter guards against deletion, where the safe answer to an unanswerable
/// question is to exclude the row, never to expose it.
fn is_canonical_checkout(repo: &str, path: &str) -> bool {
    is_canonical_checkout_decision(
        census::is_canonical_repo_root(Path::new(path)),
        default_canonical_path(repo).ok().as_deref(),
        path,
    )
}

/// Pure core of [`is_canonical_checkout`]: the structural verdict and the
/// derived canonical path are injected, so the two dispositions that guard
/// against deleting a canonical checkout are unit-testable without a workspace
/// root, a census, or a real directory tree. `canonical` is `None` when the
/// path could not be derived at all.
fn is_canonical_checkout_decision(structural: bool, canonical: Option<&Path>, path: &str) -> bool {
    if structural {
        return true;
    }
    match canonical {
        Some(c) => norm_path(&c.to_string_lossy()) == norm_path(path),
        None => true,
    }
}

/// PURE assembly + classification of the survey rows. Split out from
/// [`survey`] so it is unit-testable without coord or a disk walk.
fn build_survey_items(
    worktrees: &[WorktreeCensus],
    pull: Option<&ReclaimPull>,
) -> (Vec<SurveyItem>, usize) {
    // coord's cleared `remove` instructions, keyed by normalized path.
    let mut cleared: BTreeMap<String, &ReclaimInstruction> = BTreeMap::new();
    // coord's deferred candidates (forward-compatible; empty on today's coord).
    let mut deferred: BTreeMap<String, (SkipReason, bool)> = BTreeMap::new();
    if let Some(p) = pull {
        for i in &p.instructions {
            if i.action == ReclaimAction::Remove {
                cleared.insert(norm_path(&i.worktree_path), i);
            }
        }
        for b in &p.blocked {
            deferred.insert(
                norm_path(&b.worktree_path),
                (
                    SkipReason::from_coord_token(&b.reason),
                    b.retention.as_deref() == Some("pinned"),
                ),
            );
        }
    }

    let coord_unreachable = pull.is_none();
    let mut canonical_excluded = 0usize;
    let mut assembled: Vec<Assembled> = Vec::new();

    for w in worktrees {
        if is_canonical_checkout(&w.repo, &w.path) {
            canonical_excluded += 1;
            continue;
        }
        let key = norm_path(&w.path);
        let instr = cleared.get(&key).copied();
        let defer = deferred.get(&key).copied();

        let pinned = instr.and_then(|i| i.retention.as_deref()) == Some("pinned")
            || defer.map(|d| d.1).unwrap_or(false);
        // coord's G3 is folded into its defer reason; the runner has no
        // independent session-liveness view here.
        let session_live = defer.map(|d| d.0) == Some(SkipReason::SessionLive);

        let facts = CandidateFacts {
            coord_reapable: instr.is_some(),
            coord_block_reason: defer.map(|d| d.0),
            // Census `is_dirty` is the survey-time G1 signal; the execution
            // path re-runs `git status --porcelain` before touching anything.
            is_dirty: w.is_dirty,
            pinned,
            session_live,
            // Census carries the G6 probe result from the last walk; the
            // execution path re-probes live.
            building: w.building.unwrap_or(false),
            root_exists: true, // enumerated from disk by the census itself
            coord_unreachable,
        };

        assembled.push(Assembled {
            census: w.clone(),
            facts,
            junctioned_paths: instr
                .map(|i| i.junctioned_paths.clone())
                .unwrap_or_else(|| census_junctioned_sinks(w)),
            coord_reason: instr.map(|i| i.reason.clone()),
        });
    }

    let mut items: Vec<SurveyItem> = assembled
        .into_iter()
        .map(|a| {
            let decision = classify(&a.facts);
            let (status, reason, reason_detail) = match decision {
                Decision::Reap => ("reapable", None, None),
                Decision::Skip(r) => ("blocked", Some(r.as_str()), Some(r.detail())),
            };
            SurveyItem {
                id: norm_path(&a.census.path),
                worktree_path: a.census.path.clone(),
                repo: a.census.repo.clone(),
                branch: a.census.branch.clone(),
                status,
                reason,
                reason_detail,
                is_dirty: a.facts.is_dirty,
                building: a.facts.building,
                pinned: a.facts.pinned,
                landed_in_main: a.census.landed_in_main,
                attributable_bytes: a.census.attributable_bytes,
                junctioned_paths: a.junctioned_paths,
                coord_reason: a.coord_reason,
            }
        })
        .collect();

    // Reapable first (that is the actionable set), then biggest-first inside
    // each group so the operator sees the best win at the top.
    items.sort_by(|a, b| {
        (a.status != "reapable")
            .cmp(&(b.status != "reapable"))
            .then(b.attributable_bytes.cmp(&a.attributable_bytes))
            .then(a.id.cmp(&b.id))
    });

    (items, canonical_excluded)
}

/// Junction sinks derivable from a census row alone — used when coord has no
/// instruction for the path (so the panel can still show the operator which
/// dirs are links). Only reparse points are listed: INV-W4 requires unlinking
/// exactly these before any recursive delete.
fn census_junctioned_sinks(w: &WorktreeCensus) -> Vec<String> {
    let mut out = Vec::new();
    if w.nm_present && w.nm_is_junction {
        out.push("node_modules".to_string());
    }
    if w.target_present && w.target_is_junction {
        out.push("target".to_string());
    }
    out
}

// ---------------------------------------------------------------------------
// Execute — POST /agent-worktrees/reclaim
// ---------------------------------------------------------------------------

/// Reclaim the requested (or all) currently-reapable worktrees.
///
/// The survey is ALWAYS re-derived server-side: a client id can only ever
/// *narrow* the set coord cleared, never widen it. Every item then re-passes
/// the local guards against live disk (`root_exists`, `git status --porcelain`
/// for G1, and a fresh G6 build probe) immediately before removal, because the
/// survey the operator looked at may be seconds or minutes old.
///
/// That last sentence is now load-bearing rather than defensive: the survey's
/// disk facts come from the cached census snapshot (see [`survey`]), so the
/// candidate list IS minutes old by construction. It proposes; live disk
/// disposes. `coord`'s half of the decision is never cached — `fetch_pull`
/// runs per call.
pub async fn reclaim_now(req: ReclaimRequest) -> Result<ReclaimOutcome, String> {
    // The candidate set comes from the cached census (bounded, always answers);
    // every guard below is then re-run against LIVE disk in `execute_targets`.
    let survey = survey(SurveyQuery::default()).await?;
    let requested: Option<Vec<String>> = req
        .ids
        .map(|ids| ids.iter().map(|s| norm_path(s)).collect::<Vec<_>>());

    let mut outcome = ReclaimOutcome {
        dry_run: req.dry_run,
        census_status: survey.census_status,
        census_age_secs: survey.census_age_secs,
        ..Default::default()
    };

    // Ids the caller named that are not reapable → explicit skip, never a
    // silent drop.
    if let Some(ids) = &requested {
        for id in ids {
            let known = survey.items.iter().find(|i| &i.id == id);
            match known {
                Some(i) if i.status == "reapable" => {}
                Some(i) => outcome.skipped.push(SkippedItem {
                    id: id.clone(),
                    worktree_path: i.worktree_path.clone(),
                    reason: i.reason.unwrap_or(SkipReason::NotReapable.as_str()),
                    detail: i
                        .reason_detail
                        .unwrap_or(SkipReason::NotReapable.detail())
                        .to_string(),
                }),
                None => outcome.skipped.push(SkippedItem {
                    id: id.clone(),
                    worktree_path: id.clone(),
                    reason: SkipReason::NotReapable.as_str(),
                    detail: format!(
                        "{} (no such worktree in this device's survey)",
                        SkipReason::NotReapable.detail()
                    ),
                }),
            }
        }
    }

    let targets: Vec<SurveyItem> = survey
        .items
        .into_iter()
        .filter(|i| i.status == "reapable")
        .filter(|i| requested.as_ref().is_none_or(|ids| ids.contains(&i.id)))
        .collect();

    if targets.is_empty() {
        return Ok(outcome);
    }

    // Removal is synchronous git + filesystem work; keep it off the async
    // worker (the starvation class the poller already avoids).
    let dry_run = req.dry_run;
    let executed = tokio::task::spawn_blocking(move || execute_targets(targets, dry_run))
        .await
        .map_err(|e| format!("on-demand reclaim panicked: {e}"))?;

    outcome.removed.extend(executed.removed);
    outcome.skipped.extend(executed.skipped);
    Ok(outcome)
}

/// Blocking half of [`reclaim_now`]: re-verify the local guards against live
/// disk, then run the shared INV-W4 removal plan.
fn execute_targets(targets: Vec<SurveyItem>, dry_run: bool) -> ReclaimOutcome {
    let mut outcome = ReclaimOutcome {
        dry_run,
        ..Default::default()
    };

    for item in targets {
        let path = PathBuf::from(&item.worktree_path);

        // --- Live re-check of every runner-owned guard --------------------
        // This is why serving the SURVEY from a cached census is safe: the
        // cache only ever proposes CANDIDATES. Every runner-owned guard below
        // is re-evaluated against live disk right here, immediately before the
        // removal — a stale census can never authorize a deletion.
        let facts = CandidateFacts {
            // coord cleared it in the pull we just made (that half is always
            // live — `fetch_pull` is not cached).
            coord_reapable: true,
            coord_block_reason: None,
            // G1 — the authoritative, execution-time dirty check.
            is_dirty: reclaim::worktree_is_dirty(&path),
            pinned: item.pinned,
            session_live: false,
            // G6 — fresh probe; the survey's value came from the last census.
            building: reclaim::probe_building(&path),
            root_exists: path.exists(),
            coord_unreachable: false,
        };
        if let Decision::Skip(reason) = classify(&facts) {
            info!(
                "agent_worktrees: refusing on-demand reclaim of {} — {}",
                item.worktree_path,
                reason.as_str()
            );
            outcome.skipped.push(SkippedItem {
                id: item.id.clone(),
                worktree_path: item.worktree_path.clone(),
                reason: reason.as_str(),
                detail: reason.detail().to_string(),
            });
            continue;
        }

        if dry_run {
            outcome.removed.push(RemovedItem {
                id: item.id.clone(),
                worktree_path: item.worktree_path.clone(),
                repo: item.repo.clone(),
                freed_bytes: item.attributable_bytes,
                dry_run: true,
            });
            continue;
        }

        // --- The one removal path -----------------------------------------
        // `remove_armed = true` here is CORRECT and is not the silent-loop
        // arming flag: this branch only runs behind an explicit human button
        // press or an agent call on the loopback API, on a set coord already
        // cleared. `plan_reclaim` still applies its own `is_dirty` and
        // absent-root guards, and still orders every junction unlink BEFORE
        // the recursive removal (INV-W4).
        let instr = ReclaimInstruction {
            worktree_path: item.worktree_path.clone(),
            repo: item.repo.clone(),
            action: ReclaimAction::Remove,
            reason: item
                .coord_reason
                .clone()
                .unwrap_or_else(|| "on-demand".to_string()),
            is_dirty: false,
            junctioned_paths: item.junctioned_paths.clone(),
            retention: None,
        };
        let steps: Vec<ReclaimStep> = reclaim::plan_reclaim(
            &instr, /* rejunction_armed */ false, /* remove_armed */ true, None,
            /* root_exists */ true,
        );
        match reclaim::execute_steps(&item.worktree_path, &steps) {
            Ok(()) => {
                info!(
                    "agent_worktrees: reclaimed {} ({} bytes)",
                    item.worktree_path, item.attributable_bytes
                );
                outcome.removed.push(RemovedItem {
                    id: item.id.clone(),
                    worktree_path: item.worktree_path.clone(),
                    repo: item.repo.clone(),
                    freed_bytes: item.attributable_bytes,
                    dry_run: false,
                });
            }
            Err(e) => {
                warn!(
                    "agent_worktrees: reclaim of {} failed: {e}",
                    item.worktree_path
                );
                outcome.skipped.push(SkippedItem {
                    id: item.id.clone(),
                    worktree_path: item.worktree_path.clone(),
                    reason: "error",
                    detail: e,
                });
            }
        }
    }

    outcome
}

/// Convenience for callers holding a `&Path`.
pub fn path_id(p: &Path) -> String {
    norm_path(&p.to_string_lossy())
}

// ---------------------------------------------------------------------------
// Tests — the guard vocabulary is the load-bearing assertion.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A clean, coord-cleared, unpinned, idle worktree — the only shape that
    /// may be reclaimed.
    fn clean_cleared() -> CandidateFacts {
        CandidateFacts {
            coord_reapable: true,
            coord_block_reason: None,
            is_dirty: false,
            pinned: false,
            session_live: false,
            building: false,
            root_exists: true,
            coord_unreachable: false,
        }
    }

    #[test]
    fn coord_reapable_clean_item_is_accepted() {
        assert_eq!(classify(&clean_cleared()), Decision::Reap);
    }

    #[test]
    fn dirty_item_is_refused_with_dirty_reason() {
        let f = CandidateFacts {
            is_dirty: true,
            ..clean_cleared()
        };
        assert_eq!(classify(&f), Decision::Skip(SkipReason::Dirty));
        assert_eq!(SkipReason::Dirty.as_str(), "dirty");
    }

    #[test]
    fn pinned_item_is_refused_with_pinned_reason() {
        let f = CandidateFacts {
            pinned: true,
            ..clean_cleared()
        };
        assert_eq!(classify(&f), Decision::Skip(SkipReason::Pinned));
        assert_eq!(SkipReason::Pinned.as_str(), "pinned");
    }

    #[test]
    fn session_live_item_is_refused_with_session_live_reason() {
        let f = CandidateFacts {
            session_live: true,
            ..clean_cleared()
        };
        assert_eq!(classify(&f), Decision::Skip(SkipReason::SessionLive));
        assert_eq!(SkipReason::SessionLive.as_str(), "session-live");
    }

    #[test]
    fn build_in_flight_item_is_refused_with_building_reason() {
        // G6 — the runner-only guard coord's cleared set cannot account for.
        let f = CandidateFacts {
            building: true,
            ..clean_cleared()
        };
        assert_eq!(classify(&f), Decision::Skip(SkipReason::Building));
        assert_eq!(SkipReason::Building.as_str(), "building");
    }

    #[test]
    fn coord_clearance_never_overrides_a_runner_guard() {
        // The whole point of keeping BOTH layers: coord said yes, the runner
        // says no, and the runner wins — for every runner-owned guard.
        for (mutate, expected) in [
            (
                CandidateFacts {
                    is_dirty: true,
                    ..clean_cleared()
                },
                SkipReason::Dirty,
            ),
            (
                CandidateFacts {
                    pinned: true,
                    ..clean_cleared()
                },
                SkipReason::Pinned,
            ),
            (
                CandidateFacts {
                    session_live: true,
                    ..clean_cleared()
                },
                SkipReason::SessionLive,
            ),
            (
                CandidateFacts {
                    building: true,
                    ..clean_cleared()
                },
                SkipReason::Building,
            ),
            (
                CandidateFacts {
                    root_exists: false,
                    ..clean_cleared()
                },
                SkipReason::Absent,
            ),
        ] {
            assert!(mutate.coord_reapable, "coord cleared it");
            assert_eq!(classify(&mutate), Decision::Skip(expected));
        }
    }

    #[test]
    fn dirty_outranks_every_other_guard() {
        // G1 is inviolable and reported FIRST, so the operator always sees
        // "commit your work" rather than a lesser reason.
        let f = CandidateFacts {
            is_dirty: true,
            pinned: true,
            session_live: true,
            building: true,
            ..clean_cleared()
        };
        assert_eq!(classify(&f), Decision::Skip(SkipReason::Dirty));
    }

    #[test]
    fn uncleared_item_is_refused_with_coords_reason() {
        for (token, expected) in [
            ("not_landed", SkipReason::NotLanded),
            ("other_live_reference", SkipReason::SessionLive),
            ("serialize_claim_active", SkipReason::MainMerge),
            ("grace_pending", SkipReason::Grace),
            ("not_a_candidate", SkipReason::NotACandidate),
            ("dirty", SkipReason::Dirty),
            ("pinned", SkipReason::Pinned),
            ("something-new", SkipReason::NotCleared),
        ] {
            assert_eq!(SkipReason::from_coord_token(token), expected, "{token}");
        }
        let f = CandidateFacts {
            coord_reapable: false,
            coord_block_reason: Some(SkipReason::NotLanded),
            ..clean_cleared()
        };
        assert_eq!(classify(&f), Decision::Skip(SkipReason::NotLanded));
    }

    #[test]
    fn uncleared_without_a_reason_is_not_cleared_not_reapable() {
        // Today's coord ships only its cleared set, so an unlisted worktree
        // has no typed reason. It must read as an honest UNKNOWN, never as
        // "safe".
        let f = CandidateFacts {
            coord_reapable: false,
            coord_block_reason: None,
            ..clean_cleared()
        };
        assert_eq!(classify(&f), Decision::Skip(SkipReason::NotCleared));
    }

    #[test]
    fn coord_unreachable_blocks_everything() {
        let f = CandidateFacts {
            coord_reapable: false,
            coord_unreachable: true,
            ..clean_cleared()
        };
        assert_eq!(classify(&f), Decision::Skip(SkipReason::CoordUnreachable));
    }

    #[test]
    fn norm_path_is_separator_and_case_insensitive() {
        assert_eq!(
            norm_path("D:\\qontinui-root\\Qontinui-Runner-wt-foo\\"),
            "d:/qontinui-root/qontinui-runner-wt-foo"
        );
        assert_eq!(
            norm_path("D:/qontinui-root/qontinui-runner-wt-foo"),
            norm_path("d:\\qontinui-root\\qontinui-runner-wt-foo")
        );
    }

    fn census_row(path: &str, dirty: bool, building: Option<bool>) -> WorktreeCensus {
        WorktreeCensus {
            repo: "qontinui-runner".to_string(),
            path: path.to_string(),
            branch: Some("feat/x".to_string()),
            head_sha: None,
            head_age_secs: None,
            is_dirty: dirty,
            nm_present: true,
            nm_is_junction: true,
            nm_bytes: 0,
            target_present: true,
            target_is_junction: false,
            target_bytes: 4096,
            last_access_mtime: None,
            attributable_bytes: 4096,
            landed_in_main: Some(true),
            building,
            canonical_current_branch: None,
            canonical_is_dirty: None,
            canonical_base_divergence: None,
        }
    }

    fn pull_with(instructions: Vec<ReclaimInstruction>) -> ReclaimPull {
        serde_json::from_value(serde_json::json!({
            "remove_armed": false,
            "rejunction_armed": false,
            "instructions": instructions
                .iter()
                .map(|i| serde_json::json!({
                    "worktree_path": i.worktree_path,
                    "repo": i.repo,
                    "action": "remove",
                    "reason": i.reason,
                    "is_dirty": i.is_dirty,
                    "junctioned_paths": i.junctioned_paths,
                }))
                .collect::<Vec<_>>(),
        }))
        .expect("pull fixture parses")
    }

    fn instruction(path: &str) -> ReclaimInstruction {
        ReclaimInstruction {
            worktree_path: path.to_string(),
            repo: "qontinui-runner".to_string(),
            action: ReclaimAction::Remove,
            reason: "worktree:lifecycle:pr_merged".to_string(),
            is_dirty: false,
            junctioned_paths: vec!["node_modules".to_string()],
            retention: None,
        }
    }

    #[test]
    fn survey_marks_cleared_clean_worktree_reapable() {
        let wt = "D:/qontinui-root/qontinui-runner-wt-a";
        let pull = pull_with(vec![instruction(wt)]);
        let (items, canonical) =
            build_survey_items(&[census_row(wt, false, Some(false))], Some(&pull));
        assert_eq!(canonical, 0);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, "reapable");
        assert_eq!(items[0].reason, None);
        assert_eq!(
            items[0].coord_reason.as_deref(),
            Some("worktree:lifecycle:pr_merged")
        );
        // Junction sinks come from coord's instruction (INV-W4 input).
        assert_eq!(items[0].junctioned_paths, vec!["node_modules".to_string()]);
    }

    #[test]
    fn survey_blocks_a_building_worktree_coord_cleared() {
        // The G6 case: coord cleared it (it cannot see builds), the runner
        // refuses it and SAYS SO.
        let wt = "D:/qontinui-root/qontinui-runner-wt-b";
        let pull = pull_with(vec![instruction(wt)]);
        let (items, _) = build_survey_items(&[census_row(wt, false, Some(true))], Some(&pull));
        assert_eq!(items[0].status, "blocked");
        assert_eq!(items[0].reason, Some("building"));
        assert!(items[0].reason_detail.unwrap().contains("build"));
    }

    #[test]
    fn survey_blocks_a_dirty_worktree_coord_cleared() {
        let wt = "D:/qontinui-root/qontinui-runner-wt-c";
        let pull = pull_with(vec![instruction(wt)]);
        let (items, _) = build_survey_items(&[census_row(wt, true, Some(false))], Some(&pull));
        assert_eq!(items[0].status, "blocked");
        assert_eq!(items[0].reason, Some("dirty"));
    }

    #[test]
    fn survey_blocks_everything_when_coord_is_unreachable() {
        let wt = "D:/qontinui-root/qontinui-runner-wt-d";
        let (items, _) = build_survey_items(&[census_row(wt, false, Some(false))], None);
        assert_eq!(items[0].status, "blocked");
        assert_eq!(items[0].reason, Some("coord-unreachable"));
    }

    #[test]
    fn survey_blocks_a_pinned_worktree_from_coords_blocked_list() {
        // Forward-compat: once coord ships Phase-1 `blocked`, a pinned entry
        // renders as `pinned`, not as an opaque "not cleared".
        let wt = "D:/qontinui-root/qontinui-runner-wt-e";
        let pull: ReclaimPull = serde_json::from_value(serde_json::json!({
            "instructions": [],
            "blocked": [{
                "worktree_path": wt,
                "repo": "qontinui-runner",
                "reason": "not_a_candidate",
                "retention": "pinned",
            }],
        }))
        .unwrap();
        let (items, _) = build_survey_items(&[census_row(wt, false, Some(false))], Some(&pull));
        assert_eq!(items[0].status, "blocked");
        assert_eq!(items[0].reason, Some("pinned"));
        assert!(items[0].pinned);
    }

    #[test]
    fn survey_surfaces_coord_defer_reasons_when_present() {
        let wt = "D:/qontinui-root/qontinui-runner-wt-f";
        let pull: ReclaimPull = serde_json::from_value(serde_json::json!({
            "instructions": [],
            "blocked": [{
                "worktree_path": wt,
                "repo": "qontinui-runner",
                "reason": "other_live_reference",
            }],
        }))
        .unwrap();
        let (items, _) = build_survey_items(&[census_row(wt, false, Some(false))], Some(&pull));
        assert_eq!(items[0].status, "blocked");
        assert_eq!(items[0].reason, Some("session-live"));
    }

    #[test]
    fn survey_sorts_reapable_first_then_biggest() {
        let a = "D:/qontinui-root/qontinui-runner-wt-small";
        let b = "D:/qontinui-root/qontinui-runner-wt-big";
        let blocked = "D:/qontinui-root/qontinui-runner-wt-dirty";
        let pull = pull_with(vec![instruction(a), instruction(b)]);
        let mut small = census_row(a, false, Some(false));
        small.attributable_bytes = 10;
        let mut big = census_row(b, false, Some(false));
        big.attributable_bytes = 1000;
        let dirty = census_row(blocked, true, Some(false));
        let (items, _) = build_survey_items(&[small, dirty, big], Some(&pull));
        assert_eq!(items[0].worktree_path, b);
        assert_eq!(items[1].worktree_path, a);
        assert_eq!(items[2].status, "blocked");
    }

    #[test]
    fn survey_excludes_canonical_checkouts_and_counts_them() {
        let canonical = default_canonical_path("qontinui-runner").unwrap();
        let canonical_str = canonical.to_string_lossy().to_string();
        let wt = "D:/qontinui-root/qontinui-runner-wt-g";
        let (items, excluded) = build_survey_items(
            &[
                census_row(&canonical_str, false, Some(false)),
                census_row(wt, false, Some(false)),
            ],
            None,
        );
        assert_eq!(excluded, 1, "the canonical checkout must be filtered out");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].worktree_path, wt);
    }

    // -----------------------------------------------------------------
    // The canonical-checkout guard's two non-happy dispositions. Both
    // became reachable in slice 1 Phase 3, when the workspace root stopped
    // being a hardcoded literal that answered identically on every boot.
    // A wrong answer here puts a canonical checkout on the reclaim
    // candidate list, so each arm gets its own assertion.
    // -----------------------------------------------------------------

    /// An underivable canonical path is UNKNOWN, not "no". The safe answer for
    /// a deletion filter is to exclude the row.
    #[test]
    fn canonical_guard_excludes_the_row_when_the_path_cannot_be_derived() {
        assert!(
            is_canonical_checkout_decision(false, None, "D:/qontinui-root/qontinui-runner"),
            "an unresolvable workspace root must never expose a row as reclaimable"
        );
    }

    /// The regression this guard's structural arm exists for: the census row was
    /// taken under one root and the survey reads it under another, so the
    /// root-derived comparison misses a checkout that is genuinely canonical.
    #[test]
    fn canonical_guard_survives_a_root_that_moved_between_census_and_survey() {
        let taken_under = "D:/qontinui-root/qontinui-runner";
        let derived_now = PathBuf::from("E:/elsewhere/qontinui-runner");

        // Root-derived alone: the mismatch reads as "not canonical" — the row
        // would enter the candidate list.
        assert!(!is_canonical_checkout_decision(
            false,
            Some(&derived_now),
            taken_under
        ));

        // The structural verdict (`.git` is a DIRECTORY under a `qontinui-*`
        // name) is root-independent, so it rescues exactly that case.
        assert!(is_canonical_checkout_decision(
            true,
            Some(&derived_now),
            taken_under
        ));
    }

    /// The ordinary path still works, and separator/case differences between the
    /// census row and the derived path do not defeat it.
    #[test]
    fn canonical_guard_matches_the_derived_path_across_separator_and_case() {
        let derived = PathBuf::from("D:\\qontinui-root\\Qontinui-Runner");
        assert!(is_canonical_checkout_decision(
            false,
            Some(&derived),
            "d:/qontinui-root/qontinui-runner"
        ));
    }

    // -----------------------------------------------------------------
    // Census-cache regression tests — the ship-blocking hang this fixes.
    //
    // `survey()` used to call `census::build_census` inline on every
    // request. On a real machine that walk is MINUTES (~400 worktrees ×
    // ~10 git spawns + recursive sizing), so `GET
    // /agent-worktrees/reclaimable` hung indefinitely. These lock in the
    // two properties that fix it: the survey reads a CACHED snapshot and
    // never starts a walk, and it always answers even with no snapshot.
    // -----------------------------------------------------------------

    fn census_req(rows: Vec<WorktreeCensus>) -> census::WorktreeCensusReq {
        census::WorktreeCensusReq {
            device_id: Uuid::nil(),
            tenant_id: None,
            volumes: Vec::new(),
            worktrees: rows,
        }
    }

    /// A snapshot taken exactly `age` before `now` — exact, so the rendered
    /// "as of" label is deterministic.
    fn snapshot_of(
        rows: Vec<WorktreeCensus>,
        now: chrono::DateTime<chrono::Utc>,
        age: chrono::Duration,
    ) -> CensusSnapshot {
        CensusSnapshot {
            req: std::sync::Arc::new(census_req(rows)),
            taken_at: now - age,
            build_ms: 802_431,
        }
    }

    #[tokio::test]
    async fn survey_reads_the_cached_census_and_never_starts_a_walk() {
        // Publish a snapshot the way the periodic census task would…
        let wt = "D:/qontinui-root/qontinui-runner-wt-cached";
        census::publish_census_for_test(census_req(vec![census_row(wt, false, Some(false))]));

        let walks_before = census::census_builds_started();
        let started = std::time::Instant::now();
        let s = survey(SurveyQuery::default())
            .await
            .expect("survey must always answer");
        let elapsed = started.elapsed();

        // …and the survey must SERVE it without rebuilding the census.
        assert_eq!(
            census::census_builds_started(),
            walks_before,
            "the survey must not trigger a census walk — that is the hang"
        );
        // Bounded: coord's pull is capped at 15s, the snapshot wait at 10s,
        // and they run concurrently.
        assert!(
            elapsed < std::time::Duration::from_secs(25),
            "survey took {elapsed:?} — it must answer promptly"
        );
        assert_ne!(s.census_status, CensusStatus::Pending);
        assert!(s.census_taken_at.is_some());
        assert!(s.census_age_secs.is_some());
        assert!(s.items.iter().any(|i| i.worktree_path == wt));
    }

    #[tokio::test]
    async fn bounded_wait_returns_even_when_no_snapshot_ever_arrives() {
        // A snapshot strictly newer than "now + 1h" can never arrive, so this
        // exercises the pure timeout arm deterministically.
        let unreachable = chrono::Utc::now() + chrono::Duration::hours(1);
        let started = std::time::Instant::now();
        let got =
            census::wait_for_census_after(Some(unreachable), Duration::from_millis(150)).await;
        let elapsed = started.elapsed();
        assert!(got.is_none(), "no such snapshot can exist");
        assert!(
            elapsed < Duration::from_secs(3),
            "the wait must be bounded, took {elapsed:?}"
        );
    }

    #[test]
    fn cold_start_without_a_snapshot_is_pending_and_says_so() {
        // The endpoint must ALWAYS answer — and an empty list before the first
        // walk must read as "not known yet", never as "nothing to clean up".
        let s = assemble_survey(None, None, None, None, true, None, chrono::Utc::now());
        assert_eq!(s.census_status, CensusStatus::Pending);
        assert!(s.items.is_empty());
        assert_eq!(s.summary.reapable, 0);
        assert!(s.census_taken_at.is_none());
        assert!(s.census_age_secs.is_none());
        assert!(s.census_refreshing);
        assert!(
            s.census_note.contains("not known yet"),
            "the note must not imply an empty disk: {}",
            s.census_note
        );
    }

    fn volume_sample(
        age: chrono::Duration,
        now: chrono::DateTime<chrono::Utc>,
    ) -> census::VolumeSample {
        census::VolumeSample {
            volumes: vec![
                census::VolumeReport {
                    volume: "C:".to_string(),
                    total_bytes: 1_000,
                    free_bytes: 400,
                },
                census::VolumeReport {
                    volume: "D:".to_string(),
                    total_bytes: 4_000,
                    free_bytes: 93,
                },
            ],
            taken_at: now - age,
        }
    }

    /// INV-D1, the central rule: measurement is never gated on the conditions
    /// that gate deletion. A cold start — no census, nothing known about what
    /// is reclaimable — must STILL answer "how much disk is left".
    #[test]
    fn a_cold_start_census_still_answers_the_free_space_question() {
        let now = chrono::Utc::now();
        let sample = volume_sample(chrono::Duration::seconds(30), now);

        let s = assemble_survey(None, None, None, None, true, Some(&sample), now);

        // The reclaim half is honestly unknown…
        assert_eq!(s.census_status, CensusStatus::Pending);
        assert!(s.items.is_empty());
        // …and the disk half answers anyway.
        assert_eq!(s.summary.volumes_status, CensusStatus::Fresh);
        assert_eq!(s.summary.volumes.len(), 2);
        assert_eq!(s.summary.free_bytes_total, Some(493));
        assert_eq!(s.summary.total_bytes_total, Some(5_000));
        assert_eq!(s.summary.volumes_age_secs, Some(30));
        assert!(s.summary.volumes_observed_at.is_some());
        assert!(
            s.summary.volumes_note.contains("2 volumes"),
            "{}",
            s.summary.volumes_note
        );
    }

    /// No sample yet must read as UNKNOWN — never as a machine with zero free
    /// bytes, and never as one with nothing to report.
    #[test]
    fn an_unsampled_volume_reading_is_unknown_never_a_fabricated_zero() {
        let now = chrono::Utc::now();
        let s = assemble_survey(None, None, None, None, false, None, now);

        assert_eq!(s.summary.volumes_status, CensusStatus::Pending);
        assert!(s.summary.volumes.is_empty());
        assert_eq!(
            s.summary.free_bytes_total, None,
            "absent free space must be None, never Some(0)"
        );
        assert_eq!(s.summary.total_bytes_total, None);
        assert_eq!(s.summary.volumes_age_secs, None);
        assert_eq!(s.summary.volumes_observed_at, None);
        assert!(
            s.summary.volumes_note.contains("UNKNOWN"),
            "the note must say the reading is unknown: {}",
            s.summary.volumes_note
        );
    }

    /// A sample older than the publisher's cadence is still SHOWN — flagged,
    /// not hidden, and not silently rendered as current.
    #[test]
    fn a_stale_volume_sample_is_flagged_and_still_shown() {
        let now = chrono::Utc::now();
        let sample = volume_sample(chrono::Duration::seconds(1_800), now);
        let s = assemble_survey(None, None, None, None, false, Some(&sample), now);

        assert_eq!(s.summary.volumes_status, CensusStatus::Stale);
        assert_eq!(
            s.summary.volumes.len(),
            2,
            "stale data is flagged, not hidden"
        );
        assert_eq!(s.summary.free_bytes_total, Some(493));
        assert!(
            s.summary.volumes_note.contains("stale"),
            "{}",
            s.summary.volumes_note
        );
    }

    /// The two freshness clocks are INDEPENDENT: an hours-old census walk
    /// alongside a 30s-old volume sample is the normal steady state after
    /// Phase 1, and conflating them is the defect Phase 1 removes.
    #[test]
    fn the_volume_clock_is_independent_of_the_census_clock() {
        let wt = "D:/qontinui-root/qontinui-runner-wt-clocks";
        let now = chrono::Utc::now();
        let old_census = snapshot_of(
            vec![census_row(wt, false, Some(false))],
            now,
            chrono::Duration::seconds(7_200),
        );
        let fresh_volumes = volume_sample(chrono::Duration::seconds(15), now);

        let s = assemble_survey(
            Some(&old_census),
            None,
            None,
            None,
            false,
            Some(&fresh_volumes),
            now,
        );
        assert_eq!(s.census_status, CensusStatus::Stale);
        assert_eq!(s.census_age_secs, Some(7_200));
        assert_eq!(s.summary.volumes_status, CensusStatus::Fresh);
        assert_eq!(s.summary.volumes_age_secs, Some(15));
    }

    #[test]
    fn survey_reports_snapshot_age_and_flags_a_stale_one() {
        let wt = "D:/qontinui-root/qontinui-runner-wt-aged";
        let now = chrono::Utc::now();

        let fresh = snapshot_of(
            vec![census_row(wt, false, Some(false))],
            now,
            chrono::Duration::seconds(120),
        );
        let s = assemble_survey(Some(&fresh), None, None, None, false, None, now);
        assert_eq!(s.census_status, CensusStatus::Fresh);
        assert_eq!(s.census_age_secs, Some(120));
        assert!(s.census_note.contains("2m 0s ago"), "{}", s.census_note);
        assert_eq!(s.census_build_ms, Some(802_431));

        let old = snapshot_of(
            vec![census_row(wt, false, Some(false))],
            now,
            chrono::Duration::seconds(1800),
        );
        let s = assemble_survey(Some(&old), None, None, None, false, None, now);
        assert_eq!(s.census_status, CensusStatus::Stale);
        assert!(s.census_note.contains("stale"), "{}", s.census_note);
        // Stale data is still SHOWN — flagged, not hidden.
        assert_eq!(s.items.len(), 1);
    }

    #[test]
    fn refresh_is_opt_in_and_the_wait_is_capped() {
        assert!(!SurveyQuery::default().refresh_requested());
        for v in ["1", "true", "yes", "on", ""] {
            let q = SurveyQuery {
                refresh: Some(v.to_string()),
                wait_secs: None,
            };
            assert!(q.refresh_requested(), "{v}");
        }
        let q = SurveyQuery {
            refresh: Some("0".to_string()),
            wait_secs: Some(9_999),
        };
        assert!(!q.refresh_requested());
        assert_eq!(q.wait(), Duration::from_secs(MAX_CENSUS_WAIT_SECS));
        assert_eq!(
            SurveyQuery::default().wait(),
            Duration::from_secs(DEFAULT_CENSUS_WAIT_SECS)
        );
    }

    #[test]
    fn removal_rechecks_live_disk_and_refuses_a_vanished_worktree() {
        // The safety invariant a cached census must never weaken: the CANDIDATE
        // set may be minutes old, but `execute_targets` re-evaluates every
        // runner guard against LIVE disk before deleting. A path that no longer
        // exists is refused with `absent` — never removed on the cache's word.
        let gone = "D:/qontinui-root/qontinui-runner-wt-vanished-9d3f1a";
        assert!(!Path::new(gone).exists(), "test fixture must not exist");
        let item = SurveyItem {
            id: norm_path(gone),
            worktree_path: gone.to_string(),
            repo: "qontinui-runner".to_string(),
            branch: None,
            status: "reapable", // the cached survey said GO…
            reason: None,
            reason_detail: None,
            is_dirty: false,
            building: false,
            pinned: false,
            landed_in_main: Some(true),
            attributable_bytes: 4096,
            junctioned_paths: Vec::new(),
            coord_reason: Some("worktree:lifecycle:pr_merged".to_string()),
        };
        let outcome = execute_targets(vec![item], /* dry_run */ false);
        // …and the live re-check said NO.
        assert!(outcome.removed.is_empty());
        assert_eq!(outcome.skipped.len(), 1);
        assert_eq!(outcome.skipped[0].reason, "absent");
        assert!(!outcome.skipped[0].detail.is_empty(), "never a silent drop");
    }

    #[test]
    fn humanize_secs_renders_the_as_of_label() {
        assert_eq!(humanize_secs(0), "0s");
        assert_eq!(humanize_secs(59), "59s");
        assert_eq!(humanize_secs(95), "1m 35s");
        assert_eq!(humanize_secs(3_600), "1h 0m");
        assert_eq!(humanize_secs(7_860), "2h 11m");
    }

    #[test]
    fn skip_reasons_all_have_distinct_tokens_and_details() {
        let all = [
            SkipReason::Dirty,
            SkipReason::Pinned,
            SkipReason::SessionLive,
            SkipReason::Building,
            SkipReason::NotLanded,
            SkipReason::MainMerge,
            SkipReason::Grace,
            SkipReason::NotACandidate,
            SkipReason::NotCleared,
            SkipReason::CoordUnreachable,
            SkipReason::Absent,
            SkipReason::NotReapable,
        ];
        let mut tokens: Vec<&str> = all.iter().map(|r| r.as_str()).collect();
        tokens.sort_unstable();
        let unique = {
            let mut t = tokens.clone();
            t.dedup();
            t
        };
        assert_eq!(tokens.len(), unique.len(), "reason tokens must be distinct");
        for r in all {
            assert!(!r.detail().is_empty(), "{:?} needs a detail sentence", r);
        }
    }
}
