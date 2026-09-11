//! Wire-up + periodic trigger (Phase 3).
//!
//! Mounts the adapter as a periodic **reconcile scan** of the operator's
//! `plans/` directory, pushing each plan's parsed work-unit to coord via
//! [`super::push`]. The periodic-scan trigger (over a filesystem watch or an
//! on-edit hook) is deliberate: it is robust across runner downtime and closed
//! sessions — an edit made while the runner was down is picked up on the next
//! tick — and the edge-triggered push ([`super::push::decide_push`]) makes a
//! re-scan of an unchanged corpus free of phantom transitions. It mirrors the
//! model coord's own `plan_ingest_worker` used (~60s tick).
//!
//! ## Metrics
//!
//! The runner has no Prometheus surface, so observability is process-local
//! atomic counters named to mirror coord's `coord_plan_ingest_*` so the
//! operator reads the same signals: scanned / transitions / cycles /
//! conflicts. [`adapter_metrics`] exposes the shared instance and
//! [`AdapterMetrics::snapshot`] reads it.
//!
//! ## Opt-in
//!
//! The markdown-plan tier is armed by exactly one thing: the runner's
//! `PathSettings::plans_dir` setting — the **Paths** section of the settings
//! UI, or `paths.plans_dir` in `settings.json`. There is **no environment
//! override**: the one that used to exist was a backward-compatibility shim
//! that silently outranked the setting; the binary's `plans_dir_migration`
//! persists its value into the setting once at boot, and the env read itself
//! is gone. The markdown-plan carrier is the optional top coordination tier,
//! so a runner with nothing configured no-ops (it never scans, never pushes) —
//! claims/intent and coord-native work-units are unaffected.
//!
//! [`spawn_if_configured`] gates only on a resolvable coord base. The loop it
//! spawns re-reads the path settings **every tick** through a [`PathReader`]
//! closure, so a directory configured, changed or cleared while the runner is
//! running takes effect within one interval and never needs a restart (which
//! fleet policy forbids) — the same per-cycle posture as the `plan_capture`
//! dial ([`CaptureGate`]). The settings arrive through a closure rather than
//! being read here because this module lives in the lib crate and the settings
//! store lives in the runner binary's module tree; the binary supplies the
//! reader. [`resolve_plans_dir`] owns the resolution so every surface that
//! needs the active plans dir — the adapter here, the session-env injection
//! and the plan-library read door in the binary — resolves it identically.

use super::parser::{parse_work_unit, slug_from_filename, ParsedWorkUnit, PlanConvention};
use super::push::{
    push_archive_metadata, push_work_unit, push_work_unit_with_remote, PushOutcomeKind,
    SetDepsOutcome, WorkUnitSink,
};
use qontinui_runner_lib::wedge_diagnostics::spawn_blocking_tracked;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Process-local adapter metrics, mirroring coord's `coord_plan_ingest_*`.
#[derive(Debug, Default)]
pub struct AdapterMetrics {
    /// Plans scanned in the last cycle (gauge-like; overwritten each cycle).
    pub scanned: AtomicU64,
    /// Total status transitions emitted (counter).
    pub transitions_total: AtomicU64,
    /// Total reconcile cycles run (counter).
    pub cycles_total: AtomicU64,
    /// Total remote-divergence conflicts surfaced (counter) — coord's
    /// `coord_plan_ingest_reverts_total` analogue.
    pub conflicts_total: AtomicU64,
    /// Total per-unit push errors (counter).
    pub errors_total: AtomicU64,
    /// Total transitions SUPPRESSED by the graduation-bootstrap deferral — a
    /// real (non-adapter) agent owns the unit, so the markdown proxy emitted
    /// nothing (counter). Counted separately from `transitions_total` because a
    /// deferral is a write that did NOT happen; folding it into the refresh
    /// count (the shipped behaviour) made the adapter's most consequential
    /// decision invisible in the cycle log.
    pub deferrals_total: AtomicU64,
    /// Total dependency-edge replace-sets applied to coord's edge table
    /// (`POST /coord/work-units/:slug/deps` 2xx) (counter).
    pub deps_set_total: AtomicU64,
    /// Total dep-set calls skipped because coord returned 503 (edge table not
    /// yet migrated — benign, JSONB fallback covers it) (counter).
    pub deps_skipped_unmigrated_total: AtomicU64,
    /// Total dep-set calls that hard-errored (counter). Best-effort: an error
    /// here does NOT fail the reconcile — the unit's upsert already succeeded
    /// and edges are additive.
    pub deps_errors_total: AtomicU64,
    /// Total `metadata.archive_path` stamps written by the archive scan
    /// (counter). Metadata-only — never a status transition (D4).
    pub archive_stamped_total: AtomicU64,
    /// Slugs coord refused with a `403` and this process has therefore retired
    /// (counter, monotonic — one increment per refused slug, not per cycle).
    /// A non-zero value here with a flat `errors_total` is the healthy shape:
    /// the adapter noticed a permission verdict and stopped re-asking.
    pub forbidden_total: AtomicU64,
    /// Scan roots in effect after the loop's last path resolution (gauge):
    /// the distinct configured directories among `plans_dir`,
    /// `plans_archive_dir` and `prompts_dir`. `0` while the tier is off.
    pub scan_roots: AtomicU64,
    /// Times the loop (re)built its resolved path set (counter): `1` after the
    /// first tick, `+1` for every settings change it picked up. `0` means the
    /// loop has not ticked yet — or was never spawned — so the two gauges
    /// beside it are not yet answers.
    pub path_resolutions_total: AtomicU64,
    /// The active plans dir the loop resolved on its last tick (gauge);
    /// `None` while the tier is off or before the first tick.
    pub active_plans_dir: std::sync::Mutex<Option<String>>,
    /// The last cycle's measurement of the scanned working tree against the
    /// ref it is supposed to represent (gauge) — see [`ScanDivergence`].
    ///
    /// Written on EVERY tick including the idle one, so `None` here means
    /// exactly one thing: the loop has not ticked yet (or was never spawned).
    /// It never means "no divergence" — a tier-off machine records
    /// [`ScanDivergenceState::NotScanning`] rather than leaving this empty,
    /// because an empty slot and a healthy scan reading the same is the defect
    /// the detector exists to end.
    pub scan_divergence: std::sync::Mutex<Option<ScanDivergence>>,
    /// Slugs whose dep-edge `set_deps` call coord refused with a `403`, and
    /// whose edge push this process has therefore retired (counter, monotonic
    /// — one increment per refused slug, not per cycle). Tracked separately
    /// from `forbidden_total`: the unit's own upsert/transition route can be
    /// permitted while the edge-table route is not (coord evaluates them as
    /// separate authorization checks), so a deps-only refusal must not retire
    /// the whole unit — see the `forbidden_deps` set in [`reconcile_once`].
    pub deps_forbidden_total: AtomicU64,
    /// Total units whose `last_applied` was seeded from coord's current status
    /// because this process had no memory of them (counter). Non-zero almost
    /// exclusively on the first cycle after a runner start — but that is true
    /// only while the BULK prime is working. A DEFERRED unit deliberately never
    /// enters `last_applied` (see [`reconcile_once`] for why the memory must
    /// stay untouched), so in the per-slug fallback regime every persistently
    /// deferred unit re-seeds every cycle. The bulk prime inserts directly into
    /// `last_applied`, which is what keeps those units covered.
    pub seeded_total: AtomicU64,
    /// Total units SKIPPED because their seed read failed (counter). A unit
    /// counted here was not pushed at all this cycle — see [`reconcile_once`]
    /// for why an unreadable remote status must abstain rather than overwrite.
    pub seed_errors_total: AtomicU64,
}

/// A point-in-time read of [`AdapterMetrics`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricsSnapshot {
    pub scanned: u64,
    pub transitions_total: u64,
    pub cycles_total: u64,
    pub conflicts_total: u64,
    pub errors_total: u64,
    pub deferrals_total: u64,
    pub deps_set_total: u64,
    pub deps_skipped_unmigrated_total: u64,
    pub deps_errors_total: u64,
    pub archive_stamped_total: u64,
    pub forbidden_total: u64,
    pub scan_roots: u64,
    pub path_resolutions_total: u64,
    pub active_plans_dir: Option<String>,
    /// The loop's last scan-divergence reading; `None` only before the first
    /// tick — see [`AdapterMetrics::scan_divergence`].
    pub scan_divergence: Option<ScanDivergence>,
    pub deps_forbidden_total: u64,
    pub seeded_total: u64,
    pub seed_errors_total: u64,
}

impl AdapterMetrics {
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            scanned: self.scanned.load(Ordering::Relaxed),
            transitions_total: self.transitions_total.load(Ordering::Relaxed),
            cycles_total: self.cycles_total.load(Ordering::Relaxed),
            conflicts_total: self.conflicts_total.load(Ordering::Relaxed),
            errors_total: self.errors_total.load(Ordering::Relaxed),
            deferrals_total: self.deferrals_total.load(Ordering::Relaxed),
            deps_set_total: self.deps_set_total.load(Ordering::Relaxed),
            deps_skipped_unmigrated_total: self
                .deps_skipped_unmigrated_total
                .load(Ordering::Relaxed),
            deps_errors_total: self.deps_errors_total.load(Ordering::Relaxed),
            archive_stamped_total: self.archive_stamped_total.load(Ordering::Relaxed),
            forbidden_total: self.forbidden_total.load(Ordering::Relaxed),
            scan_roots: self.scan_roots.load(Ordering::Relaxed),
            path_resolutions_total: self.path_resolutions_total.load(Ordering::Relaxed),
            active_plans_dir: self
                .active_plans_dir
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
            scan_divergence: self
                .scan_divergence
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone(),
            deps_forbidden_total: self.deps_forbidden_total.load(Ordering::Relaxed),
            seeded_total: self.seeded_total.load(Ordering::Relaxed),
            seed_errors_total: self.seed_errors_total.load(Ordering::Relaxed),
        }
    }
}

/// The shared process-wide adapter metrics.
pub fn adapter_metrics() -> &'static AdapterMetrics {
    static METRICS: OnceLock<AdapterMetrics> = OnceLock::new();
    METRICS.get_or_init(AdapterMetrics::default)
}

// ---------------------------------------------------------------------------
// Scan-source divergence — the DETECTOR half of plan
// `2026-09-10-the-plan-scanner-reads-a-parked-working-tree-not-a-ref`.
//
// [`read_plan_dir`] scans a WORKING TREE. Nothing about that tree is pinned:
// the directory the operator points `paths.plans_dir` at is an ordinary
// checkout, and a checkout can sit parked on a peer's branch for weeks.
// Measured on the operator box 2026-09-10, the configured dir was 2153 commits
// behind and 11 ahead of its own default branch, and the plan-library rows
// sourced from it agreed with that parked tree 96% of the time and with
// `origin/main` only 76%.
//
// The reason that went unnoticed for months is not that the number was bad —
// it is that NOBODY EVER COMPUTED IT. Every downstream surface (the work-unit
// rows, the plan library, this module's own cycle log) reports the scan as
// having succeeded, because by its own lights it did. This block computes the
// missing number once per cycle and publishes it beside the other gauges.
//
// It measures and it says. It does NOT change the scan source and it does NOT
// fetch: the reading is explicitly "as of this clone's last fetch", which is
// what makes landing the detector a behaviour-free change. Moving the scan onto
// a ref is Phase 2, deferred while four open PRs rewrite this seam.
// ---------------------------------------------------------------------------

/// Why a [`ScanDivergence`] reading says what it says.
///
/// Four states, deliberately not two. The whole failure this type exists to
/// end is that "in step with the ref", "nothing is being scanned at all" and
/// "the measurement did not work" were one indistinguishable silence — the
/// `silent-empty-is-unknown` shape. So each gets its own name, and no arm is
/// allowed to render as `0 behind / 0 ahead` unless the zeros were measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanDivergenceState {
    /// No `paths.plans_dir` is configured: the tier is OFF and NOTHING is
    /// scanned. Recorded — not skipped — precisely because an unrecorded idle
    /// cycle is indistinguishable from a healthy one to every reader.
    NotScanning,
    /// A plans dir is configured but is not inside a git work tree. A
    /// supported configuration (an operator may author into a plain
    /// directory), so it is not an error — but there is no ref to compare
    /// against, which is a different statement from "it matches the ref".
    NotAGitWorkTree,
    /// Measured against [`ScanDivergence::default_ref`]: `behind` and `ahead`
    /// are real counts, and zeros here mean zero.
    ///
    /// **Scoped to commits.** The comparison is `HEAD` against the ref, so
    /// `0/0` says the checked-out COMMIT is in step — it does NOT say the
    /// bytes being scanned match the ref. [`read_plan_dir`] reads the working
    /// tree, and a tree exactly on the default branch with uncommitted or
    /// untracked plan files still publishes content no ref carries. Comparing
    /// the tree itself belongs to the phase that moves the scan onto a ref.
    Measured,
    /// A plans dir is configured, it IS a work tree, and the measurement
    /// itself failed — no `origin/HEAD`, a git that would not run, an
    /// unreadable ref. UNKNOWN, never zero, and [`ScanDivergence::detail`]
    /// names which probe failed.
    Unknown,
}

impl ScanDivergenceState {
    /// The stable snake_case tag every read surface renders.
    ///
    /// Spelled here rather than derived through serde so this module stays
    /// serde-free: the projection that crosses the Tauri boundary lives in
    /// `commands::path_settings`, and this string is the contract between the
    /// two.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotScanning => "not_scanning",
            Self::NotAGitWorkTree => "not_a_git_work_tree",
            Self::Measured => "measured",
            Self::Unknown => "unknown",
        }
    }
}

/// One cycle's measurement of the scanned working tree against the ref it is
/// supposed to represent.
///
/// Every field beyond `state` is `Option` because each is only meaningful on
/// some arms — and an absent field is UNKNOWN, never a defaulted zero or an
/// empty string. `behind`/`ahead` are populated on
/// [`ScanDivergenceState::Measured`] alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanDivergence {
    pub state: ScanDivergenceState,
    /// The directory actually scanned, as configured.
    pub plans_dir: Option<String>,
    /// The git work-tree root containing `plans_dir`. Not the same thing —
    /// the plans dir is usually a subdirectory of the checkout.
    pub repo_root: Option<String>,
    /// The repo's OWN default branch, resolved at scan time from
    /// `origin/HEAD` (e.g. `origin/main`) — never a hardcoded guess. A clone
    /// whose default branch cannot be resolved is `Unknown`, because guessing
    /// `origin/main` on a repo whose default is something else would produce a
    /// confidently wrong divergence number, which is worse than none.
    pub default_ref: Option<String>,
    /// What `default_ref` points at in this clone, **as of its last fetch**.
    /// Phase 1 never fetches, so a long-unfetched clone reports a stale ref
    /// and a small divergence honestly rather than a fresh one it did not earn.
    /// How long ago that fetch was is [`Self::ref_age_secs`].
    pub ref_sha: Option<String>,
    /// What the scanned work tree's `HEAD` points at.
    pub head_sha: Option<String>,
    /// Commits on `default_ref` that the scanned tree's HEAD does NOT have —
    /// how far the scan source is stale. This is the number that read 2153.
    pub behind: Option<u64>,
    /// Commits on the scanned tree's HEAD that `default_ref` does NOT have —
    /// content the scan is publishing that no ref carries. This is the number
    /// that read 11.
    pub ahead: Option<u64>,
    /// Seconds since `default_ref` was last known to be refreshed in this
    /// clone — the fresher of a `FETCH_HEAD` that names the default branch at
    /// the sha the ref now holds, and the ref's newest reflog entry (see
    /// [`GitRefReader::ref_refreshed_at`]).
    ///
    /// This is what turns `behind` from a number into a claim. `behind` counts
    /// against the ref AS THIS CLONE LAST SAW IT, so a clone unfetched for a
    /// week reads a confidently low `behind` — the counts are then LOWER
    /// BOUNDS, and [`Self::counts_are_floors`] says so. `None` is UNKNOWN (no
    /// readable source, or the probe failed) and is treated as a floor too,
    /// never as fresh. Read on the `Measured` arm only; `None` elsewhere.
    pub ref_age_secs: Option<u64>,
    /// One line naming why the state is `Unknown` or `NotAGitWorkTree`. Never
    /// empty on those two states: an unexplained UNKNOWN is the same dead end
    /// as the silence this type replaces. On `Measured` it is `None` unless
    /// `ref_age_secs` is absent, in which case it names why the age is unknown.
    pub detail: Option<String>,
    /// The plan-library `source_repo` key of `plans_dir` — the same
    /// `<repo>/<dir>` identity the artifacts this dir produces carry
    /// (`body_push::derive_source_repo`). Resolved beside the git probes, on
    /// the blocking pool, because it walks the filesystem for a `.git`; the
    /// scan-root report only copies it. `None` when nothing is scanned, or
    /// when the reading was not produced by the tick's measurement.
    pub source_repo: Option<String>,
    /// When this reading was taken, as unix seconds — the clock the tick read
    /// for the measurement itself, NOT the time anything later forwarded it.
    /// `None` only for a reading no tick stamped (a hand-built one).
    pub observed_at_unix: Option<i64>,
}

/// How recently `default_ref` must have been refreshed for a `Measured`
/// reading's counts to be taken as current rather than as lower bounds.
///
/// Six hours: on a runner box the worktree census refreshes each canonical
/// repo's trunk every 300 s (`agent_worktree::census::fetch_trunk`), so a
/// healthy ref is minutes old and six hours of silence means the refresh
/// itself is not happening. It is deliberately not tighter — a laptop that
/// slept over lunch is not a fault — and not looser, because at the measured
/// ~64 commits/day a six-hour-old ref can already hide a dozen plans.
pub const SCAN_REF_FRESH_WITHIN: Duration = Duration::from_secs(6 * 60 * 60);

impl ScanDivergence {
    /// All-`None` reading in `state`. Private: every public constructor below
    /// fills in whatever that state is obliged to carry.
    fn blank(state: ScanDivergenceState) -> Self {
        Self {
            state,
            plans_dir: None,
            repo_root: None,
            default_ref: None,
            ref_sha: None,
            head_sha: None,
            behind: None,
            ahead: None,
            ref_age_secs: None,
            detail: None,
            source_repo: None,
            observed_at_unix: None,
        }
    }

    /// This reading, stamped as taken at `unix` seconds — see
    /// [`Self::observed_at_unix`].
    pub fn observed_at(self, unix: i64) -> Self {
        Self {
            observed_at_unix: Some(unix),
            ..self
        }
    }

    /// The tier is off — no plans dir is configured, so nothing is scanned.
    /// Recorded on every idle cycle.
    pub fn not_scanning() -> Self {
        Self::blank(ScanDivergenceState::NotScanning)
    }

    /// UNKNOWN with a named reason. Used for the failures that happen OUTSIDE
    /// [`measure_scan_divergence`] — a probe task that could not be joined —
    /// so those never degrade into silence either.
    pub fn unknown(plans_dir: Option<String>, detail: impl Into<String>) -> Self {
        Self {
            plans_dir,
            detail: Some(detail.into()),
            ..Self::blank(ScanDivergenceState::Unknown)
        }
    }

    /// `true` when the scan source measurably differs from the ref in EITHER
    /// direction — the condition the whole plan exists to surface.
    ///
    /// Ahead counts, not just behind. A checkout 0 behind and 11 ahead is
    /// publishing eleven commits' worth of plans that exist on no ref at all,
    /// which is the same class of invisible authority as a stale one; treating
    /// only `behind` as interesting would have let exactly that arm of the
    /// measured defect log at INFO.
    pub fn is_stale(&self) -> bool {
        self.state == ScanDivergenceState::Measured
            && (self.behind.unwrap_or(0) > 0 || self.ahead.unwrap_or(0) > 0)
    }

    /// Whether the ref the counts were taken against is known to be current
    /// — refreshed within [`SCAN_REF_FRESH_WITHIN`].
    ///
    /// `None` when there is nothing to judge: the reading is not `Measured`
    /// (no counts exist), or the ref's age is unknown. Deliberately three-way —
    /// an unknown age is not a stale one, but neither is it a fresh one, and
    /// [`Self::counts_are_floors`] is where the two unknowns meet.
    pub fn ref_is_fresh(&self) -> Option<bool> {
        if self.state != ScanDivergenceState::Measured {
            return None;
        }
        self.ref_age_secs
            .map(|age| age <= SCAN_REF_FRESH_WITHIN.as_secs())
    }

    /// `true` when the `Measured` counts are LOWER BOUNDS rather than current
    /// numbers: the ref is older than [`SCAN_REF_FRESH_WITHIN`] or of unknown
    /// age.
    ///
    /// The floor rule. `behind` compares HEAD against the ref as this clone
    /// last saw it, so every commit the remote gained since that refresh is
    /// missing from the count — the true divergence is at least the reported
    /// one. That makes a floor of `0/0` the dangerous reading: it LOOKS like
    /// agreement and proves nothing, so every read surface (the log line, the
    /// IPC view, the published scan-root row) carries this flag beside the
    /// counts. Only an age PROVEN fresh lifts it; an unknown age never does.
    ///
    /// Strictly it is `behind` that is the floor: the remote gaining commits
    /// can only raise it. `ahead` moves the other way — commits of HEAD's that
    /// have since landed on the remote stop counting once the ref catches up —
    /// so a stale `ahead` may OVERSTATE. Either way the pair is not current,
    /// which is the one thing the flag asserts.
    ///
    /// `false` on every non-`Measured` state, which has no counts to qualify.
    pub fn counts_are_floors(&self) -> bool {
        self.state == ScanDivergenceState::Measured && self.ref_is_fresh() != Some(true)
    }

    /// Whether `other` says the same thing as `self` — every field equal
    /// except the two that move with the clock: the raw `ref_age_secs`, which
    /// is compared only through its freshness verdict ([`Self::ref_is_fresh`]),
    /// and `observed_at_unix`, which is when, not what.
    ///
    /// Both grow every tick, so plain equality would make every reading
    /// "new": the edge-triggered log would fire every minute and the
    /// scan-root report would post every cycle. What a reader acts on is
    /// whether the counts are current or floors, so a crossing of the
    /// freshness window IS a change and a clock advancing inside it is not.
    /// (`detail` is compared verbatim, which is why every probe failure it
    /// quotes is worded without per-run noise such as a pid — see
    /// [`ProcessGit::describe`].)
    pub fn is_same_reading(&self, other: &ScanDivergence) -> bool {
        let strip = |d: &ScanDivergence| ScanDivergence {
            ref_age_secs: None,
            observed_at_unix: None,
            ..d.clone()
        };
        strip(self) == strip(other) && self.ref_is_fresh() == other.ref_is_fresh()
    }
}

/// The four git reads [`measure_scan_divergence`] needs, behind a trait so the
/// measurement is a pure function of its answers.
///
/// Injected rather than called directly because the interesting cases — no
/// `origin/HEAD`, a non-repo directory, a tree 2153 behind — are miserable to
/// build as real repos in a unit test and trivial to state as canned answers.
/// [`ProcessGit`] is the one production implementation.
pub trait GitRefReader: Send + Sync {
    /// The git work-tree root containing `dir`.
    ///
    /// Three outcomes, not two, and the split is the point: `Ok(Some(root))`
    /// located it, `Ok(None)` established that `dir` is definitively NOT
    /// inside a work tree (an ANSWER — `NotAGitWorkTree`), and `Err` means the
    /// question could not be ASKED at all (no `git` on PATH, a plans dir that
    /// does not exist, a probe that timed out on a stalled mount). Folding
    /// that third case into `Ok(None)` would report a broken measurement as
    /// the one benign state a reader is invited to shrug at — the exact
    /// conflation this type exists to end.
    fn work_tree_root(&self, dir: &Path) -> Result<Option<PathBuf>, String>;
    /// The repo's default branch as a remote-tracking ref name, e.g.
    /// `origin/main`. `Err` when it cannot be established — the caller turns
    /// that into `Unknown`, never into a default.
    fn default_ref(&self, repo_root: &Path) -> Result<String, String>;
    /// Resolve one rev to a full object id.
    fn rev_parse(&self, repo_root: &Path, rev: &str) -> Result<String, String>;
    /// `(behind, ahead)` for `head` measured against `reference`: how many
    /// commits `reference` has that `head` lacks, then the mirror. The tuple
    /// order matches `git rev-list --left-right --count <reference>...<head>`
    /// so the wire and the type cannot drift — see [`parse_left_right_count`].
    fn count_behind_ahead(
        &self,
        repo_root: &Path,
        reference: &str,
        head: &str,
    ) -> Result<(u64, u64), String>;
    /// When `default_ref` (e.g. `origin/main`, currently at `ref_sha`) was
    /// last known to be refreshed in this clone, as unix seconds.
    ///
    /// The FRESHER of two sources, because each misses a case the other
    /// catches: a fetch that finds the branch unchanged writes no reflog entry
    /// but does rewrite `FETCH_HEAD`, while a push updates the tracking ref
    /// (and its reflog) without any fetch at all.
    ///
    /// - `FETCH_HEAD`'s mtime — counted ONLY when it carries a line naming the
    ///   default branch (`branch '<name>' of …`) at exactly `ref_sha`. A fetch
    ///   of some other branch refreshes nothing this reading compares against,
    ///   and a `FETCH_HEAD` whose sha disagrees with the tracking ref (a
    ///   rejected non-fast-forward, a fetch by URL that updates no tracking
    ///   ref) proves nothing about it either.
    /// - The ENTRY time of the newest reflog record for
    ///   `refs/remotes/<default_ref>` — when the ref last moved. Not the tip
    ///   commit's committer time, which says when someone authored a commit,
    ///   not when this clone learned of it.
    ///
    /// `Ok(None)` means neither source exists — an ANSWER (nothing records a
    /// refresh), which the caller reports as an unknown age. `Err` means a
    /// probe failed and nothing was established. Partial failure resolves
    /// toward the OLDER claim: one source's timestamp with the other's probe
    /// failed is still returned, because a missed fresher source can only make
    /// the ref look older than it is — an overstated age, never a false
    /// "fresh".
    fn ref_refreshed_at(
        &self,
        repo_root: &Path,
        default_ref: &str,
        ref_sha: &str,
    ) -> Result<Option<i64>, String>;
}

/// Measure the scan source against the ref it should be reading.
///
/// Pure over `git` and the clock: every branch is reachable from a fake
/// reader, which is what makes the four states testable without a repo on
/// disk, and `now_unix` is a parameter so the ref's age is too.
pub fn measure_scan_divergence(
    plans_dir: Option<&Path>,
    git: &dyn GitRefReader,
    now_unix: i64,
) -> ScanDivergence {
    measure_unstamped(plans_dir, git, now_unix).observed_at(now_unix)
}

/// The tick's measurement: [`measure_scan_divergence`] plus the reading's
/// plan-library `source_repo` key.
///
/// Run on the blocking pool with the git probes because
/// [`super::body_push::derive_source_repo`] walks the filesystem for a `.git`
/// — work that does not belong on the single-worker runtime the scan-root
/// report is later posted from. Kept out of [`measure_scan_divergence`] so
/// that stays pure over its fake reader.
fn measure_scan_source(dir: &Path, git: &dyn GitRefReader, now_unix: i64) -> ScanDivergence {
    ScanDivergence {
        source_repo: super::body_push::derive_source_repo(dir),
        ..measure_scan_divergence(Some(dir), git, now_unix)
    }
}

/// [`measure_scan_divergence`] without the `observed_at` stamp, so the stamp
/// is applied in ONE place and no arm can return a reading without it.
fn measure_unstamped(
    plans_dir: Option<&Path>,
    git: &dyn GitRefReader,
    now_unix: i64,
) -> ScanDivergence {
    let Some(dir) = plans_dir else {
        return ScanDivergence::not_scanning();
    };
    let dir_str = dir.display().to_string();
    let root = match git.work_tree_root(dir) {
        Ok(Some(root)) => root,
        Ok(None) => {
            return ScanDivergence {
                plans_dir: Some(dir_str.clone()),
                detail: Some(format!(
                    "the configured plans dir `{dir_str}` is not inside a git work tree, so \
                     there is no ref to compare the scanned files against (this is a supported \
                     configuration, not a fault — it is reported so it cannot be mistaken for \
                     agreement with a ref)"
                )),
                ..ScanDivergence::blank(ScanDivergenceState::NotAGitWorkTree)
            }
        }
        // NOT `NotAGitWorkTree`: we never established that it isn't one.
        Err(e) => {
            return ScanDivergence {
                plans_dir: Some(dir_str.clone()),
                detail: Some(format!(
                    "cannot tell whether the configured plans dir `{dir_str}` is inside a git \
                     work tree, so nothing about the scan source is established: {e}"
                )),
                ..ScanDivergence::blank(ScanDivergenceState::Unknown)
            }
        }
    };
    let root_str = root.display().to_string();
    let base = ScanDivergence {
        plans_dir: Some(dir_str),
        repo_root: Some(root_str.clone()),
        ..ScanDivergence::blank(ScanDivergenceState::Unknown)
    };

    let default_ref = match git.default_ref(&root) {
        Ok(r) => r,
        Err(e) => {
            return ScanDivergence {
                detail: Some(format!(
                    "cannot resolve the default branch of `{root_str}`, so there is nothing to \
                     measure against: {e}"
                )),
                ..base
            }
        }
    };
    let base = ScanDivergence {
        default_ref: Some(default_ref.clone()),
        ..base
    };

    let ref_sha = match git.rev_parse(&root, &default_ref) {
        Ok(s) => s,
        Err(e) => {
            return ScanDivergence {
                detail: Some(format!(
                    "cannot resolve `{default_ref}` in `{root_str}`: {e}"
                )),
                ..base
            }
        }
    };
    let head_sha = match git.rev_parse(&root, "HEAD") {
        Ok(s) => s,
        Err(e) => {
            return ScanDivergence {
                detail: Some(format!("cannot resolve `HEAD` in `{root_str}`: {e}")),
                ..base
            }
        }
    };
    let base = ScanDivergence {
        ref_sha: Some(ref_sha.clone()),
        head_sha: Some(head_sha),
        ..base
    };

    match git.count_behind_ahead(&root, &default_ref, "HEAD") {
        Ok((behind, ahead)) => {
            // The counts are real either way; the age decides only whether
            // they are current or floors. So an age that cannot be read keeps
            // `Measured` — dropping to `Unknown` would throw away two true
            // numbers — and says why in `detail`, while `ref_age_secs: None`
            // makes `counts_are_floors()` true.
            let (ref_age_secs, detail) = match git.ref_refreshed_at(&root, &default_ref, &ref_sha) {
                Ok(Some(refreshed_at))
                    if refreshed_at.saturating_sub(now_unix) > SCAN_REF_FUTURE_TOLERANCE_SECS =>
                {
                    (
                        None,
                        Some(format!(
                            "the newest refresh record for `{default_ref}` in `{root_str}` is \
                             dated {}s in the FUTURE (a clock correction, or a restored \
                             file's mtime), so it proves nothing about when the ref was last \
                             refreshed — its age is unknown and the counts are lower bounds",
                            refreshed_at.saturating_sub(now_unix)
                        )),
                    )
                }
                Ok(Some(refreshed_at)) => (Some(ref_age_from(now_unix, refreshed_at)), None),
                Ok(None) => (
                    None,
                    Some(format!(
                        "neither a `FETCH_HEAD` naming `{default_ref}` at its current sha \
                             nor a reflog entry for `refs/remotes/{default_ref}` exists in \
                             `{root_str}`, so when the ref was last refreshed is unknown — the \
                             counts are lower bounds"
                    )),
                ),
                Err(e) => (
                    None,
                    Some(format!(
                        "cannot read when `{default_ref}` was last refreshed in \
                             `{root_str}`, so the counts are lower bounds: {e}"
                    )),
                ),
            };
            ScanDivergence {
                state: ScanDivergenceState::Measured,
                behind: Some(behind),
                ahead: Some(ahead),
                ref_age_secs,
                detail,
                ..base
            }
        }
        Err(e) => ScanDivergence {
            detail: Some(format!(
                "cannot count `{default_ref}`...`HEAD` in `{root_str}`: {e}"
            )),
            ..base
        },
    }
}

