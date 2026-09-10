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
    /// One line naming why the state is `Unknown` or `NotAGitWorkTree`. Never
    /// empty on those two states: an unexplained UNKNOWN is the same dead end
    /// as the silence this type replaces.
    pub detail: Option<String>,
}

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
            detail: None,
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
}

/// Measure the scan source against the ref it should be reading.
///
/// Pure over `git`: every branch is reachable from a fake reader, which is
/// what makes the four states testable without a repo on disk.
pub fn measure_scan_divergence(plans_dir: Option<&Path>, git: &dyn GitRefReader) -> ScanDivergence {
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
        ref_sha: Some(ref_sha),
        head_sha: Some(head_sha),
        ..base
    };

    match git.count_behind_ahead(&root, &default_ref, "HEAD") {
        Ok((behind, ahead)) => ScanDivergence {
            state: ScanDivergenceState::Measured,
            behind: Some(behind),
            ahead: Some(ahead),
            ..base
        },
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

/// Budget for every `git` invocation the detector makes.
///
/// All four are LOCAL plumbing reads, so a healthy call is milliseconds; the
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

    fn describe(args: &[&str], reason: &crate::process_helpers::DegradeReason) -> String {
        format!("`git {}` did not answer ({reason:?})", args.join(" "))
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
}

/// Publish this cycle's reading, and log it **only when it changed**.
///
/// Every cycle records; only a transition logs. A per-cycle line for a
/// steady-state reading is pure volume at a 60s tick, and volume is how the
/// one line that matters gets missed. A stale scan source
/// ([`ScanDivergence::is_stale`]) logs at WARN — it is a live correctness
/// problem, not a status note.
fn record_scan_divergence(divergence: ScanDivergence, metrics: &AdapterMetrics) {
    let mut slot = metrics
        .scan_divergence
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    if slot.as_ref() != Some(&divergence) {
        if divergence.is_stale() {
            tracing::warn!(
                state = divergence.state.as_str(),
                plans_dir = ?divergence.plans_dir,
                repo_root = ?divergence.repo_root,
                default_ref = ?divergence.default_ref,
                behind = ?divergence.behind,
                ahead = ?divergence.ahead,
                "plan adapter: the scanned plans dir is a WORKING TREE that is behind its own \
                 default branch — every work unit and plan body pushed from this machine \
                 reflects that parked tree, not the ref. Counts are as of this clone's last \
                 fetch (the adapter never fetches)"
            );
        } else {
            tracing::info!(
                state = divergence.state.as_str(),
                plans_dir = ?divergence.plans_dir,
                repo_root = ?divergence.repo_root,
                default_ref = ?divergence.default_ref,
                behind = ?divergence.behind,
                ahead = ?divergence.ahead,
                detail = ?divergence.detail,
                "plan adapter: scan-source divergence reading changed"
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
        let prev = last_applied.get(&u.slug).cloned();
        match push_work_unit(sink, u, prev.as_deref()).await {
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
        }
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
            None => ScanDivergence::not_scanning(),
            Some(dir) => {
                let git = std::sync::Arc::clone(&self.git);
                match tokio::task::spawn_blocking(move || {
                    measure_scan_divergence(Some(Path::new(&dir)), git.as_ref())
                })
                .await
                {
                    Ok(d) => d,
                    Err(e) => ScanDivergence::unknown(
                        resolved.plans.clone(),
                        format!("the scan-divergence probe task failed to run: {e}"),
                    ),
                }
            }
        };
        record_scan_divergence(divergence, metrics);

        let Some(dir) = resolved.plans.map(PathBuf::from) else {
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
            bs.run_cycle(&self.conv).await;
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
}

impl BodySync {
    pub fn new(
        roots: Vec<super::body_push::ScanRoot>,
        sink: super::body_push::HttpArtifactSink,
        capture_gate: CaptureGate,
    ) -> Self {
        Self {
            roots,
            sink,
            state: super::body_push::ArtifactSyncState::new(),
            capture_gate,
            breaker: FailureBreaker::new(),
            last_gate_open: None,
        }
    }

    pub async fn run_cycle(&mut self, conv: &PlanConvention) {
        let gate_open = (self.capture_gate)();
        if let Some(message) = capture_gate_message(self.last_gate_open, gate_open) {
            tracing::info!(capture_enabled = gate_open, "{message}");
        }
        self.last_gate_open = Some(gate_open);
        if !gate_open {
            return;
        }
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
    }

    impl FakeGit {
        /// A healthy clone: in a work tree, `origin/main` resolvable, both
        /// revs resolvable, and `(behind, ahead)` as given.
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
    }

    /// State 1 of 4. A machine with no `paths.plans_dir` scans NOTHING, and
    /// must say so out loud: `NotScanning` with no counts at all. The one
    /// thing it may never be is `0 behind / 0 ahead`, which is what "the scan
    /// is in step" looks like.
    #[test]
    fn scan_divergence_reports_not_scanning_when_no_plans_dir_is_configured() {
        let d = measure_scan_divergence(None, &FakeGit::healthy(0, 0));
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
        let d = measure_scan_divergence(Some(Path::new("/plain/plans")), &git);
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
        let d =
            measure_scan_divergence(Some(Path::new("/repo/plans")), &FakeGit::healthy(2153, 11));
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
        let d = measure_scan_divergence(Some(Path::new("/repo/plans")), &FakeGit::healthy(0, 0));
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
        let d = measure_scan_divergence(Some(Path::new("/maybe/plans")), &git);
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
        let d = measure_scan_divergence(Some(Path::new("/repo/plans")), &git);
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
        let d = measure_scan_divergence(Some(Path::new("/repo/plans")), &git);
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
        let d = measure_scan_divergence(Some(Path::new("/repo/plans")), &git);
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
        let ahead_only =
            measure_scan_divergence(Some(Path::new("/repo/plans")), &FakeGit::healthy(0, 11));
        assert!(
            ahead_only.is_stale(),
            "0 behind / 11 ahead is not agreement"
        );
        let behind_only =
            measure_scan_divergence(Some(Path::new("/repo/plans")), &FakeGit::healthy(2153, 0));
        assert!(behind_only.is_stale());
        let in_step =
            measure_scan_divergence(Some(Path::new("/repo/plans")), &FakeGit::healthy(0, 0));
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

        let measured =
            measure_scan_divergence(Some(Path::new("/repo/plans")), &FakeGit::healthy(2153, 11));
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
    }
    #[async_trait::async_trait]
    impl WorkUnitSink for FakeSink {
        async fn current_status(&self, slug: &str) -> Result<Option<String>> {
            if self.fail_status_read_for.as_deref() == Some(slug) {
                anyhow::bail!("simulated work-unit status read failure");
            }
            Ok(self.statuses.lock().unwrap().get(slug).cloned())
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
