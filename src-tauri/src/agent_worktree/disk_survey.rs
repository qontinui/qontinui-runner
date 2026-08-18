//! Runner-local **disk-reclaim survey** — the read-only preview behind
//! `GET /disk/reclaimable`.
//!
//! Plan: `plans/2026-08-07-product-disk-monitoring-and-cleanup.md`, Phase 2.
//!
//! ## INV-D1 — measurement and preview are NEVER gated on the deletion gates
//!
//! The defect this module exists to make unrepeatable: `cargo-sweep-all.ps1
//! -WhatIf` aborts in **preflight** (`exit 2`) whenever any `cargo` / `rustc` /
//! `rust-analyzer` process is running, so the read-only discovery report is
//! unobtainable during a disk emergency — precisely when it is needed. A
//! preview that refuses to render is indistinguishable, to the person reading
//! it, from *"nothing to reclaim"*, which is the worst possible answer.
//!
//! So, concretely, **nothing on this path consults a global build-in-flight
//! condition, an arming flag, coord, or any other precondition before
//! answering**:
//!
//! * the survey calls [`super::orphan_target_reaper::classify_candidate`] — the SAME
//!   classifier the reaper's destructive cycle calls — in report mode, so the
//!   preview cannot drift from what the verb would actually do;
//! * a root with a build in flight comes back as a per-item
//!   `blocked: building`, never as a global refusal;
//! * free space ([`SurveySummary::volumes`]) rides the decoupled 60 s volume
//!   publisher, so "how much room is left?" answers on the cold-start path,
//!   mid-walk, mid-build and with coord unreachable.
//!
//! The regression test for this is
//! `survey_renders_with_a_build_in_flight_and_blocks_only_that_root`.
//!
//! ## Bounded by construction
//!
//! Sizing 244 target roots holding terabytes is a multi-minute walk, so — like
//! [`super::on_demand`]'s worktree survey, which learned this the hard way —
//! the route **serves a snapshot** published by a periodic background task
//! rather than rebuilding one per request. `?refresh=1` kicks a rebuild in the
//! background; `?waitSecs=N` (capped at [`MAX_WAIT_SECS`]) is a bounded wait for
//! one. The request always answers.
//!
//! ## Honesty contract (four distinguishable states, never collapsed)
//!
//! | State | `census_status` | `items` | byte totals |
//! |---|---|---|---|
//! | no walk has completed yet | `pending` | `[]` | `null` |
//! | the survey could not run (no workspace root) | `unavailable` | `[]` | `null`, with the REASON in `census_note` |
//! | a walk completed, saw the whole tree, and found nothing | `fresh` | `[]` | `0` |
//! | a walk completed but could NOT see the whole tree, and found nothing | `fresh` | `[]` | `null`, with the gaps in `census_note` + `scan` |
//! | a walk completed | `fresh` / `stale` | items | measured |
//!
//! A read that FAILED and a population that is genuinely EMPTY never render the
//! same, byte totals are `null` rather than `0` whenever they are unknown, and a
//! truncated walk or an unreadable subtree is reported
//! ([`ScanStats`]) instead of silently under-counting.
//!
//! Row four is the one that was wrong until the Phase 2 review: the empty branch
//! consulted neither `read_errors` nor `truncated`, and `sum_bytes` over an
//! empty iterator returns `Some(0)`. A failed `read_dir(root)` — detached drive,
//! permissions change, a bad `paths.workspace_root` — therefore produced a
//! `fresh` census whose every total was a hard zero, under a note asserting the
//! zero had been *measured*. See [`ScanStats::incomplete`].

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::census;
use super::on_demand::{humanize_secs, volume_headroom, CensusStatus};
use super::orphan_target_reaper::{
    self as reaper, Candidate, ClassifyOptions, DirtyProbe, SkipReason, TargetClass,
};

/// Env override for the survey interval (seconds). Default 3600 s, floored
/// 300 s — the walk sizes every cargo target root on the machine, so a tight
/// cadence would spend the disk it is trying to save.
pub const SURVEY_INTERVAL_SECS_ENV: &str = "QONTINUI_DISK_SURVEY_INTERVAL_SECS";
const DEFAULT_SURVEY_INTERVAL_SECS: u64 = 3_600;
const MIN_SURVEY_INTERVAL_SECS: u64 = 300;

/// Default bounded wait for a snapshot. Small on purpose — a walk takes
/// minutes, so waiting for one is never the plan.
const DEFAULT_WAIT_SECS: u64 = 2;
/// Hard ceiling on the caller-supplied `?waitSecs=`.
const MAX_WAIT_SECS: u64 = 10;

/// The verb that would remove a reclaimable root today. Rendered per item and
/// per class so a class with **no** verb is visibly different from one that
/// merely has nothing reclaimable right now.
const VERB_ORPHAN_REAPER: &str = "orphan-target-reaper";

fn survey_interval() -> Duration {
    let secs = std::env::var(SURVEY_INTERVAL_SECS_ENV)
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_SURVEY_INTERVAL_SECS)
        .max(MIN_SURVEY_INTERVAL_SECS);
    Duration::from_secs(secs)
}

/// A snapshot older than twice the cadence is flagged `stale`, so a single
/// missed tick does not cry wolf.
fn stale_after() -> Duration {
    survey_interval() * 2
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Freshness of the survey snapshot the response was derived from.
///
/// [`SurveyStatus::Unavailable`] is the variant [`CensusStatus`] lacks and this
/// surface needs: "the preview could not be computed, and here is why" must not
/// be reported as an empty result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SurveyStatus {
    /// No walk has completed yet this boot — `items` is empty because we do not
    /// KNOW, not because there is nothing.
    #[default]
    Pending,
    /// Snapshot within [`stale_after`].
    Fresh,
    /// Snapshot older than that — still shown, but flagged.
    Stale,
    /// The survey could not run at all. `census_note` carries the reason.
    Unavailable,
}

/// One enumerated cargo target root as the preview renders it.
#[derive(Debug, Clone, Serialize)]
pub struct DiskReclaimItem {
    /// Stable handle: the path, lowercased with forward slashes.
    pub id: String,
    /// The path as it appears on disk.
    pub path: String,
    /// `in-repo-canonical` | `sibling-worktree` | `container` | `sibling-nongit`.
    pub class: &'static str,
    /// `reclaimable` | `blocked`.
    pub status: &'static str,
    /// `None` for a reclaimable item; the refusal token otherwise.
    pub reason: Option<&'static str>,
    /// Operator-facing sentence for `reason`.
    pub reason_detail: Option<&'static str>,
    /// Measured bytes. **`null` when the size could not be read** — never `0`.
    pub bytes: Option<u64>,
    /// `bytes` is a LOWER BOUND: some subtree under this root was unreadable.
    pub bytes_partial: bool,
    /// Newest build-artifact mtime under the root, RFC3339. `None` when the
    /// probe could not read one (which is also why the item reads `building` —
    /// the classifier fails safe).
    pub last_used_at: Option<String>,
    /// Enclosing checkout / linked worktree, when the root lives inside one.
    pub repo_root: Option<String>,
    /// Which engine would remove this if the verb were armed. `None` means
    /// **no verb exists for this class in v1** — it is reported, not actionable.
    pub verb: Option<&'static str>,
}

/// Per-class rollup. Every class in [`TargetClass::all`] is always present, so a
/// class with zero roots is visibly zero rather than silently absent.
#[derive(Debug, Clone, Serialize)]
pub struct ClassSummary {
    pub class: &'static str,
    pub roots: usize,
    /// Sum of the measured roots. `None` when roots exist but NONE of them
    /// could be sized — an unknown total, not a zero one.
    pub bytes: Option<u64>,
    pub reclaimable_roots: usize,
    pub reclaimable_bytes: Option<u64>,
    /// Roots in this class whose size could not be read at all.
    pub roots_with_unknown_bytes: usize,
    /// `None` ⇒ report-only class (no v1 verb).
    pub verb: Option<&'static str>,
    pub note: &'static str,
}

/// Aggregate counts, per-class rollup, and the machine's free-space headroom.
#[derive(Debug, Clone, Serialize)]
pub struct SurveySummary {
    pub roots: usize,
    pub reclaimable: usize,
    pub blocked: usize,
    /// Every enumerated root, all classes. `None` until a walk has completed.
    pub total_bytes: Option<u64>,
    /// What an armed cleanup would free right now.
    pub reclaimable_bytes: Option<u64>,
    /// The in-repo-canonical class — the largest by bytes, and deliberately
    /// verbless in v1. Surfaced as its own headline number rather than folded
    /// into `total_bytes`, because "space you can see but cannot yet reclaim"
    /// is the single most valuable figure this preview carries.
    pub report_only_bytes: Option<u64>,
    /// Any byte total above is a lower bound (a truncated walk, an unreadable
    /// subtree, or a root that could not be sized).
    pub bytes_incomplete: bool,
    pub by_class: Vec<ClassSummary>,