/// Build the symmetric-difference range for the ahead/behind count.
///
/// One named function purely so the ORDER can be pinned by test. The parse
/// below reads `<left>` as behind and `<right>` as ahead; that is only correct
/// while `reference` is interpolated LEFT of the `...` and `head` RIGHT of it.
/// Swap the two here and every reading inverts — a parked tree renders as a
/// lead — while [`parse_left_right_count`]'s own tests keep passing, because
/// the parser cannot see which rev produced which column.
fn left_right_range(reference: &str, head: &str) -> String {
    format!("{reference}...{head}")
}

/// Parse `git rev-list --left-right --count <reference>...<head>` output.
///
/// The two tab-separated numbers are `<left>\t<right>`, and the orientation is
/// the one thing here that can be silently wrong: LEFT counts commits
/// reachable from the LEFT rev (`<reference>`) and not from the right — the
/// scanned tree is BEHIND by that many — and RIGHT is the mirror, the scanned
/// tree's own commits no ref carries, AHEAD. Swap them and a 2153-behind park
/// renders as 2153 AHEAD, which reads like a busy authoring machine rather
/// than a stale one. Parsed in exactly one named place, and pinned by test.
fn parse_left_right_count(raw: &str) -> Result<(u64, u64), String> {
    let mut fields = raw.split_whitespace();
    let (Some(left), Some(right), None) = (fields.next(), fields.next(), fields.next()) else {
        return Err(format!(
            "expected two whitespace-separated counts from `rev-list --left-right --count`, got {raw:?}"
        ));
    };
    let behind = left
        .parse::<u64>()
        .map_err(|e| format!("left count {left:?} is not a number: {e}"))?;
    let ahead = right
        .parse::<u64>()
        .map_err(|e| format!("right count {right:?} is not a number: {e}"))?;
    Ok((behind, ahead))
}

/// How far in the future a refresh timestamp may sit and still be read as
/// "just now" (age 0).
///
/// Five minutes of skew between the file clock and the process clock is
/// ordinary (a network filesystem, an NTP step). Beyond it the timestamp is
/// not a small skew but a record that cannot be trusted — a clock corrected
/// backwards after a fetch, a checkout restored from a backup with future
/// mtimes — and reading it as age 0 would be a FALSE "fresh", the one outcome
/// the floor rule exists to prevent. So a further-future stamp is an UNKNOWN
/// age instead ([`measure_scan_divergence`]).
const SCAN_REF_FUTURE_TOLERANCE_SECS: i64 = 300;

/// `now - refreshed_at` in whole seconds, saturating at zero.
///
/// A refresh stamped slightly in the future (within
/// [`SCAN_REF_FUTURE_TOLERANCE_SECS`]) reads as age 0 rather than wrapping to
/// an astronomically old ref; a stamp further out never reaches here.
fn ref_age_from(now_unix: i64, refreshed_at: i64) -> u64 {
    u64::try_from(now_unix.saturating_sub(refreshed_at)).unwrap_or(0)
}

/// The branch name a remote-tracking ref stands for: `origin/main` → `main`,
/// `origin/release/x` → `release/x`. This is the spelling `FETCH_HEAD` uses in
/// its `branch '<name>' of <url>` descriptions.
fn default_branch_name(default_ref: &str) -> &str {
    default_ref
        .strip_prefix("origin/")
        .or_else(|| default_ref.split_once('/').map(|(_, branch)| branch))
        .unwrap_or(default_ref)
}

/// Whether `FETCH_HEAD` `contents` record a fetch of `branch` at `ref_sha`.
///
/// Each line is `<sha>\t<"" | "not-for-merge">\t<description>`, and a branch
/// fetch's description is `branch '<name>' of <url>`. Both halves must match:
/// the NAME, because a fetch of some other branch refreshes nothing this
/// reading compares against; and the SHA, because a `FETCH_HEAD` line whose
/// sha is not what the tracking ref now holds did not land in it — a
/// non-fast-forward the refspec refused, or a fetch by URL that updates no
/// tracking ref — so its mtime says nothing about the ref's freshness.
fn fetch_head_names_ref(contents: &str, branch: &str, ref_sha: &str) -> bool {
    let wanted = format!("branch '{branch}' of ");
    contents.lines().any(|line| {
        let mut fields = line.split('\t');
        let (Some(sha), Some(_merge_marker), Some(description)) =
            (fields.next(), fields.next(), fields.next())
        else {
            return false;
        };
        sha.trim() == ref_sha && description.starts_with(&wanted)
    })
}

/// Parse `git reflog show -n1 --date=unix --format=%gd <ref>` output — e.g.
/// `origin/main@{1789129011}` — into the entry's unix time.
///
/// Empty output is `Ok(None)`: the ref exists (it was just resolved) but has no
/// reflog (`core.logAllRefUpdates` off, or expired) — an absent source, not a
/// failure. Anything else that is not a plausible unix time is an error rather
/// than a guess: in particular `@{0}`, which is what `%gd` prints if the date
/// format is not applied, must not parse as "refreshed in 1970".
fn parse_reflog_entry_time(raw: &str) -> Result<Option<i64>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let inner = raw
        .rsplit_once("@{")
        .and_then(|(_, rest)| rest.strip_suffix('}'))
        .ok_or_else(|| format!("expected `<ref>@{{<unix time>}}` from the reflog, got {raw:?}"))?;
    let secs = inner
        .parse::<i64>()
        .map_err(|e| format!("reflog selector {inner:?} is not a unix time: {e}"))?;
    // 2001-09-09: anything earlier is a selector index, not a timestamp.
    if secs < 1_000_000_000 {
        return Err(format!(
            "reflog selector {raw:?} is not a unix time (was `--date=unix` ignored?)"
        ));
    }
    Ok(Some(secs))
}

/// Combine the two refresh sources into one answer — the FRESHER wins.
///
/// Partial failure resolves toward the older claim: a known timestamp beside
/// a failed probe is returned as-is, because the probe that failed could only
/// have made the ref look FRESHER — so the result is at worst an overstated
/// age, which reads as a floor, never a false "fresh". A failed probe beside
/// an absent source is an error: nothing at all was established.
fn fresher_refresh(
    a: Result<Option<i64>, String>,
    b: Result<Option<i64>, String>,
) -> Result<Option<i64>, String> {
    match (a, b) {
        (Ok(x), Ok(y)) => Ok(x.max(y)),
        (Ok(Some(x)), Err(_)) | (Err(_), Ok(Some(x))) => Ok(Some(x)),
        (Ok(None), Err(e)) | (Err(e), Ok(None)) => Err(e),
        (Err(a), Err(b)) => Err(format!("{a}; {b}")),
    }
}

/// One `FETCH_HEAD` file's contribution: its mtime when it records a fetch of
/// `branch` at `ref_sha` ([`fetch_head_names_ref`]), `Ok(None)` when it is
/// absent or names something else.
fn fetch_head_file_refreshed_at(
    path: &Path,
    branch: &str,
    ref_sha: &str,
) -> Result<Option<i64>, String> {
    // Stat BEFORE reading. A fetch landing between the two then pairs an
    // OLDER mtime with newer contents — an overstated age — where the
    // opposite order could pair a fresh mtime from a fetch of some other
    // branch with contents that still named this one: a false "fresh".
    let modified = match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot stat `{}`: {e}", path.display())),
    };
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read `{}`: {e}", path.display())),
    };
    if !fetch_head_names_ref(&contents, branch, ref_sha) {
        return Ok(None);
    }
    let secs = modified
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("`{}` has an mtime before the epoch: {e}", path.display()))?
        .as_secs();
    Ok(Some(i64::try_from(secs).unwrap_or(i64::MAX)))
}

/// Budget for every `git` invocation the detector makes.
///
/// All of them are LOCAL plumbing reads, so a healthy call is milliseconds; the
/// bound exists for a concurrent `index.lock` or a repo on a stalled mount.
/// They run on the blocking pool (see the reconcile loop's tick), but an unbounded
/// hang there still leaks a pool thread per cycle, so every one is capped —
/// the same posture as `git_status_subset`'s `GIT_TIMEOUT`.
const SCAN_DIVERGENCE_GIT_TIMEOUT: Duration = Duration::from_secs(20);

/// The production [`GitRefReader`]: shells out to `git`, always with an
/// explicit `-C <dir>` so the probe can never pick up the runner's own cwd.
///
/// Every probe goes through `run_probe_quiet`, whose non-zero arm is DEBUG:
/// three of the four reads answer negatively as a matter of routine (a plans
/// dir outside a repo, a clone with no `origin/HEAD`), and WARNing on those
/// would bury the timeout WARN that does matter.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessGit;

impl ProcessGit {
    /// Run one probe, keeping the [`crate::process_helpers::DegradeReason`]
    /// intact. Callers need it: a non-zero exit is a real ANSWER for three of
    /// the four reads ("not a work tree", "no `origin/HEAD`"), while a spawn
    /// failure or a timeout is the measurement itself breaking, and the two
    /// must not collapse.
    fn probe(
        dir: &Path,
        args: &[&str],
        label: &str,
    ) -> Result<String, crate::process_helpers::DegradeReason> {
        let mut cmd = crate::process_helpers::no_window("git");
        cmd.arg("-C").arg(dir).args(args);
        match crate::process_helpers::run_probe_quiet(cmd, SCAN_DIVERGENCE_GIT_TIMEOUT, label) {
            crate::process_helpers::ProbeOutcome::Captured(out) => {
                Ok(String::from_utf8_lossy(&out).trim().to_string())
            }
            crate::process_helpers::ProbeOutcome::Degraded(reason) => Err(reason),
        }
    }

    /// [`Self::probe`] with every degrade flattened to a sentence — for the
    /// two reads whose non-zero exit carries no extra meaning.
    fn run(dir: &Path, args: &[&str], label: &str) -> Result<String, String> {
        Self::probe(dir, args, label).map_err(|reason| Self::describe(args, &reason))
    }

    /// One sentence for a probe that did not answer — worded WITHOUT per-run
    /// noise. `DegradeReason`'s `Debug` form carries the killed child's pid
    /// (`TimedOut { pid: 41873, .. }`), and this text reaches
    /// [`ScanDivergence::detail`], which [`ScanDivergence::is_same_reading`]
    /// compares verbatim: a pid in it made a probe that times out every tick
    /// a NEW reading every tick — a WARN and a scan-root POST per minute for
    /// what is one unchanging fault.
    fn describe(args: &[&str], reason: &crate::process_helpers::DegradeReason) -> String {
        use crate::process_helpers::DegradeReason;
        let why = match reason {
            DegradeReason::Status => "it exited non-zero".to_string(),
            DegradeReason::SpawnError => "it could not be spawned (SpawnError)".to_string(),
            DegradeReason::TimedOut { reaped, .. } => format!(
                "it overran its {}s budget and was killed (TimedOut{})",
                SCAN_DIVERGENCE_GIT_TIMEOUT.as_secs(),
                if *reaped { "" } else { ", not reaped" }
            ),
            DegradeReason::Truncated(t) => format!("its output was truncated ({t:?})"),
        };
        format!("`git {}` did not answer: {why}", args.join(" "))
    }

    /// The `FETCH_HEAD` source of [`GitRefReader::ref_refreshed_at`]: the
    /// fresher mtime of the two `FETCH_HEAD` files a checkout can have, each
    /// counted only when it records a fetch of the default branch at
    /// `ref_sha`.
    ///
    /// Two files because `FETCH_HEAD` is PER-WORKTREE while the tracking ref
    /// and its reflog are shared. In a linked worktree `--git-path FETCH_HEAD`
    /// names that worktree's own file — which the worktree census's fetch,
    /// run in the PRIMARY checkout, never writes — so reading it alone would
    /// miss the refresh that actually updated the shared ref. The primary's
    /// file is `<git-common-dir>/FETCH_HEAD`. In a primary checkout both paths
    /// are the same file and reading it twice changes nothing.
    ///
    /// Both paths come from `rev-parse`, never a hardcoded `.git/…`: git
    /// prints them relative to the `-C` dir (or absolute), so each is joined
    /// onto `repo_root` — an absolute path replaces the base on join.
    fn fetch_head_refreshed_at(
        repo_root: &Path,
        default_ref: &str,
        ref_sha: &str,
    ) -> Result<Option<i64>, String> {
        let located = Self::run(
            repo_root,
            &["rev-parse", "--git-path", "FETCH_HEAD"],
            "plan adapter: scan-divergence FETCH_HEAD path probe",
        )?;
        if located.is_empty() {
            return Err("`git rev-parse --git-path FETCH_HEAD` returned nothing".to_string());
        }
        let common = Self::run(
            repo_root,
            &["rev-parse", "--git-common-dir"],
            "plan adapter: scan-divergence git-common-dir probe",
        )?;
        if common.is_empty() {
            return Err("`git rev-parse --git-common-dir` returned nothing".to_string());
        }
        let branch = default_branch_name(default_ref);
        fresher_refresh(
            fetch_head_file_refreshed_at(&repo_root.join(located), branch, ref_sha),
            fetch_head_file_refreshed_at(
                &repo_root.join(common).join("FETCH_HEAD"),
                branch,
                ref_sha,
            ),
        )
    }

    /// The reflog source of [`GitRefReader::ref_refreshed_at`]: the ENTRY time
    /// of the newest reflog record for `refs/remotes/<default_ref>`.
    ///
    /// `%gd` with `--date=unix` renders the selector as `<ref>@{<entry time>}`
    /// — the time the ref was written. `%ct` would be the tip COMMIT's
    /// committer time, which says when a commit was authored upstream, not
    /// when this clone received it: a week-old commit fetched a minute ago
    /// would read a week stale, and a fresh commit on a clone unfetched since
    /// would read as refreshed at authoring time.
    fn reflog_refreshed_at(repo_root: &Path, default_ref: &str) -> Result<Option<i64>, String> {
        let tracking = format!("refs/remotes/{default_ref}");
        let out = Self::run(
            repo_root,
            &[
                "reflog",
                "show",
                "-n1",
                "--date=unix",
                "--format=%gd",
                &tracking,
            ],
            "plan adapter: scan-divergence reflog probe",
        )?;
        parse_reflog_entry_time(&out)
    }
}

impl GitRefReader for ProcessGit {
    fn work_tree_root(&self, dir: &Path) -> Result<Option<PathBuf>, String> {
        const ARGS: [&str; 2] = ["rev-parse", "--show-toplevel"];
        match Self::probe(dir, &ARGS, "plan adapter: scan-divergence work-tree probe") {
            Ok(out) if out.is_empty() => Ok(None),
            Ok(out) => Ok(Some(PathBuf::from(out))),
            // A non-zero exit here is git's own answer — "not a git repository"
            // — and the one degrade that means `dir` genuinely is not a work
            // tree. A missing binary, a nonexistent dir or a timeout tells us
            // nothing about `dir`, so it must not be reported as if it did.
            Err(crate::process_helpers::DegradeReason::Status) => Ok(None),
            Err(reason) => Err(Self::describe(&ARGS, &reason)),
        }
    }

    fn default_ref(&self, repo_root: &Path) -> Result<String, String> {
        const ARGS: [&str; 4] = [
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ];
        // `--quiet` makes an unset `origin/HEAD` exit NON-ZERO with empty
        // stdout rather than printing nothing and exiting 0, so the actionable
        // sentence has to hang off the `Status` arm — hung off an empty `Ok`
        // it would be unreachable, and the operator would get `did not answer`
        // for the one failure that has a one-line fix.
        //
        // Deliberately an error either way, never a fallback: `origin/main` is
        // a guess, and a divergence measured against the wrong branch is a
        // confident wrong number, which is worse than UNKNOWN.
        const UNSET: &str = "`origin/HEAD` is not set in this clone (`git remote set-head \
                             origin -a` sets it); refusing to assume a default branch";
        match Self::probe(
            repo_root,
            &ARGS,
            "plan adapter: scan-divergence default-branch probe",
        ) {
            Ok(out) if out.is_empty() => Err(UNSET.to_string()),
            Ok(out) => Ok(out),
            Err(crate::process_helpers::DegradeReason::Status) => Err(UNSET.to_string()),
            Err(reason) => Err(Self::describe(&ARGS, &reason)),
        }
    }

    fn rev_parse(&self, repo_root: &Path, rev: &str) -> Result<String, String> {
        let out = Self::run(
            repo_root,
            &["rev-parse", rev],
            "plan adapter: scan-divergence rev-parse probe",
        )?;
        if out.is_empty() {
            return Err(format!("`git rev-parse {rev}` returned nothing"));
        }
        Ok(out)
    }

    fn count_behind_ahead(
        &self,
        repo_root: &Path,
        reference: &str,
        head: &str,
    ) -> Result<(u64, u64), String> {
        let range = left_right_range(reference, head);
        let out = Self::run(
            repo_root,
            &["rev-list", "--left-right", "--count", &range],
            "plan adapter: scan-divergence ahead/behind probe",
        )?;
        parse_left_right_count(&out)
    }

    fn ref_refreshed_at(
        &self,
        repo_root: &Path,
        default_ref: &str,
        ref_sha: &str,
    ) -> Result<Option<i64>, String> {
        fresher_refresh(
            Self::fetch_head_refreshed_at(repo_root, default_ref, ref_sha),
            Self::reflog_refreshed_at(repo_root, default_ref),
        )
    }
}

/// What [`record_scan_divergence`] says about a changed reading: `(at_warn,
/// message)`. A pure function so the wording — which is the whole point of
/// the floor rule — is pinned by test rather than asserted in a comment.
///
/// Three shapes:
/// - **floors** ([`ScanDivergence::counts_are_floors`]) — WARN, and the text
///   says the counts are LOWER BOUNDS and names the ref's age (or that it is
///   unknown). This covers a floor of `0/0`: it must never reach the log as
///   the benign "reading changed" line, because a reader skimming for WARNs
///   would take it for agreement with a ref it was never compared against.
/// - **stale against a fresh ref** — WARN, the parked-tree text, with the age.
/// - everything else — INFO, a plain transition note.
fn scan_divergence_message(d: &ScanDivergence) -> (bool, String) {
    let default_ref = d.default_ref.as_deref().unwrap_or("the default branch");
    let window_hours = SCAN_REF_FRESH_WITHIN.as_secs() / 3600;
    if d.counts_are_floors() {
        let as_of = match d.ref_age_secs {
            Some(age) => format!(
                "as of a refresh {age}s (~{}h) ago, older than the {window_hours}h freshness \
                 window",
                age / 3600
            ),
            None => format!(
                "as of a refresh of UNKNOWN age (nothing proves it happened within the \
                 {window_hours}h freshness window)"
            ),
        };
        let message = if d.is_stale() {
            format!(
                "plan adapter: the scanned plans dir is a WORKING TREE that has diverged from \
                 its own default branch, and the counts are LOWER BOUNDS: they were taken \
                 against `{default_ref}` {as_of}, so the true `behind` can only be larger (and \
                 `ahead` may overstate). Every work unit and plan body pushed from this machine \
                 reflects that parked tree, not the ref (the adapter never fetches)"
            )
        } else {
            format!(
                "plan adapter: the scanned plans dir's HEAD reads 0 behind / 0 ahead of \
                 `{default_ref}`, but that is a LOWER BOUND, not agreement: the ref was compared \
                 {as_of}, so how far the scan source has fallen behind is UNKNOWN until the ref \
                 is refreshed (the adapter never fetches)"
            )
        };
        return (true, message);
    }
    if d.is_stale() {
        let as_of = d
            .ref_age_secs
            .map(|age| format!("a refresh {age}s ago"))
            .unwrap_or_else(|| "this clone's last fetch".to_string());
        return (
            true,
            format!(
                "plan adapter: the scanned plans dir is a WORKING TREE that is behind its own \
                 default branch — every work unit and plan body pushed from this machine \
                 reflects that parked tree, not the ref. Counts are as of {as_of} (the adapter \
                 never fetches)"
            ),
        );
    }
    (
        false,
        "plan adapter: scan-source divergence reading changed".to_string(),
    )
}

/// Whether [`record_scan_divergence`] logs `new` — i.e. whether it differs
/// from the reading it replaces by [`ScanDivergence::is_same_reading`], never
/// by plain equality: `observed_at_unix` is re-stamped every tick and
/// `ref_age_secs` grows every tick, so plain equality would log every minute.
/// The first reading after start (no previous) always logs.
fn scan_divergence_changed(previous: Option<&ScanDivergence>, new: &ScanDivergence) -> bool {
    !previous.is_some_and(|previous| previous.is_same_reading(new))
}

/// Publish this cycle's reading, and log it **only when it changed**.
///
/// Every cycle records; only a transition logs. A per-cycle line for a
/// steady-state reading is pure volume at a 60s tick, and volume is how the
/// one line that matters gets missed. "Changed" is
/// [`ScanDivergence::is_same_reading`], not plain equality: the ref's age
/// grows every tick, and only its crossing of the freshness window is news. A
/// stale scan source ([`ScanDivergence::is_stale`]) or a floor reading
/// ([`ScanDivergence::counts_are_floors`]) logs at WARN — see
/// [`scan_divergence_message`].
fn record_scan_divergence(divergence: ScanDivergence, metrics: &AdapterMetrics) {
    let mut slot = metrics
        .scan_divergence
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if scan_divergence_changed(slot.as_ref(), &divergence) {
        let (at_warn, message) = scan_divergence_message(&divergence);
        if at_warn {
            tracing::warn!(
                state = divergence.state.as_str(),
                plans_dir = ?divergence.plans_dir,
                repo_root = ?divergence.repo_root,
                default_ref = ?divergence.default_ref,
                behind = ?divergence.behind,
                ahead = ?divergence.ahead,
                ref_age_secs = ?divergence.ref_age_secs,
                counts_are_floors = divergence.counts_are_floors(),
                detail = ?divergence.detail,
                "{message}"
            );
        } else {
            tracing::info!(
                state = divergence.state.as_str(),
                plans_dir = ?divergence.plans_dir,
                repo_root = ?divergence.repo_root,
                default_ref = ?divergence.default_ref,
                behind = ?divergence.behind,
                ahead = ?divergence.ahead,
                ref_age_secs = ?divergence.ref_age_secs,
                counts_are_floors = divergence.counts_are_floors(),
                detail = ?divergence.detail,
                "{message}"
            );
        }
    }
    *slot = Some(divergence);
}

/// Outcome of one reconcile cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ReconcileSummary {
    pub scanned: u64,
    pub transitions: u64,
    pub conflicts: u64,
    pub errors: u64,
    /// Transitions suppressed because a real agent owns the unit
    /// ([`PushOutcomeKind::Deferred`]). Not an error and not a transition.
    pub deferred: u64,
    /// Dependency-edge replace-sets applied to coord's edge table this cycle.
    pub deps_set: u64,
    /// Dep-set calls skipped because coord's edge table isn't migrated yet.
    pub deps_skipped_unmigrated: u64,
    /// Dep-set calls that hard-errored (does not count toward `errors`, which
    /// is reserved for the unit upsert/transition path — a dep-edge failure is
    /// non-fatal and additive).
    pub deps_errors: u64,
    /// Units skipped or retired this cycle because coord answered `403`
    /// ([`super::push::ForbiddenByCoord`]). Deliberately NOT folded into
    /// `errors`: `errors` means "retryable, and we will retry", which is the
    /// one thing a permission verdict is not.
    pub forbidden: u64,
    /// Dep-edge pushes skipped or retired this cycle because coord answered
    /// `403` on `POST /coord/work-units/:slug/deps` specifically. Not folded
    /// into `deps_errors` for the same reason `forbidden` is kept out of
    /// `errors` — and not folded into `forbidden`, since a deps refusal does
    /// not imply the unit's own upsert/transition route is refused too.
    pub deps_forbidden: u64,
    /// Units whose `last_applied` was seeded from coord's current status this
    /// cycle (no in-process memory of them).
    pub seeded: u64,
    /// Units SKIPPED this cycle because the seed read failed. These are NOT
    /// counted in `errors` (nothing was pushed) and NOT in `scanned`'s
    /// success sense — they are an explicit abstention.
    pub seed_errors: u64,
}

/// The provenance path RECORDED for one scanned plan file: the scan root's
/// repo-relative prefix, joined with the file's own name, `/`-separated.
///
/// **Never the absolute path.** This string is what the adapter ships to coord
/// as `metadata.source_path` (and, for an archived plan, `metadata.archive_path`),
/// and onward to the plan library as `agent.work_artifacts.source_path` — a
/// corpus every machine in the fleet reads. An authoring machine's own
/// filesystem path resolves nowhere else, and resolving nowhere is
/// indistinguishable from the plan not existing. Measured 2026-09-06: 1177 of
/// 2648 served work units were in exactly that state, split between
/// `D:\qontinui-root\...` and `/home/<user>/...`, because this function used
/// to be `entry.path().to_string_lossy()`.
///
/// `root` is [`super::body_push::derive_source_repo`] of the scan dir — the
/// same two-component `<repo>/<dir relative to the repo root>` form the plan
/// library already stores as `source_repo`, so the two layers join by
/// construction: `source_path == source_repo + "/" + file name`.
fn relative_source_path(root: Option<&str>, path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        // A directory entry always has a file name; fall back to the whole
        // path rather than dropping the entry, and normalize separators so
        // even this arm cannot emit a backslash.
        .unwrap_or_else(|| path.to_string_lossy().replace('\\', "/"));
    match root {
        Some(r) if !r.is_empty() => format!("{r}/{name}"),
        _ => name,
    }
}

/// Read + parse every `*.md` in `dir` (non-recursive — the plans dir is flat,
/// matching coord's `walk_root`). IO errors on individual files are logged and
/// skipped; a missing dir yields an empty vec.
///
/// The absolute path is still what is OPENED and what is logged on an IO
/// error; only the path RECORDED on the parsed unit is made relative — see
/// [`relative_source_path`].
pub fn read_plan_dir(dir: &Path, conv: &PlanConvention) -> Vec<ParsedWorkUnit> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(dir = %dir.display(), error = %e, "plan adapter: cannot read plans dir");
            return Vec::new();
        }
    };
    // Resolved ONCE per scan, not per file: it walks the ancestor chain
    // looking for `.git`, and every entry in this directory shares the answer.
    let source_root = super::body_push::derive_source_repo(dir);
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let path_str = path.to_string_lossy().to_string();
        let source_path = relative_source_path(source_root.as_deref(), &path);
        match std::fs::read_to_string(&path) {
            Ok(body) => {
                let slug = slug_from_filename(&path_str);
                out.push(parse_work_unit(&slug, &source_path, &body, conv));
            }
            Err(e) => {
                tracing::warn!(path = %path_str, error = %e, "plan adapter: cannot read plan file");
            }
        }
    }
    out
}

