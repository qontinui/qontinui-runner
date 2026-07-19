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

use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use super::canonical_paths::default_canonical_path;
use super::census::{self, WorktreeCensus};
use super::reclaim::{self, ReclaimAction, ReclaimInstruction, ReclaimPull, ReclaimStep};

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

/// Aggregate counts for the panel header.
#[derive(Debug, Clone, Serialize, Default)]
pub struct SurveySummary {
    pub reapable: usize,
    pub blocked: usize,
    /// Bytes that pressing "Clean up safe worktrees" would free.
    pub reclaimable_bytes: u64,
}

/// Body of `GET /agent-worktrees/reclaimable`.
#[derive(Debug, Clone, Serialize)]
pub struct Survey {
    pub device_id: Option<Uuid>,
    /// `false` when coord could not be reached — the panel says so instead of
    /// implying "nothing to clean".
    pub coord_reachable: bool,
    /// Present when `coord_reachable == false`.
    pub coord_error: Option<String>,
    /// Echo of coord's arming flags — informational only. The on-demand path
    /// deliberately does NOT depend on them.
    pub remove_armed: bool,
    pub rejunction_armed: bool,
    /// Canonical repo checkouts filtered out of the list (never reclaimable).
    /// Reported as a count so the omission is visible, not silent.
    pub canonical_excluded: usize,
    pub items: Vec<SurveyItem>,
    pub summary: SurveySummary,
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

/// Build the survey: coord's decision ⋈ this device's on-disk census, with
/// the runner-local G6 guard applied on top.
///
/// Never errors on a coord failure — it degrades to `coord_reachable: false`
/// with every item blocked as `coord-unreachable`, which is the honest
/// fail-safe (no decision ⇒ no eligibility).
pub async fn survey() -> Result<Survey, String> {
    // coord's decision first (async), then the disk walk (blocking).
    let pull = reclaim::fetch_pull().await;
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

    let census_req = tokio::task::spawn_blocking(census::build_census)
        .await
        .map_err(|e| format!("census walk panicked: {e}"))?;
    let Some(census_req) = census_req else {
        return Ok(Survey {
            device_id,
            coord_reachable: pull.is_some(),
            coord_error,
            remove_armed: pull.as_ref().is_some_and(|p| p.remove_armed),
            rejunction_armed: pull.as_ref().is_some_and(|p| p.rejunction_armed),
            canonical_excluded: 0,
            items: Vec::new(),
            summary: SurveySummary::default(),
        });
    };

    let (items, canonical_excluded) = build_survey_items(&census_req.worktrees, pull.as_ref());
    let summary = SurveySummary {
        reapable: items.iter().filter(|i| i.status == "reapable").count(),
        blocked: items.iter().filter(|i| i.status == "blocked").count(),
        reclaimable_bytes: items
            .iter()
            .filter(|i| i.status == "reapable")
            .map(|i| i.attributable_bytes)
            .sum(),
    };

    Ok(Survey {
        device_id: Some(census_req.device_id),
        coord_reachable: pull.is_some(),
        coord_error,
        remove_armed: pull.as_ref().is_some_and(|p| p.remove_armed),
        rejunction_armed: pull.as_ref().is_some_and(|p| p.rejunction_armed),
        canonical_excluded,
        items,
        summary,
    })
}

/// True when `path` IS the canonical checkout of `repo` (not a worktree of
/// it). The census enumerates the canonical tree too; it is never a reclaim
/// candidate, so it is filtered out (and counted, so the omission is visible).
fn is_canonical_checkout(repo: &str, path: &str) -> bool {
    match default_canonical_path(repo) {
        Ok(canonical) => norm_path(&canonical.to_string_lossy()) == norm_path(path),
        Err(_) => false,
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
pub async fn reclaim_now(req: ReclaimRequest) -> Result<ReclaimOutcome, String> {
    let survey = survey().await?;
    let requested: Option<Vec<String>> = req
        .ids
        .map(|ids| ids.iter().map(|s| norm_path(s)).collect::<Vec<_>>());

    let mut outcome = ReclaimOutcome {
        dry_run: req.dry_run,
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
        let facts = CandidateFacts {
            coord_reapable: true, // coord cleared it in the survey we just built
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