    // --- Volume headroom (Phase 1's 60 s publisher — INV-D1) ---
    /// Free/total bytes per mounted volume. EMPTY while `volumes_status` is
    /// `pending` — read the status before reading this.
    pub volumes: Vec<census::VolumeReport>,
    /// `pending` (never sampled — UNKNOWN) | `fresh` | `stale`.
    pub volumes_status: CensusStatus,
    pub volumes_observed_at: Option<String>,
    pub volumes_age_secs: Option<u64>,
    /// `None` — NOT `0` — while `pending`.
    pub free_bytes_total: Option<u64>,
    pub total_bytes_total: Option<u64>,
    pub volumes_note: String,
}

/// What the walk itself managed to see. Present whenever a snapshot exists, so
/// an incomplete answer can be recognised as incomplete.
#[derive(Debug, Clone, Serialize)]
pub struct ScanStats {
    pub dirs_visited: usize,
    /// The visit ceiling was hit — `items` is a PREFIX of the population.
    pub truncated: bool,
    /// Directories whose read failed (a `read_dir` open, or the `.git` probe
    /// that decides ownership). A failed read is never folded into "nothing
    /// there".
    pub read_errors: Vec<ScanError>,
    /// Directory ENTRIES that errored after their directory opened. Counted
    /// separately from `read_errors`, which records only open failures — a dir
    /// that opens and then errors mid-iteration under-reports otherwise.
    pub entry_errors: usize,
    /// Directories not descended into because the walk's depth bound was
    /// reached. Surfaced because a target root below the bound is simply ABSENT
    /// from `items` — the bound is a designed limit, but an invisible one is
    /// indistinguishable from "there was nothing there".
    ///
    /// Deliberately NOT folded into `bytes_incomplete`: the bound is reached on
    /// any deep tree, so folding it in would leave that flag permanently true
    /// and destroy its ability to signal a real failure.
    pub depth_limited_dirs: usize,
    /// Reparse points (junctions/symlinks) the walk refused to follow. A
    /// junctioned target root appears neither as an item nor as an error, by
    /// design — following it would double-count the tree behind it — so it is
    /// counted here rather than vanishing.
    pub reparse_dirs_skipped: usize,
    pub roots_with_unknown_bytes: usize,
    pub roots_with_partial_bytes: usize,
}

impl ScanStats {
    /// True when the walk did not see the whole tree it was asked about. Used
    /// to decide whether an EMPTY item list is a measured zero or an unknown
    /// population — the two must never render the same.
    ///
    /// The depth bound is excluded on purpose (see `depth_limited_dirs`); these
    /// are the failures, not the designed limits.
    fn incomplete(&self) -> bool {
        self.truncated || !self.read_errors.is_empty() || self.entry_errors > 0
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ScanError {
    pub path: String,
    pub error: String,
}

/// Body of `GET /disk/reclaimable`.
#[derive(Debug, Clone, Serialize)]
pub struct DiskSurvey {
    /// The workspace root the walk covered. `None` when it could not be
    /// resolved — which is also the `unavailable` case.
    pub workspace_root: Option<String>,
    pub items: Vec<DiskReclaimItem>,
    pub summary: SurveySummary,

    // --- Freshness triple (+ provenance) ---
    /// `pending` | `fresh` | `stale` | `unavailable`.
    pub census_status: SurveyStatus,
    pub census_taken_at: Option<String>,
    pub census_age_secs: Option<u64>,
    /// How long the snapshot's walk took (ms) — explains the cadence.
    pub census_build_ms: Option<u64>,
    /// A walk is running right now (periodic tick or `?refresh=1`).
    pub census_refreshing: bool,
    /// One operator-facing sentence describing the data's provenance. Always
    /// present, and explicitly says "not ready" / "could not be computed"
    /// rather than implying empty.
    pub census_note: String,
    pub scan: Option<ScanStats>,
}

/// Query string of `GET /disk/reclaimable`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskSurveyQuery {
    /// `?refresh=1` — kick a fresh walk in the BACKGROUND. Never awaited past
    /// [`DiskSurveyQuery::wait`].
    #[serde(default)]
    pub refresh: Option<String>,
    /// Bounded override of the snapshot wait, capped at [`MAX_WAIT_SECS`].
    ///
    /// **Deliberately a `String`, not a `u64`.** Typed as `Option<u64>`, axum's
    /// `Query` extractor REJECTS a malformed `?waitSecs=` (`abc`, `-1`, `1.5`)
    /// with a bodiless 400 before the handler ever runs — which contradicts this
    /// surface's whole claim that it always renders an answer, and hands the
    /// caller a response with no `census_note` to read. A typo in a query knob
    /// is not a reason to withhold the disk report. Parsed and clamped in
    /// [`DiskSurveyQuery::wait`] instead.
    #[serde(default)]
    pub wait_secs: Option<String>,
}

impl DiskSurveyQuery {
    pub fn refresh_requested(&self) -> bool {
        matches!(
            self.refresh.as_deref(),
            Some("1" | "true" | "yes" | "on" | "")
        )
    }

    /// The bounded wait: parsed leniently, clamped to [`MAX_WAIT_SECS`], and
    /// falling back to [`DEFAULT_WAIT_SECS`] on anything unparseable.
    pub fn wait(&self) -> Duration {
        let secs = self
            .wait_secs
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_WAIT_SECS);
        Duration::from_secs(secs.min(MAX_WAIT_SECS))
    }
}

// ---------------------------------------------------------------------------
// The snapshot — built in the background, served by the route.
// ---------------------------------------------------------------------------

/// A completed survey walk.
#[derive(Debug, Clone)]
pub(super) struct DiskSurveySnapshot {
    pub(super) root: String,
    pub(super) items: Vec<DiskReclaimItem>,
    pub(super) scan: ScanStats,
    pub(super) taken_at: chrono::DateTime<chrono::Utc>,
    pub(super) build_ms: u64,
}

static LATEST: OnceLock<RwLock<Option<Arc<DiskSurveySnapshot>>>> = OnceLock::new();
static NOTIFY: OnceLock<tokio::sync::Notify> = OnceLock::new();
static BUILD_ACTIVE: AtomicBool = AtomicBool::new(false);

fn latest_cell() -> &'static RwLock<Option<Arc<DiskSurveySnapshot>>> {
    LATEST.get_or_init(|| RwLock::new(None))
}

fn notify() -> &'static tokio::sync::Notify {
    NOTIFY.get_or_init(tokio::sync::Notify::new)
}

/// The most recent snapshot, or `None` before the first walk completes.
/// NEVER blocks and NEVER triggers a walk.
pub(super) fn latest_snapshot() -> Option<Arc<DiskSurveySnapshot>> {
    latest_cell().read().ok().and_then(|g| g.clone())
}

/// True while a survey walk is in flight.
pub(super) fn build_active() -> bool {
    BUILD_ACTIVE.load(Ordering::Acquire)
}

fn publish(snapshot: DiskSurveySnapshot) {
    if let Ok(mut g) = latest_cell().write() {
        *g = Some(Arc::new(snapshot));
    }
    notify().notify_waiters();
}

// ---------------------------------------------------------------------------
// The walk — PURE over its inputs. No env, no arming, no coord, no abort.
// ---------------------------------------------------------------------------