/// Push every parsed unit through the edge-trigger + conflict logic, updating
/// the client-side `last_applied` memory and the shared metrics. Pure of IO
/// beyond the sink, so it is unit-tested with a fake sink.
pub async fn reconcile_once<S: WorkUnitSink + ?Sized>(
    parsed_units: &[ParsedWorkUnit],
    last_applied: &mut HashMap<String, String>,
    last_deps: &mut HashMap<String, Vec<String>>,
    forbidden: &mut HashSet<String>,
    forbidden_deps: &mut HashSet<String>,
    sink: &S,
    metrics: &AdapterMetrics,
) -> ReconcileSummary {
    let mut summary = ReconcileSummary {
        scanned: parsed_units.len() as u64,
        ..Default::default()
    };
    for u in parsed_units {
        // A slug coord has already refused (403) is retired for the life of the
        // process: the request would be byte-identical, so the verdict would be
        // too. Skipping here — rather than merely muting the log — is what makes
        // this a fix and not a mute: it also stops the HTTP call.
        if forbidden.contains(&u.slug) {
            summary.forbidden += 1;
            continue;
        }
        // `last_applied` is per-PROCESS memory, rebuilt empty by `run_loop` on
        // every runner start. Without seeding, the first cycle after a start
        // sees `None` for EVERY slug — including slugs coord already has rows
        // for — and `decide_push` returns `UpsertWithStatus`, which sends
        // `status: Some(<file status>)`. That path bypasses BOTH safety
        // mechanisms below it:
        //
        //   * the agent-ownership deferral in `push_work_unit` is gated on
        //     `PushAction::Transition`, on the reasoning that an
        //     `UpsertWithStatus` is a brand-new unit with no agent owner to
        //     defer to — true per-process, NOT true per-row; and
        //   * the remote-divergence conflict check is inside
        //     `if let Some(prev) = last_applied`, so it is skipped entirely and
        //     logs nothing.
        //
        // coord then applies `status = COALESCE($3, work_units.status)`, and a
        // non-NULL `$3` OVERWRITES. Measured on this fleet 2026-09-01 against
        // 488 plan-backed units: 233 file statuses diverged from coord and 88
        // would have demoted a TERMINAL status (48 shipped -> in_progress,
        // 11 shipped -> partial, 5 shipped -> draft, ...) — silently, on every
        // runner start, not just the first.
        //
        // Seeding from the remote row restores the intended semantics: a unit
        // coord already knows becomes `RefreshOnly` (status unchanged, sends
        // `None`, COALESCE preserves) or a `Transition` that DOES pass through
        // the ownership deferral and conflict check. A slug coord has never
        // seen still reads `None` and is still created, which is correct.
        let mut prev = last_applied.get(&u.slug).cloned();
        // The seed read, when this cycle paid for one. `Some(_)` is a
        // SUCCESSFUL read and is handed to the push as `known_remote` so the
        // deferral's convergence check and the conflict check below it do not
        // re-read the same value: without it every seeded unit costs TWO GETs,
        // not one, and the cold cycle is ~2,400 serialized reads rather than
        // ~1,200. There is no spelling of the hint that means "my read failed"
        // — a failed read abstains and never reaches the push, which is exactly
        // the contract `push_work_unit_with_remote` documents.
        let mut seed_read: Option<Option<String>> = None;
        if prev.is_none() {
            match sink.current_status(&u.slug).await {
                // Coord already has this unit. Treat its status as what we last
                // applied so the edge-trigger compares against reality.
                Ok(remote) => {
                    if let Some(remote) = &remote {
                        summary.seeded += 1;
                        metrics.seeded_total.fetch_add(1, Ordering::Relaxed);
                        prev = Some(remote.clone());
                    }
                    // `Ok(None)` is genuinely absent -> a real create, and
                    // `prev` stays `None`, which is correct.
                    seed_read = Some(remote);
                }
                // UNKNOWN. Falling through with `prev = None` would re-create
                // the row from the file and is exactly the overwrite this seed
                // exists to prevent, so ABSTAIN: skip the unit this cycle and
                // count it. The next cycle retries; nothing is lost, because a
                // plan file that still differs is still there to be pushed.
                Err(e) => {
                    summary.seed_errors += 1;
                    metrics.seed_errors_total.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        slug = %u.slug,
                        error = %format!("{e:#}"),
                        "plan adapter: cannot read remote status to seed last-applied; \
                         SKIPPING this unit rather than risk overwriting coord's status"
                    );
                    continue;
                }
            }
        }
        let known_remote = seed_read.as_ref().map(|s| s.as_deref());
        match push_work_unit_with_remote(sink, u, prev.as_deref(), known_remote).await {
            Ok(outcome) => {
                if outcome.conflict {
                    summary.conflicts += 1;
                    metrics.conflicts_total.fetch_add(1, Ordering::Relaxed);
                }
                if matches!(outcome.kind, PushOutcomeKind::Transitioned { .. }) {
                    summary.transitions += 1;
                    metrics.transitions_total.fetch_add(1, Ordering::Relaxed);
                }
                let deferred = matches!(outcome.kind, PushOutcomeKind::Deferred { .. });
                if deferred {
                    summary.deferred += 1;
                    metrics.deferrals_total.fetch_add(1, Ordering::Relaxed);
                }
                // Record what we just applied so the next cycle is edge-triggered
                // — but ONLY when something was actually applied. A deferral
                // wrote NOTHING, so recording it as applied would be a lie with
                // two consequences: the next cycle would answer `RefreshOnly`
                // and stop re-checking (so a PERSISTENT deferral would be
                // counted exactly once, in the first cycle after start, and
                // every later cycle would log `deferred=0` — indistinguishable
                // from "no divergence", the very defect this counter closes);
                // and once the file moved again the stale memory would make
                // `push_work_unit`'s conflict check warn "file wins (loud
                // override)" every cycle forever while the file demonstrably did
                // not win. Leaving the memory untouched makes the deferral
                // re-evaluated every cycle, so `deferred` reads as a live gauge
                // of "units an agent currently owns and the file disagrees
                // with".
                if !deferred {
                    last_applied.insert(u.slug.clone(), u.status.clone());
                }

                // After the unit's upsert/transition succeeded, ALSO push its
                // dependency set to coord's first-class edge table (additive to
                // the metadata.depends_on JSONB fallback the upsert already
                // wrote). Best-effort: a 503 (table not migrated) is benign and
                // a hard error does NOT fail the reconcile — the unit already
                // landed and edges are additive. Edge-triggered: only re-send
                // when the dep set changed since we last applied it (the
                // replace-set is idempotent, so this is purely an optimization).
                let deps_changed =
                    !u.depends_on.is_empty() && last_deps.get(&u.slug) != Some(&u.depends_on);
                if deps_changed && forbidden_deps.contains(&u.slug) {
                    // Mirrors the top-level `forbidden` skip above, scoped to the
                    // deps route alone: coord refused THIS route for THIS slug
                    // before, and the replace-set would be byte-identical, so
                    // re-asking cannot change the verdict.
                    summary.deps_forbidden += 1;
                } else if deps_changed {
                    match sink.set_deps(&u.slug, &u.depends_on).await {
                        Ok(SetDepsOutcome::Ok { edges_set }) => {
                            summary.deps_set += 1;
                            metrics.deps_set_total.fetch_add(1, Ordering::Relaxed);
                            last_deps.insert(u.slug.clone(), u.depends_on.clone());
                            tracing::debug!(
                                slug = %u.slug,
                                edges_set,
                                "plan adapter: dep edges set on coord edge table"
                            );
                        }
                        Ok(SetDepsOutcome::TableNotMigrated) => {
                            summary.deps_skipped_unmigrated += 1;
                            metrics
                                .deps_skipped_unmigrated_total
                                .fetch_add(1, Ordering::Relaxed);
                            // Do NOT cache last_deps: the table isn't there yet,
                            // so we want to retry the edge write next cycle once
                            // the migration lands.
                            tracing::debug!(
                                slug = %u.slug,
                                "plan adapter: dep edge table not yet migrated; \
                                 JSONB fallback covers deps, will retry"
                            );
                        }
                        Err(e) => {
                            // Same distinction as the main push above, scoped to
                            // this route: a 403 here is settled and retired, an
                            // ordinary failure is retried every cycle (best-effort,
                            // as before — it still does not fail the reconcile).
                            if let Some(f) = e.downcast_ref::<crate::plan_workunit_adapter::push::ForbiddenByCoord>()
                            {
                                forbidden_deps.insert(u.slug.clone());
                                summary.deps_forbidden += 1;
                                metrics.deps_forbidden_total.fetch_add(1, Ordering::Relaxed);
                                tracing::warn!(
                                    slug = %u.slug,
                                    route = %f.route,
                                    detail = %f.detail,
                                    "plan adapter: coord refused this unit's dep-edge set (403); \
                                     retiring the edge push for the life of this process (the \
                                     unit's own upsert/transition route is unaffected). Restart \
                                     the runner after fixing the principal's permission."
                                );
                            } else {
                                summary.deps_errors += 1;
                                metrics.deps_errors_total.fetch_add(1, Ordering::Relaxed);
                                tracing::warn!(
                                    slug = %u.slug,
                                    error = %format!("{e:#}"),
                                    "plan adapter: dep-edge set failed (non-fatal; \
                                     unit upsert succeeded, edges are additive)"
                                );
                            }
                        }
                    }
                }
            }
            Err(e) => {
                // A 403 is a settled permission verdict, not a transient
                // failure. Retire the slug and say so ONCE; every later cycle
                // takes the `forbidden.contains` skip above and logs nothing.
                //
                // TWO shapes carry that verdict, and both must retire. The READ
                // routes funnel through `push::classify_failure` into
                // `ForbiddenByCoord`; the WRITE routes carry status + body in
                // `CoordWriteError` (which is strictly richer — it keeps coord's
                // machine-readable denial code). Routing only one of them here
                // would leave half the retry storm running.
                let write = super::push::coord_write_error(&e);
                let verdict = write.map(|w| w.verdict());
                let settled_403 = e
                    .downcast_ref::<crate::plan_workunit_adapter::push::ForbiddenByCoord>()
                    .map(|f| (f.route.to_string(), f.detail.clone()))
                    .or_else(|| {
                        write
                            .filter(|w| w.status == Some(403))
                            .map(|w| (w.op.to_string(), w.body.clone()))
                    });
                if let Some((route, detail)) = settled_403 {
                    forbidden.insert(u.slug.clone());
                    summary.forbidden += 1;
                    metrics.forbidden_total.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        slug = %u.slug,
                        route = %route,
                        detail = %detail,
                        denial = ?verdict.as_ref().and_then(|v| v.denial.as_ref().map(|d| d.as_code())),
                        "plan adapter: coord refused this work unit (403); retiring the \
                         slug for the life of this process — an identical retry \
                         cannot change the verdict. Restart the runner after \
                         fixing the principal's permission."
                    );
                } else {
                    summary.errors += 1;
                    metrics.errors_total.fetch_add(1, Ordering::Relaxed);
                    // The status coord answered with used to be formatted into
                    // the error string and thrown away here, so a `422`
                    // structural refusal and a `502` transport blip read
                    // identically in the log and to any code downstream.
                    // `CoordWriteError` now carries it, and the ONE shared
                    // classifier turns it into a verdict — `disposition` (retry
                    // or not) plus coord's own `denial` code when it named one.
                    // Nothing acts on the verdict yet (the keyed terminal store
                    // is a later phase); this makes the distinction VISIBLE,
                    // which is what 33 hours of byte-identical cycle summaries
                    // never were.
                    tracing::warn!(
                        slug = %u.slug,
                        error = %format!("{e:#}"),
                        disposition = ?verdict.as_ref().map(|v| v.disposition),
                        denial = ?verdict.as_ref().and_then(|v| v.denial.as_ref().map(|d| d.as_code())),
                        "plan adapter: push failed"
                    );
                }
            }
        }
    }
    metrics.scanned.store(summary.scanned, Ordering::Relaxed);
    summary
}

/// One unit the agent-owner deferral suppressed, carried out of the backfill so
/// the caller can NAME them. `deferred=N` with no names is a number an operator
/// cannot act on, and the `info!` inside [`push_work_unit`] is below the CLI's
/// default filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredUnit {
    pub slug: String,
    /// `by_actor` of the unit's newest status-history row.
    pub owner: String,
    /// The status the file wanted to apply, and did not.
    pub wanted: String,
}

/// Outcome of one [`backfill_work_units_once`] pass.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkUnitBackfillSummary {
    /// Plan files parsed into work units.
    pub scanned: u64,
    /// Units that did not exist in coord and were created WITH their status.
    pub created: u64,
    /// Units already carrying the file's status — title/metadata refreshed,
    /// no status write, no history row.
    pub refreshed: u64,
    /// Units whose coord status differed from the file's and were moved.
    pub transitioned: u64,
    /// Transitions the agent-owner deferral suppressed
    /// ([`PushOutcomeKind::Deferred`]).
    pub deferred: u64,
    /// Units whose read or push errored. The pass continues past each one.
    pub failed: u64,
    /// The deferred units, named. `deferred == deferred_units.len()`.
    pub deferred_units: Vec<DeferredUnit>,
}

/// One-shot **work-unit** backfill: the catch-up path for a machine whose
/// reconcile loop never ran.
///
/// Sibling of the plan-library body backfill
/// (`super::body_push::backfill_once`, driven by
/// `qontinui-pr plan-library-backfill`) — same scanner, same one-shot shape —
/// but it drives `coord.work_units` through [`push_work_unit`] instead of
/// pushing bodies to `agent.work_artifacts`. Neither one is a substitute for
/// the other: they fill different halves of the corpus, and until this existed
/// the work-unit half had **no** catch-up path at all, so an unconfigured
/// runner's ingestion gap could only be closed by arming the tier and waiting
/// for a future runner start.
///
/// ## Why it seeds `last_applied` from coord instead of starting empty
///
/// [`reconcile_once`] carries a client-side last-applied memory that a
/// long-lived loop accumulates. A one-shot has none — and starting from an
/// empty map would make [`super::push::decide_push`] answer `UpsertWithStatus` for **every**
/// unit, which writes a status unconditionally: it would clobber a status an
/// agent had set, and a second run would churn the whole corpus. So each unit's
/// seed is coord's CURRENT status, read from the sink. That makes the three
/// arms fall out correctly and makes the run idempotent by construction:
///
/// - absent in coord → seed `None` → `UpsertWithStatus` → **created**;
/// - present with the same status → `RefreshOnly` → metadata-only upsert;
/// - present with a different status → `Transition` → and therefore **through
///   the agent-owner deferral** ([`push_work_unit`]'s P2a gate), which is the
///   only arm that gate covers. A backfill that started from an empty memory
///   would route every unit down `UpsertWithStatus` and bypass the deferral
///   entirely — silently overwriting exactly the statuses it protects.
///
/// Dependency edges are deliberately NOT pushed here: `build_metadata` already
/// carries `depends_on` in the `metadata` JSONB (the documented fallback), and
/// the edge table is the reconcile loop's incremental business.
pub async fn backfill_work_units_once<S: WorkUnitSink + ?Sized>(
    parsed_units: &[ParsedWorkUnit],
    sink: &S,
) -> WorkUnitBackfillSummary {
    let mut summary = WorkUnitBackfillSummary {
        scanned: parsed_units.len() as u64,
        ..Default::default()
    };
    for u in parsed_units {
        let seed = match sink.current_status(&u.slug).await {
            Ok(s) => s,
            Err(e) => {
                summary.failed += 1;
                tracing::warn!(
                    slug = %u.slug,
                    error = %format!("{e:#}"),
                    "plan backfill: cannot read current work-unit status; skipping this unit \
                     (a push with an unknown seed could clobber a status an agent set)"
                );
                continue;
            }
        };
        // Hand the already-read status through: `push_work_unit` would
        // otherwise re-read it to run a conflict check against a `prev` that IS
        // that read — an answer fixed by construction, bought with a second GET
        // per existing unit.
        match push_work_unit_with_remote(sink, u, seed.as_deref(), Some(seed.as_deref())).await {
            Ok(outcome) => match outcome.kind {
                PushOutcomeKind::Created => summary.created += 1,
                PushOutcomeKind::Refreshed => summary.refreshed += 1,
                PushOutcomeKind::Transitioned { .. } => summary.transitioned += 1,
                PushOutcomeKind::Deferred { owner, wanted } => {
                    summary.deferred += 1;
                    summary.deferred_units.push(DeferredUnit {
                        slug: u.slug.clone(),
                        owner,
                        wanted,
                    });
                }
            },
            Err(e) => {
                summary.failed += 1;
                tracing::warn!(
                    slug = %u.slug,
                    error = %format!("{e:#}"),
                    "plan backfill: push failed"
                );
            }
        }
    }
    summary
}

/// Outcome of one metadata-only archive scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ArchiveSummary {
    /// Archived plans scanned this cycle.
    pub scanned: u64,
    /// `metadata.archive_path` stamps written (metadata-only upserts).
    pub stamped: u64,
    /// Per-unit archive-upsert errors.
    pub errors: u64,
}

/// Metadata-only reconcile of the **archive** directory (D4). For every plan
/// found in the archive dir, stamp `metadata.archive_path` provenance via
/// [`push_archive_metadata`] — **never** a status transition. Pure of IO beyond
/// the sink, so it is unit-tested with a fake sink.
///
/// The archive scan carries no client-side edge-trigger memory: an archived
/// plan is terminal, its `archive_path` is stable, and the upsert is idempotent,
/// so re-stamping each cycle is harmless (and re-asserts provenance a coord
/// restart might have missed). It records nothing into `last_applied`, so it can
/// never influence the active-dir transition path.
pub async fn reconcile_archive_once<S: WorkUnitSink + ?Sized>(
    archived_units: &[ParsedWorkUnit],
    sink: &S,
    metrics: &AdapterMetrics,
) -> ArchiveSummary {
    let mut summary = ArchiveSummary {
        scanned: archived_units.len() as u64,
        ..Default::default()
    };
    for u in archived_units {
        match push_archive_metadata(sink, u).await {
            Ok(()) => {
                summary.stamped += 1;
                metrics
                    .archive_stamped_total
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                summary.errors += 1;
                metrics.errors_total.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    slug = %u.slug,
                    error = %format!("{e:#}"),
                    "plan adapter: archive metadata stamp failed"
                );
            }
        }
    }
    summary
}

/// Pure disappeared-slug detection (D4). A slug we have previously applied
/// (present in `known`) that is now absent from BOTH the active scan
/// (`active_slugs`) and the archive scan (`archive_slugs`), and has not already
/// been warned about (`warned`), is "disappeared": the plan file left the
/// active dir without landing in the archive. Returns those newly-disappeared
/// slugs and records them in `warned` so each is surfaced **once per process**.
///
/// The caller only *warns* on the result — the work unit is left untouched.
/// Terminal state is owned by coord's derive engine; the adapter must never
/// push `shipped`/`archived` to fill the gap (a second-writer race).
pub fn newly_disappeared_slugs(
    known: &HashMap<String, String>,
    active_slugs: &HashSet<String>,
    archive_slugs: &HashSet<String>,
    warned: &mut HashSet<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for slug in known.keys() {
        if !active_slugs.contains(slug) && !archive_slugs.contains(slug) && !warned.contains(slug) {
            warned.insert(slug.clone());
            out.push(slug.clone());
        }
    }
    out
}

/// The path settings the adapter resolves every tick — the three
/// `PathSettings` directories exactly as configured. Blank and unset are both
/// "unset"; the resolvers ([`resolve_plans_dir`] and siblings) normalise them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathInputs {
    pub plans_dir: Option<String>,
    pub plans_archive_dir: Option<String>,
    pub prompts_dir: Option<String>,
}

/// Per-tick reader of the path settings.
///
/// A closure rather than a value for the same reason as [`CaptureGate`]: the
/// settings store lives in the runner binary, this loop lives in the lib
/// crate, and the value must be re-read on every cycle so an edit made in the
/// settings UI takes effect within one interval — never at "the next runner
/// start", which fleet policy forbids anyway.
pub type PathReader = std::sync::Arc<dyn Fn() -> PathInputs + Send + Sync>;

/// One tick's resolution of [`PathInputs`] through the three resolvers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ResolvedDirs {
    plans: Option<String>,
    archive: Option<String>,
    prompts: Option<String>,
}

impl ResolvedDirs {
    fn resolve(inputs: PathInputs) -> Self {
        Self {
            plans: resolve_plans_dir(inputs.plans_dir),
            archive: resolve_plans_archive_dir(inputs.plans_archive_dir),
            prompts: resolve_prompts_dir(inputs.prompts_dir),
        }
    }
}

/// Everything one reconcile loop carries from tick to tick.
///
/// Factored out of [`run_loop`] so a tick is a plain `async fn` a test can
/// drive without timers — the loop itself is nothing but
/// `interval.tick().await; state.tick(..).await`.
struct LoopState {
    conv: PlanConvention,
    paths: PathReader,
    /// The body-sync sink, when the sync is enabled and a backend resolved at
    /// spawn. The sync itself ([`BodySync`]) is rebuilt from it whenever the
    /// resolved root set changes.
    body_sync_sink: Option<super::body_push::HttpArtifactSink>,
    capture_gate: CaptureGate,
    body_sync: Option<BodySync>,
    /// The resolution the current `body_sync` and the `scan_roots` gauge were
    /// built from. `None` before the first tick, so the first tick always
    /// builds them and always logs the tier's state.
    resolved: Option<ResolvedDirs>,
    last_applied: HashMap<String, String>,
    last_deps: HashMap<String, Vec<String>>,
    warned_disappeared: HashSet<String>,
    /// Slugs coord answered `403` for. Owned by the loop (there is exactly one
    /// per process), so "retired" means "for this process's lifetime".
    forbidden: HashSet<String>,
    /// Same, scoped to the dep-edge route alone: coord evaluates the unit's own
    /// upsert/transition route and the edge-table route as separate authorization
    /// checks, so a deps-only 403 must not retire the whole unit. See
    /// [`AdapterMetrics::deps_forbidden_total`].
    forbidden_deps: HashSet<String>,
    /// The git reader the per-cycle scan-divergence measurement uses.
    /// [`ProcessGit`] in production; injected in tests so a tick neither
    /// shells out to a real `git` nor needs a real repo on disk.
    git: std::sync::Arc<dyn GitRefReader>,
    /// Whether the cold-start BULK seed has been attempted for the current
    /// corpus. `false` means the next tick will try to prime `last_applied`
    /// from one paged read instead of paying `reconcile_once`'s per-slug seed
    /// on every plan. Re-armed by [`LoopState::apply_resolution`] whenever the
    /// active plans dir moves, because that clears `last_applied` and the
    /// per-slug fallback would otherwise cost one round-trip per plan again.
    bulk_seeded: bool,
    /// Test-only: the scan-root reporter every rebuilt [`BodySync`] gets
    /// instead of its sink, so a tick-level test can observe reports.
    #[cfg(test)]
    scan_reporter: Option<std::sync::Arc<dyn super::body_push::ScanRootReporter>>,
}

impl LoopState {
    fn new(
        paths: PathReader,
        body_sync_sink: Option<super::body_push::HttpArtifactSink>,
        capture_gate: CaptureGate,
    ) -> Self {
        Self {
            conv: PlanConvention::operator_default(),
            paths,
            git: std::sync::Arc::new(ProcessGit),
            body_sync_sink,
            capture_gate,
            body_sync: None,
            resolved: None,
            last_applied: HashMap::new(),
            last_deps: HashMap::new(),
            warned_disappeared: HashSet::new(),
            forbidden: HashSet::new(),
            forbidden_deps: HashSet::new(),
            bulk_seeded: false,
            #[cfg(test)]
            scan_reporter: None,
        }
    }

    /// Route every rebuilt [`BodySync`]'s scan-root reports to `reporter`.
    /// Test-only, like [`Self::with_git`].
    #[cfg(test)]
    fn with_scan_reporter(
        mut self,
        reporter: std::sync::Arc<dyn super::body_push::ScanRootReporter>,
    ) -> Self {
        self.scan_reporter = Some(reporter);
        self
    }

    /// Swap in a different [`GitRefReader`]. Test-only: production always
    /// wants [`ProcessGit`], and a seam that can be reconfigured at runtime
    /// would be a way for the detector to be quietly disarmed.
    #[cfg(test)]
    fn with_git(mut self, git: std::sync::Arc<dyn GitRefReader>) -> Self {
        self.git = git;
        self
    }

    /// The path settings changed (or this is the first tick): publish the new
    /// resolution to the metrics, rebuild the body sync's scan roots, re-seed
    /// the edge-detection memory when the active dir moved, and log the
    /// tier's state — **once per transition**, here, never per tick.
    fn apply_resolution(&mut self, resolved: ResolvedDirs, metrics: &AdapterMetrics) {
        let previous = self.resolved.replace(resolved.clone());
        let roots = super::body_push::scan_roots(
            resolved.plans.clone(),
            resolved.archive.clone(),
            resolved.prompts.clone(),
        );
        let root_count = roots.len();
        metrics
            .scan_roots
            .store(root_count as u64, Ordering::Relaxed);
        metrics
            .path_resolutions_total
            .fetch_add(1, Ordering::Relaxed);
        *metrics
            .active_plans_dir
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = resolved.plans.clone();
        // Rebuilding the body sync resets its digest memory — a re-seed, the
        // same posture as a supervised restart: nothing is replayed, the next
        // cycle re-reads what is on disk and pushes only what differs from
        // what the backend already holds.
        self.body_sync = self
            .body_sync_sink
            .as_ref()
            .map(|sink| BodySync::new(roots, sink.clone(), self.capture_gate.clone()));
        #[cfg(test)]
        if let Some(reporter) = self.scan_reporter.clone() {
            self.body_sync = self.body_sync.take().map(|bs| bs.with_reporter(reporter));
        }

        let first_tick = previous.is_none();
        let previous_plans = previous.and_then(|p| p.plans);
        if !first_tick && previous_plans == resolved.plans {
            tracing::info!(
                archive_dir = ?resolved.archive,
                prompts_dir = ?resolved.prompts,
                scan_roots = root_count,
                "plan adapter: path settings changed; scan roots rebuilt"
            );
            return;
        }
        // A different active dir is a different corpus: the edge memory keyed
        // by slug would otherwise flag every old slug as disappeared and
        // suppress a legitimate first-seen transition in the new one.
        // `forbidden` stays — a 403 is coord's verdict on the slug, not on
        // where its file lives.
        self.last_applied.clear();
        self.last_deps.clear();
        self.warned_disappeared.clear();
        // `last_applied` is empty again, so the corpus is cold again: re-arm the
        // bulk seed rather than leave the per-slug fallback to pay one
        // round-trip per plan on the next cycle.
        self.bulk_seeded = false;
        match &resolved.plans {
            Some(dir) => tracing::info!(
                dir = %dir,
                archive_dir = ?resolved.archive,
                prompts_dir = ?resolved.prompts,
                scan_roots = root_count,
                "plan adapter: markdown-plan tier is ON — scanning the active plans dir every cycle"
            ),
            // Say it out loud, at `info`, and NAME what arms the tier so the
            // reader does not have to find this function to learn it. A
            // silent idle here is indistinguishable from a healthy scan — the
            // `silent-empty-is-unknown` shape — and it is exactly how a
            // fleet-wide work-unit ingestion gap went unreported for months.
            None => tracing::info!(
                setting = "paths.plans_dir",
                "plan adapter: markdown-plan tier is OFF on this machine — no active plans \
                 dir is configured, so NO plan file is scanned and NO work unit is pushed to \
                 coord from this runner. Arm it by setting `paths.plans_dir` in the Paths \
                 section of the runner's settings; it takes effect within one scan interval, \
                 no restart needed. Catch a machine up immediately with \
                 `qontinui-pr plan-workunit-backfill --plans-dir <dir>`"
            ),
        }
    }

    /// Cold-start BULK seed, attempted once per corpus.
    ///
    /// [`reconcile_once`] already seeds `last_applied` per slug when this
    /// process has no memory of it — that is the CORRECTNESS path and it
    /// abstains rather than overwriting. But it costs one `current_status`
    /// round-trip per plan on the first cycle after a runner start (~1,200
    /// serialized GETs on this fleet). One paged [`WorkUnitSink::list_statuses`]
    /// read collapses that.
    ///
    /// Failure here is deliberately NON-FATAL in every arm, and the two arms
    /// differ in whether they are TERMINAL. A sink with no bulk door returns
    /// `Ok(None)` and will not grow one mid-process, so that arm retires the
    /// attempt. An `Err` is logged and RETRIED on the next tick — a transient
    /// failure must not retire the seed for the life of the process, because
    /// the likeliest moment for one is the first cycle after a runner start.
    /// Either way the per-slug seed still runs; nothing about correctness
    /// depends on this method succeeding.
    ///
    /// Only slugs ACTUALLY IN the scanned dir are primed. Priming every unit
    /// coord knows would feed `newly_disappeared_slugs` a set full of
    /// coord-native units that were never plan-backed, and warn that each of
    /// them had "disappeared from the active dir".
    async fn bulk_seed<S: WorkUnitSink + ?Sized>(
        &mut self,
        units: &[ParsedWorkUnit],
        sink: &S,
        metrics: &AdapterMetrics,
    ) {
        // An EMPTY corpus must not consume the one attempt: `units` is empty
        // both when the plans dir has not been populated yet and when the
        // `spawn_blocking` walk failed with a JoinError. Burning the flag there
        // primes nothing and never retries until the dir path changes.
        if self.bulk_seeded || units.is_empty() {
            return;
        }
        match sink.list_statuses().await {
            Ok(Some(remote)) => {
                // Arm only on a COMPLETED read. Arming before the await would
                // let a transient failure retire the seed permanently — and the
                // likeliest moment for that failure is the first cycle after a
                // runner start, when coord may not be reachable yet, which is
                // precisely the cycle the bulk read exists to make cheap.
                self.bulk_seeded = true;
                let mut primed = 0u64;
                for u in units {
                    if let Some(status) = remote.get(&u.slug) {
                        self.last_applied.insert(u.slug.clone(), status.clone());
                        primed += 1;
                    }
                }
                metrics.seeded_total.fetch_add(primed, Ordering::Relaxed);
                tracing::info!(
                    primed,
                    remote_units = remote.len(),
                    scanned = units.len(),
                    "plan adapter: cold-start bulk seed applied"
                );
            }
            Ok(None) => {
                // A sink with no bulk door will not grow one mid-process, so
                // this arm IS terminal — unlike the error arm below it.
                self.bulk_seeded = true;
                tracing::debug!("plan adapter: sink has no bulk seed door; per-slug seed only");
            }
            Err(e) => {
                // Deliberately leaves `bulk_seeded` false: the next tick retries.
                // The per-slug seed carries correctness in the meantime, and it
                // abstains rather than overwriting, so a retry costs only reads.
                tracing::warn!(
                    error = %format!("{e:#}"),
                    "plan adapter: bulk seed failed; retrying next cycle, per-slug seed meanwhile"
                );
            }
        }
    }