/// Normalize a path to the stable `id` form used by the wire: forward slashes,
/// lowercased.
fn normalize_id(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// Render one enumerated candidate. Sizing and mtime probing happen HERE and
/// only here, so a blocked item is measured exactly like a reclaimable one —
/// the bytes you cannot reclaim are as visible as the bytes you can.
fn render_item(
    c: &Candidate,
    verdict: Result<(), SkipReason>,
    now: chrono::DateTime<chrono::Utc>,
) -> DiskReclaimItem {
    let measured = reaper::measure_dir_size(&c.path);
    let last_used_at = reaper::newest_artifact_age(&c.path).and_then(|age| {
        chrono::Duration::from_std(age)
            .ok()
            .map(|d| (now - d).to_rfc3339())
    });
    DiskReclaimItem {
        id: normalize_id(&c.path),
        path: path_string(&c.path),
        class: c.class.as_str(),
        status: if verdict.is_ok() {
            "reclaimable"
        } else {
            "blocked"
        },
        reason: verdict.err().map(SkipReason::token),
        reason_detail: verdict.err().map(SkipReason::detail),
        bytes: measured.map(|m| m.bytes),
        bytes_partial: measured.is_some_and(|m| m.unreadable_dirs > 0),
        last_used_at,
        repo_root: c.repo_root.as_deref().map(path_string),
        // Derived from the VERDICT, not from the class alone. Class-derived, a
        // `<wt>/target` blocked with `reason: "owned-by-worktree-reclaim"` still
        // advertised `verb: "orphan-target-reaper"` — this engine claiming
        // another engine's territory in the very field that names the owner.
        // A refusal on THIS engine's own guards (dirty, kept, grace, building)
        // keeps the verb: the verb says who would act, and that is still us.
        verb: match verdict {
            Err(reason) if reason.owned_elsewhere() => None,
            _ => c.class.has_verb().then_some(VERB_ORPHAN_REAPER),
        },
    }
}

/// Build a survey snapshot for `root`.
///
/// **The report-mode entry point, and the seam every test drives.** Pure over
/// `root` + `opts`: it reads disk and nothing else — no env, no arming flag, no
/// coord round-trip, and, the property INV-D1 turns on, **no preflight check
/// that could refuse to produce a report**. A build in flight changes one
/// item's verdict; it can never change whether there is a report.
pub(super) fn build_snapshot(root: &Path, opts: &ClassifyOptions) -> DiskSurveySnapshot {
    let started = Instant::now();
    let now = chrono::Utc::now();
    let enumeration = reaper::enumerate_all(root);

    let mut items = Vec::with_capacity(enumeration.candidates.len());
    for c in &enumeration.candidates {
        let verdict = reaper::classify_candidate(c, opts);
        items.push(render_item(c, verdict, now));
    }
    // Biggest first — the preview's job is to put the reclaimable terabyte at
    // the top, and an unmeasurable root sorts last rather than pretending to be
    // zero-sized somewhere in the middle.
    items.sort_by_key(|i| std::cmp::Reverse(i.bytes.unwrap_or(0)));

    let scan = ScanStats {
        dirs_visited: enumeration.dirs_visited,
        truncated: enumeration.truncated,
        read_errors: enumeration
            .read_errors
            .iter()
            .map(|(p, e)| ScanError {
                path: path_string(p),
                error: e.clone(),
            })
            .collect(),
        entry_errors: enumeration.entry_errors,
        depth_limited_dirs: enumeration.depth_limited_dirs,
        reparse_dirs_skipped: enumeration.reparse_dirs_skipped,
        roots_with_unknown_bytes: items.iter().filter(|i| i.bytes.is_none()).count(),
        roots_with_partial_bytes: items.iter().filter(|i| i.bytes_partial).count(),
    };

    DiskSurveySnapshot {
        root: path_string(root),
        items,
        scan,
        taken_at: now,
        build_ms: started.elapsed().as_millis() as u64,
    }
}

// ---------------------------------------------------------------------------
// Summary assembly — PURE over the snapshot + volume sample.
// ---------------------------------------------------------------------------

fn class_note(class: TargetClass) -> &'static str {
    match class {
        TargetClass::InRepoCanonical => {
            "Build dirs inside canonical repo checkouts. The LARGEST class by bytes and the one \
             v1 deliberately gives no cleanup verb: removing a live `<repo>/target` whole is not \
             what is wanted, and pruning inside one needs its own guard design."
        }
        TargetClass::SiblingWorktree => {
            "Renamed build dirs inside linked git worktrees. `<worktree>/target` itself belongs \
             to the worktree reclaim engine and is reported separately; a dirty worktree blocks \
             everything inside it."
        }
        TargetClass::Container => {
            "Build dirs under the shared container directories (`_wt`, `_targets`, \
             `_cargo-targets`, `cargo-targets`, `.wt-targets`)."
        }
        TargetClass::SiblingNonGit => {
            "Bare out-of-tree build dirs with no enclosing git repo (`target-wt-<slug>`, \
             `cargo-target-<slug>`, …)."
        }
    }
}

/// Sum the `Some` bytes of a slice, returning `None` when items exist and NONE
/// of them could be measured — an unknown total is never rendered as zero.
fn sum_bytes<'a>(items: impl Iterator<Item = &'a DiskReclaimItem>) -> Option<u64> {
    let mut any = false;
    let mut known = false;
    let mut total = 0u64;
    for i in items {
        any = true;
        if let Some(b) = i.bytes {
            known = true;
            total = total.saturating_add(b);
        }
    }
    if any && !known {
        None
    } else {
        Some(total)
    }
}

/// PURE assembly of the response from an (optional) snapshot and an (optional)
/// volume sample. Split out so every freshness and honesty branch is unit
/// testable without a disk walk.
pub(super) fn assemble(
    snapshot: Option<&DiskSurveySnapshot>,
    root_error: Option<String>,
    volume_sample: Option<&census::VolumeSample>,
    refreshing: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> DiskSurvey {
    let h = volume_headroom(volume_sample, now);
    let volumes_block = |summary: &mut SurveySummary| {
        summary.volumes = h.volumes.clone();
        summary.volumes_status = h.status;
        summary.volumes_observed_at = h.observed_at.clone();
        summary.volumes_age_secs = h.age_secs;
        summary.free_bytes_total = h.free_bytes_total;
        summary.total_bytes_total = h.total_bytes_total;
        summary.volumes_note = h.note.clone();
    };

    let empty_summary = |by_class: Vec<ClassSummary>| {
        let mut s = SurveySummary {
            roots: 0,
            reclaimable: 0,
            blocked: 0,
            total_bytes: None,
            reclaimable_bytes: None,
            report_only_bytes: None,
            bytes_incomplete: false,
            by_class,
            volumes: Vec::new(),
            volumes_status: CensusStatus::default(),
            volumes_observed_at: None,
            volumes_age_secs: None,
            free_bytes_total: None,
            total_bytes_total: None,
            volumes_note: String::new(),
        };
        volumes_block(&mut s);
        s
    };

    // --- The survey could not run at all. Say so, WITH the reason. ---
    if let Some(reason) = root_error {
        return DiskSurvey {
            workspace_root: None,
            items: Vec::new(),
            summary: empty_summary(Vec::new()),
            census_status: SurveyStatus::Unavailable,
            census_taken_at: None,
            census_age_secs: None,
            census_build_ms: None,
            census_refreshing: refreshing,
            census_note: format!(
                "The disk-reclaim preview could not be computed: {reason}. This is an ERROR, not \
                 an empty result — nothing here says there is nothing to reclaim."
            ),
            scan: None,
        };
    }

    // --- Cold start: no walk has completed yet. ---
    let Some(snapshot) = snapshot else {
        return DiskSurvey {
            workspace_root: None,
            items: Vec::new(),
            // Every class listed with zero roots would imply we had LOOKED.
            // We have not, so the per-class list is empty and the status says
            // why.
            summary: empty_summary(Vec::new()),
            census_status: SurveyStatus::Pending,
            census_taken_at: None,
            census_age_secs: None,
            census_build_ms: None,
            census_refreshing: refreshing,
            census_note: "No disk-reclaim walk has completed yet this session, so the candidate \
                          list is UNKNOWN — not empty. Free space below is answered independently \
                          and is unaffected by this."
                .to_string(),
            scan: None,
        };
    };

    let age_secs = (now - snapshot.taken_at).num_seconds().max(0) as u64;
    let status = if Duration::from_secs(age_secs) <= stale_after() {
        SurveyStatus::Fresh
    } else {
        SurveyStatus::Stale
    };

    // An empty item list from an INCOMPLETE walk is an UNKNOWN population, not
    // a measured zero.
    //
    // The defect this closes: `sum_bytes` over an empty iterator returns
    // `Some(0)`, so a walk whose `read_dir(root)` FAILED — a detached drive, a
    // permissions change, a bad `paths.workspace_root` — produced
    // `census_status: "fresh"`, every total a hard `0`, every `by_class` row
    // `roots: 0`, and a note flatly asserting the zero had been MEASURED. The
    // worst possible answer ("nothing to reclaim") rendered from a failed read.
    let population_unknown = snapshot.items.is_empty() && snapshot.scan.incomplete();
    // Nullify every total when the population itself is unknown. Applied to the
    // OUTPUT of `sum_bytes` rather than inside it, because `sum_bytes`'s own
    // contract — "no items at all ⇒ a known zero" — is correct and is what the
    // genuinely-empty case relies on.
    let known = |total: Option<u64>| if population_unknown { None } else { total };

    let by_class: Vec<ClassSummary> = TargetClass::all()
        .into_iter()
        .map(|class| {
            let token = class.as_str();
            let in_class = || snapshot.items.iter().filter(move |i| i.class == token);
            let reclaimable = || in_class().filter(|i| i.status == "reclaimable");
            ClassSummary {
                class: token,
                roots: in_class().count(),
                bytes: known(sum_bytes(in_class())),
                reclaimable_roots: reclaimable().count(),
                reclaimable_bytes: known(sum_bytes(reclaimable())),
                roots_with_unknown_bytes: in_class().filter(|i| i.bytes.is_none()).count(),
                verb: class.has_verb().then_some(VERB_ORPHAN_REAPER),
                note: class_note(class),
            }
        })
        .collect();

    let report_only_bytes = by_class
        .iter()
        .find(|c| c.class == TargetClass::InRepoCanonical.as_str())
        .and_then(|c| c.bytes);

    let mut summary = SurveySummary {
        roots: snapshot.items.len(),
        reclaimable: snapshot
            .items
            .iter()
            .filter(|i| i.status == "reclaimable")
            .count(),
        blocked: snapshot
            .items
            .iter()
            .filter(|i| i.status == "blocked")
            .count(),
        total_bytes: known(sum_bytes(snapshot.items.iter())),
        reclaimable_bytes: known(sum_bytes(
            snapshot.items.iter().filter(|i| i.status == "reclaimable"),
        )),
        report_only_bytes,
        bytes_incomplete: snapshot.scan.incomplete()
            || snapshot.scan.roots_with_unknown_bytes > 0
            || snapshot.scan.roots_with_partial_bytes > 0,
        by_class,
        volumes: Vec::new(),
        volumes_status: CensusStatus::default(),
        volumes_observed_at: None,
        volumes_age_secs: None,
        free_bytes_total: None,
        total_bytes_total: None,
        volumes_note: String::new(),
    };
    volumes_block(&mut summary);

    // What the walk failed to see, in the operator's words. Empty when it saw
    // everything it set out to.
    let gaps = {
        let mut parts: Vec<String> = Vec::new();
        if snapshot.scan.truncated {
            parts.push("it hit its visit ceiling".to_string());
        }
        if !snapshot.scan.read_errors.is_empty() {
            parts.push(format!(
                "{} director{} could not be read (see `scan.read_errors`)",
                snapshot.scan.read_errors.len(),
                if snapshot.scan.read_errors.len() == 1 {
                    "y"
                } else {
                    "ies"
                }
            ));
        }
        if snapshot.scan.entry_errors > 0 {
            parts.push(format!(
                "{} director{} entr{} errored mid-listing",
                snapshot.scan.entry_errors,
                if snapshot.scan.entry_errors == 1 {
                    "y"
                } else {
                    "ies"
                },
                if snapshot.scan.entry_errors == 1 {
                    "y"
                } else {
                    "ies"
                }
            ));
        }
        parts.join("; ")
    };

    let note = if population_unknown {
        // The R1 branch. An empty list here is the SHAPE of a failed read, and
        // saying "measured zero" over it is the single most misleading sentence
        // this surface could print.
        format!(
            "The walk under {} completed {} ago and found no cargo target roots, but it did NOT \
             see the whole tree: {}. The population is therefore UNKNOWN, not zero — the byte \
             totals are null rather than 0, and nothing here says there is nothing to reclaim.",
            snapshot.root,
            humanize_secs(age_secs),
            gaps,
        )
    } else if snapshot.items.is_empty() {
        format!(
            "The walk under {} completed {} ago, read every directory it set out to, and found \
             NO cargo target roots. This is a measured zero, not a missing reading.",
            snapshot.root,
            humanize_secs(age_secs),
        )
    } else {
        format!(
            "Disk state as of {} ago, from a {} ms walk of {} under {}.{}{}{} Removal always \
             re-checks live disk before touching anything.",
            humanize_secs(age_secs),
            snapshot.build_ms,
            snapshot.root,
            if snapshot.scan.dirs_visited == 1 {
                " 1 directory".to_string()
            } else {
                format!(" {} directories", snapshot.scan.dirs_visited)
            },
            if gaps.is_empty() {
                String::new()
            } else {
                format!(" This list is INCOMPLETE: {gaps}.")
            },
            if snapshot.scan.depth_limited_dirs > 0 {
                format!(
                    " {} director{} were not descended into at the walk's depth bound, so a \
                     target root below it is absent rather than measured.",
                    snapshot.scan.depth_limited_dirs,
                    if snapshot.scan.depth_limited_dirs == 1 {
                        "y"
                    } else {
                        "ies"
                    }
                )
            } else {
                String::new()
            },
            if summary.bytes_incomplete && gaps.is_empty() {
                " Some sizes could not be read, so the byte totals are lower bounds."
            } else {
                ""
            },
        )
    };

    DiskSurvey {
        workspace_root: Some(snapshot.root.clone()),
        items: snapshot.items.clone(),
        summary,
        census_status: status,
        census_taken_at: Some(snapshot.taken_at.to_rfc3339()),
        census_age_secs: Some(age_secs),
        census_build_ms: Some(snapshot.build_ms),
        census_refreshing: refreshing,
        census_note: note,
        scan: Some(snapshot.scan.clone()),
    }
}

// ---------------------------------------------------------------------------
// Background build + the request path.
// ---------------------------------------------------------------------------

/// Run one survey walk and publish it. Returns `false` when a walk was already
/// in flight (never piles a second disk walk on the first) or when the
/// workspace root could not be resolved.
pub(super) async fn build_and_publish() -> bool {
    if BUILD_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    let Some(root) = census::qontinui_root() else {
        BUILD_ACTIVE.store(false, Ordering::Release);
        return false;
    };
    let opts = ClassifyOptions::from_env();
    // The walk stats every file under every cargo target root on the machine —
    // a synchronous multi-minute disk walk. Run it on the blocking pool so the
    // async runtime is never pinned for the duration.
    let built = tokio::task::spawn_blocking(move || build_snapshot(&root, &opts)).await;
    BUILD_ACTIVE.store(false, Ordering::Release);
    match built {
        Ok(snapshot) => {
            info!(
                "disk_survey: walk completed in {}ms — {} roots, {} dirs visited, truncated={}",
                snapshot.build_ms,
                snapshot.items.len(),
                snapshot.scan.dirs_visited,
                snapshot.scan.truncated,
            );
            publish(snapshot);
            true
        }
        Err(e) => {
            warn!("disk_survey: walk task panicked/cancelled: {e}");
            false
        }
    }
}

/// Kick a walk in the BACKGROUND. `true` when one was started.
pub(super) fn spawn_rebuild() -> bool {
    if build_active() {
        return false;
    }
    tokio::spawn(async {
        build_and_publish().await;
    });
    true
}

/// Wait up to `timeout` for a snapshot strictly newer than `after`
/// (`after: None` ⇒ any snapshot at all). `None` on timeout — the caller then
/// degrades to whatever [`latest_snapshot`] holds, or to an honest
/// "not ready yet". NEVER starts a walk.
async fn wait_for_snapshot_after(
    after: Option<chrono::DateTime<chrono::Utc>>,
    timeout: Duration,
) -> Option<Arc<DiskSurveySnapshot>> {
    let fresh_enough = |s: &Arc<DiskSurveySnapshot>| after.is_none_or(|prev| s.taken_at > prev);
    let deadline = Instant::now() + timeout;
    loop {
        // Register the waiter BEFORE reading, so a publish between the read and
        // the await cannot be missed.
        let notified = notify().notified();
        if let Some(s) = latest_snapshot() {
            if fresh_enough(&s) {
                return Some(s);
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return latest_snapshot().filter(fresh_enough);
        }
        tokio::select! {
            _ = notified => {}
            _ = tokio::time::sleep(remaining) => {
                return latest_snapshot().filter(fresh_enough);
            }
        }
    }
}

/// Answer `GET /disk/reclaimable`.
///
/// Always returns a body — there is no error arm, because "could not compute"
/// is itself a reportable state ([`SurveyStatus::Unavailable`]) rather than a
/// 500 that a client would have to guess the meaning of.
pub async fn survey(query: DiskSurveyQuery) -> DiskSurvey {
    let previous = latest_snapshot();

    // A cold start self-heals: with no snapshot at all, kick a walk so the
    // NEXT request has one, without ever waiting for it here.
    if query.refresh_requested() || previous.is_none() {
        spawn_rebuild();
    }

    let wait_for = if query.refresh_requested() {
        previous.as_ref().map(|s| s.taken_at)
    } else {
        None
    };
    let waited = wait_for_snapshot_after(wait_for, query.wait()).await;
    let snapshot = waited.or_else(latest_snapshot).or(previous);

    let root_error = if snapshot.is_some() {
        None
    } else {
        // Only report "unavailable" when the reason is structural. A resolvable
        // root with no snapshot yet is `pending`, not an error.
        census::qontinui_root().is_none().then(|| {
            "the workspace root could not be resolved (set QONTINUI_ROOT, or the \
             paths.workspace_root setting)"
                .to_string()
        })
    };

    assemble(
        snapshot.as_deref(),
        root_error,
        // The free-space half is read from the 60s volume publisher and shares
        // nothing with this walk — INV-D1.
        census::latest_volume_sample().as_ref(),
        build_active(),
        chrono::Utc::now(),
    )
}

/// Spawn the periodic survey walk. Cadence from
/// [`SURVEY_INTERVAL_SECS_ENV`] (default 3600 s, floored 300 s). The first tick
/// fires immediately, so the preview stops being `pending` as soon as the first
/// walk finishes rather than one interval later.
///
/// ## Instance-gated, exactly like [`super::census::spawn_census`]
///
/// The walk is MACHINE-wide: it sizes every cargo target root on the box, so
/// every runner process on it produces the same answer from the same disk.
/// Unguarded, the primary, every secondary and every supervisor-spawned temp
/// runner each started a full-machine sizing walk at boot and hourly — several
/// multi-minute I/O storms over identical data, on a box whose disk pressure is
/// the reason this feature exists.
///
/// This is the **cost** half of the census's guard and nothing more: this module
/// publishes nothing to coord and holds no machine identity, so there is no
/// identity half to enforce at a chokepoint. The on-demand path is deliberately
/// left open — `GET /disk/reclaimable` on a secondary still self-heals via
/// [`spawn_rebuild`], the same asymmetry `census::spawn_census_rebuild` has.
pub fn spawn_disk_surveyor() {
    if !crate::instance::owns_shared_root_state() {
        info!(
            "disk_survey: not the shared-root-owning instance — skipping the PERIODIC reclaim \
             survey (the walk is machine-wide and the owning instance covers this disk). \
             `GET /disk/reclaimable` still answers here and can still refresh on demand."
        );
        return;
    }
    let interval = survey_interval();
    info!(
        "disk_survey: starting periodic reclaim survey, interval={}s",
        interval.as_secs()
    );
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            build_and_publish().await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // -----------------------------------------------------------------------
    // Fixtures
    // -----------------------------------------------------------------------

    /// A cargo target root with a `CACHEDIR.TAG` and a `debug/` profile.
    fn mk_target_root(dir: &Path) {
        fs::create_dir_all(dir.join("debug/deps")).unwrap();
        fs::write(dir.join("CACHEDIR.TAG"), b"Signature: 8a477f597d28d172").unwrap();
        fs::write(dir.join("debug/deps/libfoo.rlib"), vec![7u8; 64]).unwrap();
    }

    /// A canonical repo checkout: `.git` is a DIRECTORY.
    fn mk_canonical_checkout(dir: &Path) {
        fs::create_dir_all(dir.join(".git/refs")).unwrap();
    }

    /// A linked git worktree: `.git` is a FILE. Testing `is_dir()` alone would
    /// mis-assign every one of the 110 linked worktrees Phase 0 measured.
    fn mk_linked_worktree(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(".git"), b"gitdir: /repo/.git/worktrees/wt\n").unwrap();
    }

    /// Push every mtime under `dir` back by `age`, so the root reads as IDLE to
    /// the classifier's deepest-artifact probe.
    ///
    /// Load-bearing for the INV-D1 test below: the fixtures write their
    /// artifacts *now*, so without aging EVERY root is inside any realistic
    /// grace window and the "this one has a build in flight" arm passes on the
    /// grace window alone — i.e. with its live-build stand-in deleted. Directory
    /// mtimes matter as much as file ones, because `newest_artifact_age` reads
    /// the mtime of each ENTRY of its probe dirs, and those entries include
    /// subdirectories; `filetime` handles dirs on Windows too, which
    /// `File::set_times` does not without backup semantics.
    fn age_target_root(dir: &Path, age: Duration) {
        let when = filetime::FileTime::from_system_time(std::time::SystemTime::now() - age);
        fn walk(p: &Path, when: filetime::FileTime) {
            if let Ok(entries) = fs::read_dir(p) {
                for entry in entries.flatten() {
                    let child = entry.path();
                    if child.is_dir() {
                        walk(&child, when);
                    }
                    filetime::set_file_mtime(&child, when).unwrap();
                }
            }
            filetime::set_file_mtime(p, when).unwrap();
        }
        walk(dir, when);
    }

    /// Options with the git probe stubbed to "clean", zero grace, no pins.
    fn clean_opts() -> ClassifyOptions {
        ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Clean)
    }

    fn dirty_opts() -> ClassifyOptions {
        ClassifyOptions::with_probe(Duration::ZERO, |_| DirtyProbe::Dirty)
    }

    fn find<'a>(s: &'a DiskSurveySnapshot, needle: &str) -> &'a DiskReclaimItem {
        s.items
            .iter()
            .find(|i| i.path.ends_with(needle))
            .unwrap_or_else(|| {
                panic!(
                    "no item ending in {needle}; got {:?}",
                    s.items.iter().map(|i| &i.path).collect::<Vec<_>>()
                )
            })
    }

    // -----------------------------------------------------------------------
    // INV-D1 — the load-bearing regression
    // -----------------------------------------------------------------------

    /// Make `target_root` look like it has a live cargo build.
    ///
    /// On Windows this is the REAL discriminator the classifier uses: cargo
    /// holds `debug/.cargo-lock` open with no write sharing, so our probe's
    /// write-open fails with a sharing violation. Returns the held handle (the
    /// caller must keep it alive) and the reason the classifier must report.
    #[cfg(windows)]
    fn hold_a_build(target_root: &Path) -> (impl std::any::Any, &'static str) {
        use std::os::windows::fs::OpenOptionsExt;
        let lock = target_root.join("debug/.cargo-lock");
        fs::write(&lock, b"").unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(&lock)
            .expect("the live-build stand-in must actually hold the lock");
        (held, "building")
    }

    /// POSIX: cargo's `.cargo-lock` is `flock`ed, which a write-open does not
    /// see, so the live-build discriminator on this platform is the artifact
    /// mtime inside the grace window. A just-written artifact under a real
    /// grace is exactly what a build in flight looks like here.
    ///
    /// This is only a discriminator because the caller AGES both roots first.
    /// It previously was not: `mk_target_root` had just written
    /// `debug/deps/libfoo.rlib` at the same instant, so both roots were inside
    /// the grace window regardless and every POSIX assertion passed with this
    /// function's body deleted.
    #[cfg(not(windows))]
    fn hold_a_build(target_root: &Path) -> (impl std::any::Any, &'static str) {
        fs::write(target_root.join("debug/deps/live.rlib"), b"in-flight").unwrap();
        ((), "grace-pending")
    }

    /// **INV-D1.** The preview renders with a build in flight, and the affected
    /// root is reported as a per-item `blocked` verdict — never a refusal to
    /// answer. This is the test that makes the `cargo-sweep-all.ps1 -WhatIf`
    /// preflight-abort defect (`exit 2` whenever any cargo process is running)
    /// unrepeatable here.
    #[test]
    fn survey_renders_with_a_build_in_flight_and_blocks_only_that_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let building = root.join("target-wt-live");
        let idle = root.join("target-wt-idle");
        mk_target_root(&building);
        mk_target_root(&idle);
        // Age BOTH roots past any grace FIRST. Without this the fixtures' own
        // just-written artifacts put every root inside the window, and the
        // POSIX arm below proves only that the grace window exists — it passes
        // with `hold_a_build`'s body deleted. Aged, the ONLY thing that can put
        // `building` back inside the window is the live-build stand-in itself.
        for root_dir in [&building, &idle] {
            age_target_root(root_dir, Duration::from_secs(7 * 86_400));
        }

        // The live-build stand-in must be held for the WHOLE walk.
        let (_held, expected_reason) = hold_a_build(&building);

        // A grace long enough that a freshly-written artifact is inside it —
        // the POSIX live-build discriminator. On Windows the held lock fires
        // first regardless.
        let opts = ClassifyOptions::with_probe(Duration::from_secs(86_400), |_| DirtyProbe::Clean);
        let snapshot = build_snapshot(root, &opts);

        // 1. It ANSWERED, with the full population — not an empty list, not an
        //    error, not a hang.
        assert_eq!(
            snapshot.items.len(),
            2,
            "every root must still be reported while a build is in flight"
        );
        // 2. The building root is blocked, with a reason naming the build.
        let live = find(&snapshot, "target-wt-live");
        assert_eq!(live.status, "blocked");
        assert_eq!(live.reason, Some(expected_reason));
        assert!(
            live.reason_detail.is_some(),
            "a refusal is never unexplained"
        );
        // 3. It is still MEASURED — the bytes you cannot reclaim are visible.
        assert!(
            live.bytes.unwrap_or(0) > 0,
            "a blocked root is sized like any other"
        );
        // 4. And the OTHER root — same walk, same 24h grace, aged past it — is
        //    RECLAIMABLE. This is the discrimination: one build in flight blocks
        //    exactly one item and leaves the rest of the report intact. It is
        //    also what makes the arm above non-vacuous on EVERY platform: with
        //    `hold_a_build` neutered, `building` is aged like its sibling and
        //    assertion 2 fails.
        let other = find(&snapshot, "target-wt-idle");
        assert_eq!(
            other.status, "reclaimable",
            "an idle, aged root stays reclaimable while its neighbour builds"
        );
        assert_eq!(other.reason, None);
        // Pin the aging itself, on both platforms. If `age_target_root` silently
        // stopped working, the idle root would be freshly-written like the
        // fixture made it, the two roots would agree again for a reason that has
        // nothing to do with the build, and this test would quietly go back to
        // proving only that a grace window exists.
        let last_used = |i: &DiskReclaimItem| {
            chrono::DateTime::parse_from_rfc3339(
                i.last_used_at
                    .as_deref()
                    .expect("a sized root has an mtime"),
            )
            .expect("last_used_at is RFC3339")
            .with_timezone(&chrono::Utc)
        };
        let now = chrono::Utc::now();
        assert!(
            (now - last_used(other)).num_hours() >= 24,
            "the idle root must really be idle, not merely outside the window by luck"
        );
        assert!(
            (now - last_used(live)).num_hours() < 1,
            "and the build-in-flight root must really be fresh"
        );

        // Now the same walk at zero grace: the build-in-flight root STAYS
        // blocked on Windows (the held lock is checked before any mtime) and
        // becomes reclaimable on POSIX, where mtime is the only signal.
        let zero = build_snapshot(root, &clean_opts());
        assert_eq!(find(&zero, "target-wt-idle").status, "reclaimable");
        #[cfg(windows)]
        {
            let live = find(&zero, "target-wt-live");
            assert_eq!(live.status, "blocked");
            assert_eq!(live.reason, Some("building"));
        }
    }

    /// A build in flight on EVERY candidate still yields a full report with a
    /// populated summary — the "nothing may consult a global build-in-flight
    /// condition" half of INV-D1. The old script's failure mode was exactly
    /// this shape: any build anywhere ⇒ no report at all.
    #[test]
    fn survey_answers_even_when_every_candidate_is_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for name in ["target-a", "target-b", "target-c"] {
            mk_target_root(&root.join(name));
        }
        // A 24h grace blocks all three as freshly-built.
        let opts = ClassifyOptions::with_probe(Duration::from_secs(86_400), |_| DirtyProbe::Clean);
        let snapshot = build_snapshot(root, &opts);
        let survey = assemble(Some(&snapshot), None, None, false, chrono::Utc::now());

        assert_eq!(survey.census_status, SurveyStatus::Fresh);
        assert_eq!(survey.summary.roots, 3);
        assert_eq!(survey.summary.blocked, 3);
        assert_eq!(survey.summary.reclaimable, 0);
        // Reclaimable bytes are a MEASURED zero here (we looked, nothing
        // qualified) — distinct from the `None` a pending survey reports.
        assert_eq!(survey.summary.reclaimable_bytes, Some(0));
        assert!(survey.summary.total_bytes.unwrap_or(0) > 0);
    }

    // -----------------------------------------------------------------------
    // Class enumeration + the sibling-worktree boundary
    // -----------------------------------------------------------------------

    #[test]
    fn all_four_classes_are_enumerated_and_tagged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // 1. sibling-nongit — a bare out-of-tree root.
        mk_target_root(&root.join("target-wt-bare"));
        // 2. container.
        mk_target_root(&root.join("_cargo-targets/blastfix"));
        // 3. sibling-worktree — `.git` is a FILE.
        let wt = root.join("qontinui-runner-wt-x");
        mk_linked_worktree(&wt);
        mk_target_root(&wt.join("target-x"));
        // 4. in-repo-canonical — `.git` is a DIRECTORY.
        let repo = root.join("qontinui-runner");
        mk_canonical_checkout(&repo);
        mk_target_root(&repo.join("target-agentish"));

        let snapshot = build_snapshot(root, &clean_opts());
        assert_eq!(find(&snapshot, "target-wt-bare").class, "sibling-nongit");
        assert_eq!(find(&snapshot, "blastfix").class, "container");
        assert_eq!(find(&snapshot, "target-x").class, "sibling-worktree");
        assert_eq!(
            find(&snapshot, "target-agentish").class,
            "in-repo-canonical"
        );
    }

    /// The `.git`-as-FILE detection is load-bearing: a `test -d .git`
    /// classifier mis-assigns all 110 linked worktrees Phase 0 measured. Assert
    /// the two forms land in DIFFERENT classes with different verbs.
    #[test]
    fn git_as_file_and_git_as_dir_are_different_classes() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let linked = root.join("repo-wt-a");
        mk_linked_worktree(&linked);
        mk_target_root(&linked.join("target-a"));
        let canonical = root.join("repo");
        mk_canonical_checkout(&canonical);
        mk_target_root(&canonical.join("target-a"));

        let snapshot = build_snapshot(root, &clean_opts());
        let linked_item = snapshot
            .items
            .iter()
            .find(|i| i.path.contains("repo-wt-a"))
            .unwrap();
        let canonical_item = snapshot
            .items
            .iter()
            .find(|i| i.path.contains("/repo/"))
            .unwrap();
        assert_eq!(linked_item.class, "sibling-worktree");
        assert_eq!(linked_item.verb, Some(VERB_ORPHAN_REAPER));
        assert_eq!(linked_item.status, "reclaimable");
        assert_eq!(canonical_item.class, "in-repo-canonical");
        assert_eq!(canonical_item.verb, None, "v1 gives this class no verb");
        assert_eq!(canonical_item.status, "blocked");
        assert_eq!(canonical_item.reason, Some("report-only"));
    }

    /// Risk 1's mitigation: a DIRTY worktree's build dirs are never candidates.
    /// This is worktree-reclaim's inviolable G1, taken where it applies.
    #[test]
    fn a_dirty_worktree_blocks_every_build_dir_inside_it() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root.join("repo-wt-dirty");
        mk_linked_worktree(&wt);
        mk_target_root(&wt.join("target-slug"));
        // A bare out-of-tree root is unaffected by the worktree's dirtiness.
        mk_target_root(&root.join("target-bare"));

        let clean = build_snapshot(root, &clean_opts());
        assert_eq!(find(&clean, "target-slug").status, "reclaimable");

        let dirty = build_snapshot(root, &dirty_opts());
        let item = find(&dirty, "target-slug");
        assert_eq!(item.status, "blocked");
        assert_eq!(item.reason, Some("worktree-dirty"));
        // ...and the dirty verdict does NOT leak outside the worktree.
        assert_eq!(find(&dirty, "target-bare").status, "reclaimable");
    }

    /// The other half of the boundary: paths another engine owns are reported
    /// (with bytes) but never given this reaper's verb, so the two engines can
    /// never race on one path.
    ///
    /// The build-pool fixture deliberately sits OUTSIDE the worktree. The pool
    /// lives inside a canonical checkout on the real machine, which made the old
    /// fixture — `target-pool` nested in a linked worktree — assert protection
    /// this reaper was granting for the wrong reason, and one that a failed
    /// `.git` read could take away. Its location must not be what protects it.
    #[test]
    fn paths_owned_by_other_engines_are_reported_but_never_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root.join("repo-wt-b");
        mk_linked_worktree(&wt);
        // worktree-reclaim owns `<wt>/target` and `<wt>/src-tauri/target`.
        mk_target_root(&wt.join("target"));
        mk_target_root(&wt.join("src-tauri/target"));
        // the build system owns `target-agent` and `target-pool/<slot>` —
        // wherever they sit. `target-pool` here is a bare sibling of the
        // worktree, with no enclosing checkout to strip a prefix against.
        mk_target_root(&root.join("target-agent"));
        mk_target_root(&root.join("target-pool/slot-0"));
        mk_target_root(&root.join("target-pool/lkg"));

        let snapshot = build_snapshot(root, &clean_opts());
        assert_eq!(snapshot.items.len(), 5, "all five are ENUMERATED");
        for (needle, reason) in [
            ("repo-wt-b/target", "owned-by-worktree-reclaim"),
            ("src-tauri/target", "owned-by-worktree-reclaim"),
            ("target-agent", "owned-by-build-pool"),
            ("slot-0", "owned-by-build-pool"),
            ("lkg", "owned-by-build-pool"),
        ] {
            let item = find(&snapshot, needle);
            assert_eq!(item.status, "blocked", "{needle} must never be a candidate");
            assert_eq!(item.reason, Some(reason), "{needle}");
            // Still measured — the point of the preview is that you SEE it.
            assert!(item.bytes.unwrap_or(0) > 0, "{needle} must still be sized");
            // And it must NOT carry this reaper's verb. `verb` names who would
            // act; advertising `orphan-target-reaper` next to
            // `reason: "owned-by-worktree-reclaim"` is this engine claiming
            // another engine's territory in the field that names the owner.
            assert_eq!(
                item.verb, None,
                "{needle} is owned elsewhere — it must advertise no verb of ours"
            );
        }
        // Non-vacuous: a root this engine DOES own still carries the verb.
        mk_target_root(&wt.join("target-renamed"));
        let with_ours = build_snapshot(root, &clean_opts());
        let ours = find(&with_ours, "target-renamed");
        assert_eq!(ours.status, "reclaimable");
        assert_eq!(ours.verb, Some(VERB_ORPHAN_REAPER));
    }

    /// A refusal on THIS engine's own guards keeps the verb — the field says who
    /// WOULD act, and a dirty worktree or an unelapsed grace is a "not yet",
    /// not a change of owner.
    #[test]
    fn a_root_blocked_by_our_own_guards_keeps_its_verb() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let wt = root.join("repo-wt-dirty-verb");
        mk_linked_worktree(&wt);
        mk_target_root(&wt.join("target-slug"));
        let dirty = build_snapshot(root, &dirty_opts());
        let item = find(&dirty, "target-slug");
        assert_eq!(item.reason, Some("worktree-dirty"));
        assert_eq!(
            item.verb,
            Some(VERB_ORPHAN_REAPER),
            "we are still the engine that would reap it once the tree is clean"
        );
    }

    #[test]
    fn a_pinned_root_is_never_a_candidate_even_at_zero_grace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let pinned = root.join("target-pinned");
        mk_target_root(&pinned);
        fs::write(pinned.join(".reap-keep"), b"").unwrap();
        let snapshot = build_snapshot(root, &clean_opts());
        let item = find(&snapshot, "target-pinned");
        assert_eq!(item.status, "blocked");
        assert_eq!(item.reason, Some("kept"));
    }

    #[test]
    fn the_walk_never_descends_into_a_discovered_target_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let outer = root.join("target-outer");
        mk_target_root(&outer);
        // A nested marker dir inside the target must NOT become a second item.
        mk_target_root(&outer.join("debug/nested-target"));
        let snapshot = build_snapshot(root, &clean_opts());
        assert_eq!(snapshot.items.len(), 1);
        assert!(snapshot.items[0].path.ends_with("target-outer"));
    }

    // -----------------------------------------------------------------------
    // Honesty contract
    // -----------------------------------------------------------------------

    /// Cold start: `pending`, an empty list that is LABELLED pending, and byte
    /// totals that are `null` rather than a fabricated `0`.
    #[test]
    fn cold_start_is_pending_with_null_totals_and_a_note() {
        let s = assemble(None, None, None, false, chrono::Utc::now());
        assert_eq!(s.census_status, SurveyStatus::Pending);
        assert!(s.items.is_empty());
        assert_eq!(s.summary.total_bytes, None);
        assert_eq!(s.summary.reclaimable_bytes, None);
        assert!(s.census_note.contains("UNKNOWN"));
        assert!(s.scan.is_none());
    }

    /// A FAILED read and a GENUINELY EMPTY population must never render the
    /// same. This asserts the rendered strings differ, not merely the enums.
    #[test]
    fn unavailable_and_empty_render_differently() {
        let now = chrono::Utc::now();
        let unavailable = assemble(
            None,
            Some("the workspace root could not be resolved".to_string()),
            None,
            false,
            now,
        );
        let tmp = tempfile::tempdir().unwrap();
        let empty_snapshot = build_snapshot(tmp.path(), &clean_opts());
        let empty = assemble(Some(&empty_snapshot), None, None, false, now);

        assert_eq!(unavailable.census_status, SurveyStatus::Unavailable);
        assert_eq!(empty.census_status, SurveyStatus::Fresh);
        assert_ne!(unavailable.census_note, empty.census_note);
        assert!(unavailable.census_note.contains("could not be computed"));
        assert!(empty.census_note.contains("measured zero"));
        // The decisive difference: unknown totals vs a measured zero.
        assert_eq!(unavailable.summary.total_bytes, None);
        assert_eq!(empty.summary.total_bytes, Some(0));
        // Both have an empty item list — which is exactly why the list alone
        // must never be the thing a caller reads.
        assert!(unavailable.items.is_empty() && empty.items.is_empty());
    }

    /// **R1 — the empty result of a FAILED walk is not "a measured zero".**
    ///
    /// `read_dir(root)` fails (detached drive, a permissions change, a bad
    /// `paths.workspace_root`): the walk records one read error and returns zero
    /// candidates. The empty branch consulted neither `read_errors` nor
    /// `truncated`, and `sum_bytes` over an empty iterator returns `Some(0)` —
    /// so `census_status` came back `fresh`, every byte total a hard `0`, every
    /// `by_class` row `roots: 0`, under a note asserting the zero was MEASURED.
    /// The web then printed "Nothing to reclaim … a measured answer, not a
    /// missing one." A failed read rendered as the worst possible answer.
    #[test]
    fn a_failed_root_read_is_unknown_not_a_measured_zero() {
        let tmp = tempfile::tempdir().unwrap();
        // Never created — `read_dir` on it fails exactly like a detached drive.
        let missing = tmp.path().join("detached-volume");
        let snapshot = build_snapshot(&missing, &clean_opts());
        assert!(snapshot.items.is_empty(), "the walk found nothing…");
        assert_eq!(snapshot.scan.read_errors.len(), 1, "…because it FAILED");

        let s = assemble(Some(&snapshot), None, None, false, chrono::Utc::now());

        // Every total is UNKNOWN, not zero.
        assert_eq!(s.summary.total_bytes, None);
        assert_eq!(s.summary.reclaimable_bytes, None);
        assert_eq!(s.summary.report_only_bytes, None);
        for c in &s.summary.by_class {
            assert_eq!(c.bytes, None, "{} must not report a zero", c.class);
            assert_eq!(c.reclaimable_bytes, None, "{}", c.class);
        }
        // `bytes_incomplete` was already the one surviving signal — keep it, and
        // make the prose agree with it instead of contradicting it.
        assert!(s.summary.bytes_incomplete);
        assert!(
            !s.census_note.contains("measured zero"),
            "note asserted a measured zero over a failed read: {}",
            s.census_note
        );
        assert!(s.census_note.contains("UNKNOWN"), "{}", s.census_note);
        assert!(
            s.census_note.contains("could not be read"),
            "the note must name the gap: {}",
            s.census_note
        );

        // Non-vacuous: a walk that SUCCEEDED over an empty tree still reports a
        // measured zero, and the two notes differ.
        let empty_tree = tempfile::tempdir().unwrap();
        let ok = build_snapshot(empty_tree.path(), &clean_opts());
        let ok = assemble(Some(&ok), None, None, false, chrono::Utc::now());
        assert_eq!(ok.summary.total_bytes, Some(0));
        assert!(ok.census_note.contains("measured zero"));
        assert_ne!(ok.census_note, s.census_note);
    }

    /// The same distinction one layer down: a walk that found roots but could
    /// not read part of the tree keeps its measured (lower-bound) totals — only
    /// an EMPTY result from an incomplete walk becomes unknown.
    #[test]
    fn a_partial_walk_that_found_roots_keeps_its_lower_bound_totals() {
        let tmp = tempfile::tempdir().unwrap();
        mk_target_root(&tmp.path().join("target-found"));
        let mut snapshot = build_snapshot(tmp.path(), &clean_opts());
        snapshot.scan.read_errors.push(ScanError {
            path: "/somewhere/unreadable".to_string(),
            error: "permission denied".to_string(),
        });
        let s = assemble(Some(&snapshot), None, None, false, chrono::Utc::now());
        assert!(
            s.summary.total_bytes.unwrap_or(0) > 0,
            "what WAS measured is still reported"
        );
        assert!(s.summary.bytes_incomplete, "…as a lower bound");
        assert!(s.census_note.contains("INCOMPLETE"), "{}", s.census_note);
    }

    /// A pending survey must not suppress the free-space answer. That
    /// asymmetry is the whole point of INV-D1.
    #[test]
    fn free_space_answers_while_the_survey_is_pending() {
        let sample = census::VolumeSample {
            volumes: vec![census::VolumeReport {
                volume: "D:".to_string(),
                drive_letter: Some("D".to_string()),
                total_bytes: 4_000,
                free_bytes: 93,
            }],
            taken_at: chrono::Utc::now(),
        };
        let s = assemble(None, None, Some(&sample), false, chrono::Utc::now());
        assert_eq!(s.census_status, SurveyStatus::Pending);
        assert_eq!(s.summary.volumes_status, CensusStatus::Fresh);
        assert_eq!(s.summary.free_bytes_total, Some(93));
        assert_eq!(s.summary.volumes.len(), 1);
    }

    /// With no volume sample the free-space half is UNKNOWN — `null`, never 0.
    #[test]
    fn absent_volume_sample_is_unknown_not_zero() {
        let s = assemble(None, None, None, false, chrono::Utc::now());
        assert_eq!(s.summary.volumes_status, CensusStatus::Pending);
        assert_eq!(s.summary.free_bytes_total, None);
        assert_eq!(s.summary.total_bytes_total, None);
        assert!(s.summary.volumes_note.contains("UNKNOWN, not zero"));
    }

    /// Every class is listed even at zero roots, so "we looked and found none"
    /// is distinguishable from "this class was never considered".
    #[test]
    fn every_class_appears_in_the_rollup_even_at_zero_roots() {
        let tmp = tempfile::tempdir().unwrap();
        mk_target_root(&tmp.path().join("target-only-one"));
        let snapshot = build_snapshot(tmp.path(), &clean_opts());
        let s = assemble(Some(&snapshot), None, None, false, chrono::Utc::now());
        assert_eq!(s.summary.by_class.len(), 4);
        let nongit = s
            .summary
            .by_class
            .iter()
            .find(|c| c.class == "sibling-nongit")
            .unwrap();
        assert_eq!(nongit.roots, 1);
        let canonical = s
            .summary
            .by_class
            .iter()
            .find(|c| c.class == "in-repo-canonical")
            .unwrap();
        assert_eq!(canonical.roots, 0);
        assert_eq!(canonical.bytes, Some(0));
        assert_eq!(canonical.verb, None);
        assert!(canonical.note.contains("no cleanup verb"));
    }

    /// The report-only class is surfaced as its own headline number, because it
    /// is the biggest one and the user cannot currently see it anywhere.
    #[test]
    fn report_only_bytes_are_surfaced_separately() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let repo = root.join("repo");
        mk_canonical_checkout(&repo);
        mk_target_root(&repo.join("target-x"));
        let snapshot = build_snapshot(root, &clean_opts());
        let s = assemble(Some(&snapshot), None, None, false, chrono::Utc::now());
        assert!(s.summary.report_only_bytes.unwrap_or(0) > 0);
        assert_eq!(
            s.summary.reclaimable_bytes,
            Some(0),
            "report-only bytes are never counted as reclaimable"
        );
    }

    #[test]
    fn sum_bytes_reports_unknown_rather_than_a_fabricated_zero() {
        let mk = |bytes: Option<u64>| DiskReclaimItem {
            id: "x".into(),
            path: "x".into(),
            class: "container",
            status: "blocked",
            reason: None,
            reason_detail: None,
            bytes,
            bytes_partial: false,
            last_used_at: None,
            repo_root: None,
            verb: None,
        };
        // No items at all → a known zero.
        let none: [DiskReclaimItem; 0] = [];
        assert_eq!(sum_bytes(none.iter()), Some(0));
        // Items, none measurable → UNKNOWN.
        let unknown = [mk(None), mk(None)];
        assert_eq!(sum_bytes(unknown.iter()), None);
        // Partially measurable → the known part, as a lower bound.
        let partial = [mk(None), mk(Some(10))];
        assert_eq!(sum_bytes(partial.iter()), Some(10));
    }

    #[test]
    fn query_wait_is_capped_and_refresh_parses() {
        let q = DiskSurveyQuery {
            refresh: Some("1".into()),
            wait_secs: Some("9999".into()),
        };
        assert!(q.refresh_requested());
        assert_eq!(q.wait(), Duration::from_secs(MAX_WAIT_SECS));
        let d = DiskSurveyQuery::default();
        assert!(!d.refresh_requested());
        assert_eq!(d.wait(), Duration::from_secs(DEFAULT_WAIT_SECS));
        assert_eq!(
            DiskSurveyQuery {
                refresh: None,
                wait_secs: Some("3".into())
            }
            .wait(),
            Duration::from_secs(3)
        );
    }

    /// A malformed `?waitSecs=` must be ACCEPTED and clamped, never rejected.
    /// Typed `Option<u64>`, axum's `Query` extractor 400s with an empty body
    /// before the handler runs — so a typo in a query knob withheld the disk
    /// report entirely, and the response carried no `census_note` explaining
    /// itself. That directly contradicts this route's "there is no error arm".
    #[test]
    fn a_malformed_wait_secs_is_accepted_and_clamped() {
        for junk in ["abc", "-1", "1.5", "", "   ", "99999999999999999999999"] {
            let q = DiskSurveyQuery {
                refresh: None,
                wait_secs: Some(junk.to_string()),
            };
            assert_eq!(
                q.wait(),
                Duration::from_secs(DEFAULT_WAIT_SECS),
                "?waitSecs={junk} must fall back to the default, not reject the request"
            );
        }
        // And the real extractor accepts it — the half that used to 400 lives
        // in axum's `Query`, not in `wait()`, so assert against `Query` itself.
        let uri: axum::http::Uri = "/disk/reclaimable?waitSecs=abc&refresh=1".parse().unwrap();
        let axum::extract::Query(parsed) =
            axum::extract::Query::<DiskSurveyQuery>::try_from_uri(&uri).expect(
                "a malformed waitSecs must still EXTRACT — this rejection is the bodiless 400 \
                 the String type exists to prevent",
            );
        assert!(parsed.refresh_requested());
        assert_eq!(parsed.wait(), Duration::from_secs(DEFAULT_WAIT_SECS));
    }

    /// **R6** — the periodic walk is machine-wide, so it must not run once per
    /// runner process on the box. Source-level for the same reason
    /// `census::the_census_post_chokepoint_carries_the_guard` is: the predicate
    /// reads process-global env that parallel test threads mutate.
    #[test]
    fn the_periodic_surveyor_is_instance_gated() {
        const SRC: &str = include_str!("disk_survey.rs");
        let prod = SRC
            .split_once(
                "
#[cfg(test)]
mod tests {",
            )
            .map(|(before, _)| before)
            .unwrap_or(SRC);
        let spawn = prod
            .split_once("pub fn spawn_disk_surveyor() {")
            .map(|(_, after)| after)
            .expect("spawn_disk_surveyor must exist; move this pin if it is renamed");
        let body = spawn.split_once("\n}\n").map(|(b, _)| b).unwrap_or(spawn);
        assert!(
            body.contains("owns_shared_root_state()"),
            "the hourly full-machine sizing walk is spawned with no instance guard, so every \
             secondary and every supervisor-spawned temp runner starts its own copy of it — \
             several multi-minute I/O storms over identical data, on a disk-pressured box."
        );
    }

    #[test]
    fn survey_interval_is_floored() {
        let prev = std::env::var(SURVEY_INTERVAL_SECS_ENV).ok();
        std::env::set_var(SURVEY_INTERVAL_SECS_ENV, "1");
        let floored = survey_interval();
        match prev {
            Some(v) => std::env::set_var(SURVEY_INTERVAL_SECS_ENV, v),
            None => std::env::remove_var(SURVEY_INTERVAL_SECS_ENV),
        }
        assert_eq!(floored, Duration::from_secs(MIN_SURVEY_INTERVAL_SECS));
    }
}