    /// Re-resolve the path settings, rebuild whatever depends on them if they
    /// moved, then run one reconcile cycle — or idle, when no active plans dir
    /// is configured.
    ///
    /// Each cycle: reconcile the active dir (edge-triggered status
    /// transitions), then — when an archive dir is configured — metadata-only
    /// stamp every archived plan's `archive_path` (never a transition, D4),
    /// then warn once about any slug that vanished from both dirs.
    async fn tick<S: WorkUnitSink + ?Sized>(&mut self, sink: &S, metrics: &AdapterMetrics) {
        let resolved = ResolvedDirs::resolve((self.paths)());
        if self.resolved.as_ref() != Some(&resolved) {
            self.apply_resolution(resolved.clone(), metrics);
        }

        // Measure the scan source against the ref it should be reading —
        // BEFORE the early return below, so the idle cycle records
        // `NotScanning` instead of leaving the last reading (or nothing at
        // all) standing. A tier-off machine that reports silence is
        // indistinguishable from one whose scan is in step, and that
        // indistinguishability is the whole defect.
        //
        // Same RT-P0 reasoning as the scan itself: these are `git` subprocess
        // reads, blocking, on a runtime built with `worker_threads(1)`. They
        // go to the blocking pool for the same reason `read_plan_dir` does —
        // parking that single worker also stops its time driver.
        let divergence = match resolved.plans.clone() {
            // Nothing configured: the answer is `NotScanning` and it needs no
            // git at all, so it is computed INLINE. Hopping an unarmed tick to
            // the blocking pool would buy nothing and cost every idle runner a
            // pool round-trip per minute.
            None => ScanDivergence::not_scanning().observed_at(chrono::Utc::now().timestamp()),
            Some(dir) => {
                let git = std::sync::Arc::clone(&self.git);
                match tokio::task::spawn_blocking(move || {
                    // The clock is read HERE, beside the probes, so the ref's
                    // age — and the reading's `observed_at` — are as of the
                    // measurement rather than of the tick's start; the
                    // function itself stays pure over it.
                    measure_scan_source(
                        Path::new(&dir),
                        git.as_ref(),
                        chrono::Utc::now().timestamp(),
                    )
                })
                .await
                {
                    Ok(d) => d,
                    Err(e) => ScanDivergence::unknown(
                        resolved.plans.clone(),
                        format!("the scan-divergence probe task failed to run: {e}"),
                    )
                    .observed_at(chrono::Utc::now().timestamp()),
                }
            }
        };
        record_scan_divergence(divergence, metrics);

        let Some(dir) = resolved.plans.map(PathBuf::from) else {
            // Nothing is scanned — but a device whose plans dir was just
            // cleared must SAY so to the read side, or its last `measured` row
            // keeps being quoted until it ages out. The body sync's library
            // scan stays off here (unchanged); only its scan-root report runs.
            if let Some(bs) = self.body_sync.as_mut() {
                bs.report_while_idle(metrics).await;
            }
            return;
        };
        let archive_dir = resolved.archive.map(PathBuf::from);

        // RT-P0: `read_plan_dir` is a SYNCHRONOUS walk — one `std::fs::read_dir`
        // plus a `read_to_string` of every `*.md` in the plans dir (~1,100
        // files; the loop's own tick comment measures the first cycle at
        // five minutes). This loop lives on the `fleet-publishers` runtime,
        // which is built with `worker_threads(1)` (`main.rs`), and it shares
        // that single worker with the census, reclaim, the orphan reaper, the
        // maintenance executor, the fs backstop and the agent runtime.
        //
        // Run inline, the walk parked that one worker for the whole scan, which
        // also stops the runtime's TIME DRIVER — so every timer on it dilates by
        // the scan duration. That is the mechanism behind a 20s keepalive firing
        // 264s late and an 8s backoff taking 25.5 minutes. See `off_runtime.rs`
        // for why a `tokio::time::timeout` cannot rescue this on its own.
        let units = {
            let dir = dir.clone();
            let conv = self.conv.clone();
            match tokio::task::spawn_blocking(move || read_plan_dir(&dir, &conv)).await {
                Ok(u) => u,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "plan adapter: plans-dir scan task failed; skipping this cycle"
                    );
                    Vec::new()
                }
            }
        };
        self.bulk_seed(&units, sink, metrics).await;
        let summary = reconcile_once(
            &units,
            &mut self.last_applied,
            &mut self.last_deps,
            &mut self.forbidden,
            &mut self.forbidden_deps,
            sink,
            metrics,
        )
        .await;
        metrics.cycles_total.fetch_add(1, Ordering::Relaxed);
        tracing::info!(
            scanned = summary.scanned,
            transitions = summary.transitions,
            conflicts = summary.conflicts,
            errors = summary.errors,
            deferred = summary.deferred,
            deps_set = summary.deps_set,
            deps_skipped_unmigrated = summary.deps_skipped_unmigrated,
            deps_errors = summary.deps_errors,
            forbidden = summary.forbidden,
            deps_forbidden = summary.deps_forbidden,
            seeded = summary.seeded,
            seed_errors = summary.seed_errors,
            "plan adapter: reconcile cycle complete"
        );

        // Archive scan (metadata-only) + disappeared-slug detection. When no
        // archive dir is configured, `read_plan_dir` on `None` is skipped and
        // the archive slug set is empty — a slug that vanishes from the active
        // dir with no archive configured is still surfaced as disappeared.
        // Same reasoning as the active scan above: off the single worker.
        let archived = match archive_dir {
            Some(a) => {
                let conv = self.conv.clone();
                match tokio::task::spawn_blocking(move || read_plan_dir(&a, &conv)).await {
                    Ok(u) => u,
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "plan adapter: archive-dir scan task failed; skipping this cycle"
                        );
                        Vec::new()
                    }
                }
            }
            None => Vec::new(),
        };
        if !archived.is_empty() {
            let asum = reconcile_archive_once(&archived, sink, metrics).await;
            tracing::info!(
                scanned = asum.scanned,
                stamped = asum.stamped,
                errors = asum.errors,
                "plan adapter: archive scan complete (metadata-only)"
            );
        }
        // Plan & prompt library body sync — opt-in, see `BodySync`.
        if let Some(bs) = self.body_sync.as_mut() {
            bs.run_cycle(&self.conv, metrics).await;
        }

        let active_slugs: HashSet<String> = units.iter().map(|u| u.slug.clone()).collect();
        let archive_slugs: HashSet<String> = archived.iter().map(|u| u.slug.clone()).collect();
        for slug in newly_disappeared_slugs(
            &self.last_applied,
            &active_slugs,
            &archive_slugs,
            &mut self.warned_disappeared,
        ) {
            tracing::warn!(
                slug = %slug,
                "plan adapter: work-unit slug disappeared from the active dir and is absent \
                 from the archive dir; leaving the unit untouched (terminal state is owned by \
                 coord's derive engine — the adapter never pushes shipped/archived)"
            );
        }
    }
}

/// The periodic reconcile loop. Runs until the task is dropped.
///
/// The path settings are re-read on every tick ([`PathReader`]); what a tick
/// does with them is [`LoopState::tick`].
async fn run_loop<S: WorkUnitSink + ?Sized>(
    paths: PathReader,
    body_sync_sink: Option<super::body_push::HttpArtifactSink>,
    capture_gate: CaptureGate,
    sink: &S,
    interval_secs: u64,
) {
    let mut state = LoopState::new(paths, body_sync_sink, capture_gate);
    let metrics = adapter_metrics();
    let mut tick = tokio::time::interval(Duration::from_secs(interval_secs.max(1)));
    // A cycle can legitimately outrun the interval (the first one walks and
    // pushes ~1,100 files). The default `Burst` behaviour then fires every
    // missed tick back to back, so a 5-minute first cycle is followed by four
    // immediate no-gap cycles — the opposite of what a periodic reconcile
    // wants. `Delay` drops the missed ticks and simply restarts the interval
    // from now.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    tracing::info!(
        interval_secs,
        "plan adapter: reconcile loop started (path settings are re-read every tick)"
    );
    loop {
        tick.tick().await;
        state.tick(sink, metrics).await;
    }
}

/// Per-machine **kill switch** for the plan & prompt **library body sync**
/// riding along with the reconcile loop (plan
/// `2026-08-10-plan-and-prompt-library-in-web` Phase 2). **On by default**;
/// disabled only by an exact `"0"` (plan
/// `2026-09-03-plan-library-write-door-nonce-authorized-and-body-sync-on-by-default`
/// Phase 3 — the predecessor's decided-but-unshipped 4b).
///
/// **Why on by default now.** This shipped opt-in because the qontinui-web
/// plan-library routes were Cognito-only, and an on-by-default sync would have
/// emitted ~1,100 failed requests every 60s on every runner in the fleet. That
/// premise is gone — the routes accept the device bearer — and what remained
/// was a switch nobody flipped, on any measured device, so the corpus the
/// fleet declared authoritative for reads stayed frozen (served policy
/// `engineering-priorities` `capability-ships-enabled`). What bounds the sync is
/// not this flag: the backend-resolution guard (`HttpArtifactSink::from_env`
/// answers `None` with no resolvable backend, and a release build refuses a
/// machine-local one — see `main.rs`), the tenant's `plan_capture` dial
/// consulted every cycle ([`CaptureGate`]), and the five-cycle failure breaker.
/// The name is kept so an operator's existing `=1` keeps meaning "on".
pub const PLAN_LIBRARY_SYNC_ENV: &str = "QONTINUI_PLAN_LIBRARY_SYNC";

/// Whether the library body sync is enabled for this process — `true` unless
/// [`PLAN_LIBRARY_SYNC_ENV`] is exactly `"0"` (whitespace-trimmed). Read once
/// at spawn (unlike the write-door kill switch, which is read per request):
/// this one decides whether a long-lived loop *has* a sync at all, and a
/// mid-flight change of that shape has no meaning.
pub fn body_sync_enabled() -> bool {
    !matches!(std::env::var(PLAN_LIBRARY_SYNC_ENV), Ok(v) if v.trim() == "0")
}

/// The line [`body_sync_if_enabled`] logs when the body sync is killed, as a
/// pure function of the flag's observed value so a test can pin its content
/// without a log-capture dependency (the crate has none) — plan
/// `2026-08-27-plan-corpus-read-path-is-dark` Phase 1 (D6): a spawn-time flag
/// whose state is unobservable is worse than a flag that is off. The disabled
/// arm used to be a bare `None` — the only one of the three arms with no
/// signal at all — so "is the body sync on for this device?" had no answer in
/// any log.
///
/// `observed` is the raw env value — `None` when the variable is unset — so a
/// value that is not the killing `"0"` yet still landed here (which cannot
/// happen today, but the line must not lie if the predicate ever moves) is
/// printed back verbatim rather than collapsed into "off".
pub fn body_sync_disabled_message(observed: Option<&str>) -> String {
    format!(
        "plan library: body sync is KILLED on this machine — {PLAN_LIBRARY_SYNC_ENV} is {} — so \
         BodySync was NOT constructed and NO plan body reaches agent.work_artifacts from \
         this runner (the work-unit reconcile is unaffected). It is on by default: unset the \
         variable before the runner starts; it is read once at spawn",
        match observed {
            Some(v) => format!("set to {v:?} (the exact string \"0\" kills it)"),
            None => "unset".to_string(),
        }
    )
}

/// What [`BodySync::run_cycle`] says about the tenant's `plan_capture` dial
/// this cycle, given the verdict it recorded last cycle: the dial is announced
/// on the FIRST cycle unconditionally, on every later cycle only when it flips,
/// and otherwise not at all.
///
/// The first-cycle arm exists because a runner that boots with the dial OFF
/// used to say nothing recognisable about it — the previous "changed" wording
/// fired then too, but described a boot as a transition, and a reader grepping
/// for the dial's boot state found no line that named it as such.
pub fn capture_gate_message(previous: Option<bool>, gate_open: bool) -> Option<&'static str> {
    match (previous, gate_open) {
        (None, true) => Some(
            "plan library: tenant plan_capture dial is OPEN on the body sync's first cycle — \
             scanned plan bodies are pushed to agent.work_artifacts",
        ),
        (None, false) => Some(
            "plan library: tenant plan_capture dial is CLOSED on the body sync's first cycle — \
             plan bodies are NOT pushed to agent.work_artifacts until the dial opens (it is \
             re-read every cycle, no restart needed)",
        ),
        (Some(prev), now) if prev != now => {
            Some("plan library: tenant plan_capture level changed the body sync's authorization")
        }
        _ => None,
    }
}

/// Whether the tenant's fleet dial currently authorizes plan capture.
///
/// A callback rather than a direct read because the dial's cache lives in the
/// runner **binary** (`crate::mcp::fleet_policy_poller`) while this adapter
/// lives in the lib crate, which cannot see it. The binary supplies the reader
/// at spawn time; the lib stays free of the poller.
pub type CaptureGate = std::sync::Arc<dyn Fn() -> bool + Send + Sync>;

/// How many **consecutive** entirely-failed cycles pause the sync.
///
/// The axis is consecutive cycles, not a sample-size floor on one cycle. A
/// floor cannot work here: in steady state the digest memory skips almost
/// everything, so a cycle in which the operator edited one plan legitimately
/// attempts exactly ONE push — a floor of, say, 10 would make the breaker
/// unreachable in precisely the state it has to protect, while a floor of 1
/// (the shipped behaviour) lets a single transient 500, a 30-second network
/// blip or a mid-rotation 401 latch the sync off. Requiring the failure to
/// persist across five cycles (~5 minutes at the default tick) distinguishes
/// "this backend is down" from "one request was unlucky" without reference to
/// how many files happened to change.
const TOTAL_FAILURE_CYCLES_BEFORE_PAUSE: u32 = 5;

/// How many cycles a tripped breaker sits out before trying again.
///
/// ~30 minutes at the default 60s tick. The breaker is a **pause that
/// re-arms**, never the one-way latch it started as: the latch's own error
/// message prescribed restarting the runner, which served policy
/// `production-and-cost` `runner-lifecycle` forbids outright — so a tripped
/// latch was unrecoverable for the process's whole life, which on this fleet
/// means indefinitely. A pause costs one cycle's worth of failed requests per
/// half hour while the backend is down, and resumes on its own the moment it
/// comes back.
const PAUSE_CYCLES: u32 = 30;

/// The body sync's failure breaker, as a pure state machine.
///
/// Factored out of [`BodySync`] so the property that matters — a single
/// transient failure must NOT disable the sync — is a unit test over the
/// shipping logic rather than a claim about it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FailureBreaker {
    consecutive_total_failures: u32,
    pause_cycles_remaining: u32,
}

impl FailureBreaker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Consume one cycle of the current pause, if any. Returns `true` when the
    /// caller should SKIP this cycle entirely.
    pub fn should_skip_cycle(&mut self) -> bool {
        if self.pause_cycles_remaining == 0 {
            return false;
        }
        self.pause_cycles_remaining -= 1;
        if self.pause_cycles_remaining == 0 {
            tracing::info!(
                "plan library: body-sync pause elapsed — retrying one cycle. If the backend \
                 or credential is still broken it will pause again; no restart is needed \
                 either way."
            );
        }
        true
    }

    /// Record a completed cycle. `attempted` counts only the pushes that
    /// actually reached the network (a locally-skipped unchanged file is not
    /// an attempt and cannot fail). Returns `true` when this call TRIPPED the
    /// breaker.
    pub fn record_cycle(&mut self, attempted: u64, errors: u64) -> bool {
        let totally_failed = attempted > 0 && errors == attempted;
        if !totally_failed {
            self.consecutive_total_failures = 0;
            return false;
        }
        self.consecutive_total_failures += 1;
        if self.consecutive_total_failures < TOTAL_FAILURE_CYCLES_BEFORE_PAUSE {
            return false;
        }
        self.consecutive_total_failures = 0;
        self.pause_cycles_remaining = PAUSE_CYCLES;
        true
    }

    pub fn is_paused(&self) -> bool {
        self.pause_cycles_remaining > 0
    }

    pub fn consecutive_total_failures(&self) -> u32 {
        self.consecutive_total_failures
    }
}

/// How often an UNCHANGED scan-root reading is re-posted to the web read side.
///
/// A heartbeat, so the row's `observed_at` ages honestly: the web side reads a
/// row older than three of these (45 min) as `unknown` / `observation_stale`,
/// which is how a device that stopped reporting — killed, offline, its sync
/// switched off — stops being quoted as current. Without it an in-step device
/// would post once and then look dead.
pub const SCAN_REPORT_HEARTBEAT: Duration = Duration::from_secs(15 * 60);

/// After a failed scan-root post, how long before the next attempt.
///
/// The report rides the reconcile tick (60 s), and a backend that does not
/// serve the route yet — the web half ships separately — would otherwise take
/// one doomed request a minute from every runner. Five minutes keeps a
/// recovered backend current well inside the 45-minute staleness horizon.
const SCAN_REPORT_RETRY_AFTER_FAILURE: Duration = Duration::from_secs(5 * 60);

/// Whether [`BodySync`] should post the current scan-root reading this cycle.
///
/// Pure over its inputs so the posting policy is a unit test:
/// - no reading yet (`current` is `None` — the reconcile loop has not ticked)
///   → nothing to post;
/// - inside the retry backoff after a failed post → wait;
/// - never posted → due;
/// - the reading CHANGED ([`ScanDivergence::is_same_reading`], so a ref age
///   advancing inside its freshness window is not a change) → due;
/// - unchanged, but [`SCAN_REPORT_HEARTBEAT`] has elapsed since the last
///   successful post → due;
/// - otherwise → not due.
pub fn scan_report_due(
    last_posted: Option<(&ScanDivergence, std::time::Instant)>,
    last_failed_at: Option<std::time::Instant>,
    current: Option<&ScanDivergence>,
    now: std::time::Instant,
) -> bool {
    let Some(current) = current else {
        return false;
    };
    if last_failed_at.is_some_and(|failed| {
        now.saturating_duration_since(failed) < SCAN_REPORT_RETRY_AFTER_FAILURE
    }) {
        return false;
    }
    match last_posted {
        None => true,
        Some((previous, posted_at)) => {
            !previous.is_same_reading(current)
                || now.saturating_duration_since(posted_at) >= SCAN_REPORT_HEARTBEAT
        }
    }
}

/// The library body-sync half of a reconcile cycle: re-scan the three roots and
/// push any artifact whose body digest moved.
///
/// Holds its own [`super::body_push::ArtifactSyncState`], so steady state costs
/// one directory walk and zero HTTP calls — the whole point of the digest
/// memory (pass 2's edge memo is what makes the "zero HTTP calls" half true;
/// without it the edge pass re-POSTed every `depends_on` edge every tick).
/// Kept in the same tick as the work-unit reconcile rather than on its own
/// timer so the two can never observe different filesystem states.
///
/// ## The fleet dial governs this, not just the briefing
///
/// `run_cycle` consults [`CaptureGate`] — the tenant's `plan_capture` level —
/// on every cycle, and does nothing at `off`. Without that the dial would be
/// advisory for everything except the system-prompt clause: a runner with
/// the body sync on would keep pushing the whole corpus at fleet level `off`.
/// Capture is two independent switches in the same direction — a per-machine
/// kill switch (`QONTINUI_PLAN_LIBRARY_SYNC`, on by default) AND a tenant-wide
/// dial — so the dial is a real fleet off-switch that does not require touching
/// env on every machine (and cannot, since restarting runners is forbidden).
#[derive(Clone)]
pub struct BodySync {
    roots: Vec<super::body_push::ScanRoot>,
    sink: super::body_push::HttpArtifactSink,
    state: super::body_push::ArtifactSyncState,
    capture_gate: CaptureGate,
    breaker: FailureBreaker,
    /// Last gate verdict observed, so a flip is logged ONCE rather than every
    /// tick. `None` until the first cycle.
    last_gate_open: Option<bool>,
    /// The scan-root reading last ACCEPTED by the web read side, and when —
    /// see [`scan_report_due`]. Reset with the rest of this struct when the
    /// path settings move, so a new plans dir is reported on its first cycle.
    last_scan_report: Option<(ScanDivergence, std::time::Instant)>,
    /// The last failed scan-root post's error and time: the time drives the
    /// retry backoff, the text makes the failure WARN edge-triggered (a
    /// repeat of the same error drops to DEBUG). Cleared by a success.
    last_scan_report_failure: Option<(String, std::time::Instant)>,
    /// Where scan-root reports go: the same web sink as the body pushes in
    /// production; a recording fake in tests (see
    /// [`super::body_push::ScanRootReporter`]).
    reporter: std::sync::Arc<dyn super::body_push::ScanRootReporter>,
}

impl BodySync {
    pub fn new(
        roots: Vec<super::body_push::ScanRoot>,
        sink: super::body_push::HttpArtifactSink,
        capture_gate: CaptureGate,
    ) -> Self {
        Self {
            roots,
            reporter: std::sync::Arc::new(sink.clone()),
            sink,
            state: super::body_push::ArtifactSyncState::new(),
            capture_gate,
            breaker: FailureBreaker::new(),
            last_gate_open: None,
            last_scan_report: None,
            last_scan_report_failure: None,
        }
    }

    /// Swap in a different scan-root reporter. Test-only, for the same reason
    /// as [`LoopState::with_git`]: production always reports through its own
    /// sink.
    #[cfg(test)]
    fn with_reporter(
        mut self,
        reporter: std::sync::Arc<dyn super::body_push::ScanRootReporter>,
    ) -> Self {
        self.reporter = reporter;
        self
    }

    /// The reading the reconcile tick just recorded in `metrics`.
    fn current_reading(metrics: &AdapterMetrics) -> Option<ScanDivergence> {
        metrics
            .scan_divergence
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// The idle tick's share of a body-sync cycle: no plans dir is configured,
    /// so nothing is scanned or pushed — but the scan-root reading
    /// (`not_scanning`) is still reported, under the same capture gate and
    /// posting policy as an armed cycle. Without it a device whose plans dir
    /// was cleared goes silent and its last `measured` row is quoted until it
    /// ages out.
    pub async fn report_while_idle(&mut self, metrics: &AdapterMetrics) {
        if !(self.capture_gate)() {
            return;
        }
        self.report_scan_root_if_due(Self::current_reading(metrics), std::time::Instant::now())
            .await;
    }

    /// Publish this device's scan-root reading to the web read side
    /// (`POST /api/v1/plan-library/scan-roots`) when [`scan_report_due`] says
    /// so — plan `2026-09-11-the-plan-corpus-scan-root-does-not-report-its-own-drift`
    /// Revised Phase 2.
    ///
    /// Without this, the reading that says "this corpus is fed from a tree N
    /// commits behind" exists only in this process and its local UI, while the
    /// corpus it qualifies is read fleet-wide. A failure is logged and dropped:
    /// it never counts toward the body-push breaker (a backend that does not
    /// serve this route yet must not pause plan-body capture) and never
    /// affects the rest of the cycle.
    async fn report_scan_root_if_due(
        &mut self,
        current: Option<ScanDivergence>,
        now: std::time::Instant,
    ) {
        let due = scan_report_due(
            self.last_scan_report.as_ref().map(|(d, at)| (d, *at)),
            self.last_scan_report_failure.as_ref().map(|(_, at)| *at),
            current.as_ref(),
            now,
        );
        let Some(current) = current.filter(|_| due) else {
            return;
        };
        let report =
            super::body_push::ScanRootReport::from_divergence(&current, chrono::Utc::now());
        match self.reporter.report_scan_root(&report).await {
            Ok(()) => {
                if self.last_scan_report_failure.take().is_some() {
                    tracing::info!(
                        state = %report.state,
                        counts_are_floors = report.counts_are_floors,
                        "plan library: scan-root reading accepted again by the web read side"
                    );
                } else if self.last_scan_report.is_none() {
                    tracing::info!(
                        state = %report.state,
                        source_repo = ?report.source_repo,
                        behind = ?report.behind,
                        ahead = ?report.ahead,
                        ref_age_secs = ?report.ref_age_secs,
                        counts_are_floors = report.counts_are_floors,
                        "plan library: published this device's scan-root reading (re-posted on \
                         change, and at least every 15 min)"
                    );
                } else {
                    tracing::debug!(
                        state = %report.state,
                        "plan library: scan-root reading re-posted"
                    );
                }
                self.last_scan_report = Some((current, now));
            }
            Err(error) => {
                let repeat = self
                    .last_scan_report_failure
                    .as_ref()
                    .is_some_and(|(previous, _)| *previous == error);
                if repeat {
                    tracing::debug!(
                        error = %error,
                        "plan library: scan-root report still failing (same error)"
                    );
                } else {
                    tracing::warn!(
                        error = %error,
                        retry_after_secs = SCAN_REPORT_RETRY_AFTER_FAILURE.as_secs(),
                        "plan library: could not publish this device's scan-root reading to the \
                         web read side — readers of the corpus cannot see how far this device's \
                         plans dir has drifted. Plan-body capture is unaffected and this does \
                         not count toward its breaker; retrying after the backoff"
                    );
                }
                self.last_scan_report_failure = Some((error, now));
            }
        }
    }

    /// One body-sync cycle. `metrics` is the reconcile loop's own — the tick
    /// that calls this has just recorded its scan-divergence reading there,
    /// and that reading is what the scan-root report publishes.
    pub async fn run_cycle(&mut self, conv: &PlanConvention, metrics: &AdapterMetrics) {
        let gate_open = (self.capture_gate)();
        if let Some(message) = capture_gate_message(self.last_gate_open, gate_open) {
            tracing::info!(capture_enabled = gate_open, "{message}");
        }
        self.last_gate_open = Some(gate_open);
        if !gate_open {
            return;
        }
        // The scan-root report goes AFTER the capture gate (a tenant at
        // `plan_capture = off` publishes nothing from its plans dir) and BEFORE
        // both the breaker's pause and the `artifacts.is_empty()` return below.
        // Those two are exactly the cycles whose corpus is NOT being refreshed
        // — a paused sync, an empty scan — so they are the last ones that
        // should go quiet about the scan source.
        self.report_scan_root_if_due(Self::current_reading(metrics), std::time::Instant::now())
            .await;
        if self.breaker.should_skip_cycle() {
            return;
        }

        // `scan_all_roots` does ~1,100 synchronous `read_to_string` calls. On
        // the async path that blocks a tokio worker thread for the whole walk,
        // starving every other task sharing it — so it runs on the blocking
        // pool and the result comes back by value.
        let roots = self.roots.clone();
        let conv = conv.clone();
        let scanned =
            spawn_blocking_tracked(move || super::body_push::scan_all_roots(&roots, &conv)).await;
        let (artifacts, skipped) = match scanned {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "plan library: scan task failed to join; skipping this cycle"
                );
                return;
            }
        };
        if artifacts.is_empty() {
            return;
        }
        let summary =
            super::body_push::backfill_once(&self.sink, &artifacts, &mut self.state).await;
        // Only pushes that reached the network count as attempts — a file the
        // digest memory skipped made no call and cannot have failed.
        let attempted =
            summary.created + summary.updated + summary.unchanged_remote + summary.errors;
        tracing::info!(
            scanned = artifacts.len(),
            skipped = skipped.len(),
            created = summary.created,
            updated = summary.updated,
            unchanged = summary.unchanged_remote,
            skipped_local = summary.skipped_local,
            kind_forks = summary.ambiguous_kind,
            errors = summary.errors,
            edges_set = summary.edges_set,
            edges_skipped = summary.edges_skipped_applied,
            edges_given_up = summary.edges_given_up,
            "plan library: body sync cycle complete"
        );
        if self.breaker.record_cycle(attempted, summary.errors) {
            tracing::error!(
                errors = summary.errors,
                consecutive_cycles = TOTAL_FAILURE_CYCLES_BEFORE_PAUSE,
                pause_cycles = PAUSE_CYCLES,
                env_var = PLAN_LIBRARY_SYNC_ENV,
                "plan library: every push failed for {TOTAL_FAILURE_CYCLES_BEFORE_PAUSE} \
                 consecutive cycles — pausing the body sync for {PAUSE_CYCLES} cycles rather \
                 than retrying the same failures every tick. It RE-ARMS on its own; no restart \
                 is needed (and restarting a runner is forbidden by fleet policy). Fix the \
                 backend/credential, or use `qontinui-pr plan-library-backfill` for a one-shot \
                 run."
            );
        }
    }
}

/// Blank is unset, everywhere: a path setting configured to `""` (or
/// whitespace) disables that directory rather than scanning a directory
/// named `""`.
fn non_blank(configured: Option<String>) -> Option<String> {
    configured.filter(|s| !s.trim().is_empty())
}

/// Resolve the **active** plans directory from the runner's
/// `PathSettings::plans_dir`, or `None` when the markdown-plan tier is off.
///
/// There is deliberately **no environment override**. The one that used to
/// sit above this setting was a backward-compatibility shim for a
/// pre-settings deployment, and it silently outranked the setting — the
/// settings UI could show a directory that was not the one in effect. It was
/// migrated into the setting once at boot (the binary's `plans_dir_migration`)
/// and then deleted: one precedence chain, one source of truth.
///
/// Kept as its own name rather than having callers spell the filter
/// themselves because it is the documented seam every surface that needs
/// "the plans dir" goes through — the adapter, the session-env injection and
/// the plan-library read door — so they resolve it identically by
/// construction. The three resolvers share one body for the same reason they
/// keep three names: each is the seam for one directory.
pub fn resolve_plans_dir(configured: Option<String>) -> Option<String> {
    non_blank(configured)
}

/// Resolve the plans **archive** directory (D4) from
/// `PathSettings::plans_archive_dir`. Deliberately not derivable from the
/// active dir (it commonly lives in a different repo), and blank counts as
/// unset — see [`resolve_plans_dir`].
pub fn resolve_plans_archive_dir(configured: Option<String>) -> Option<String> {
    non_blank(configured)
}

/// Resolve the **prompts** directory (plan `2026-08-10-plan-and-prompt-library-in-web`
/// Phase 2) from `PathSettings::prompts_dir`: the third scan root, and the
/// value exported to agent sessions as `QONTINUI_PROMPTS_DIR`.
///
/// **Not derivable from the plans dir.** `/create-plan` currently *guesses*
/// `$QONTINUI_PLANS_DIR/../prompts/*.md`, which is exactly the guess this
/// setting exists to replace — the operator's prompts live in more than one
/// repo and the sibling-of-plans relationship does not hold in general. Blank
/// counts as unset — see [`resolve_plans_dir`].
pub fn resolve_prompts_dir(configured: Option<String>) -> Option<String> {
    non_blank(configured)
}

/// Spawn the reconcile loop iff a coord base resolves for this runner
/// (`COORD_HTTP_URL`, or the active profile's `coord_url`). Returns `None`
/// (no-op) otherwise.
///
/// Whether the loop actually **scans** is decided on every tick from `paths`
/// — see [`PathReader`]: a runner with no `paths.plans_dir` spawns the loop
/// and idles in it, logging the tier as OFF once, until the setting is filled
/// in; filling it in takes effect within one interval with no restart. The
/// archive dir gates only the metadata-only archive scan (D4) and the prompts
/// dir only the library scan; the loop reconciles the active dir whether or
/// not either is set. Interval overridable via
/// `QONTINUI_PLAN_ADAPTER_INTERVAL_SECS` (default 60s).
///
/// `configured_backend_url` must be the **persisted** web-integration URL (and
/// `None` when web integration is disabled or unset), NOT an already-defaulted
/// one: [`super::body_push::resolve_backend_base`] promises to answer `None`
/// rather than guess a host, and a caller that pre-substitutes a build default
/// turns that promise into "always configured, possibly at production".
///
/// `capture_gate` reads the tenant's `plan_capture` fleet dial — see
/// [`CaptureGate`]. It is consulted every cycle, so flipping the dial takes
/// effect without a restart.
pub fn spawn_if_configured(
    paths: PathReader,
    configured_backend_url: Option<String>,
    capture_gate: CaptureGate,
) -> Option<tokio::task::JoinHandle<()>> {
    let sink = match super::push::HttpWorkUnitSink::from_profile() {
        Some(s) => s,
        None => {
            tracing::warn!(
                env_var = "COORD_HTTP_URL",
                setting = "profiles.<active>.coord_url",
                "plan adapter: no coord base configured; not starting — NO work unit is \
                 pushed to coord from this runner. Arm it by exporting COORD_HTTP_URL or \
                 connecting the active profile to a coord deployment"
            );
            return None;
        }
    };

    // Plan & prompt library body sync: on unless killed, AND needs a resolvable
    // web backend — see `body_sync_sink_if_enabled`. The work-unit reconcile is
    // unaffected either way, exactly as the archive scan is optional. Only the
    // SINK is fixed here; its scan roots follow the path settings tick by tick.
    let body_sync_sink = body_sync_sink_if_enabled(configured_backend_url);

    let interval_secs = std::env::var("QONTINUI_PLAN_ADAPTER_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    // Supervised (plan 2026-09-03-…-supervisor Phase 4): a panic mid-cycle
    // used to end plan ingestion for the process's lifetime, silently. The
    // factory rebuilds `run_loop` from the same inputs — its edge-detection
    // maps and the body sync's digest state are per-run, so a rebuilt loop
    // re-seeds from coord exactly as a fresh process does (no transition is
    // replayed: the first cycle reads coord's CURRENT status as its seed).
    let sink = std::sync::Arc::new(sink);
    Some(crate::worker_supervisor::spawn_supervised(
        "plan_workunit_adapter.reconcile_loop",
        move || {
            let paths = paths.clone();
            let body_sync_sink = body_sync_sink.clone();
            let capture_gate = capture_gate.clone();
            let sink = sink.clone();
            async move {
                run_loop(paths, body_sync_sink, capture_gate, &*sink, interval_secs).await;
            }
        },
    ))
}

/// The web sink the library body sync pushes through, or `None`.
///
/// Two things answer `None`, and only ONE of them is the flag: the per-machine
/// kill switch ([`body_sync_enabled`], on by default), and — the guard that
/// actually protects a release build now that the sync is on by default — the
/// backend-resolution guard, `HttpArtifactSink::from_env` answering `None` when
/// no web backend resolves from env or `configured_backend_url`. Only the SINK
/// is decided at spawn; the scan roots follow the path settings every tick
/// ([`LoopState`]), so no sink means no `BodySync` on any tick. Factored out of
/// [`spawn_if_configured`] so that guard is a unit test rather than a claim.
fn body_sync_sink_if_enabled(
    configured_backend_url: Option<String>,
) -> Option<super::body_push::HttpArtifactSink> {
    if !body_sync_enabled() {
        // Not silent: a flag-off arm that logged nothing was indistinguishable
        // from a healthy sync (the plans-dir-absent branch in
        // `spawn_if_configured`, same shape, same reason).
        let observed = std::env::var(PLAN_LIBRARY_SYNC_ENV).ok();
        tracing::info!(
            env_var = PLAN_LIBRARY_SYNC_ENV,
            observed = observed.as_deref().unwrap_or("unset"),
            "{}",
            body_sync_disabled_message(observed.as_deref())
        );
        return None;
    }
    match super::body_push::HttpArtifactSink::from_env(configured_backend_url) {
        Some(sink) => {
            tracing::info!(
                backend = %sink.base(),
                "plan library: body sync enabled (scan roots follow the path settings \
                 every tick; still gated per-cycle on the tenant's plan_capture fleet \
                 dial)"
            );
            Some(sink)
        }
        None => {
            tracing::warn!(
                env_var = PLAN_LIBRARY_SYNC_ENV,
                "plan library: body sync is on (it is on by default; {PLAN_LIBRARY_SYNC_ENV}=0 \
                 kills it) but no qontinui-web backend is configured; not syncing"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::parser::ParsedWorkUnit;
    use super::super::push::{SetDepsOutcome, TransitionBody, UpsertBody, ADAPTER_ACTOR};
    use super::*;
    use anyhow::Result;
    use std::sync::Mutex;

    // ---- scan-source divergence detector (plan 2026-09-10-…-not-a-ref, P1) ----

    /// A [`GitRefReader`] built from canned answers, so every arm of
    /// [`measure_scan_divergence`] — including the ones that need a repo 2153
    /// commits behind its own default branch — is reachable without touching a
    /// filesystem or spawning `git`.
    struct FakeGit {
        root: Result<Option<PathBuf>, String>,
        default_ref: Result<String, String>,
        /// rev -> resolved oid, or an error for that rev.
        revs: HashMap<String, Result<String, String>>,
        counts: Result<(u64, u64), String>,
        /// The canned answer to `ref_refreshed_at`.
        refreshed_at: Result<Option<i64>, String>,
    }

    /// The fixed "now" every pure measurement in this module is taken at.
    const NOW: i64 = 1_789_000_000;

    impl FakeGit {
        /// A healthy clone: in a work tree, `origin/main` resolvable, both
        /// revs resolvable, `(behind, ahead)` as given, and the ref refreshed
        /// a minute before [`NOW`] — fresh.
        fn healthy(behind: u64, ahead: u64) -> Self {
            Self {
                root: Ok(Some(PathBuf::from("/repo"))),
                default_ref: Ok("origin/main".to_string()),
                revs: [
                    ("origin/main".to_string(), Ok("a".repeat(40))),
                    ("HEAD".to_string(), Ok("b".repeat(40))),
                ]
                .into_iter()
                .collect(),
                counts: Ok((behind, ahead)),
                refreshed_at: Ok(Some(NOW - 60)),
            }
        }

        /// [`Self::healthy`] with the ref last refreshed as given.
        fn refreshed(behind: u64, ahead: u64, refreshed_at: Result<Option<i64>, String>) -> Self {
            Self {
                refreshed_at,
                ..Self::healthy(behind, ahead)
            }
        }
    }

    impl GitRefReader for FakeGit {
        fn work_tree_root(&self, _dir: &Path) -> Result<Option<PathBuf>, String> {
            self.root.clone()
        }
        fn default_ref(&self, _repo_root: &Path) -> Result<String, String> {
            self.default_ref.clone()
        }
        fn rev_parse(&self, _repo_root: &Path, rev: &str) -> Result<String, String> {
            self.revs
                .get(rev)
                .cloned()
                .unwrap_or_else(|| Err(format!("no canned answer for {rev}")))
        }
        fn count_behind_ahead(
            &self,
            _repo_root: &Path,
            _reference: &str,
            _head: &str,
        ) -> Result<(u64, u64), String> {
            self.counts.clone()
        }
        fn ref_refreshed_at(
            &self,
            _repo_root: &Path,
            _default_ref: &str,
            _ref_sha: &str,
        ) -> Result<Option<i64>, String> {
            self.refreshed_at.clone()
        }
    }

    /// State 1 of 4. A machine with no `paths.plans_dir` scans NOTHING, and
    /// must say so out loud: `NotScanning` with no counts at all. The one
    /// thing it may never be is `0 behind / 0 ahead`, which is what "the scan
    /// is in step" looks like.
    #[test]
    fn scan_divergence_reports_not_scanning_when_no_plans_dir_is_configured() {
        let d = measure_scan_divergence(None, &FakeGit::healthy(0, 0), NOW);
        assert_eq!(d.state, ScanDivergenceState::NotScanning);
        assert_eq!(d.state.as_str(), "not_scanning");
        assert_eq!(d.plans_dir, None);
        assert_eq!(d.repo_root, None);
        assert_eq!(d.default_ref, None);
        assert_eq!(
            (d.behind, d.ahead),
            (None, None),
            "a tier-off machine has no divergence to report, and reporting zero would claim it \
             agrees with a ref it never read"
        );
        assert!(!d.is_stale());
    }

    /// State 2 of 4. A plans dir outside any repo is a SUPPORTED
    /// configuration, so it is not an error — but it is not zero divergence
    /// either. There is no ref, so there are no counts.
    #[test]
    fn scan_divergence_reports_not_a_git_work_tree_rather_than_zero() {
        let git = FakeGit {
            root: Ok(None),
            ..FakeGit::healthy(0, 0)
        };
        let d = measure_scan_divergence(Some(Path::new("/plain/plans")), &git, NOW);
        assert_eq!(d.state, ScanDivergenceState::NotAGitWorkTree);
        assert_eq!(d.state.as_str(), "not_a_git_work_tree");
        assert_eq!(d.plans_dir.as_deref(), Some("/plain/plans"));
        assert_eq!(d.repo_root, None);
        assert_eq!((d.behind, d.ahead), (None, None));
        let detail = d.detail.expect("NotAGitWorkTree must name why");
        assert!(!detail.is_empty());
        assert!(
            detail.contains("/plain/plans"),
            "the detail names the dir it is talking about: {detail}"
        );
    }

    /// State 3 of 4, and the number the plan was written for: a plans dir
    /// parked 2153 behind / 11 ahead of its own default branch reads exactly
    /// that, with both shas and the resolved ref carried.
    #[test]
    fn scan_divergence_measures_a_parked_working_tree() {
        let d = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::healthy(2153, 11),
            NOW,
        );
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.state.as_str(), "measured");
        assert_eq!(d.plans_dir.as_deref(), Some("/repo/plans"));
        assert_eq!(d.repo_root.as_deref(), Some("/repo"));
        assert_eq!(d.default_ref.as_deref(), Some("origin/main"));
        assert_eq!(d.ref_sha.as_deref(), Some("a".repeat(40).as_str()));
        assert_eq!(d.head_sha.as_deref(), Some("b".repeat(40).as_str()));
        assert_eq!(d.behind, Some(2153));
        assert_eq!(d.ahead, Some(11));
        assert_eq!(d.detail, None, "a clean measurement explains nothing");
        assert!(d.is_stale());
    }

    /// `Measured` zeros are REAL zeros — the one state allowed to render
    /// `0 behind / 0 ahead`, because it actually compared.
    ///
    /// What it compared is HEAD against the ref, and nothing more: a checkout
    /// sitting exactly on the default branch with uncommitted plan files still
    /// reads `0/0` here while publishing bytes no ref carries. The name says
    /// `head_is_in_step` rather than `is_a_real_zero` so no reader concludes
    /// more than was measured.
    #[test]
    fn scan_divergence_measured_zero_means_head_is_in_step_not_the_working_tree() {
        let d =
            measure_scan_divergence(Some(Path::new("/repo/plans")), &FakeGit::healthy(0, 0), NOW);
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!((d.behind, d.ahead), (Some(0), Some(0)));
        assert!(!d.is_stale());
    }

    /// The conflation this phase exists to end, in its sharpest form: a root
    /// probe that FAILED (no `git` on PATH, a nonexistent dir, a 20s timeout
    /// on a stalled mount) must read `Unknown`, never the benign
    /// `NotAGitWorkTree`. Those two differ by whether anything was
    /// established, which is exactly the distinction a shrugging reader loses.
    #[test]
    fn scan_divergence_is_unknown_when_the_work_tree_probe_itself_fails() {
        let git = FakeGit {
            root: Err("`git rev-parse --show-toplevel` did not answer (SpawnError)".to_string()),
            ..FakeGit::healthy(0, 0)
        };
        let d = measure_scan_divergence(Some(Path::new("/maybe/plans")), &git, NOW);
        assert_eq!(
            d.state,
            ScanDivergenceState::Unknown,
            "a probe that could not ask establishes nothing, least of all that the dir is not \
             a repo"
        );
        assert_ne!(d.state, ScanDivergenceState::NotAGitWorkTree);
        assert_eq!(d.plans_dir.as_deref(), Some("/maybe/plans"));
        assert_eq!(d.repo_root, None);
        assert_eq!((d.behind, d.ahead), (None, None));
        assert!(d.detail.unwrap().contains("SpawnError"));
    }

    /// State 4 of 4, arm A: a work tree whose default branch cannot be
    /// resolved is UNKNOWN with a reason — never a hardcoded `origin/main`,
    /// because a divergence measured against the wrong branch is a
    /// confidently wrong number.
    #[test]
    fn scan_divergence_is_unknown_when_the_default_branch_does_not_resolve() {
        let git = FakeGit {
            default_ref: Err("`origin/HEAD` is not set in this clone".to_string()),
            ..FakeGit::healthy(5, 5)
        };
        let d = measure_scan_divergence(Some(Path::new("/repo/plans")), &git, NOW);
        assert_eq!(d.state, ScanDivergenceState::Unknown);
        assert_eq!(d.state.as_str(), "unknown");
        assert_eq!(d.repo_root.as_deref(), Some("/repo"));
        assert_eq!(
            d.default_ref, None,
            "nothing resolved, so nothing is claimed — least of all a guess"
        );
        assert_eq!((d.behind, d.ahead), (None, None));
        let detail = d.detail.expect("Unknown must name why");
        assert!(
            detail.contains("origin/HEAD"),
            "the failing probe's own words survive into the detail: {detail}"
        );
    }

    /// State 4 of 4, arm B: the ref and HEAD resolve but the count fails. What
    /// WAS established is kept (repo root, ref name, both shas) and only the
    /// counts stay UNKNOWN.
    #[test]
    fn scan_divergence_is_unknown_when_the_count_fails_but_keeps_what_resolved() {
        let git = FakeGit {
            counts: Err("bad revision".to_string()),
            ..FakeGit::healthy(1, 1)
        };
        let d = measure_scan_divergence(Some(Path::new("/repo/plans")), &git, NOW);
        assert_eq!(d.state, ScanDivergenceState::Unknown);
        assert_eq!(d.default_ref.as_deref(), Some("origin/main"));
        assert_eq!(d.ref_sha.as_deref(), Some("a".repeat(40).as_str()));
        assert_eq!((d.behind, d.ahead), (None, None));
        assert!(d.detail.unwrap().contains("bad revision"));
        assert!(!d.state.as_str().is_empty());
    }

    /// State 4 of 4, arm C: HEAD itself is unresolvable (an unborn branch in a
    /// fresh clone). Still UNKNOWN, still explained.
    #[test]
    fn scan_divergence_is_unknown_when_head_does_not_resolve() {
        let mut git = FakeGit::healthy(1, 1);
        git.revs.insert(
            "HEAD".to_string(),
            Err("ambiguous argument 'HEAD': unknown revision".to_string()),
        );
        let d = measure_scan_divergence(Some(Path::new("/repo/plans")), &git, NOW);
        assert_eq!(d.state, ScanDivergenceState::Unknown);
        assert_eq!(d.head_sha, None);
        assert_eq!((d.behind, d.ahead), (None, None));
        assert!(d.detail.unwrap().contains("unknown revision"));
    }

    /// Ahead-only divergence is stale too. This machine, once it catches up,
    /// reads 0 behind / 11 ahead — eleven commits of plans on no ref — and
    /// that must reach the log at WARN, not INFO.
    #[test]
    fn ahead_only_divergence_counts_as_stale() {
        let ahead_only = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::healthy(0, 11),
            NOW,
        );
        assert!(
            ahead_only.is_stale(),
            "0 behind / 11 ahead is not agreement"
        );
        let behind_only = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::healthy(2153, 0),
            NOW,
        );
        assert!(behind_only.is_stale());
        let in_step =
            measure_scan_divergence(Some(Path::new("/repo/plans")), &FakeGit::healthy(0, 0), NOW);
        assert!(!in_step.is_stale());
        // The three non-Measured states never claim staleness — they have no
        // counts to claim it from.
        assert!(!ScanDivergence::not_scanning().is_stale());
        assert!(!ScanDivergence::unknown(None, "x").is_stale());
    }

    /// The other half of the orientation, and the half a parser test cannot
    /// see: WHICH rev lands on the left of the `...`. Swap the two
    /// interpolations and every reading inverts while
    /// `left_right_count_parses_behind_then_ahead` still passes.
    #[test]
    fn left_right_range_puts_the_reference_on_the_left() {
        assert_eq!(
            left_right_range("origin/main", "HEAD"),
            "origin/main...HEAD"
        );
        assert_eq!(
            left_right_range("origin/trunk", "HEAD"),
            "origin/trunk...HEAD"
        );
    }

    /// The orientation, pinned at the wire. `rev-list --left-right --count
    /// <ref>...HEAD` emits `<left>\t<right>` where LEFT is what the ref has
    /// and HEAD lacks (BEHIND) and RIGHT is the mirror (AHEAD). Swapping them
    /// turns a 2153-commit park into a 2153-commit lead, which reads like a
    /// busy authoring machine instead of a stale one — the failure this whole
    /// detector would then be reporting backwards.
    #[test]
    fn left_right_count_parses_behind_then_ahead() {
        assert_eq!(parse_left_right_count("2153\t11"), Ok((2153, 11)));
        assert_eq!(parse_left_right_count("2153\t11\n"), Ok((2153, 11)));
        assert_eq!(parse_left_right_count("0\t0"), Ok((0, 0)));
        // A space-separated variant parses identically — the split is on
        // whitespace, so no locale or pager setting can flip the meaning.
        assert_eq!(parse_left_right_count("7 2"), Ok((7, 2)));
    }

    /// Anything that is not exactly two numbers is an ERROR, not a defaulted
    /// zero: a parse that silently yields `(0, 0)` would report a parked tree
    /// as in step.
    #[test]
    fn left_right_count_refuses_to_default_on_unparseable_output() {
        for raw in ["", "12", "1\t2\t3", "a\tb", "-1\t2"] {
            assert!(
                parse_left_right_count(raw).is_err(),
                "{raw:?} must not parse to a count"
            );
        }
    }

    /// The STORE overwrites rather than merges: a later reading fully replaces
    /// an earlier one, so a measurement can never outlive the configuration it
    /// was taken under.
    ///
    /// This pins the store alone — it calls [`record_scan_divergence`]
    /// directly, so it says nothing about WHERE in the tick the call sits.
    /// That the idle path reaches it at all is pinned by
    /// `an_idle_tick_records_not_scanning_rather_than_nothing`.
    #[test]
    fn recording_a_divergence_overwrites_the_previous_reading() {
        let metrics = AdapterMetrics::default();
        assert_eq!(
            metrics.snapshot().scan_divergence,
            None,
            "before the first tick the reading is UNKNOWN, not 'no divergence'"
        );

        let measured = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::healthy(2153, 11),
            NOW,
        );
        record_scan_divergence(measured.clone(), &metrics);
        assert_eq!(metrics.snapshot().scan_divergence, Some(measured));

        // The operator clears `paths.plans_dir`: the idle tick must replace
        // the stale measurement with `NotScanning`, never leave it in place.
        record_scan_divergence(ScanDivergence::not_scanning(), &metrics);
        let after = metrics
            .snapshot()
            .scan_divergence
            .expect("idle still records");
        assert_eq!(after.state, ScanDivergenceState::NotScanning);
        assert_eq!((after.behind, after.ahead), (None, None));
    }

    /// The out-of-band UNKNOWN constructor (a probe task that would not run)
    /// keeps the dir it was measuring and always carries a reason.
    #[test]
    fn unknown_constructor_carries_the_dir_and_a_reason() {
        let d = ScanDivergence::unknown(Some("/repo/plans".to_string()), "task join failed");
        assert_eq!(d.state, ScanDivergenceState::Unknown);
        assert_eq!(d.plans_dir.as_deref(), Some("/repo/plans"));
        assert_eq!(d.detail.as_deref(), Some("task join failed"));
        assert_eq!((d.behind, d.ahead), (None, None));
    }

    // ---- ref freshness + the floor rule (plan 2026-09-11-…-its-own-drift, Revised P1) ----

    const HOUR: i64 = 3600;

    fn measured_with(
        refreshed_at: Result<Option<i64>, String>,
        behind: u64,
        ahead: u64,
    ) -> ScanDivergence {
        measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::refreshed(behind, ahead, refreshed_at),
            NOW,
        )
    }

    /// A ref refreshed five minutes ago — the census's own cadence — is
    /// current: the counts are real numbers, not floors, and a clean
    /// measurement still explains nothing.
    #[test]
    fn a_fresh_ref_makes_the_counts_current_not_floors() {
        let d = measured_with(Ok(Some(NOW - 300)), 2153, 11);
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.ref_age_secs, Some(300));
        assert_eq!(d.ref_is_fresh(), Some(true));
        assert!(!d.counts_are_floors());
        assert_eq!(d.detail, None);
        assert!(
            d.is_stale(),
            "is_stale keeps its meaning: measured non-zero counts"
        );
    }

    /// A ref seven hours old is past the window: the counts become LOWER
    /// BOUNDS, and the measured numbers themselves are preserved — a floor is
    /// a qualification of the counts, not a reason to drop them.
    #[test]
    fn a_seven_hour_old_ref_makes_the_counts_floors_and_keeps_them() {
        let d = measured_with(Ok(Some(NOW - 7 * HOUR)), 2153, 11);
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.ref_age_secs, Some(7 * 3600));
        assert_eq!(d.ref_is_fresh(), Some(false));
        assert!(d.counts_are_floors());
        assert_eq!((d.behind, d.ahead), (Some(2153), Some(11)));
        assert!(d.is_stale());
    }

    /// The window's edge: exactly six hours is still fresh, one second more
    /// is a floor.
    #[test]
    fn the_freshness_window_is_inclusive_at_six_hours() {
        let window = i64::try_from(SCAN_REF_FRESH_WITHIN.as_secs()).unwrap();
        assert_eq!(window, 6 * HOUR);
        assert!(!measured_with(Ok(Some(NOW - window)), 0, 0).counts_are_floors());
        assert!(measured_with(Ok(Some(NOW - window - 1)), 0, 0).counts_are_floors());
    }

    /// No source records a refresh at all: the age is UNKNOWN, and unknown is
    /// a floor — never taken as fresh. The state stays `Measured` (the counts
    /// are real) and the detail says why the age is missing.
    #[test]
    fn an_unknown_ref_age_is_a_floor_never_fresh() {
        let d = measured_with(Ok(None), 0, 0);
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.ref_age_secs, None);
        assert_eq!(d.ref_is_fresh(), None);
        assert!(
            d.counts_are_floors(),
            "a 0/0 against a ref of unknown age proves nothing and must not read as in step"
        );
        assert_eq!((d.behind, d.ahead), (Some(0), Some(0)));
        let detail = d.detail.expect("an absent age is explained");
        assert!(detail.contains("unknown"), "{detail}");
    }

    /// The age PROBE failing is not the measurement failing: `Measured` is
    /// kept with the real counts, `ref_age_secs` is `None`, the counts are
    /// floors, and the probe's own words reach the detail.
    #[test]
    fn a_failed_age_probe_keeps_measured_with_no_age() {
        let d = measured_with(
            Err("`git reflog show` did not answer (Timeout)".to_string()),
            40,
            2,
        );
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.ref_age_secs, None);
        assert_eq!((d.behind, d.ahead), (Some(40), Some(2)));
        assert!(d.counts_are_floors());
        assert!(d.detail.unwrap().contains("Timeout"));
    }

    /// A refresh stamped in the future (clock skew) saturates at age 0 rather
    /// than wrapping into an astronomically old ref.
    #[test]
    fn a_future_refresh_saturates_at_age_zero() {
        let d = measured_with(Ok(Some(NOW + 120)), 0, 0);
        assert_eq!(d.ref_age_secs, Some(0));
        assert_eq!(ref_age_from(NOW, NOW + 120), 0);
        assert_eq!(ref_age_from(NOW, NOW - 5), 5);
        // The tolerance's edge is still ordinary skew.
        let edge = measured_with(Ok(Some(NOW + SCAN_REF_FUTURE_TOLERANCE_SECS)), 0, 0);
        assert_eq!(edge.ref_age_secs, Some(0));
    }

    /// A refresh record dated FAR in the future — a clock corrected backwards
    /// after a fetch, a checkout restored with future mtimes — proves nothing
    /// about when the ref was refreshed. Clamping it to age 0 would be a false
    /// "fresh"; it is an UNKNOWN age instead, so the counts are floors, and
    /// the detail says why.
    #[test]
    fn a_far_future_refresh_is_an_unknown_age_not_fresh() {
        let d = measured_with(Ok(Some(NOW + SCAN_REF_FUTURE_TOLERANCE_SECS + 1)), 0, 0);
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!(d.ref_age_secs, None);
        assert_eq!(d.ref_is_fresh(), None);
        assert!(d.counts_are_floors());
        let detail = d.detail.expect("an unknown age is explained");
        assert!(
            detail.contains("FUTURE") && detail.contains("301s"),
            "{detail}"
        );

        let a_day_ahead = measured_with(Ok(Some(NOW + 24 * HOUR)), 7, 0);
        assert!(a_day_ahead.counts_are_floors());
        assert_eq!(
            a_day_ahead.behind,
            Some(7),
            "the counts themselves are kept"
        );
    }

    /// The three non-`Measured` states have no counts, so there is nothing
    /// for the floor rule to qualify — and nothing to call fresh either.
    #[test]
    fn non_measured_states_have_no_freshness_and_no_floors() {
        for d in [
            ScanDivergence::not_scanning(),
            ScanDivergence::unknown(None, "x"),
            measure_scan_divergence(
                Some(Path::new("/plain/plans")),
                &FakeGit {
                    root: Ok(None),
                    ..FakeGit::healthy(0, 0)
                },
                NOW,
            ),
        ] {
            assert_eq!(d.ref_is_fresh(), None, "{:?}", d.state);
            assert!(!d.counts_are_floors(), "{:?}", d.state);
            assert_eq!(d.ref_age_secs, None);
        }
    }

    /// The log wording IS the floor rule's read surface. A `0/0` floor must
    /// never produce the benign "reading changed" line a fresh `0/0` produces:
    /// it logs at WARN and says the counts are a lower bound, naming the age —
    /// or naming that the age is unknown.
    #[test]
    fn a_floor_of_zero_never_logs_as_in_step() {
        let (fresh_warn, fresh_text) =
            scan_divergence_message(&measured_with(Ok(Some(NOW - 60)), 0, 0));
        assert!(!fresh_warn, "a proven-current 0/0 is the benign reading");
        assert_eq!(
            fresh_text,
            "plan adapter: scan-source divergence reading changed"
        );

        let (warn, text) = scan_divergence_message(&measured_with(Ok(Some(NOW - 7 * HOUR)), 0, 0));
        assert!(warn);
        assert_ne!(text, fresh_text);
        assert!(text.contains("LOWER BOUND"), "{text}");
        assert!(text.contains("25200s"), "names the age: {text}");

        let (warn, text) = scan_divergence_message(&measured_with(Ok(None), 0, 0));
        assert!(warn);
        assert!(
            text.contains("LOWER BOUND") && text.contains("UNKNOWN age"),
            "{text}"
        );

        // A stale floor says both things: diverged, and at least this much.
        let (warn, text) =
            scan_divergence_message(&measured_with(Ok(Some(NOW - 7 * HOUR)), 2153, 11));
        assert!(warn);
        assert!(
            text.contains("LOWER BOUNDS") && text.contains("WORKING TREE"),
            "{text}"
        );

        // A stale reading against a FRESH ref keeps the parked-tree text and
        // names how old the ref it counted against is.
        let (warn, text) = scan_divergence_message(&measured_with(Ok(Some(NOW - 300)), 2153, 11));
        assert!(warn);
        assert!(!text.contains("LOWER BOUND"), "{text}");
        assert!(text.contains("300s ago"), "{text}");
    }

    /// The ref's age grows every tick; only its CROSSING of the window is a
    /// change. Plain equality would re-log the reading every minute and
    /// re-post it every cycle.
    #[test]
    fn a_reading_is_the_same_while_its_age_advances_inside_the_window() {
        let a = measured_with(Ok(Some(NOW - 60)), 5, 0);
        let b = measured_with(Ok(Some(NOW - 120)), 5, 0);
        assert_ne!(a, b, "the raw ages differ");
        assert!(a.is_same_reading(&b));

        let crossed = measured_with(Ok(Some(NOW - 7 * HOUR)), 5, 0);
        assert!(!a.is_same_reading(&crossed), "fresh -> floors is news");
        let unknown = measured_with(Ok(None), 5, 0);
        assert!(!a.is_same_reading(&unknown), "fresh -> unknown age is news");

        let moved = measured_with(Ok(Some(NOW - 60)), 6, 0);
        assert!(!a.is_same_reading(&moved), "a count change is news");
    }

    /// The store always keeps the LATEST reading, age included, even when the
    /// change detector (and so the log) treats it as the same reading.
    #[test]
    fn recording_keeps_the_latest_age_even_when_the_reading_is_the_same() {
        let metrics = AdapterMetrics::default();
        record_scan_divergence(measured_with(Ok(Some(NOW - 60)), 5, 0), &metrics);
        record_scan_divergence(measured_with(Ok(Some(NOW - 120)), 5, 0), &metrics);
        assert_eq!(
            metrics.snapshot().scan_divergence.unwrap().ref_age_secs,
            Some(120)
        );
    }

    // ---- the two refresh sources, parsed and combined ----

    #[test]
    fn default_branch_name_strips_the_remote() {
        assert_eq!(default_branch_name("origin/main"), "main");
        assert_eq!(default_branch_name("origin/release/2026"), "release/2026");
        assert_eq!(default_branch_name("upstream/trunk"), "trunk");
        assert_eq!(default_branch_name("main"), "main");
    }

    /// `FETCH_HEAD` counts only when it names the default branch AT the sha
    /// the tracking ref now holds.
    #[test]
    fn fetch_head_counts_only_the_default_branch_at_the_current_sha() {
        let sha = "a".repeat(40);
        let other = "c".repeat(40);
        let url = "https://github.com/qontinui/qontinui-dev-notes";
        // A single-branch fetch (the census's refspec shape).
        assert!(fetch_head_names_ref(
            &format!("{sha}\t\tbranch 'main' of {url}\n"),
            "main",
            &sha
        ));
        // A full `git fetch origin`: main marked not-for-merge among others.
        let all = format!(
            "{other}\t\tbranch 'feature' of {url}\n{sha}\tnot-for-merge\tbranch 'main' of {url}\n"
        );
        assert!(fetch_head_names_ref(&all, "main", &sha));
        // Another branch only: refreshes nothing we compare against.
        assert!(!fetch_head_names_ref(
            &format!("{other}\t\tbranch 'feature' of {url}\n"),
            "main",
            &other
        ));
        // A prefix of the name is a different branch.
        assert!(!fetch_head_names_ref(
            &format!("{sha}\t\tbranch 'main-old' of {url}\n"),
            "main",
            &sha
        ));
        // Right name, wrong sha: the fetch did not land in the tracking ref.
        assert!(!fetch_head_names_ref(
            &format!("{other}\t\tbranch 'main' of {url}\n"),
            "main",
            &sha
        ));
        // Tags and garbage.
        assert!(!fetch_head_names_ref(
            &format!("{sha}\tnot-for-merge\ttag 'main' of {url}\n"),
            "main",
            &sha
        ));
        assert!(!fetch_head_names_ref("", "main", &sha));
        assert!(!fetch_head_names_ref("not a fetch head", "main", &sha));
    }

    /// The reflog selector's number is the ENTRY time. Empty is an absent
    /// source; `@{0}` (an index, not a date) must not parse as 1970.
    #[test]
    fn reflog_entry_time_parses_the_selector_and_refuses_an_index() {
        assert_eq!(
            parse_reflog_entry_time("origin/main@{1789129011}"),
            Ok(Some(1_789_129_011))
        );
        assert_eq!(
            parse_reflog_entry_time("refs/remotes/origin/main@{1789129011}\n"),
            Ok(Some(1_789_129_011))
        );
        assert_eq!(parse_reflog_entry_time(""), Ok(None));
        assert_eq!(parse_reflog_entry_time("  \n"), Ok(None));
        for raw in [
            "origin/main@{0}",
            "origin/main@{3}",
            "origin/main",
            "origin/main@{soon}",
        ] {
            assert!(parse_reflog_entry_time(raw).is_err(), "{raw:?}");
        }
    }

    /// The fresher source wins; a failed probe beside a known timestamp keeps
    /// the timestamp (an overstated age at worst), and beside nothing is an
    /// error.
    #[test]
    fn the_fresher_refresh_source_wins_and_partial_failure_leans_old() {
        let err = || Err::<Option<i64>, String>("probe failed".to_string());
        assert_eq!(fresher_refresh(Ok(Some(10)), Ok(Some(20))), Ok(Some(20)));
        assert_eq!(fresher_refresh(Ok(Some(30)), Ok(Some(20))), Ok(Some(30)));
        assert_eq!(fresher_refresh(Ok(None), Ok(Some(20))), Ok(Some(20)));
        assert_eq!(fresher_refresh(Ok(None), Ok(None)), Ok(None));
        assert_eq!(fresher_refresh(Ok(Some(10)), err()), Ok(Some(10)));
        assert_eq!(fresher_refresh(err(), Ok(Some(10))), Ok(Some(10)));
        assert!(fresher_refresh(Ok(None), err()).is_err());
        assert!(fresher_refresh(err(), Ok(None)).is_err());
        assert!(fresher_refresh(err(), err()).is_err());
    }

    // ---- ProcessGit against real repos ----

    /// Run `git` in `dir` with an isolated identity and no signing, optionally
    /// pinning the committer date (which is also the REFLOG entry time).
    fn real_git(dir: &Path, args: &[&str], committer_date: Option<&str>) -> String {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=CI",
                "-c",
                "user.email=ci@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args);
        if let Some(date) = committer_date {
            cmd.env("GIT_COMMITTER_DATE", date);
        }
        let out = cmd.output().expect("git spawns");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A bare `origin` carrying `main` and `feature`, and a `reader` clone
    /// that has fetched `main` with its REFLOG entry pinned to 2020 — so the
    /// reflog alone would read the ref as six years old.
    fn origin_and_old_reflog_reader() -> (tempfile::TempDir, PathBuf, String) {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let writer = tmp.path().join("writer");
        let reader = tmp.path().join("reader");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&writer).unwrap();
        std::fs::create_dir_all(&reader).unwrap();
        real_git(&origin, &["init", "-q", "--bare", "-b", "main"], None);
        real_git(&writer, &["init", "-q", "-b", "main"], None);
        real_git(
            &writer,
            &["remote", "add", "origin", origin.to_str().unwrap()],
            None,
        );
        real_git(
            &writer,
            &["commit", "-q", "--allow-empty", "-m", "one"],
            None,
        );
        real_git(&writer, &["push", "-q", "origin", "main"], None);
        real_git(&writer, &["checkout", "-q", "-b", "feature"], None);
        real_git(&writer, &["commit", "-q", "--allow-empty", "-m", "f"], None);
        real_git(&writer, &["push", "-q", "origin", "feature"], None);
        real_git(&reader, &["init", "-q", "-b", "main"], None);
        real_git(
            &reader,
            &["remote", "add", "origin", origin.to_str().unwrap()],
            None,
        );
        real_git(
            &reader,
            &["fetch", "-q", "origin", "main"],
            Some("@1600000000 +0000"),
        );
        let sha = real_git(&reader, &["rev-parse", "refs/remotes/origin/main"], None);
        (tmp, reader, sha)
    }

    /// The plain case on real git output: a clone that has just fetched reads
    /// a refresh within seconds of now — through the reflog AND `FETCH_HEAD`.
    #[test]
    fn process_git_reads_a_just_fetched_ref_as_refreshed_now() {
        let tmp = tempfile::tempdir().unwrap();
        let origin = tmp.path().join("origin.git");
        let clone = tmp.path().join("clone");
        std::fs::create_dir_all(&origin).unwrap();
        std::fs::create_dir_all(&clone).unwrap();
        real_git(&origin, &["init", "-q", "--bare", "-b", "main"], None);
        real_git(&clone, &["init", "-q", "-b", "main"], None);
        real_git(
            &clone,
            &["remote", "add", "origin", origin.to_str().unwrap()],
            None,
        );
        real_git(
            &clone,
            &["commit", "-q", "--allow-empty", "-m", "one"],
            None,
        );
        real_git(&clone, &["push", "-q", "origin", "main"], None);
        real_git(&clone, &["fetch", "-q", "origin"], None);
        let sha = real_git(&clone, &["rev-parse", "refs/remotes/origin/main"], None);

        let now = chrono::Utc::now().timestamp();
        let at = ProcessGit
            .ref_refreshed_at(&clone, "origin/main", &sha)
            .expect("the probes answer")
            .expect("a just-fetched ref has a refresh time");
        assert!((now - at).abs() <= 10, "refreshed at {at}, now {now}");
        // Each source on its own agrees.
        assert!(ProcessGit::reflog_refreshed_at(&clone, "origin/main")
            .unwrap()
            .is_some());
        let fetch_head = ProcessGit::fetch_head_refreshed_at(&clone, "origin/main", &sha)
            .unwrap()
            .expect("FETCH_HEAD names main at the tracking sha");
        assert!((now - fetch_head).abs() <= 10);
    }

    /// `FETCH_HEAD` fresher than the reflog WINS: a fetch that found `main`
    /// unchanged writes no reflog entry, so the reflog alone would call a
    /// just-verified ref six years old.
    #[test]
    fn process_git_prefers_a_fresher_fetch_head_over_an_old_reflog() {
        let (_tmp, reader, sha) = origin_and_old_reflog_reader();
        assert_eq!(
            ProcessGit::reflog_refreshed_at(&reader, "origin/main"),
            Ok(Some(1_600_000_000)),
            "the reflog entry time is the pinned committer date, not the commit's"
        );
        let now = chrono::Utc::now().timestamp();
        let at = ProcessGit
            .ref_refreshed_at(&reader, "origin/main", &sha)
            .unwrap()
            .unwrap();
        assert!(
            (now - at).abs() <= 10,
            "FETCH_HEAD's mtime should win: {at} vs now {now}"
        );
    }

    /// A `FETCH_HEAD` naming only ANOTHER branch refreshes nothing this
    /// reading compares against, so the (old) reflog answers — even though
    /// that `FETCH_HEAD` was written seconds ago.
    #[test]
    fn process_git_ignores_a_fetch_head_that_names_only_another_branch() {
        let (_tmp, reader, sha) = origin_and_old_reflog_reader();
        real_git(&reader, &["fetch", "-q", "origin", "feature"], None);
        let fetch_head_path = reader.join(real_git(
            &reader,
            &["rev-parse", "--git-path", "FETCH_HEAD"],
            None,
        ));
        let contents = std::fs::read_to_string(&fetch_head_path).unwrap();
        assert!(contents.contains("branch 'feature' of") && !contents.contains("branch 'main' of"));

        assert_eq!(
            ProcessGit::fetch_head_refreshed_at(&reader, "origin/main", &sha),
            Ok(None)
        );
        assert_eq!(
            ProcessGit.ref_refreshed_at(&reader, "origin/main", &sha),
            Ok(Some(1_600_000_000)),
            "the reflog answers, not the fresh FETCH_HEAD of another branch"
        );
    }

    /// Neither source: a clone whose tracking ref has no reflog and no
    /// `FETCH_HEAD` answers `Ok(None)` — an unknown age, not an error.
    #[test]
    fn process_git_answers_none_when_neither_source_exists() {
        let (_tmp, reader, sha) = origin_and_old_reflog_reader();
        let git_dir = reader.join(real_git(&reader, &["rev-parse", "--git-dir"], None));
        std::fs::remove_file(git_dir.join("FETCH_HEAD")).unwrap();
        std::fs::remove_file(git_dir.join("logs/refs/remotes/origin/main")).unwrap();
        assert_eq!(
            ProcessGit.ref_refreshed_at(&reader, "origin/main", &sha),
            Ok(None)
        );
    }

    /// A LINKED worktree whose primary checkout did the fetch — the census's
    /// shape: it fetches in the canonical checkout, while the plans dir may be
    /// a worktree of it. `FETCH_HEAD` is per-worktree, so the linked tree's
    /// own `--git-path FETCH_HEAD` does not exist; the refresh is recorded
    /// only in the common dir's file. Reading the per-worktree file alone
    /// would fall back to the (2020) reflog and call a just-fetched ref six
    /// years old.
    #[test]
    fn process_git_reads_the_primary_fetch_head_from_a_linked_worktree() {
        let (tmp, primary, sha) = origin_and_old_reflog_reader();
        let wt = tmp.path().join("linked");
        real_git(
            &primary,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "wtb",
                wt.to_str().unwrap(),
                "origin/main",
            ],
            None,
        );
        let own = wt.join(real_git(
            &wt,
            &["rev-parse", "--git-path", "FETCH_HEAD"],
            None,
        ));
        assert!(
            !own.exists(),
            "the linked tree has no FETCH_HEAD of its own: {}",
            own.display()
        );
        assert_eq!(
            fetch_head_file_refreshed_at(&own, "main", &sha),
            Ok(None),
            "the per-worktree file alone establishes nothing"
        );

        let now = chrono::Utc::now().timestamp();
        let at = ProcessGit
            .ref_refreshed_at(&wt, "origin/main", &sha)
            .unwrap()
            .expect("the primary's FETCH_HEAD answers");
        assert!(
            (now - at).abs() <= 10,
            "the common dir's FETCH_HEAD should win over the 2020 reflog: {at} vs now {now}"
        );
    }

    /// A probe that times out every tick is ONE unchanging fault, not a new
    /// reading every tick. The killed child's pid used to reach `detail`
    /// through `DegradeReason`'s `Debug` form, and `detail` is compared
    /// verbatim — so each tick WARNed and re-posted.
    #[test]
    fn a_timed_out_probe_is_the_same_reading_whatever_the_pid() {
        use crate::process_helpers::DegradeReason;
        let args = ["reflog", "show"];
        let first = ProcessGit::describe(
            &args,
            &DegradeReason::TimedOut {
                pid: 41_873,
                reaped: true,
            },
        );
        let second = ProcessGit::describe(
            &args,
            &DegradeReason::TimedOut {
                pid: 52_004,
                reaped: true,
            },
        );
        assert_eq!(first, second);
        assert!(
            !first.contains("41873") && !first.contains("pid"),
            "{first}"
        );
        assert!(
            first.contains("TimedOut"),
            "the failure stays named: {first}"
        );

        let a = measured_with(Err(first), 5, 0);
        let b = measured_with(Err(second), 5, 0);
        assert!(a.is_same_reading(&b));
        // An unreaped kill IS a different fault, and still says so.
        let unreaped = ProcessGit::describe(
            &args,
            &DegradeReason::TimedOut {
                pid: 1,
                reaped: false,
            },
        );
        assert!(unreaped.contains("not reaped"), "{unreaped}");
    }

    /// Every reading the measurement returns carries the time it was taken,
    /// and that time is not part of what the reading SAYS.
    #[test]
    fn a_reading_is_stamped_with_its_measurement_time() {
        let d = measured_with(Ok(Some(NOW - 60)), 5, 0);
        assert_eq!(d.observed_at_unix, Some(NOW));
        for other in [
            measure_scan_divergence(None, &FakeGit::healthy(0, 0), NOW),
            measure_scan_divergence(
                Some(Path::new("/repo/plans")),
                &FakeGit {
                    default_ref: Err("no origin/HEAD".to_string()),
                    ..FakeGit::healthy(0, 0)
                },
                NOW,
            ),
        ] {
            assert_eq!(other.observed_at_unix, Some(NOW), "{:?}", other.state);
        }
        let later = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::refreshed(5, 0, Ok(Some(NOW - 60))),
            NOW + 60,
        );
        assert_eq!(later.observed_at_unix, Some(NOW + 60));
        assert!(
            d.is_same_reading(&later),
            "a later measurement of the same state is the same reading"
        );
    }

    /// The tick's measurement resolves the reading's `source_repo` — the
    /// filesystem walk runs there, on the blocking pool, so the scan-root
    /// report only copies it.
    #[test]
    fn the_tick_measurement_resolves_source_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("qontinui-dev-notes");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("plans")).unwrap();
        let plans = repo.join("plans");
        let d = measure_scan_source(&plans, &FakeGit::healthy(0, 0), NOW);
        assert_eq!(d.source_repo.as_deref(), Some("qontinui-dev-notes/plans"));
        assert_eq!(
            d.source_repo,
            super::super::body_push::derive_source_repo(&plans)
        );
        assert_eq!(d.observed_at_unix, Some(NOW));
    }

    /// Live check against this box's own plans checkout — machine-specific, so
    /// ignored. Run with `--ignored` and compare against
    /// `git reflog show -n1 --date=unix --format=%gd refs/remotes/origin/main`
    /// and `stat -c %Y .git/FETCH_HEAD` in the same repo. The checkout is
    /// named by `QONTINUI_LIVE_PLANS_REPO` (the repo root holding `plans/`);
    /// unset, the test says so and skips.
    #[test]
    #[ignore = "reads a machine-specific checkout named by QONTINUI_LIVE_PLANS_REPO"]
    fn live_ref_refreshed_at_matches_the_shell() {
        let Ok(repo) = std::env::var("QONTINUI_LIVE_PLANS_REPO") else {
            println!(
                "SKIPPED: set QONTINUI_LIVE_PLANS_REPO to a checkout's root (the repo holding \
                 plans/) to compare ProcessGit's refresh read against the shell"
            );
            return;
        };
        let repo = Path::new(&repo);
        let default_ref = ProcessGit.default_ref(repo).unwrap();
        let sha = ProcessGit.rev_parse(repo, &default_ref).unwrap();
        let now = chrono::Utc::now().timestamp();
        let fetch_head = ProcessGit::fetch_head_refreshed_at(repo, &default_ref, &sha);
        let reflog = ProcessGit::reflog_refreshed_at(repo, &default_ref);
        let combined = ProcessGit.ref_refreshed_at(repo, &default_ref, &sha);
        println!(
            "LIVE repo={} default_ref={default_ref} sha={sha} now={now} fetch_head={fetch_head:?} \
             reflog={reflog:?} combined={combined:?}",
            repo.display()
        );
        let d = measure_scan_divergence(Some(repo.join("plans").as_path()), &ProcessGit, now);
        println!(
            "LIVE reading state={} behind={:?} ahead={:?} ref_age_secs={:?} counts_are_floors={} detail={:?}",
            d.state.as_str(),
            d.behind,
            d.ahead,
            d.ref_age_secs,
            d.counts_are_floors(),
            d.detail
        );
        assert!(combined.unwrap().is_some());
    }

    // ---- the scan-root report's posting policy (Revised P2, runner half) ----

    fn instant_plus(base: std::time::Instant, secs: u64) -> std::time::Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn scan_report_is_due_on_first_reading_and_never_before_one() {
        let t0 = std::time::Instant::now();
        let reading = measured_with(Ok(Some(NOW - 60)), 5, 0);
        assert!(
            scan_report_due(None, None, Some(&reading), t0),
            "never posted -> due"
        );
        assert!(
            !scan_report_due(None, None, None, t0),
            "no reading yet (the loop has not ticked) -> nothing to post"
        );
    }

    #[test]
    fn an_unchanged_reading_is_not_reposted_inside_the_heartbeat() {
        let t0 = std::time::Instant::now();
        let posted = measured_with(Ok(Some(NOW - 60)), 5, 0);
        // Same reading one tick later: its ref age advanced, nothing else.
        let now = measured_with(Ok(Some(NOW - 120)), 5, 0);
        let heartbeat = SCAN_REPORT_HEARTBEAT.as_secs();
        assert!(!scan_report_due(
            Some((&posted, t0)),
            None,
            Some(&now),
            instant_plus(t0, 60)
        ));
        assert!(!scan_report_due(
            Some((&posted, t0)),
            None,
            Some(&now),
            instant_plus(t0, heartbeat - 1)
        ));
        assert!(
            scan_report_due(
                Some((&posted, t0)),
                None,
                Some(&now),
                instant_plus(t0, heartbeat)
            ),
            "the heartbeat re-posts an unchanged reading so observed_at ages honestly"
        );
    }

    #[test]
    fn a_changed_reading_is_posted_at_once() {
        let t0 = std::time::Instant::now();
        let posted = measured_with(Ok(Some(NOW - 60)), 5, 0);
        let moved = measured_with(Ok(Some(NOW - 60)), 9, 0);
        assert!(scan_report_due(
            Some((&posted, t0)),
            None,
            Some(&moved),
            instant_plus(t0, 60)
        ));
        let went_floor = measured_with(Ok(Some(NOW - 7 * HOUR)), 5, 0);
        assert!(
            scan_report_due(
                Some((&posted, t0)),
                None,
                Some(&went_floor),
                instant_plus(t0, 60)
            ),
            "crossing into floors is a change the read side must see"
        );
        assert!(scan_report_due(
            Some((&posted, t0)),
            None,
            Some(&ScanDivergence::not_scanning()),
            instant_plus(t0, 60)
        ));
    }

    #[test]
    fn a_failed_post_backs_off_before_retrying() {
        let t0 = std::time::Instant::now();
        let reading = measured_with(Ok(Some(NOW - 60)), 5, 0);
        let backoff = SCAN_REPORT_RETRY_AFTER_FAILURE.as_secs();
        assert!(!scan_report_due(
            None,
            Some(t0),
            Some(&reading),
            instant_plus(t0, 60)
        ));
        assert!(!scan_report_due(
            None,
            Some(t0),
            Some(&reading),
            instant_plus(t0, backoff - 1)
        ));
        assert!(scan_report_due(
            None,
            Some(t0),
            Some(&reading),
            instant_plus(t0, backoff)
        ));
        assert!(
            backoff < SCAN_REPORT_HEARTBEAT.as_secs(),
            "a recovered backend is caught up faster than a heartbeat"
        );
    }

    // ---- BodySync's ordering rules, over a recording reporter ----

    /// Records every scan-root report it is handed; answers as configured.
    #[derive(Default)]
    struct FakeReporter {
        sent: Mutex<Vec<super::super::body_push::ScanRootReport>>,
        fail: bool,
    }

    impl FakeReporter {
        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::default()
            }
        }
        fn states(&self) -> Vec<String> {
            self.sent
                .lock()
                .unwrap()
                .iter()
                .map(|r| r.state.clone())
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl super::super::body_push::ScanRootReporter for FakeReporter {
        async fn report_scan_root(
            &self,
            report: &super::super::body_push::ScanRootReport,
        ) -> Result<(), String> {
            self.sent.lock().unwrap().push(report.clone());
            if self.fail {
                Err("POST …/scan-roots -> 404 Not Found".to_string())
            } else {
                Ok(())
            }
        }
    }

    /// A body sync over NO scan roots (so a cycle's scan is empty and never
    /// reaches the network) whose reports go to `reporter`.
    fn body_sync_reporting_to(reporter: std::sync::Arc<FakeReporter>, gate_open: bool) -> BodySync {
        BodySync::new(
            Vec::new(),
            super::super::body_push::HttpArtifactSink::new("http://127.0.0.1:9"),
            std::sync::Arc::new(move || gate_open) as CaptureGate,
        )
        .with_reporter(reporter)
    }

    fn metrics_with(reading: ScanDivergence) -> AdapterMetrics {
        let metrics = AdapterMetrics::default();
        record_scan_divergence(reading, &metrics);
        metrics
    }

    /// A PAUSED body sync still reports its scan root: a paused sync is
    /// exactly when the corpus stops being refreshed, so it is the last cycle
    /// that should go quiet about the scan source.
    ///
    /// Neuter check: move the `report_scan_root_if_due` call in `run_cycle`
    /// below `if self.breaker.should_skip_cycle() { return; }` and this fails.
    #[tokio::test]
    async fn a_paused_body_sync_still_reports_its_scan_root() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_reporting_to(reporter.clone(), true);
        bs.breaker = FailureBreaker {
            consecutive_total_failures: 0,
            pause_cycles_remaining: 3,
        };
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 2153, 11));

        bs.run_cycle(&PlanConvention::operator_default(), &metrics)
            .await;

        assert_eq!(reporter.states(), vec!["measured"]);
        assert_eq!(
            bs.breaker.pause_cycles_remaining, 2,
            "the cycle WAS a paused one — the pause was consumed"
        );
    }

    /// An EMPTY scan still reports: the `artifacts.is_empty()` early return
    /// comes after the report.
    #[tokio::test]
    async fn an_empty_scan_still_reports_its_scan_root() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_reporting_to(reporter.clone(), true);
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 0, 0));

        bs.run_cycle(&PlanConvention::operator_default(), &metrics)
            .await;
        assert_eq!(reporter.states(), vec!["measured"]);

        // And the posting policy holds across cycles: the same reading is not
        // re-sent inside the heartbeat.
        bs.run_cycle(&PlanConvention::operator_default(), &metrics)
            .await;
        assert_eq!(reporter.states().len(), 1);
    }

    /// A report that fails, cycle after cycle, never feeds the body-push
    /// breaker: a backend that does not serve the scan-roots route yet must
    /// not pause plan-body capture. The breaker starts one failure short of
    /// tripping, so a single counted report failure would pause it.
    #[tokio::test]
    async fn a_failed_scan_root_report_never_touches_the_breaker() {
        let reporter = std::sync::Arc::new(FakeReporter::failing());
        let mut bs = body_sync_reporting_to(reporter.clone(), true);
        let armed = FailureBreaker {
            consecutive_total_failures: TOTAL_FAILURE_CYCLES_BEFORE_PAUSE - 1,
            pause_cycles_remaining: 0,
        };
        bs.breaker = armed;
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0));

        for _ in 0..(TOTAL_FAILURE_CYCLES_BEFORE_PAUSE + 2) {
            // Skip the retry backoff so every cycle really attempts a post.
            bs.last_scan_report_failure = None;
            bs.run_cycle(&PlanConvention::operator_default(), &metrics)
                .await;
        }

        assert_eq!(
            reporter.states().len(),
            usize::try_from(TOTAL_FAILURE_CYCLES_BEFORE_PAUSE + 2).unwrap(),
            "every cycle attempted a report"
        );
        assert!(
            bs.last_scan_report_failure.is_some(),
            "the failure is remembered"
        );
        assert!(bs.last_scan_report.is_none(), "nothing was accepted");
        assert_eq!(bs.breaker, armed, "the breaker never saw a report failure");
        assert!(!bs.breaker.is_paused());
    }

    /// Two readings identical except for WHEN they were taken (and the ref
    /// age that grows with it) are the same reading at every change-detection
    /// point — no log transition, not report-due inside the heartbeat — yet
    /// the store keeps the NEWER instant, and the heartbeat re-post carries it.
    /// The web ages a row from `observed_at` and reads one past 2700 s as
    /// unknown, so a heartbeat carrying the first measurement's time would make
    /// a live device read stale.
    #[tokio::test]
    async fn only_the_measurement_instant_differs_so_nothing_changes_but_the_post_is_current() {
        let first = measured_with(Ok(Some(NOW - 60)), 5, 0);
        let second = measure_scan_divergence(
            Some(Path::new("/repo/plans")),
            &FakeGit::refreshed(5, 0, Ok(Some(NOW - 60))),
            NOW + 600,
        );
        assert_eq!(first.observed_at_unix, Some(NOW));
        assert_eq!(second.observed_at_unix, Some(NOW + 600));
        assert_ne!(first, second, "the instant (and the age) did move");
        assert!(first.is_same_reading(&second));
        assert!(
            !scan_divergence_changed(Some(&first), &second),
            "no log transition"
        );
        assert!(
            scan_divergence_changed(None, &second),
            "the first reading always logs"
        );

        let t0 = std::time::Instant::now();
        assert!(
            !scan_report_due(
                Some((&first, t0)),
                None,
                Some(&second),
                instant_plus(t0, 600)
            ),
            "not report-due inside the heartbeat"
        );

        let metrics = AdapterMetrics::default();
        record_scan_divergence(first.clone(), &metrics);
        record_scan_divergence(second.clone(), &metrics);
        assert_eq!(
            metrics.snapshot().scan_divergence.unwrap().observed_at_unix,
            Some(NOW + 600),
            "the store holds the latest tick's instant"
        );

        // Through the body sync: post the first, then let the heartbeat
        // elapse — the re-post carries the SECOND reading's instant.
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_reporting_to(reporter.clone(), true);
        bs.run_cycle(&PlanConvention::operator_default(), &metrics_with(first))
            .await;
        let later = metrics_with(second);
        bs.run_cycle(&PlanConvention::operator_default(), &later)
            .await;
        assert_eq!(
            reporter.states().len(),
            1,
            "same reading inside the heartbeat"
        );
        let (posted, _) = bs.last_scan_report.take().unwrap();
        bs.last_scan_report = Some((
            posted,
            std::time::Instant::now() - SCAN_REPORT_HEARTBEAT - Duration::from_secs(1),
        ));
        bs.run_cycle(&PlanConvention::operator_default(), &later)
            .await;
        let sent = reporter.sent.lock().unwrap().clone();
        assert_eq!(sent.len(), 2, "the heartbeat re-posted");
        let want = chrono::DateTime::<chrono::Utc>::from_timestamp(NOW + 600, 0)
            .unwrap()
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert_eq!(
            sent[1].observed_at, want,
            "the re-post carries the newer instant"
        );
        assert_ne!(sent[0].observed_at, sent[1].observed_at);
    }

    /// A tenant at `plan_capture = off` publishes nothing about its plans dir.
    #[tokio::test]
    async fn a_closed_capture_gate_reports_nothing() {
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut bs = body_sync_reporting_to(reporter.clone(), false);
        let metrics = metrics_with(measured_with(Ok(Some(NOW - 60)), 5, 0));
        bs.run_cycle(&PlanConvention::operator_default(), &metrics)
            .await;
        bs.report_while_idle(&metrics).await;
        assert!(reporter.states().is_empty());
    }

    /// A device with NO plans dir still reports `not_scanning` when it has a
    /// body sync — through the idle tick, which never reaches `run_cycle` —
    /// and under the same posting policy: once, not every tick.
    #[tokio::test]
    async fn an_idle_tick_reports_not_scanning_through_the_body_sync() {
        let (_cell, reader) = switchable_paths();
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let reporter = std::sync::Arc::new(FakeReporter::default());
        let mut state = LoopState::new(
            reader,
            Some(super::super::body_push::HttpArtifactSink::new(
                "http://127.0.0.1:9",
            )),
            std::sync::Arc::new(|| true) as CaptureGate,
        )
        .with_git(std::sync::Arc::new(FakeGit::healthy(0, 0)))
        .with_scan_reporter(reporter.clone());

        state.tick(&sink, &metrics).await;
        assert_eq!(reporter.states(), vec!["not_scanning"]);
        assert!(
            metrics
                .snapshot()
                .scan_divergence
                .unwrap()
                .observed_at_unix
                .is_some(),
            "the idle reading is stamped with the tick's clock too"
        );
        let sent = reporter.sent.lock().unwrap()[0].clone();
        assert_eq!(
            (sent.behind, sent.ahead, sent.counts_are_floors),
            (None, None, false)
        );

        state.tick(&sink, &metrics).await;
        assert_eq!(
            reporter.states().len(),
            1,
            "the unchanged reading is not re-posted inside the heartbeat"
        );
    }

    // ---- body-sync kill switch (plan 2026-09-03-…-on-by-default Phase 3) ----

    /// Serialized against the other env-touching tests; restores the flag AND the two web
    /// backend env layers, so the "no backend" arm below really has none.
    fn with_body_sync_env<T>(value: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _guard = crate::test_env::env_lock();
        let _restore = crate::test_env::EnvVarRestore::capture(&[
            PLAN_LIBRARY_SYNC_ENV,
            super::super::body_push::WEB_BACKEND_URL_ENV,
            super::super::body_push::WEB_BACKEND_URL_ENV_ALT,
        ]);
        std::env::remove_var(super::super::body_push::WEB_BACKEND_URL_ENV);
        std::env::remove_var(super::super::body_push::WEB_BACKEND_URL_ENV_ALT);
        match value {
            Some(v) => std::env::set_var(PLAN_LIBRARY_SYNC_ENV, v),
            None => std::env::remove_var(PLAN_LIBRARY_SYNC_ENV),
        }
        f()
    }

    /// On by default is the whole point: absent reads as on, and so does every
    /// value except the one exact spelling that kills it.
    #[test]
    fn the_body_sync_is_on_unless_the_env_is_exactly_zero() {
        assert!(with_body_sync_env(None, body_sync_enabled), "absent is ON");
        assert!(with_body_sync_env(Some("1"), body_sync_enabled));
        assert!(with_body_sync_env(Some(""), body_sync_enabled));
        assert!(with_body_sync_env(Some("true"), body_sync_enabled));
        assert!(
            with_body_sync_env(Some("off"), body_sync_enabled),
            "only `0` kills"
        );
        assert!(!with_body_sync_env(Some("0"), body_sync_enabled));
        assert!(
            !with_body_sync_env(Some(" 0 "), body_sync_enabled),
            "trimmed"
        );
    }

    // ---- body-sync posture is observable at spawn (plan 2026-08-27-… Phase 1, D6) ----

    /// The killed arm's line names the env var, its observed state, that
    /// `BodySync` was not built, and the consequence — the four things a reader
    /// of a runner log needs to answer "is the body sync on for this device?".
    #[test]
    fn the_killed_arm_names_the_env_var_and_the_consequence() {
        let (enabled, observed) = with_body_sync_env(Some("0"), || {
            (
                body_sync_enabled(),
                std::env::var(PLAN_LIBRARY_SYNC_ENV).ok(),
            )
        });
        assert!(!enabled);
        assert_eq!(observed.as_deref(), Some("0"));
        let msg = body_sync_disabled_message(observed.as_deref());
        assert!(msg.contains(PLAN_LIBRARY_SYNC_ENV), "{msg}");
        assert!(msg.contains("set to \"0\""), "{msg}");
        assert!(msg.contains("BodySync was NOT constructed"), "{msg}");
        assert!(msg.contains("agent.work_artifacts"), "{msg}");
        assert!(
            msg.contains("unset the variable"),
            "the line must say how to re-arm it: {msg}"
        );
        // The unset spelling is honest too, should the predicate ever move.
        assert!(body_sync_disabled_message(None).contains("is unset"));
    }

    /// The dial is announced on the first cycle whichever way it points, on a
    /// flip afterwards, and never on a steady cycle.
    #[test]
    fn the_capture_dial_is_announced_on_the_first_cycle_and_on_flips_only() {
        let first_closed = capture_gate_message(None, false).expect("first cycle, closed");
        assert!(first_closed.contains("CLOSED"), "{first_closed}");
        assert!(first_closed.contains("first cycle"), "{first_closed}");
        let first_open = capture_gate_message(None, true).expect("first cycle, open");
        assert!(first_open.contains("OPEN"), "{first_open}");
        assert!(first_open.contains("first cycle"), "{first_open}");

        assert_eq!(capture_gate_message(Some(false), false), None);
        assert_eq!(capture_gate_message(Some(true), true), None);

        let flipped = capture_gate_message(Some(false), true).expect("flip");
        assert!(flipped.contains("changed"), "{flipped}");
        assert_eq!(capture_gate_message(Some(true), false), Some(flipped));
    }

    /// With the sync on by default, the thing that protects a release build is
    /// the backend-resolution GUARD, not the flag: env unset and no backend ⇒
    /// no sink (so no `BodySync` on any tick); a backend ⇒ one; the kill switch
    /// ⇒ none even with one.
    #[test]
    fn with_no_backend_the_guard_not_the_flag_constructs_no_body_sync() {
        let build = |backend: Option<&str>| body_sync_sink_if_enabled(backend.map(str::to_string));
        assert!(with_body_sync_env(None, || build(None)).is_none());
        assert!(with_body_sync_env(None, || build(Some("http://web.example"))).is_some());
        assert!(with_body_sync_env(Some("0"), || build(Some("http://web.example"))).is_none());
    }

    // ---- path resolution (settings only) ------------------------------------

    /// The setting is the ONLY source. There is no env rung above it any
    /// more, and this resolver reads nothing but its argument — so the value
    /// the settings UI shows is, by construction, the value in effect.
    #[test]
    fn the_setting_is_the_only_source_of_the_plans_dir() {
        assert_eq!(
            resolve_plans_dir(Some("/settings/plans".to_string())).as_deref(),
            Some("/settings/plans")
        );
    }

    /// Nothing configured ⇒ the markdown-plan tier is off. This is the no-op
    /// the adapter's opt-in contract rests on.
    #[test]
    fn nothing_configured_resolves_to_none() {
        assert_eq!(resolve_plans_dir(None), None);
        assert_eq!(resolve_plans_archive_dir(None), None);
        assert_eq!(resolve_prompts_dir(None), None);
    }

    /// A blank setting is unset, not a directory named "" — for all three.
    #[test]
    fn blank_setting_resolves_to_none() {
        assert_eq!(resolve_plans_dir(Some("  ".to_string())), None);
        assert_eq!(resolve_plans_archive_dir(Some("".to_string())), None);
        assert_eq!(resolve_prompts_dir(Some("\t".to_string())), None);
    }

    // ---- the body-sync failure breaker ----------------------------------

    /// THE regression this breaker was rewritten for.
    ///
    /// In steady state the digest memory skips ~everything, so a cycle in which
    /// the operator edited one plan attempts exactly ONE push. The shipped
    /// predicate was `attempted > 0 && errors == attempted`, latched
    /// permanently — so that single push hitting a transient 500, a 30-second
    /// blip or a mid-rotation 401 killed the body sync for the whole process,
    /// and the error message prescribed restarting the runner, which fleet
    /// policy forbids.
    #[test]
    fn a_single_transient_failure_does_not_disable_the_sync() {
        let mut b = FailureBreaker::new();
        assert!(!b.record_cycle(1, 1), "one bad cycle must not trip");
        assert!(!b.is_paused());
        assert!(!b.should_skip_cycle(), "the next cycle still runs");
        // And the very next success clears the count entirely.
        assert!(!b.record_cycle(1, 0));
        assert_eq!(b.consecutive_total_failures(), 0);
        assert!(!b.is_paused());
    }

    /// A failure run BROKEN by one good cycle never trips, however long it is.
    #[test]
    fn non_consecutive_failures_never_trip_the_breaker() {
        let mut b = FailureBreaker::new();
        for _ in 0..20 {
            for _ in 0..(TOTAL_FAILURE_CYCLES_BEFORE_PAUSE - 1) {
                assert!(!b.record_cycle(3, 3));
            }
            // One partial success resets the run.
            assert!(!b.record_cycle(3, 1));
        }
        assert!(!b.is_paused());
    }

    /// A genuinely persistent failure DOES pause — the breaker still does its
    /// original job of not flooding the log every 60s forever.
    #[test]
    fn consecutive_total_failures_pause_the_sync() {
        let mut b = FailureBreaker::new();
        for cycle in 1..TOTAL_FAILURE_CYCLES_BEFORE_PAUSE {
            assert!(!b.record_cycle(5, 5), "cycle {cycle} is too early to trip");
        }
        assert!(
            b.record_cycle(5, 5),
            "the {TOTAL_FAILURE_CYCLES_BEFORE_PAUSE}th consecutive total failure trips it"
        );
        assert!(b.is_paused());
    }

    /// And the pause RE-ARMS. The old latch was one-way, and its own remedy
    /// (restart the runner) is forbidden by served policy
    /// `production-and-cost` `runner-lifecycle` — so a tripped sync was dead
    /// for the process's life. This one resumes on its own.
    #[test]
    fn the_pause_re_arms_rather_than_latching_forever() {
        let mut b = FailureBreaker::new();
        for _ in 0..TOTAL_FAILURE_CYCLES_BEFORE_PAUSE {
            b.record_cycle(2, 2);
        }
        assert!(b.is_paused());

        // It sits out exactly PAUSE_CYCLES cycles…
        for cycle in 0..PAUSE_CYCLES {
            assert!(b.should_skip_cycle(), "cycle {cycle} is still paused");
        }
        // …then runs again, with a clean failure count.
        assert!(!b.should_skip_cycle(), "the sync must resume by itself");
        assert!(!b.is_paused());
        assert_eq!(b.consecutive_total_failures(), 0);

        // A recovered backend then just works.
        assert!(!b.record_cycle(10, 0));
        assert!(!b.should_skip_cycle());
    }

    /// A cycle that made no network call at all is not a failure — otherwise a
    /// steady state in which every file is locally skipped would look like a
    /// total failure.
    #[test]
    fn a_cycle_with_no_attempts_is_not_a_failure() {
        let mut b = FailureBreaker::new();
        for _ in 0..(TOTAL_FAILURE_CYCLES_BEFORE_PAUSE * 3) {
            assert!(!b.record_cycle(0, 0));
        }
        assert!(!b.is_paused());
        assert_eq!(b.consecutive_total_failures(), 0);
    }

    fn unit(slug: &str, status: &str) -> ParsedWorkUnit {
        unit_with_deps(slug, status, vec![])
    }

    fn unit_with_deps(slug: &str, status: &str, depends_on: Vec<String>) -> ParsedWorkUnit {
        ParsedWorkUnit {
            slug: slug.to_string(),
            title: None,
            status: status.to_string(),
            depends_on,
            phases: vec![],
            source_path: format!("plans/{slug}.md"),
            content: String::new(),
        }
    }

    /// How the fake sink should answer `set_deps`.
    #[derive(Clone, Copy, Default)]
    enum DepsBehavior {
        #[default]
        Ok,
        TableNotMigrated,
        Error,
        Forbidden,
    }

    #[derive(Default)]
    struct FakeSink {
        statuses: Mutex<HashMap<String, String>>,
        transitions: Mutex<u64>,
        /// Configured `by_actor` of every unit's latest history row (default
        /// None ⇒ no history ⇒ no owner to defer to).
        last_actor: Option<String>,
        deps_behavior: DepsBehavior,
        /// Slug whose `current_status` read should hard-error, so the backfill's
        /// per-unit failure path is reachable without live HTTP.
        fail_status_read_for: Option<String>,
        /// Slug whose `upsert` should hard-error, so the OTHER failure branch
        /// (an `Err` out of `push_work_unit` itself) is reachable too.
        fail_upsert_for: Option<String>,
        /// Statuses the sink silently REWRITES on store, modelling coord's own
        /// normalisation. Idempotence must survive a backend that does not echo
        /// what it was handed.
        normalize: Option<(String, String)>,
        /// When true, EVERY `current_status` read hard-errors regardless of
        /// slug — the UNKNOWN branch the cold-start seed must abstain on
        /// rather than fall through to an overwrite. Distinct from
        /// `fail_status_read_for`, which targets one slug for the backfill's
        /// per-unit failure path.
        fail_current_status: bool,
        deps_calls: Mutex<Vec<(String, Vec<String>)>>,
        /// When set, every `upsert` answers with a coord `403` — the shape that
        /// used to be retried, and re-logged, once per slug per cycle forever.
        upsert_forbidden: bool,
        /// When set, every `upsert` answers with an ordinary (retryable) error.
        upsert_errors: bool,
        /// When set, every `upsert` fails with the WRITE-shaped rejection
        /// (`CoordWriteError`, carrying coord's status and body) rather than the
        /// READ-shaped `ForbiddenByCoord`. Both shapes reach the same `Err` arm
        /// and a `403` must retire the slug from EITHER.
        upsert_write_status: Option<u16>,
        /// Total `upsert` calls received, so a test can prove a retired slug
        /// stops making the HTTP call at all — not merely stops logging.
        upsert_calls: Mutex<u64>,
        /// Every upsert body seen, so the archive scan can be asserted to write
        /// `metadata.archive_path` with no status.
        upserts: Mutex<Vec<UpsertBody>>,
        /// When set, the sink HAS a bulk seed door and answers it with this map.
        /// `None` (the default) models a sink with no bulk door, so the trait's
        /// defaulted `Ok(None)` and the per-slug fallback stay the tested norm.
        bulk: Option<HashMap<String, String>>,
        /// Total `current_status` reads served, so a test can prove the bulk
        /// prime REMOVED the per-slug round-trips rather than merely duplicating
        /// them.
        current_status_calls: Mutex<u64>,
        /// Total `list_statuses` reads served, so "attempted once per corpus"
        /// is asserted rather than assumed.
        list_statuses_calls: Mutex<u64>,
    }
    #[async_trait::async_trait]
    impl WorkUnitSink for FakeSink {
        async fn current_status(&self, slug: &str) -> Result<Option<String>> {
            *self.current_status_calls.lock().unwrap() += 1;
            if self.fail_current_status {
                anyhow::bail!("simulated current_status failure");
            }
            if self.fail_status_read_for.as_deref() == Some(slug) {
                anyhow::bail!("simulated work-unit status read failure");
            }
            Ok(self.statuses.lock().unwrap().get(slug).cloned())
        }
        async fn list_statuses(&self) -> Result<Option<HashMap<String, String>>> {
            *self.list_statuses_calls.lock().unwrap() += 1;
            Ok(self.bulk.clone())
        }
        async fn last_actor(&self, _slug: &str) -> Result<Option<String>> {
            Ok(self.last_actor.clone())
        }
        async fn upsert(&self, body: &UpsertBody) -> Result<()> {
            *self.upsert_calls.lock().unwrap() += 1;
            if self.fail_upsert_for.as_deref() == Some(body.slug.as_str()) {
                anyhow::bail!("simulated work-unit upsert failure");
            }
            if self.upsert_forbidden {
                return Err(anyhow::Error::new(
                    crate::plan_workunit_adapter::push::ForbiddenByCoord {
                        route: "POST /coord/work-units/upsert",
                        detail: r#"{"error":"self_attestation_forbidden"}"#.to_string(),
                    },
                ));
            }
            if let Some(status) = self.upsert_write_status {
                return Err(crate::plan_workunit_adapter::push::CoordWriteError {
                    op: "upsert",
                    slug: body.slug.clone(),
                    status: Some(status),
                    body: r#"{"error":"self_attestation_forbidden"}"#.to_string(),
                }
                .into());
            }
            if self.upsert_errors {
                anyhow::bail!("simulated transient upsert failure");
            }
            if let Some(s) = &body.status {
                let stored = match &self.normalize {
                    Some((from, to)) if from == s => to.clone(),
                    _ => s.clone(),
                };
                self.statuses
                    .lock()
                    .unwrap()
                    .insert(body.slug.clone(), stored);
            }
            self.upserts.lock().unwrap().push(body.clone());
            Ok(())
        }
        async fn transition(&self, slug: &str, body: &TransitionBody) -> Result<()> {
            *self.transitions.lock().unwrap() += 1;
            self.statuses
                .lock()
                .unwrap()
                .insert(slug.to_string(), body.to_status.clone());
            Ok(())
        }
        async fn set_deps(&self, slug: &str, depends_on: &[String]) -> Result<SetDepsOutcome> {
            self.deps_calls
                .lock()
                .unwrap()
                .push((slug.to_string(), depends_on.to_vec()));
            match self.deps_behavior {
                DepsBehavior::Ok => Ok(SetDepsOutcome::Ok {
                    edges_set: depends_on.len() as u64,
                }),
                DepsBehavior::TableNotMigrated => Ok(SetDepsOutcome::TableNotMigrated),
                DepsBehavior::Error => anyhow::bail!("simulated deps endpoint failure"),
                DepsBehavior::Forbidden => Err(anyhow::Error::new(
                    crate::plan_workunit_adapter::push::ForbiddenByCoord {
                        route: "POST /coord/work-units/:slug/deps",
                        detail: r#"{"error":"self_attestation_forbidden"}"#.to_string(),
                    },
                )),
            }
        }
    }

    #[tokio::test]
    async fn reconcile_is_idempotent_across_cycles() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let units = vec![unit("a", "vetted"), unit("b", "draft")];

        // First cycle: both created, no transitions.
        let s1 = reconcile_once(
            &units,
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s1.scanned, 2);
        assert_eq!(s1.transitions, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        // Second cycle, unchanged corpus: NO phantom transitions.
        let s2 = reconcile_once(
            &units,
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s2.scanned, 2);
        assert_eq!(s2.transitions, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn reconcile_emits_one_transition_on_status_edge() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        // Plan edited: vetted -> shipped.
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.transitions, 1);
        assert_eq!(*sink.transitions.lock().unwrap(), 1);
        assert_eq!(metrics.snapshot().transitions_total, 1);
    }

    // --- Cold-start seeding: a runner restart must not re-create coord's rows -
    //
    // `last_applied` is per-PROCESS. Before seeding, the first cycle after every
    // runner start saw `None` for every slug and emitted `UpsertWithStatus` with
    // the FILE's status — bypassing the ownership deferral (Transition-only) and
    // the conflict check (inside `if let Some(prev)`), and landing on coord's
    // `COALESCE($3, status)` which overwrites on a non-NULL `$3`.

    #[tokio::test]
    async fn cold_start_does_not_demote_a_unit_a_real_agent_owns() {
        // coord says `shipped`; the plan file still says `in_progress` (the
        // single commonest divergence on this fleet: 48 of 488 units).
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "shipped".to_string());
        let metrics = AdapterMetrics::default();
        // COLD: exactly the state `run_loop` builds on every runner start.
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("a", "in_progress")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        // Assert the DEMOTION first, so a regression fails on the behaviour
        // this test exists for rather than on the mechanism that prevents it.
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("shipped"),
            "coord's terminal status must survive a cold start (without seeding \
             this reads `in_progress` — the measured 2026-09-01 demotion)"
        );
        assert_eq!(s.transitions, 0, "a real agent owns it -> DEFER");
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(s.seeded, 1, "the remote status must be seeded");
        assert_eq!(
            *sink.current_status_calls.lock().unwrap(),
            1,
            "the seed read is handed to the push as `known_remote`, so the \
             deferral's convergence check must NOT pay a second GET"
        );
        // And no upsert may carry a status: that is the overwrite vector.
        assert!(
            sink.upserts
                .lock()
                .unwrap()
                .iter()
                .all(|b| b.status.is_none()),
            "no upsert may carry a status for a unit coord already has"
        );
    }

    #[tokio::test]
    async fn cold_start_with_matching_status_refreshes_without_a_transition() {
        let sink = FakeSink::default();
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "vetted".to_string());
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        assert_eq!(s.seeded, 1);
        assert_eq!(s.transitions, 0, "status unchanged -> RefreshOnly");
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("vetted")
        );
    }

    #[tokio::test]
    async fn cold_start_still_creates_a_slug_coord_has_never_seen() {
        // The seed must not break the legitimate create path.
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("new", "draft")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        assert_eq!(s.seeded, 0, "nothing to seed from — genuinely absent");
        assert_eq!(s.seed_errors, 0);
        assert_eq!(
            sink.statuses.lock().unwrap().get("new").map(String::as_str),
            Some("draft"),
            "a brand-new unit is still created WITH its status"
        );
    }

    #[tokio::test]
    async fn seed_read_failure_abstains_rather_than_overwriting() {
        // UNKNOWN must not degrade to "no prior status", which is precisely the
        // fall-through that produced the demotions.
        let sink = FakeSink {
            fail_current_status: true,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("a", "in_progress")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        assert_eq!(s.seed_errors, 1);
        assert_eq!(s.seeded, 0);
        assert_eq!(s.errors, 0, "an abstention is not a push error");
        assert!(
            sink.upserts.lock().unwrap().is_empty(),
            "an unreadable remote status must produce NO write at all"
        );
        assert_eq!(metrics.snapshot().seed_errors_total, 1);
        // The unit is not remembered, so the next cycle retries it.
        assert!(!mem.contains_key("a"));
    }

    #[tokio::test]
    async fn bulk_seed_primes_last_applied_and_removes_the_per_slug_reads() {
        // What `run_loop`'s bulk prime does, asserted at the reconcile level:
        // a pre-primed `last_applied` means reconcile_once issues NO per-slug
        // seed at all.
        //
        // `last_actor` is set because priming ALONE does not protect anything —
        // it restores the edge-trigger, and the OWNERSHIP DEFERRAL is what then
        // withholds the transition. A unit with no history legitimately
        // transitions (see `proceeds_when_no_history`), so a default sink here
        // would demote and would be asserting the wrong thing. Every real coord
        // row this adapter has never pushed has a non-adapter actor.
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "shipped".to_string());
        let metrics = AdapterMetrics::default();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();
        // Primed exactly as run_loop primes it, from the bulk read.
        let mut mem: HashMap<String, String> = [("a".to_string(), "shipped".to_string())]
            .into_iter()
            .collect();

        let s = reconcile_once(
            &[unit("a", "in_progress")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        assert_eq!(s.seeded, 0, "the bulk prime already covered this slug");
        assert_eq!(
            *sink.current_status_calls.lock().unwrap(),
            1,
            "ZERO seed reads — the one read is the deferral's convergence check \
             inside push_work_unit, not a seed"
        );
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("shipped"),
            "priming must protect the terminal status just as the per-slug seed does"
        );
    }

    /// A sink that does NOT implement `list_statuses` at all must inherit
    /// `Ok(None)` from the trait — "I have no bulk door" — and never `Ok(Some(
    /// empty))`, which would read as "coord has no units" and seed nothing
    /// while claiming a successful read.
    ///
    /// `NoBulkSink` exists because `FakeSink` DOES override `list_statuses` (it
    /// counts the calls), so asserting against `FakeSink::default()` would only
    /// prove that its own `bulk` field defaults to `None` — deleting the trait
    /// default entirely would not fail it. This sink implements every
    /// REQUIRED method and nothing else, so the assertion below reaches the
    /// default body or it does not compile.
    #[derive(Default)]
    struct NoBulkSink;

    #[async_trait::async_trait]
    impl WorkUnitSink for NoBulkSink {
        async fn current_status(&self, _slug: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn last_actor(&self, _slug: &str) -> Result<Option<String>> {
            Ok(None)
        }
        async fn upsert(&self, _body: &UpsertBody) -> Result<()> {
            Ok(())
        }
        async fn transition(&self, _slug: &str, _body: &TransitionBody) -> Result<()> {
            Ok(())
        }
        async fn set_deps(&self, _slug: &str, _depends_on: &[String]) -> Result<SetDepsOutcome> {
            Ok(SetDepsOutcome::Ok { edges_set: 0 })
        }
    }

    #[tokio::test]
    async fn a_sink_without_a_bulk_door_falls_back_to_the_per_slug_seed() {
        assert!(
            NoBulkSink.list_statuses().await.unwrap().is_none(),
            "the trait's DEFAULT bulk door must be None, not an empty map — an \
             empty map would read as 'coord has no units' and seed nothing"
        );
    }

    #[tokio::test]
    async fn seeding_is_a_first_cycle_cost_only() {
        // Steady state must not pay a current_status read per unit per cycle.
        let sink = FakeSink::default();
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "vetted".to_string());
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let units = vec![unit("a", "vetted")];

        let s1 = reconcile_once(
            &units,
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        let s2 = reconcile_once(
            &units,
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;

        assert_eq!(s1.seeded, 1, "seeded on the cold cycle");
        assert_eq!(s2.seeded, 0, "in-process memory serves the second cycle");
    }

    // --- Graduation-bootstrap P2a: markdown proxy defers to real agents ------

    #[tokio::test]
    async fn defers_transition_when_real_agent_owns_unit() {
        // A real agent last drove the unit (its own agent-scoped actor). The
        // file's status edge (vetted -> shipped) WOULD transition, but the proxy
        // must DEFER so it doesn't collapse the agent's transition to the system
        // actor: ZERO transitions emitted.
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        // Establish last-applied=vetted (create; UpsertWithStatus is never gated).
        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        // File edited vetted -> shipped: transition WOULD fire, but defer.
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.transitions, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(metrics.snapshot().transitions_total, 0);
        // The deferral is COUNTED, not silently folded into the refresh tally.
        assert_eq!(s.deferred, 1);
        assert_eq!(metrics.snapshot().deferrals_total, 1);

        // …and a PERSISTENT deferral is counted every cycle, not once. Recording
        // the un-applied status in `last_applied` would make the next cycle
        // answer `RefreshOnly`, so `deferred` would read 0 from here on —
        // indistinguishable from "the divergence went away".
        let s3 = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s3.deferred, 1, "a standing deferral stays visible");
        assert_eq!(s3.transitions, 0);
        assert_eq!(
            s3.conflicts, 0,
            "and it never degrades into a permanent bogus conflict warning"
        );
        assert_eq!(metrics.snapshot().deferrals_total, 2);
    }

    #[tokio::test]
    async fn proceeds_when_last_actor_is_adapter() {
        // The adapter itself last drove the unit ⇒ no real agent owns it ⇒ the
        // proxy proceeds with its transition as normal.
        let sink = FakeSink {
            last_actor: Some(ADAPTER_ACTOR.to_string()),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.transitions, 1);
        assert_eq!(*sink.transitions.lock().unwrap(), 1);
        assert_eq!(metrics.snapshot().transitions_total, 1);
    }

    #[tokio::test]
    async fn proceeds_when_no_history() {
        // No history (last_actor = None) ⇒ nobody owns the unit ⇒ proceed.
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        let s = reconcile_once(
            &[unit("a", "shipped")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.transitions, 1);
        assert_eq!(*sink.transitions.lock().unwrap(), 1);
        assert_eq!(metrics.snapshot().transitions_total, 1);
    }

    #[tokio::test]
    async fn unit_with_deps_pushes_dep_edges_with_right_args() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string(), "p2".to_string()]);

        let s = reconcile_once(
            &[u],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.deps_set, 1);
        assert_eq!(s.errors, 0);
        let calls = sink.deps_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "p4");
        assert_eq!(calls[0].1, vec!["p1".to_string(), "p2".to_string()]);
        assert_eq!(metrics.snapshot().deps_set_total, 1);
    }

    #[tokio::test]
    async fn empty_deps_unit_makes_no_dep_call_and_no_error() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s = reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s.deps_set, 0);
        assert_eq!(s.deps_errors, 0);
        assert_eq!(s.errors, 0);
        assert!(sink.deps_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn dep_edges_are_edge_triggered_across_cycles() {
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        // First cycle sends deps.
        reconcile_once(
            std::slice::from_ref(&u),
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        // Second cycle, unchanged dep set: no re-send (idempotent edge-trigger).
        let s2 = reconcile_once(
            &[u],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s2.deps_set, 0);
        assert_eq!(sink.deps_calls.lock().unwrap().len(), 1);

        // Dep set changed -> re-send.
        let u2 = unit_with_deps("p4", "vetted", vec!["p1".to_string(), "p3".to_string()]);
        let s3 = reconcile_once(
            &[u2],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s3.deps_set, 1);
        assert_eq!(sink.deps_calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn table_not_migrated_does_not_fail_reconcile_and_retries() {
        let sink = FakeSink {
            deps_behavior: DepsBehavior::TableNotMigrated,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        let s = reconcile_once(
            std::slice::from_ref(&u),
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        // 503 is benign: no reconcile error, the unit upsert still succeeded.
        assert_eq!(s.errors, 0);
        assert_eq!(s.deps_errors, 0);
        assert_eq!(s.deps_set, 0);
        assert_eq!(s.deps_skipped_unmigrated, 1);
        assert_eq!(metrics.snapshot().deps_skipped_unmigrated_total, 1);

        // last_deps NOT cached on 503 -> next cycle retries the edge write.
        let s2 = reconcile_once(
            &[u],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s2.deps_skipped_unmigrated, 1);
        assert_eq!(sink.deps_calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn dep_edge_hard_error_is_non_fatal() {
        let sink = FakeSink {
            deps_behavior: DepsBehavior::Error,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        let s = reconcile_once(
            &[u],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        // A dep-edge failure does NOT fail the reconcile (unit upsert landed).
        assert_eq!(s.errors, 0);
        assert_eq!(s.deps_errors, 1);
        assert_eq!(s.deps_set, 0);
        assert_eq!(metrics.snapshot().deps_errors_total, 1);
    }

    /// RT7: a coord `403` retires the slug. Before this, the adapter re-issued
    /// the identical refused request — and emitted a full WARN — once per slug
    /// per cycle, forever; at ~343 plans on a ~68s cycle that was the single
    /// largest consumer of the runner's log budget.
    ///
    /// The assertion that makes this a fix rather than a mute is
    /// `upsert_calls == 1`: the retired slug stops making the HTTP call, not
    /// just the log line. `forbidden_total` incrementing exactly once is the
    /// "<= 1 log line per slug per process" property, since the warn sits on
    /// the same branch as that increment.
    #[tokio::test]
    async fn a_403_retires_the_slug_for_the_life_of_the_process() {
        let sink = FakeSink {
            upsert_forbidden: true,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        let s1 = reconcile_once(
            &[unit("a", "vetted")],
            &mut mem,
            &mut deps,
            &mut forb,
            &mut forb_deps,
            &sink,
            &metrics,
        )
        .await;
        assert_eq!(s1.forbidden, 1);
        assert_eq!(
            s1.errors, 0,
            "a permission verdict is not a retryable error"
        );

        for cycle in 0..5 {
            let s = reconcile_once(
                &[unit("a", "vetted")],
                &mut mem,
                &mut deps,
                &mut forb,
                &mut forb_deps,
                &sink,
                &metrics,
            )
            .await;
            assert_eq!(s.forbidden, 1, "cycle {cycle} still counts the skip");
            assert_eq!(s.errors, 0, "cycle {cycle} must not re-error");
        }

        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "the refused slug must be asked exactly once, not once per cycle"
        );
        assert_eq!(
            metrics.snapshot().forbidden_total,
            1,
            "one increment per refused slug — and so one WARN per slug per process"
        );
        assert_eq!(metrics.snapshot().errors_total, 0);
    }

    /// A `403` on a WRITE retires the slug exactly like a `403` on a read.
    ///
    /// The two carry the verdict in different types — reads in
    /// `ForbiddenByCoord`, writes in `CoordWriteError` — and this arm reads
    /// both. Honouring only the read shape would leave the write half of the
    /// retry storm running while every test above still passed.
    #[tokio::test]
    async fn a_write_shaped_403_retires_the_slug_too() {
        let sink = FakeSink {
            upsert_write_status: Some(403),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        for cycle in 0..3 {
            let s = reconcile_once(
                &[unit("a", "vetted")],
                &mut mem,
                &mut deps,
                &mut forb,
                &mut forb_deps,
                &sink,
                &metrics,
            )
            .await;
            assert_eq!(s.forbidden, 1, "cycle {cycle}");
            assert_eq!(s.errors, 0, "cycle {cycle}");
        }
        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "the refused slug must be asked exactly once, not once per cycle"
        );
        assert_eq!(metrics.snapshot().forbidden_total, 1);
    }

    /// ...and the retirement stays narrow on the write shape too: a `422` is a
    /// structural refusal the classifier reports, NOT a permission verdict, so
    /// it must keep retrying rather than freeze the unit.
    #[tokio::test]
    async fn a_write_shaped_422_is_not_retired() {
        let sink = FakeSink {
            upsert_write_status: Some(422),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        for _ in 0..3 {
            let s = reconcile_once(
                &[unit("a", "vetted")],
                &mut mem,
                &mut deps,
                &mut forb,
                &mut forb_deps,
                &sink,
                &metrics,
            )
            .await;
            assert_eq!(s.errors, 1);
            assert_eq!(s.forbidden, 0);
        }
        assert_eq!(*sink.upsert_calls.lock().unwrap(), 3);
        assert!(forb.is_empty());
    }

    /// The retirement is narrow: an ORDINARY failure still retries every cycle.
    /// Widening it would turn a coord blip into a silently frozen work-unit
    /// layer, which is the failure the retry loop exists to prevent.
    #[tokio::test]
    async fn a_non_403_failure_is_still_retried_every_cycle() {
        let sink = FakeSink {
            upsert_errors: true,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();

        for _ in 0..3 {
            let s = reconcile_once(
                &[unit("a", "vetted")],
                &mut mem,
                &mut deps,
                &mut forb,
                &mut forb_deps,
                &sink,
                &metrics,
            )
            .await;
            assert_eq!(s.errors, 1);
            assert_eq!(s.forbidden, 0);
        }
        assert_eq!(*sink.upsert_calls.lock().unwrap(), 3);
        assert!(
            forb.is_empty(),
            "a transient failure must not retire a slug"
        );
    }

    /// A 403 on the dep-edge route ALONE must retire only the edge push, not
    /// the whole unit: coord evaluates `POST .../upsert` and
    /// `POST .../:slug/deps` as separate authorization checks (the deps route
    /// mutates a different table), so a unit can be permitted on one and
    /// refused on the other. Before this, the deps error path had no 403
    /// classification at all, so a refused edge push would re-issue the
    /// identical request — and re-WARN — every cycle forever, reproducing the
    /// exact log-flood shape the unit-level fix (`a_403_retires_the_slug...`)
    /// closed for the upsert/transition route.
    #[tokio::test]
    async fn a_403_on_deps_retires_only_the_edge_push() {
        let sink = FakeSink {
            deps_behavior: DepsBehavior::Forbidden,
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut mem = HashMap::new();
        let mut deps = HashMap::new();
        let mut forb: HashSet<String> = HashSet::new();
        let mut forb_deps: HashSet<String> = HashSet::new();
        let u = unit_with_deps("p4", "vetted", vec!["p1".to_string()]);

        for cycle in 0..5 {
            let s = reconcile_once(
                std::slice::from_ref(&u),
                &mut mem,
                &mut deps,
                &mut forb,
                &mut forb_deps,
                &sink,
                &metrics,
            )
            .await;
            assert_eq!(s.deps_forbidden, 1, "cycle {cycle} still counts the skip");
            assert_eq!(s.deps_errors, 0, "cycle {cycle} must not re-error");
            // The unit's own upsert route is unaffected by the deps-route
            // refusal: no `forbidden` (unit-level) skip.
            assert_eq!(
                s.forbidden, 0,
                "cycle {cycle} must not retire the unit itself"
            );
        }

        assert_eq!(
            sink.deps_calls.lock().unwrap().len(),
            1,
            "the refused edge push must be asked exactly once, not once per cycle"
        );
        // Unlike the edge-triggered deps/transition routes, `upsert` is NOT
        // gated on a change (see `UpsertWithStatus is never gated` elsewhere in
        // this file) — it runs every cycle regardless. The point of this
        // assertion is not "once": it is "still 5, not fewer" — proving the
        // deps-route refusal above did not also, incorrectly, retire or skip
        // the unit's own upsert route.
        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            5,
            "the unit upsert is unaffected by the deps-route refusal and keeps running every cycle"
        );
        assert_eq!(
            metrics.snapshot().deps_forbidden_total,
            1,
            "one increment per refused slug — and so one WARN per slug per process"
        );
        assert_eq!(metrics.snapshot().deps_errors_total, 0);
        assert_eq!(metrics.snapshot().forbidden_total, 0);
        assert!(
            forb.is_empty(),
            "only the deps route is retired, not the unit"
        );
    }

    #[test]
    fn metrics_snapshot_reads_counters() {
        let m = AdapterMetrics::default();
        m.scanned.store(5, Ordering::Relaxed);
        m.transitions_total.store(2, Ordering::Relaxed);
        let snap = m.snapshot();
        assert_eq!(snap.scanned, 5);
        assert_eq!(snap.transitions_total, 2);
    }

    // ---- Phase 4: metadata-only archive scan (D4) ----

    /// Write `body` to `<dir>/<slug>.md` and return the full path string.
    fn write_plan(dir: &Path, slug: &str, body: &str) -> String {
        let path = dir.join(format!("{slug}.md"));
        std::fs::write(&path, body).unwrap();
        path.to_string_lossy().to_string()
    }

    // ---- `source_path` is repo-relative, never the authoring machine's ----

    /// THE REGRESSION TEST. A scan run from an ABSOLUTE root must record a
    /// repo-relative `source_path`. Coord serves this field to every machine
    /// in the fleet as the only pointer from a work unit back to its plan
    /// document, so an authoring-machine absolute path resolves nowhere else
    /// — and resolving nowhere is indistinguishable from the plan not
    /// existing. Measured 2026-09-06: 1177 of 2648 served rows were in that
    /// state.
    ///
    /// The planted `.git` is what makes the expectation deterministic: it
    /// pins which ancestor `derive_source_repo` stops at, so the temp dir's
    /// random name can never leak into the recorded path.
    #[test]
    fn read_plan_dir_records_a_repo_relative_source_path() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let plans = repo.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        write_plan(&plans, "s", "# S\n\n> **Status:** draft\n");

        // The loop hands `read_plan_dir` an absolute dir; reproduce that.
        assert!(
            plans.is_absolute(),
            "the scan root under test must be absolute"
        );

        let scanned = read_plan_dir(&plans, &PlanConvention::operator_default());
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].source_path, "myrepo/plans/s.md");
        assert_eq!(scanned[0].slug, "s");
    }

    /// The no-`.git` arm — the shape `D:\qontinui-root\plans` has on the
    /// operator box, where the workspace root is not a repository.
    /// `derive_source_repo` falls back to the last two components, so the
    /// recorded path is still relative and still names its scan root.
    #[test]
    fn read_plan_dir_source_path_is_relative_without_a_git_root() {
        let tmp = tempfile::tempdir().unwrap();
        // Premise: the temp dir is not itself inside a git work tree. Assert
        // it, so a machine where that is false says so instead of failing on
        // an expectation that was never the point.
        assert!(
            !tmp.path().ancestors().any(|a| a.join(".git").exists()),
            "temp dir is inside a git work tree; this test's premise does not hold"
        );
        let plans = tmp.path().join("qontinui-root").join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        write_plan(&plans, "s", "# S\n");

        let scanned = read_plan_dir(&plans, &PlanConvention::operator_default());
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].source_path, "qontinui-root/plans/s.md");
    }

    /// Every scanned entry gets the SAME root prefix and its OWN file name —
    /// the cardinality arm the single-file tests above cannot reach.
    #[test]
    fn read_plan_dir_relative_source_path_holds_for_many_files() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("myrepo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let plans = repo.join("plans");
        std::fs::create_dir_all(&plans).unwrap();
        write_plan(&plans, "a", "# A\n");
        write_plan(&plans, "b", "# B\n");
        write_plan(&plans, "c", "# C\n");

        let mut got: Vec<String> = read_plan_dir(&plans, &PlanConvention::operator_default())
            .into_iter()
            .map(|u| u.source_path)
            .collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                "myrepo/plans/a.md".to_string(),
                "myrepo/plans/b.md".to_string(),
                "myrepo/plans/c.md".to_string(),
            ]
        );
    }

    /// The load-bearing D4 test: an archive scan of real `*.md` files — one
    /// whose `> **Status:` says the coord-derived `shipped`, one whose status is
    /// the non-vocabulary `archived` (which coord silently classifies `Free` and
    /// would ACCEPT) — produces ZERO status transitions, only a
    /// `metadata.archive_path` stamp per slug pointing at the archived file.
    #[tokio::test]
    async fn archive_scan_stamps_path_and_never_transitions() {
        let tmp = tempfile::tempdir().unwrap();
        // A planted `.git` pins which ancestor `derive_source_repo` stops at,
        // so `archive_path` below is a LITERAL rather than a re-derivation of
        // the temp dir's random name.
        let repo = tmp.path().join("myrepo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let archive = repo.join("archive");
        std::fs::create_dir_all(&archive).unwrap();
        write_plan(
            &archive,
            "2026-01-01-shipped-plan",
            "# Shipped Plan\n\n> **Status:** shipped 2026-01-01.\n",
        );
        write_plan(
            &archive,
            "2026-01-02-archived-plan",
            "# Archived Plan\n\n> **Status:** archived\n",
        );

        // Reuse the production scan path — its missing-dir-yields-empty-vec
        // behavior is exactly the right unset semantics.
        let conv = PlanConvention::operator_default();
        let scanned = read_plan_dir(&archive, &conv);
        assert_eq!(scanned.len(), 2);

        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let summary = reconcile_archive_once(&scanned, &sink, &metrics).await;

        // ZERO transitions from ANY archive-scanned entry — the only D4 guard,
        // since coord will not reject either `shipped` or `archived` here.
        assert_eq!(
            *sink.transitions.lock().unwrap(),
            0,
            "archive scan must NEVER emit a status transition"
        );
        assert_eq!(summary.stamped, 2);
        assert_eq!(summary.errors, 0);
        assert_eq!(metrics.snapshot().archive_stamped_total, 2);
        assert_eq!(metrics.snapshot().transitions_total, 0);

        // Exactly one metadata-only upsert per slug: no status, archive_path set
        // to the archived file's path.
        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), 2);
        for up in ups.iter() {
            assert!(up.status.is_none(), "archive upsert carries no status");
        }
        let by_slug: HashMap<&str, &UpsertBody> =
            ups.iter().map(|u| (u.slug.as_str(), u)).collect();
        // The archive writer carries the slug-derived authoring date too, so
        // an archived-only plan is dated (harmless under coord's COALESCE).
        assert_eq!(
            by_slug["2026-01-01-shipped-plan"].authored_at.as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            by_slug["2026-01-02-archived-plan"].authored_at.as_deref(),
            Some("2026-01-02T00:00:00Z")
        );
        // `archive_path` is the same string as `source_path` under another
        // name, so it is repo-relative too. Until 2026-09-06 this assertion
        // compared against the ABSOLUTE path `write_plan` returned — a test
        // pinning the defect, which reddened the moment the defect was fixed
        // [policy: a-test-must-be-able-to-fail, shape 2].
        assert_eq!(
            by_slug["2026-01-01-shipped-plan"]
                .metadata
                .as_ref()
                .unwrap()["archive_path"],
            serde_json::json!("myrepo/archive/2026-01-01-shipped-plan.md")
        );
        assert_eq!(
            by_slug["2026-01-02-archived-plan"]
                .metadata
                .as_ref()
                .unwrap()["archive_path"],
            serde_json::json!("myrepo/archive/2026-01-02-archived-plan.md")
        );
    }

    /// A missing/unset archive dir yields an empty scan (no writes) — the same
    /// unset semantics as the active dir.
    #[tokio::test]
    async fn archive_scan_of_missing_dir_is_empty_noop() {
        let conv = PlanConvention::operator_default();
        let scanned = read_plan_dir(Path::new("/definitely/not/a/dir/xyz"), &conv);
        assert!(scanned.is_empty());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let summary = reconcile_archive_once(&scanned, &sink, &metrics).await;
        assert_eq!(summary.scanned, 0);
        assert_eq!(summary.stamped, 0);
        assert!(sink.upserts.lock().unwrap().is_empty());
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
    }

    /// Disappeared-slug rule (D4): a slug we applied that is gone from the active
    /// dir AND absent from the archive dir is surfaced ONCE per process, and the
    /// detection never transitions (it only warns — no sink call at all).
    #[test]
    fn disappeared_slug_warns_once_and_never_transitions() {
        let mut known: HashMap<String, String> = HashMap::new();
        known.insert("a".to_string(), "in_progress".to_string());
        known.insert("b".to_string(), "vetted".to_string());
        known.insert("c".to_string(), "shipped".to_string());

        // `a` still active, `b` moved to archive, `c` vanished from both.
        let active: HashSet<String> = ["a".to_string()].into_iter().collect();
        let archive: HashSet<String> = ["b".to_string()].into_iter().collect();
        let mut warned: HashSet<String> = HashSet::new();

        let first = newly_disappeared_slugs(&known, &active, &archive, &mut warned);
        assert_eq!(first, vec!["c".to_string()], "only c disappeared");

        // Warn-once: a second identical scan yields nothing new.
        let second = newly_disappeared_slugs(&known, &active, &archive, &mut warned);
        assert!(
            second.is_empty(),
            "a disappeared slug is warned at most once per process"
        );
    }

    // ---- one-shot work-unit backfill (`qontinui-pr plan-workunit-backfill`) --

    /// The catch-up path's core promise: a corpus coord has never seen is
    /// CREATED, and running the same backfill again writes no status and emits
    /// no transition. A backfill that started from an empty last-applied memory
    /// (the naive shape) would take the `UpsertWithStatus` arm on every run and
    /// re-stamp a status every time — this pins the seeded-from-coord behaviour
    /// that makes it idempotent.
    ///
    /// Neuter check: seed `push_work_unit` with `None` instead of
    /// `sink.current_status(...)` in `backfill_work_units_once` and the second
    /// run's assertions fail.
    #[tokio::test]
    async fn backfill_creates_missing_units_then_is_idempotent() {
        let sink = FakeSink::default();
        let units = [unit("a", "draft"), unit("b", "in_progress")];

        let first = backfill_work_units_once(&units, &sink).await;
        assert_eq!(first.scanned, 2);
        assert_eq!(first.created, 2);
        assert_eq!(first.refreshed, 0);
        assert_eq!(first.transitioned, 0);
        assert_eq!(first.deferred, 0);
        assert_eq!(first.failed, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("draft")
        );

        let upserts_after_first = sink.upserts.lock().unwrap().len();

        // Re-run over the unchanged corpus.
        let second = backfill_work_units_once(&units, &sink).await;
        assert_eq!(second.created, 0, "nothing is created twice");
        assert_eq!(second.refreshed, 2);
        assert_eq!(second.transitioned, 0);
        assert_eq!(second.failed, 0);
        assert_eq!(
            *sink.transitions.lock().unwrap(),
            0,
            "an unchanged corpus emits NO transition on re-run"
        );
        let ups = sink.upserts.lock().unwrap();
        assert_eq!(ups.len(), upserts_after_first + 2);
        for u in &ups[upserts_after_first..] {
            assert!(
                u.status.is_none(),
                "the idempotent re-run refreshes metadata only, never a status"
            );
        }
    }

    /// The agent-owner deferral (graduation-bootstrap P2a) must survive the
    /// backfill path — it is what stops a bulk catch-up clobbering a status an
    /// agent set, and it is the direction Phase 3's coord -> body reconcile
    /// depends on. Reachable here ONLY because the seed makes the unit take the
    /// `Transition` arm; a `None` seed would route it through
    /// `UpsertWithStatus`, which the deferral never gates.
    #[tokio::test]
    async fn backfill_defers_when_a_real_agent_owns_the_unit() {
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        // coord already holds `shipped` (an agent drove it there); the stale
        // body on disk still says `in_progress`.
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "shipped".to_string());

        let s = backfill_work_units_once(&[unit("a", "in_progress")], &sink).await;
        assert_eq!(s.deferred, 1);
        assert_eq!(s.transitioned, 0);
        assert_eq!(s.created, 0);
        assert_eq!(s.failed, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("shipped"),
            "the agent-set status is left exactly as it was"
        );
    }

    /// The other side of the deferral: when NO real agent owns the unit (no
    /// history), a genuine disk/coord divergence is still corrected — the
    /// deferral narrows the backfill, it does not disable it.
    #[tokio::test]
    async fn backfill_transitions_an_unowned_diverged_unit() {
        let sink = FakeSink::default();
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "draft".to_string());

        let s = backfill_work_units_once(&[unit("a", "vetted")], &sink).await;
        assert_eq!(s.transitioned, 1);
        assert_eq!(s.deferred, 0);
        assert_eq!(*sink.transitions.lock().unwrap(), 1);
        assert_eq!(
            sink.statuses.lock().unwrap().get("a").map(String::as_str),
            Some("vetted")
        );
    }

    /// A per-unit failure is counted and the pass continues — a one-shot
    /// catch-up over ~1,400 files must not abort on one bad row.
    #[tokio::test]
    async fn backfill_counts_a_failed_unit_and_keeps_going() {
        let sink = FakeSink {
            fail_status_read_for: Some("bad".to_string()),
            ..Default::default()
        };
        let s =
            backfill_work_units_once(&[unit("bad", "draft"), unit("good", "draft")], &sink).await;
        assert_eq!(s.scanned, 2);
        assert_eq!(s.failed, 1);
        assert_eq!(s.created, 1, "the second unit still landed");
    }

    /// The other failure branch: the seed read succeeds and the WRITE fails.
    #[tokio::test]
    async fn backfill_counts_a_failed_push_and_keeps_going() {
        let sink = FakeSink {
            fail_upsert_for: Some("bad".to_string()),
            ..Default::default()
        };
        let s =
            backfill_work_units_once(&[unit("bad", "draft"), unit("good", "draft")], &sink).await;
        assert_eq!(s.failed, 1);
        assert_eq!(s.created, 1);
    }

    /// Idempotence must not rest on the backend echoing the status it was
    /// handed. Coord classifies and can normalise (`push_work_unit`'s own docs
    /// note `archived` lands as Free and `shipped` is derived), so a sink that
    /// stores something OTHER than what was pushed is the realistic case: the
    /// second run must not churn just because the round-trip is lossy.
    ///
    /// It legitimately transitions ONCE — the stored value really does differ
    /// from the file — and then settles, because the transition path writes the
    /// file's word through. What must never happen is an unbounded re-transition
    /// on every subsequent run.
    #[tokio::test]
    async fn backfill_settles_against_a_normalizing_backend() {
        let sink = FakeSink {
            normalize: Some(("in_progress".to_string(), "in-progress".to_string())),
            ..Default::default()
        };
        let units = [unit("a", "in_progress")];

        let r1 = backfill_work_units_once(&units, &sink).await;
        assert_eq!(r1.created, 1);
        let r2 = backfill_work_units_once(&units, &sink).await;
        assert_eq!(
            r2.transitioned, 1,
            "the lossy round-trip costs one correction"
        );
        let r3 = backfill_work_units_once(&units, &sink).await;
        assert_eq!(
            (r3.transitioned, r3.refreshed),
            (0, 1),
            "and then it SETTLES — no unbounded churn"
        );
    }

    /// `deferred` is not just a count: the units are named, so an operator can
    /// act on them without re-running under RUST_LOG=info.
    #[tokio::test]
    async fn backfill_names_the_units_it_deferred() {
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            ..Default::default()
        };
        sink.statuses
            .lock()
            .unwrap()
            .insert("a".to_string(), "shipped".to_string());
        let s = backfill_work_units_once(&[unit("a", "in_progress")], &sink).await;
        assert_eq!(s.deferred as usize, s.deferred_units.len());
        assert_eq!(s.deferred_units[0].slug, "a");
        assert_eq!(s.deferred_units[0].owner, "device:d:agent:a");
        assert_eq!(s.deferred_units[0].wanted, "in_progress");
    }

    // ---- per-tick path resolution + tier visibility -------------------------

    /// Capture everything logged at `info` and above on THIS thread while the
    /// returned guard lives. Thread-local (`set_default`, not the global
    /// subscriber), so it composes with `#[tokio::test]`'s current-thread
    /// runtime and never leaks into sibling tests.
    struct CapturedLogs {
        buf: std::sync::Arc<Mutex<Vec<u8>>>,
        _guard: tracing::subscriber::DefaultGuard,
    }

    impl CapturedLogs {
        fn start() -> Self {
            use std::io::Write;
            use std::sync::Arc;

            #[derive(Clone, Default)]
            struct Writer(Arc<Mutex<Vec<u8>>>);
            impl Write for Writer {
                fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                    self.0
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .extend_from_slice(buf);
                    Ok(buf.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Writer {
                type Writer = Writer;
                fn make_writer(&'a self) -> Self::Writer {
                    self.clone()
                }
            }

            let writer = Writer::default();
            let buf = writer.0.clone();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(writer)
                .with_max_level(tracing::Level::INFO)
                .finish();
            let guard = tracing::subscriber::set_default(subscriber);
            Self { buf, _guard: guard }
        }

        fn text(&self) -> String {
            String::from_utf8_lossy(&self.buf.lock().unwrap_or_else(|e| e.into_inner())).to_string()
        }
    }

    /// A [`PathReader`] whose answer a test can change between ticks — the
    /// stand-in for an operator editing the Paths settings section while the
    /// loop runs.
    fn switchable_paths() -> (std::sync::Arc<Mutex<PathInputs>>, PathReader) {
        let cell = std::sync::Arc::new(Mutex::new(PathInputs::default()));
        let reader: PathReader = {
            let cell = cell.clone();
            std::sync::Arc::new(move || cell.lock().unwrap_or_else(|e| e.into_inner()).clone())
        };
        (cell, reader)
    }

    /// A plans dir holding one parseable plan, so a scan is observable as one
    /// `upsert` on the fake sink.
    fn one_plan_dir() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("2026-01-01-one-plan.md"),
            "# One plan\n\n> **Status: DRAFT**\n\nBody.\n",
        )
        .unwrap();
        tmp
    }

    fn plans_dir_input(dir: &Path) -> PathInputs {
        PathInputs {
            plans_dir: Some(dir.to_string_lossy().to_string()),
            ..PathInputs::default()
        }
    }

    /// The phase's headline acceptance criterion, pinned at the TICK — the
    /// only place it can actually be pinned.
    ///
    /// A machine with no plans dir returns early from `tick`, so the
    /// measurement has to be taken BEFORE that return. Testing
    /// [`record_scan_divergence`] directly cannot see the ordering: move the
    /// measurement block below the early return and every store-level test
    /// still passes while a tier-off device silently reports nothing again.
    ///
    /// Neuter check: move the `record_scan_divergence` call in `tick` after
    /// the `let Some(dir) = … else { return }` and this fails.
    #[tokio::test]
    async fn an_idle_tick_records_not_scanning_rather_than_nothing() {
        let (_cell, reader) = switchable_paths();
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate)
            .with_git(std::sync::Arc::new(FakeGit::healthy(2153, 11)));

        assert_eq!(
            metrics.snapshot().scan_divergence,
            None,
            "before the first tick there is no reading at all"
        );

        state.tick(&sink, &metrics).await;

        let d = metrics
            .snapshot()
            .scan_divergence
            .expect("the IDLE tick must record too — that is the whole criterion");
        assert_eq!(d.state, ScanDivergenceState::NotScanning);
        assert_eq!(
            (d.behind, d.ahead),
            (None, None),
            "a machine that scanned nothing may not report agreement with a ref"
        );
        assert_eq!(metrics.snapshot().cycles_total, 0, "still not a reconcile");
    }

    /// The armed half, and the proof the tick reads its INJECTED git rather
    /// than shelling out: a configured plans dir records the fake's numbers.
    #[tokio::test]
    async fn an_armed_tick_records_the_measurement_from_its_git_reader() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate)
            .with_git(std::sync::Arc::new(FakeGit::healthy(2153, 11)));

        state.tick(&sink, &metrics).await;

        let d = metrics
            .snapshot()
            .scan_divergence
            .expect("an armed tick records a reading");
        assert_eq!(d.state, ScanDivergenceState::Measured);
        assert_eq!((d.behind, d.ahead), (Some(2153), Some(11)));
        assert!(d.is_stale());
        assert!(
            d.ref_age_secs.is_some(),
            "the tick hands the measurement a clock, so an answered age probe yields an age"
        );
        assert_eq!(
            d.plans_dir.as_deref(),
            Some(dir.path().display().to_string().as_str()),
            "the reading names the dir that was actually scanned"
        );
    }

    /// The observability half: a machine with NO plans dir must SAY the
    /// markdown-plan tier is off, at `info`, naming the setting that arms it
    /// and the restart-free catch-up path. A silent idle is indistinguishable
    /// from a healthy scan — the defect that let a fleet-wide ingestion gap
    /// run unreported.
    ///
    /// Neuter check: drop the `None` arm's log in `apply_resolution` and this
    /// fails.
    #[tokio::test]
    async fn tier_off_machine_says_so_at_info() {
        let logs = CapturedLogs::start();
        let (_cell, reader) = switchable_paths();
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate);

        state.tick(&sink, &metrics).await;

        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            0,
            "an unarmed tier scans nothing"
        );
        assert_eq!(
            metrics.snapshot().cycles_total,
            0,
            "an idle tick is not a reconcile cycle"
        );
        let logged = logs.text();
        assert!(
            logged.contains("markdown-plan tier is OFF"),
            "the tier-off line must be emitted; got: {logged}"
        );
        assert!(
            logged.contains("paths.plans_dir"),
            "it must name the setting that arms the tier; got: {logged}"
        );
        assert!(
            logged.contains("plan-workunit-backfill"),
            "it must name the restart-free catch-up path; got: {logged}"
        );
        assert!(
            logged.contains("INFO"),
            "the line must be `info`, not `debug` — a debug line is invisible \
             at the fleet's default filter; got: {logged}"
        );
    }

    /// THE property Phase 4 exists for: a plans dir configured AFTER the loop
    /// started is scanned on the very next tick — no restart, no re-spawn.
    /// The shipped code baked the dir into `run_loop`'s arguments at spawn, so
    /// an edit looked inert until the next runner start.
    #[tokio::test]
    async fn a_plans_dir_configured_after_spawn_is_scanned_on_the_next_tick() {
        let logs = CapturedLogs::start();
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate);

        // Tick 1: nothing configured — idle, and the gauges say so.
        state.tick(&sink, &metrics).await;
        let snap = metrics.snapshot();
        assert_eq!(*sink.upsert_calls.lock().unwrap(), 0);
        assert_eq!(snap.active_plans_dir, None);
        assert_eq!(snap.scan_roots, 0);
        assert_eq!(
            snap.path_resolutions_total, 1,
            "the first tick resolves once"
        );

        // The operator saves the Paths section…
        *cell.lock().unwrap() = plans_dir_input(dir.path());

        // …and tick 2 scans it.
        state.tick(&sink, &metrics).await;
        let upserts = sink.upserts.lock().unwrap();
        assert_eq!(upserts.len(), 1, "the newly configured dir must be scanned");
        assert_eq!(upserts[0].slug, "2026-01-01-one-plan");
        drop(upserts);
        let snap = metrics.snapshot();
        assert_eq!(
            snap.active_plans_dir.as_deref(),
            Some(dir.path().to_string_lossy().as_ref())
        );
        assert_eq!(snap.scan_roots, 1);
        assert_eq!(snap.cycles_total, 1);
        assert_eq!(
            snap.path_resolutions_total, 2,
            "the change was picked up as one re-resolution"
        );

        // Tick 3, unchanged: no re-resolution, no phantom transition.
        state.tick(&sink, &metrics).await;
        assert_eq!(metrics.snapshot().path_resolutions_total, 2);
        assert_eq!(*sink.transitions.lock().unwrap(), 0);

        let logged = logs.text();
        assert_eq!(
            logged.matches("markdown-plan tier is OFF").count(),
            1,
            "got: {logged}"
        );
        assert_eq!(
            logged.matches("markdown-plan tier is ON").count(),
            1,
            "got: {logged}"
        );
    }

    /// The reverse transition: clearing the setting stops the scan on the
    /// next tick, and the OFF line is logged exactly ONCE — per transition,
    /// never per tick, however many idle ticks follow.
    #[tokio::test]
    async fn clearing_the_plans_dir_stops_scanning_and_logs_off_exactly_once() {
        let logs = CapturedLogs::start();
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate);

        state.tick(&sink, &metrics).await;
        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "armed from the first tick"
        );
        assert_eq!(logs.text().matches("markdown-plan tier is OFF").count(), 0);

        // The operator clears the field.
        *cell.lock().unwrap() = PathInputs::default();
        for _ in 0..3 {
            state.tick(&sink, &metrics).await;
        }

        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "no scan may run after the dir is cleared"
        );
        let snap = metrics.snapshot();
        assert_eq!(snap.cycles_total, 1);
        assert_eq!(snap.active_plans_dir, None);
        assert_eq!(snap.scan_roots, 0);
        assert_eq!(
            snap.path_resolutions_total, 2,
            "on→off is one re-resolution, not three"
        );
        let logged = logs.text();
        assert_eq!(
            logged.matches("markdown-plan tier is OFF").count(),
            1,
            "OFF is logged once per transition, not per idle tick; got: {logged}"
        );
    }

    /// The COLD-START BULK SEED, at the loop level where it actually runs.
    ///
    /// `reconcile_once`'s per-slug seed is the correctness path and is tested
    /// above; this asserts the cheap path that removes ~1,200 serialized
    /// round-trips on the first cycle after a runner start — that the loop
    /// primes `last_applied` from ONE `list_statuses` read, that the primed
    /// status is what protects coord's terminal row, and that the read is
    /// attempted once per corpus rather than once per cycle.
    ///
    /// Neuter check: delete the `self.bulk_seed(..)` call in `tick` and the
    /// `list_statuses_calls` assertion fails.
    #[tokio::test]
    async fn the_loop_bulk_seeds_once_and_the_prime_protects_a_terminal_status() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        // coord says `shipped`; the file still says DRAFT — the divergence that
        // demoted 88 terminal units per runner start before the seed existed.
        // `last_actor` is a real agent because priming alone protects nothing:
        // it restores the edge-trigger, and the OWNERSHIP DEFERRAL is what then
        // withholds the transition.
        let sink = FakeSink {
            last_actor: Some("device:d:agent:a".to_string()),
            bulk: Some(
                [("2026-01-01-one-plan".to_string(), "shipped".to_string())]
                    .into_iter()
                    .collect(),
            ),
            ..Default::default()
        };
        sink.statuses
            .lock()
            .unwrap()
            .insert("2026-01-01-one-plan".to_string(), "shipped".to_string());
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate);

        state.tick(&sink, &metrics).await;

        assert_eq!(
            sink.statuses
                .lock()
                .unwrap()
                .get("2026-01-01-one-plan")
                .map(String::as_str),
            Some("shipped"),
            "the primed status must survive the first cycle after a start"
        );
        assert_eq!(*sink.transitions.lock().unwrap(), 0, "a real agent owns it");
        assert_eq!(
            *sink.list_statuses_calls.lock().unwrap(),
            1,
            "one bulk read on the cold cycle"
        );
        assert_eq!(
            metrics.snapshot().seeded_total,
            1,
            "the one slug present in the plans dir was primed"
        );

        state.tick(&sink, &metrics).await;

        assert_eq!(
            *sink.list_statuses_calls.lock().unwrap(),
            1,
            "the bulk seed is a cold-start cost, not a per-cycle one"
        );
    }

    /// A sink with NO bulk door must still tick — the trait's defaulted
    /// `Ok(None)` is a documented fallback to the per-slug seed, not an error,
    /// and must not stop the cycle.
    #[tokio::test]
    async fn the_loop_falls_back_to_the_per_slug_seed_when_the_sink_has_no_bulk_door() {
        let dir = one_plan_dir();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(dir.path());
        let sink = FakeSink::default(); // bulk: None
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate);

        state.tick(&sink, &metrics).await;

        assert_eq!(*sink.list_statuses_calls.lock().unwrap(), 1);
        assert_eq!(
            *sink.upsert_calls.lock().unwrap(),
            1,
            "the cycle still ran and still created the unit"
        );
    }

    /// A corpus switch clears `last_applied`, so the corpus is COLD again and
    /// the bulk seed must re-arm — otherwise the per-slug fallback pays one
    /// round-trip per plan on the very next cycle, which is the cost the bulk
    /// read exists to remove.
    ///
    /// Neuter check: drop `self.bulk_seeded = false;` from `apply_resolution`
    /// and the second `list_statuses_calls` assertion reads 1.
    #[tokio::test]
    async fn changing_the_plans_dir_re_arms_the_bulk_seed() {
        let first = one_plan_dir();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(
            second.path().join("2026-02-02-another-plan.md"),
            "# Another plan\n\n> **Status: DRAFT**\n",
        )
        .unwrap();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(first.path());
        let sink = FakeSink {
            bulk: Some(HashMap::new()),
            ..Default::default()
        };
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate);

        state.tick(&sink, &metrics).await;
        assert_eq!(*sink.list_statuses_calls.lock().unwrap(), 1);

        *cell.lock().unwrap() = plans_dir_input(second.path());
        state.tick(&sink, &metrics).await;

        assert_eq!(
            *sink.list_statuses_calls.lock().unwrap(),
            2,
            "a new corpus is cold again, so the bulk seed re-arms"
        );
    }

    /// Moving the active dir to a different directory re-seeds the edge
    /// memory: the slugs of the old corpus must not be reported as
    /// "disappeared" from the new one.
    #[tokio::test]
    async fn changing_the_plans_dir_reseeds_rather_than_reporting_the_old_corpus_as_gone() {
        let logs = CapturedLogs::start();
        let first = one_plan_dir();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(
            second.path().join("2026-02-02-another-plan.md"),
            "# Another plan\n\n> **Status: DRAFT**\n",
        )
        .unwrap();
        let (cell, reader) = switchable_paths();
        *cell.lock().unwrap() = plans_dir_input(first.path());
        let sink = FakeSink::default();
        let metrics = AdapterMetrics::default();
        let mut state = LoopState::new(reader, None, std::sync::Arc::new(|| true) as CaptureGate);

        state.tick(&sink, &metrics).await;
        *cell.lock().unwrap() = plans_dir_input(second.path());
        state.tick(&sink, &metrics).await;

        let slugs: Vec<String> = sink
            .upserts
            .lock()
            .unwrap()
            .iter()
            .map(|u| u.slug.clone())
            .collect();
        assert_eq!(
            slugs,
            vec!["2026-01-01-one-plan", "2026-02-02-another-plan"]
        );
        assert!(
            !logs.text().contains("disappeared from the active dir"),
            "a corpus switch is a re-seed, not a mass disappearance; got: {}",
            logs.text()
        );
        assert_eq!(logs.text().matches("markdown-plan tier is ON").count(), 2);
    }

    /// A slug archived (not vanished) is NOT flagged disappeared — the archive
    /// set suppresses it.
    #[test]
    fn archived_slug_is_not_disappeared() {
        let mut known: HashMap<String, String> = HashMap::new();
        known.insert("done".to_string(), "shipped".to_string());
        let active: HashSet<String> = HashSet::new();
        let archive: HashSet<String> = ["done".to_string()].into_iter().collect();
        let mut warned: HashSet<String> = HashSet::new();
        assert!(newly_disappeared_slugs(&known, &active, &archive, &mut warned).is_empty());
        assert!(warned.is_empty());
    }
}
