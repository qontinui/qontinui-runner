//! Admission control for CI dispatches — defer-not-reject (the
//! `ContinuationGuard::AtCap` idiom from `agent_runtime`):
//!
//! - HARD reject (POST `cancelled` + reason): ci_node disabled, repo not in
//!   the local allowlist, unsafe identifiers, missing repo checkout, disk
//!   below the floor. These can't succeed by waiting.
//! - DEFER (hold in a FIFO queue, re-admit when a slot frees): at the
//!   concurrency cap, or below live resource headroom. The queue is drained by
//!   the build-finished hook — the capacity-freed re-poll pattern — and, for a
//!   headroom defer with nothing running to finish, by a waker
//!   ([`spawn_headroom_waker`]).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::{reporting, CiDispatchPayload};
use crate::settings::CiNodeSettings;

/// Pure admission verdict. `Reject` carries the operator-readable reason
/// that lands in the result summary.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admission {
    Proceed,
    Defer,
    Reject(String),
}

/// Live resource headroom, as injected into [`admission_decision`].
///
/// `admission_decision` stays PURE — it takes this, it never probes. That is
/// why the whole ladder is unit-testable without a live box, and it is also why
/// a wrong threshold can be argued about in a test rather than in production.
///
/// Every field is `Option`: an unreadable sensor is UNKNOWN, and unknown means
/// **no headroom opinion at all** (fail open). A telemetry gap must never brick
/// the lane — the same posture the disk and commit probes already take.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Headroom {
    /// Swap ceiling and how much of it is spent. Reported as a pair because a
    /// bare byte count cannot be read as pressure — the ceiling differs per
    /// host and per job.
    pub(crate) swap_total_bytes: Option<u64>,
    pub(crate) swap_used_bytes: Option<u64>,
    /// Free commit (Windows) / MemAvailable (elsewhere) — the same quantity
    /// [`MIN_FREE_COMMIT_GB`] rejects on, and the same one the A1 snapshot
    /// publishes as `commit_available_bytes`.
    pub(crate) commit_available_bytes: Option<u64>,
    /// This machine's EFFECTIVE live-session WARN floor —
    /// `max(local override, cached fleet default, hardcoded default)` as
    /// computed by [`crate::resource_guard::effective_session_floors`] over
    /// [`crate::settings::SessionGuardSettings::warn_free_commit_bytes`] — or
    /// `None` when the session guard is switched off, the same "no readable
    /// opinion" the other fields express when their sensor is dark.
    ///
    /// It is a **floor**, not a reading: the other fields say what the box has,
    /// this one says what the box's owner has declared it needs to keep. See
    /// [`MAX_SESSION_DEFER_FLOOR_GB`] for why a floor that protects interactive
    /// sessions gets a vote on whether this box accepts CI work, and
    /// [`defer_commit_floor_gb`] for how it is combined and clamped. It arrives
    /// as an injected field rather than being read inside
    /// [`headroom_defers`] so both that function and [`admission_decision`] stay
    /// pure over their inputs.
    pub(crate) session_warn_floor_bytes: Option<u64>,
    /// This box's `host`-lane **saturation** reading — threads or PIDs against
    /// the ceiling that bounds them — or `None` when nothing on this platform
    /// reports a complete pair.
    ///
    /// The THIRD axis, and the reason it is here rather than folded into one of
    /// the two above: it is **instrumentally independent of memory**. On
    /// 2026-08-27 this fleet reached a state where no process in the WSL VM
    /// could `fork()` — 190,840 tasks against a `kernel.threads-max` of 192,146,
    /// 99.3% — while every memory gauge on the same box read ≤ 21% and
    /// `/admin/coord/devops` was green on every lane. Swap and commit could not
    /// have seen it, because it was never a memory event. A metric that
    /// co-varies with an existing one adds no coverage; this one demonstrably
    /// does not co-vary.
    ///
    /// It arrives as [`crate::fleet::resource_sample::Saturation`] — the same
    /// type, from the same probe, that the publisher puts on the wire — because
    /// that type cannot hold half a pair or a non-positive ceiling, so
    /// [`Self::saturation_ratio`] needs no divisor guard. `None` is the ordinary
    /// reading on a machine whose platform exposes no enforced ceiling, and it
    /// contributes no term at all: unknown means **no headroom opinion**, the
    /// same fail-open posture every other field here takes.
    pub(crate) saturation: Option<crate::fleet::resource_sample::Saturation>,
}

impl Headroom {
    /// Fraction of the swap ceiling in use, or `None` when swap is unreadable
    /// or the ceiling is zero (a box with no swap has no swap pressure to
    /// measure — it has an OOM killer instead, which the commit floor covers).
    pub(crate) fn swap_used_ratio(&self) -> Option<f64> {
        let total = self.swap_total_bytes?;
        let used = self.swap_used_bytes?;
        (total > 0).then(|| used as f64 / total as f64)
    }

    /// Fraction of this lane's saturation ceiling in use, or `None` when the
    /// box reported no complete pair.
    ///
    /// No divisor guard here, unlike [`Self::swap_used_ratio`]: the reading's
    /// own constructor already rejected a zero ceiling and a missing half, so
    /// "unreadable" and "unbounded" both arrive as `None` rather than as a
    /// number that would have to be re-validated. That is the whole reason the
    /// field carries the publisher's type instead of two loose `Option<i64>`.
    pub(crate) fn saturation_ratio(&self) -> Option<f64> {
        self.saturation.map(|s| s.ratio())
    }
}

/// Swap-utilisation fraction at which we stop **adding** CI load to this box.
///
/// **Swap leads, not `mem_available`.** This fleet measured it: on a saturated
/// box `mem_used`/`mem_avail` are pinned by the kernel reserve and stay flat —
/// −13.5 ± 11.2 M/day, indistinguishable from zero — while `swap_used` moved
/// +138.6 ± 41.7 M/day over the same runs (plan
/// `2026-07-28-coord-ci-memory-headroom-sizing-review`; the finding is written
/// into `qontinui-coord/.github/scripts/resource-sampler.sh`'s own header:
/// *"Leading with mem_avail is what let a saturating metric read as an
/// all-clear."*). Ranking on memory-available here would reproduce the
/// 2026-08-02 misdiagnosis in code.
///
/// **Why 0.5.** The threshold has to answer "can the remaining ceiling absorb
/// one more of these jobs?", and a coord `rust-ci` job is known to consume
/// ~14 GB while `coord-db-tests` fallocates 12 G of swap for itself. On a host
/// whose swap ceiling is sized for roughly one such job, half-spent means the
/// next one has nowhere to go. Half is also the point past which the ratio has
/// stopped being noise on an idle box (steady-state here sits under 5%).
///
/// The cost of being wrong is bounded by the verdict, which is why this is a
/// round number rather than a fitted one: too low and a build waits ~60s longer
/// than it needed to; too high and it starts on a thrashing box. Deferring is
/// recoverable in a way that a 0xc0000409 rustc abort — which poisons the
/// incremental cache and makes the *next* build cold — is not.
pub(crate) const SWAP_DEFER_RATIO: f64 = 0.5;

/// Saturation fraction (threads or PIDs against the ceiling that bounds them)
/// at which we stop **adding** CI load to this box.
///
/// The runner-side half of plan
/// `2026-08-27-fleet-telemetry-has-no-saturation-dimension-but-memory`. Phase 2
/// wired the same ratio into coord's dispatch ranking
/// (`HEADROOM_ORDER_SQL = "GREATEST(h.pressure, h.saturation) ASC NULLS LAST"`);
/// this is the arm in the repo that owns `ci_node`, so the node decides with
/// the same number coord ranks on. Sharing the ratio without the threshold is
/// not sharing the decision — §C1 of plan
/// `2026-08-02-fleet-resource-telemetry-and-ci-allocation` shipped exactly that
/// and the strip disagreed with the dispatcher anyway.
///
/// **Why 0.80.** Deliberately below the 99.3% the 2026-08-27 incident reached,
/// and three orders of magnitude above any healthy steady-state reading in the
/// evidence: every container on that box except the leaking one sat at ≤ 68
/// PIDs against a 192,146 ceiling (~0.04%). There is no false-positive pressure
/// on this number — the gap between "healthy" and "the box cannot `fork()`" is
/// the entire range, so a round number in the middle of it is honest and a
/// fitted one would be false precision. Calibrate against real samples once the
/// fleet has published this axis for a day; a first threshold is a starting
/// point, not a constant to defend.
///
/// **A defer, never a reject** — like every other term in [`headroom_defers`].
/// Deferring is not filtering: a saturated box stays a ranking candidate and is
/// simply out-ranked, because with one sample-less machine and one busy one,
/// excluding would elect nobody. If this should ever *gate* dispatch that is a
/// drain predicate, a different mechanism, and it must not be smuggled in here.
pub(crate) const SATURATION_DEFER_RATIO: f64 = 0.80;

/// Free-commit level at which we defer, expressed in GiB.
///
/// Strictly above [`MIN_FREE_COMMIT_GB`], and deliberately so: **you defer
/// before you reject.** The reject floor is the last line — a build that gets
/// there is turned away and coord must find another home for it. The defer band
/// above it is where a build simply waits for the box to breathe, which is what
/// memory pressure usually needs, because memory frees on its own and disk does
/// not. Collapsing the two onto one number would turn every transient spike
/// into a rejected dispatch.
///
/// It also sits above the supervisor's 5 GiB defer floor, which is intentional
/// and not a drift: **CI work has somewhere else to go and a local build does
/// not.** A deferred dispatch is one coord can hand to the other host; a
/// deferred supervisor build is an operator waiting at a keyboard. The lane
/// with an alternative should be the first to step back.
pub(crate) const DEFER_FREE_COMMIT_GB: u64 = MIN_FREE_COMMIT_GB * 2;

/// Ceiling (GiB) on how far the live-session warn floor may push
/// [`DEFER_FREE_COMMIT_GB`] up.
///
/// ## Why a session floor may raise the CI defer band at all
///
/// `session_guard.warn_free_commit_bytes` is the level below which starting
/// another **interactive** session on this box is unsafe (plan
/// `2026-08-07-runner-resource-guard-and-session-protection`, Part C item 1;
/// the overnight 2026-08-06→07 incident, where Claude Code sessions died inside
/// runner-spawned terminals as commit charge was exhausted and nothing local
/// was watching). Once the box is under that level, admitting a fresh CI
/// dispatch spends precisely the headroom a live session needs — and it spends
/// it on the lane that has an alternative. A deferred dispatch is one coord can
/// hand to the other host; the human's session in front of the operator cannot
/// be re-homed anywhere. That is the argument [`DEFER_FREE_COMMIT_GB`] already
/// makes against the supervisor's 5 GiB build floor ("the lane with an
/// alternative should be the first to step back"), carried one rung further
/// out: CI steps back for a *session*, not only for another build.
///
/// The term can only ever RAISE the threshold, never lower it. A machine owner
/// who sets a *low* warn floor is saying "warn me later about my own spawns";
/// they are not authorising CI to run this box further down than
/// [`DEFER_FREE_COMMIT_GB`] already allows. And it stays in the DEFER arm:
/// memory pressure is transient, so a node that rejected on it would make coord
/// re-home work that would have run fine in a minute (see this module's header
/// and [`admission_decision`]).
///
/// ## Why it is bounded — and why the bound is 12, not the shell lane's 8
///
/// The setting has no server-side upper bound, and an operator can set a warn
/// floor no box on this fleet ever reaches — 16 GiB is an entirely reasonable
/// thing to type after an incident whose top consumer was `vmmemWSL` at ~17 GB.
/// [`crate::resource_guard::SESSION_FLOOR_MAX_BYTES`] now caps the *effective*
/// floor at this same 12 GiB before it ever gets here (that lane needs its own
/// bound for a harder reason: an unreachable spawn floor refuses every
/// unattended session with no timeout to fail open through). This clamp still
/// stands on its own: the two lanes must be able to move independently, and a
/// pure function that trusts an injected bound it does not enforce is a
/// property one edit away from being false. `cargo-guard.sh` caps its own
/// session term for the same reason, but the *consequence* of an unreachable
/// floor differs per lane, so the numbers must too:
///
/// - In the shell lane it **fails open by time**: the wait loop sleeps out
///   `MEM_WAIT_MAX` and then builds anyway. It costs one stall. Its cap is this
///   lane's `DEFER_FREE_COMMIT_GB` (8) on the reasoning that a local build
///   should not be made to wait past the point CI already defers.
/// - Here there is no timeout to fail open through. An unreachable threshold
///   makes [`headroom_defers`] true at *every* reading: every dispatch defers,
///   [`spawn_headroom_waker`] re-tests every [`HEADROOM_RETRY_SECS`] forever,
///   and coord keeps re-homing work away from a perfectly healthy box. This
///   lane fails CLOSED, which is the worse failure and argues for a tight
///   bound.
///
/// Copying the shell lane's 8 here would not be tight, it would be **empty**:
/// this lane's defer band already *is* 8, so `max(8, min(x, 8))` is 8 for every
/// setting and the session term could never do anything. The bound has to sit
/// above [`DEFER_FREE_COMMIT_GB`] to exist at all, and the ladder's own unit is
/// [`MIN_FREE_COMMIT_GB`] — 4 GiB, one rung, the same unit
/// `DEFER_FREE_COMMIT_GB` is built from. So the most a session floor may buy is
/// one rung above the shipped band: 12 GiB. The worst thing an operator can
/// then express is "this node behaves as though its defer band were one rung
/// wider" — never "this node is unreachable by construction". 12 also stays
/// inside the headroom any box that can host this fleet's ~14 GB `rust-ci` job
/// has while idle, so a machine healthy enough to want the work can still clear
/// the raised bar.
pub(crate) const MAX_SESSION_DEFER_FLOOR_GB: u64 = DEFER_FREE_COMMIT_GB + MIN_FREE_COMMIT_GB;

/// The free-commit level (GiB) below which this node defers, given the machine
/// owner's session warn floor: `max(DEFER_FREE_COMMIT_GB, min(floor, cap))`.
///
/// Pure over the injected floor, and split out of [`headroom_defers`] for the
/// same reason `headroom_defers` is split out of [`admission_decision`]: the
/// clamp is the part with an argument in it, so it should be assertable on its
/// own rather than only through a whole admission verdict.
///
/// `None` contributes no term at all and leaves [`DEFER_FREE_COMMIT_GB`]
/// exactly as it was. That is the same posture every other [`Headroom`] field
/// takes, and it is the right reading of a disabled guard specifically: an
/// owner who switched the session guard off has said this box does not police
/// interactive headroom, and synthesising a CI floor out of a switch they
/// turned off would be inventing an opinion from its absence.
///
/// The byte→GiB conversion rounds UP, matching `cargo-guard.sh`'s
/// `session_warn_floor_gb`. Truncating a 3.5 GiB floor to 3 would enforce
/// something weaker than what was configured, and the shipped critical default
/// (1.5 GiB) shows fractional-GiB values are ordinary here, not hypothetical.
pub(crate) fn defer_commit_floor_gb(session_warn_floor_bytes: Option<u64>) -> u64 {
    let Some(floor_bytes) = session_warn_floor_bytes else {
        return DEFER_FREE_COMMIT_GB;
    };
    let session_gb = floor_bytes.div_ceil(1024 * 1024 * 1024);
    DEFER_FREE_COMMIT_GB.max(session_gb.min(MAX_SESSION_DEFER_FLOOR_GB))
}

/// Pure admission decision over injected inputs (no globals — unit-tested
/// without settings files or live state).
///
/// `max_concurrent` is the node's resolved capacity —
/// [`CiNodeSettings::effective_max_concurrent_builds_for`] over a host probe —
/// resolved ONCE by the caller (next to [`probe_headroom`], outside the state
/// lock) and passed in, so an unset `max_concurrent_builds` never makes this
/// function probe the host. The same number gates the under-lock re-check in
/// `start_build` and sizes the dispatch's host share in the executor.
///
/// Order is load-bearing: **rejects first, then defers.** Nothing below the
/// allowlist check can turn a `Defer` into a `Reject`, which is the property
/// `at_cap_defers_never_rejects` pins and which the headroom arm must not
/// weaken — coord *prefers*, the node *decides*, and a node that rejects on a
/// transient reading makes coord re-home work that would have run fine in a
/// minute.
pub(crate) fn admission_decision(
    settings: &CiNodeSettings,
    max_concurrent: u32,
    repo: &str,
    running_count: usize,
    headroom: Headroom,
) -> Admission {
    if !settings.enabled {
        return Admission::Reject("ci_node is disabled on this device".to_string());
    }
    if !repo_allowed(&settings.repo_allowlist, repo) {
        return Admission::Reject(format!(
            "repo {repo:?} is not in this device's ci_node.repo_allowlist"
        ));
    }
    if running_count >= max_concurrent.max(1) as usize {
        return Admission::Defer;
    }
    if headroom_defers(headroom) {
        return Admission::Defer;
    }
    Admission::Proceed
}

/// `true` when live headroom says "not one more build right now".
///
/// Split out from [`admission_decision`] so the threshold policy can be
/// exercised on its own, and so a future caller can ask the question without
/// assembling a whole `CiNodeSettings`.
pub(crate) fn headroom_defers(headroom: Headroom) -> bool {
    // Swap FIRST — see `SWAP_DEFER_RATIO`.
    if let Some(ratio) = headroom.swap_used_ratio() {
        if ratio >= SWAP_DEFER_RATIO {
            return true;
        }
    }
    // Saturation SECOND, and ahead of commit for the same reason swap is: it is
    // the axis a memory reading cannot see. A box at 99.3% of its task ceiling
    // reported 73.3 GB free commit, so consulting commit first would have
    // proceeded straight into a machine where the build's own `fork()` fails.
    if let Some(ratio) = headroom.saturation_ratio() {
        if ratio >= SATURATION_DEFER_RATIO {
            return true;
        }
    }
    if let Some(free) = headroom.commit_available_bytes {
        // The band is `DEFER_FREE_COMMIT_GB`, widened by the machine owner's
        // live-session floor when they have declared one above it — see
        // `defer_commit_floor_gb` and `MAX_SESSION_DEFER_FLOOR_GB`.
        if free / (1024 * 1024 * 1024) < defer_commit_floor_gb(headroom.session_warn_floor_bytes) {
            return true;
        }
    }
    // Every reading unavailable ⇒ no opinion ⇒ proceed. Fail open.
    false
}

/// Live headroom probe. Every field degrades to `None` independently, so a
/// partially-blind box still contributes the half it can read.
///
/// ## Windows deliberately supplies no swap reading
///
/// The swap-leads finding was measured on Linux, from `/proc/meminfo` and
/// `free` — a real pagefile figure. On Windows, sysinfo derives "swap" from the
/// commit charge (roughly commit-limit minus physical), so populating the swap
/// fields there would not give the decision a second, independent signal: it
/// would give it the SAME commit reading a second time, wearing a Linux name,
/// and then compare it against a ratio calibrated on Linux. On this box that
/// derived ratio sits near 0.44 while the machine is comfortably idle — it
/// would defer builds on a healthy host.
///
/// So Windows leaves swap `None` and lets the commit arm decide, which is
/// exactly the arm the supervisor and `cargo-guard.sh` already guard on. What
/// must never happen — and does not, on either platform — is leading on
/// *memory-available*, the metric this fleet measured as pinned under
/// saturation. The `wsl` lane's REAL swap figures still reach coord in the A1
/// sample (`fleet::resource_sample` reads them from `/proc/meminfo` inside the
/// VM), where §B1's cross-machine ranking can use them honestly.
fn probe_headroom() -> Headroom {
    #[cfg(windows)]
    let (swap_total_bytes, swap_used_bytes) = (None, None);

    #[cfg(not(windows))]
    let (swap_total_bytes, swap_used_bytes) = {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let total = sys.total_swap();
        (
            (total > 0).then_some(total),
            (total > 0).then(|| sys.used_swap()),
        )
    };

    // The live-session floor is read HERE, not inside the decision, so
    // `admission_decision` / `headroom_defers` stay pure over injected inputs.
    // A disabled guard yields `None` rather than the floor it happens to have
    // stored: the switch is the owner's statement that this box does not police
    // interactive headroom, and `None` is how every other field in this struct
    // spells "no opinion".
    //
    // It is the EFFECTIVE floor — `max(local override, cached fleet default,
    // hardcoded default)`, `resource_guard::effective_session_floors` — not the
    // raw local setting. There is one live-session floor on this machine, and
    // the spawn gate and this lane must read the same one; a tenant-wide
    // tightening that reached the spawn gate but not CI admission would leave
    // coord dispatching builds into exactly the headroom the fleet just declared
    // it wants kept for sessions. The `host` lane specifically, because
    // `commit_available_bytes` above IS the host-lane reading — never judge one
    // lane's reading against another lane's floor.
    //
    // An unreachable floor cannot wedge this lane: `effective_session_floors`
    // already caps what it returns at `resource_guard::SESSION_FLOOR_MAX_BYTES`,
    // and `defer_commit_floor_gb` clamps whatever arrives at
    // `MAX_SESSION_DEFER_FLOOR_GB` regardless — belt and braces, deliberately,
    // because this lane's clamp must not depend on the other lane keeping a
    // bound it is free to change.
    //
    // The floors also arrive with the ladder already coerced (`critical <=
    // warn`), so the warn floor read below is never the transposed one — but
    // only the warn floor is read here anyway, and only as a raise.
    let local_guard = crate::settings::get_session_guard_settings();
    let session_guard = crate::resource_guard::effective_session_floors(
        &local_guard,
        crate::fleet::resource_sample::Lane::Host.as_str(),
    );

    Headroom {
        swap_total_bytes,
        swap_used_bytes,
        commit_available_bytes: crate::fleet::resource_sample::available_commit_bytes(),
        session_warn_floor_bytes: session_guard
            .enabled
            .then_some(session_guard.warn_free_commit_bytes),
        // The SAME probe the published sample carries, for the same reason the
        // commit figure above is: the node's defer verdict and coord's fleet
        // strip must be two instants of one instrument rather than two
        // instruments that agree on a name. The `host` lane specifically —
        // never judge one lane's reading against another lane's floor.
        saturation: crate::fleet::resource_sample::host_saturation(),
    }
}

/// Allowlist match: an entry equals the full slug (`owner/name`) or the
/// bare basename.
pub(crate) fn repo_allowed(allowlist: &[String], repo: &str) -> bool {
    let basename = crate::agent_runtime::local_repo_name(repo);
    allowlist
        .iter()
        .any(|entry| entry == repo || entry == basename)
}

/// Pick the volume holding `root` from a `(mount_point, total_bytes,
/// available_bytes)` list — longest matching mount wins. Pure for tests; the
/// live caller feeds it `sysinfo::Disks`.
pub(crate) fn pick_volume<'a>(
    mounts: &'a [(PathBuf, u64, u64)],
    root: &Path,
) -> Option<&'a (PathBuf, u64, u64)> {
    mounts
        .iter()
        .filter(|(mount, _, _)| root.starts_with(mount))
        .max_by_key(|(mount, _, _)| mount.as_os_str().len())
}

/// Live volume probe for `root`: `(mount_point, total_bytes,
/// available_bytes)`. `None` when the volume can't be resolved — every caller
/// fails OPEN with a warning (a telemetry gap must not brick the lane; the
/// 20 GiB floor is a guard, not a security boundary).
///
/// One probe site, shared by the disk floor and by the A1 resource sample
/// (`fleet::resource_sample`), so the number the dashboard renders and the
/// number the gate trips on are literally the same reading. Two probes of "free
/// disk" that disagree is how an operator ends up debugging the dashboard
/// instead of the machine.
pub(crate) fn probe_volume_for(root: &Path) -> Option<(PathBuf, u64, u64)> {
    pick_volume(&enumerate_mounts(), root).cloned()
}

/// Every mounted volume as `(mount_point, total_bytes, available_bytes)`.
///
/// The single `sysinfo::Disks` enumeration site for the whole runner — split
/// out of [`probe_volume_for`] so the disk-monitoring publisher
/// ([`crate::agent_worktree::census::collect_all_volumes`]) samples the SAME
/// reading the CI-node admission floor trips on. That is the "one probe site"
/// property [`probe_volume_for`]'s doc argues for, extended to the third
/// consumer: two probes of "free disk" that disagree is how an operator ends
/// up debugging the dashboard instead of the machine.
///
/// An EMPTY result is a failed/blind probe, not "this machine has no disks" —
/// callers must render it as UNKNOWN and never as zero free space.
pub(crate) fn enumerate_mounts() -> Vec<(PathBuf, u64, u64)> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .map(|d| {
            (
                d.mount_point().to_path_buf(),
                d.total_space(),
                d.available_space(),
            )
        })
        .collect()
}

fn free_disk_gb_for(root: &Path) -> Option<u64> {
    probe_volume_for(root).map(|(_, _, avail)| avail / (1024 * 1024 * 1024))
}

/// Verdict of the pre-build disk gate. `Reject` carries the operator-readable
/// reason verbatim, so [`start_build`] does not re-word it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DiskGate {
    Ok,
    Reject(String),
}

/// Pure disk-admission policy: free space, the floor, and — new in plan
/// `2026-08-07-external-storage-tiering-for-fleet-disk-pressure` Phase 5 —
/// whether the build path sits on a **declared external volume**.
///
/// # The polarity is per-path, and that is the whole change
///
/// `free_gb == None` means the volume could not be resolved. For an internal
/// path that stays **fail-OPEN**, exactly as before: a telemetry gap must not
/// brick the lane, and the floor is a guard rather than a security boundary.
/// For a path on a removable volume it becomes **fail-CLOSED**, because the
/// cost asymmetry inverts. Proceeding on an unresolvable internal probe risks
/// a failed build; proceeding on an unresolvable *external* probe risks a
/// partially written artifact tree on a volume that just went away — or 300
/// GiB written into the un-mounted stub, filling the very disk the relocation
/// exists to relieve. A refusal is strictly recoverable; that is not.
///
/// `external` is `None` when no external volume is declared **or** this path
/// is not under it — which, on a machine with no dock, is every path. That is
/// how this ships with no behaviour change: with nothing declared, every arm
/// below is byte-identical to the pre-Phase-5 code.
///
/// Pure so the polarity can be pinned in unit tests with **no dock attached
/// and no filesystem touched** — the same reason [`pick_volume`] and the
/// supervisor's `disk_guard_allows` are pure.
pub(crate) fn disk_gate(
    free_gb: Option<u64>,
    floor_gb: u64,
    external: Option<&crate::external_volume::ExternalVolumeState>,
    root: &Path,
) -> DiskGate {
    // A declared-but-not-provably-present external volume is refused BEFORE
    // free space is even considered: "how much room is on it" is not a
    // meaningful question about a volume that is absent, or about one that
    // turned out to be a different volume than the one declared.
    if let Some(state) = external {
        if let Some(reason) = state.refusal_reason(root) {
            return DiskGate::Reject(reason);
        }
    }

    match free_gb {
        Some(free) if free < floor_gb => DiskGate::Reject(format!(
            "free disk {free} GiB on the {} volume is below the ci_node.min_free_disk_gb \
             floor ({floor_gb} GiB)",
            root.display()
        )),
        Some(_) => DiskGate::Ok,
        None if external.is_some() => DiskGate::Reject(format!(
            "could not resolve free disk for {} — it is on the declared EXTERNAL volume, \
             so this refuses rather than proceeding (fail-closed guard): an unresolvable \
             probe is precisely the condition under which a build on a removable volume \
             must not start",
            root.display()
        )),
        // Internal path, unresolvable probe: unchanged fail-open.
        None => DiskGate::Ok,
    }
}

/// Minimum free **commit** (GiB) to START a build (plan §4.6: "minimum free
/// RAM" alongside the disk floor).
///
/// ## The quantity (plan §A3)
///
/// Renamed from `MIN_FREE_RAM_GB` because the old name was the bug. Three
/// lanes guard builds on this machine — the supervisor's build pool
/// (5 GiB), `cargo-guard.sh` (5 GiB), and this one — and the first two already
/// read Windows **free commit** by design, `available_commit_bytes()`
/// documenting that "keeping both lanes on one metric is the point". `ci_node`
/// was the sole divergence: it probed sysinfo's available-memory reading,
/// which on Windows is physical-available, not commit. So "4 GB free" here and
/// "5 GB free" there were not 1 GiB apart — they were different quantities that
/// happened to share a unit, and nothing could detect that, because nothing
/// could see both. It now reads
/// [`crate::fleet::resource_sample::available_commit_bytes`], the same function
/// that produces the A1 snapshot's `commit_available_bytes` column.
///
/// ## The number, and why it is LOWER than the supervisor's 5
///
/// Converging the quantity must not converge the **verdict**, and it has not:
/// this floor is a hard **reject** (a dispatch coord must re-home), while the
/// supervisor's is a **defer** (a build that waits). A rejecting lane must sit
/// *below* a deferring one, or it would turn away work the deferring lane would
/// happily have run a minute later. 4 GiB is well under any machine that can
/// build these workspaces at all, so the gate only trips when the box is
/// genuinely starved and one more rustc would push it into OOM/thrash territory
/// — and [`DEFER_FREE_COMMIT_GB`] gives this lane its own defer band above it.
pub(crate) const MIN_FREE_COMMIT_GB: u64 = 4;

/// `true` when free commit is below the floor. Pure over injected bytes.
pub(crate) fn commit_below_floor(available_bytes: u64, floor_gb: u64) -> bool {
    available_bytes / (1024 * 1024 * 1024) < floor_gb
}

/// When a queued dispatch arrived, and the two DEADLINES it carries — stored
/// as expiry instants and compared `<= now`, so no clock is ever reconstructed
/// by subtracting from an [`Instant`] (on Windows `Instant`'s epoch is boot,
/// and a subtraction reaching below it had to be clamped to a wrong answer).
///
/// - `queued_since`: when it FIRST entered this device's queue (for logs).
/// - `release_at`: the queue deadline — arrival + [`queue_release_after`], or
///   coord's `queued_renewal_deadline` less [`QUEUE_RELEASE_MARGIN`] when that
///   is sooner. Past it the dispatch is released as
///   [`DEFERRED_PAST_LEASE_REASON`].
/// - `renew_by`: [`UNRENEWED_HOLD`] after coord last confirmed the lease — a
///   `Renewed` queue heartbeat moves it to that tick + the hold; until one
///   lands it is arrival + the hold, or coord's `lease_expires_at` less
///   [`QUEUE_RELEASE_MARGIN`] when that is sooner. Coord sweeps an unrenewed
///   row when its lease runs out, so a queue heartbeat that keeps FAILING must
///   not leave the runner holding a row coord has abandoned; past `renew_by`
///   it is released as [`QUEUED_RENEWAL_LAPSED_REASON`].
///
/// Coord's two wall-clock deadlines are AUTHORITATIVE where sent; the
/// arrival-based values are the fallback and a cap. A coord-derived deadline
/// is never allowed to fall before [`FIRST_RENEWAL_GRACE`] after arrival for a
/// dispatch the keeper renews, so the first tick's heartbeat always goes out
/// before any release such a deadline would cause — see
/// [`QueueStamp::on_arrival`].
///
/// The stamp survives re-submission: a dispatch popped for re-admission that
/// defers again goes back in with its original stamp, so a dispatch bounced
/// between the queue and a failed re-test cannot reset either deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QueueStamp {
    queued_since: Instant,
    release_at: Instant,
    renew_by: Instant,
}

impl QueueStamp {
    /// A dispatch queued at `now` with no coord-supplied deadline: its lease
    /// was last (re)started by coord's dispatch, which is as close to `now` as
    /// the runner can know.
    ///
    /// `release_at` is built with `checked_add`: this runs inside
    /// [`enqueue_locked`] under the `ci_state()` lock, and a panic there would
    /// poison the mutex for every later caller until restart. Should the sum
    /// ever be unrepresentable, the dispatch falls back to the unrenewed hold
    /// — the deadline a dispatch from a coord with no ceiling gets.
    fn fresh(payload: &CiDispatchPayload, now: Instant) -> Self {
        let renew_by = now + UNRENEWED_HOLD;
        Self {
            queued_since: now,
            release_at: now
                .checked_add(queue_release_after(payload))
                .unwrap_or(renew_by),
            renew_by,
        }
    }

    /// The stamp a dispatch gets on its FIRST enqueue at `now` (`wall_now` is
    /// the same moment on the wall clock).
    ///
    /// Each coord-supplied deadline, less [`QUEUE_RELEASE_MARGIN`], tightens
    /// its arrival-based counterpart: `lease_expires_at` the `renew_by`,
    /// `queued_renewal_deadline` the `release_at`. A coord deadline can only
    /// SHORTEN a hold — one further out than the arrival-based value (clock
    /// skew, or a longer coord lease) is capped by it.
    ///
    /// Floor: for a dispatch the keeper will renew, neither coord-derived
    /// deadline lands before `now + FIRST_RENEWAL_GRACE`. A runner clock
    /// AHEAD of coord's inflates the age the wall-clock deadlines imply, and
    /// without the floor a healthy dispatch could be released as
    /// `queued_renewal_lapsed` before the runner had asked coord once. With
    /// it, the first keeper tick (at most one interval away) sends a
    /// heartbeat first; coord's answer then decides — `Renewed` extends
    /// `renew_by`, `Terminal` drops a row coord already swept, `Refused`
    /// releases it. A dispatch the keeper does NOT renew gets no floor: no
    /// answer is coming, so an already-past deadline releases on the first
    /// tick.
    ///
    /// The floor is not free. For a GENUINELY late delivery (no skew) whose
    /// first heartbeat fails because coord is unreachable, it can move the
    /// release from just before coord's sweep to just after it; the release
    /// is then answered `409 dispatch_terminal` and its reason is lost, the
    /// row ending a reason-free `lost`. That is accepted: protecting a healthy
    /// dispatch on a skewed clock from a release coord never asked for is
    /// worth more than a reason on a row coord could not be reached about.
    fn on_arrival(
        payload: &CiDispatchPayload,
        now: Instant,
        wall_now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        let mut stamp = Self::fresh(payload, now);
        let floor = if payload.coord_accepts_queue_heartbeat() {
            now + FIRST_RENEWAL_GRACE
        } else {
            now
        };
        let inside = |deadline| margin_inside(deadline, wall_now, now);
        if let Some(by) = payload.lease_expires_at.and_then(inside) {
            stamp.renew_by = stamp.renew_by.min(by).max(floor);
        }
        if let Some(at) = payload.queued_renewal_deadline.and_then(inside) {
            stamp.release_at = stamp.release_at.min(at).max(floor);
        }
        if let Some(age) = coord_age_at_arrival(payload, wall_now) {
            if age > COORD_AGE_WARN {
                warn!(
                    "ci_node: dispatch {} for {} arrived {}s after coord created or leased \
                     it, by coord's own deadlines — a late delivery, or this runner's clock \
                     ahead of coord's; coord-derived queue deadlines are floored at {:?} \
                     after arrival so a lease renewal is attempted first",
                    payload.dispatch_id,
                    payload.repo,
                    age.num_seconds(),
                    FIRST_RENEWAL_GRACE
                );
            }
        }
        stamp
    }
}

/// The monotonic instant [`QUEUE_RELEASE_MARGIN`] before the wall-clock
/// `deadline`, carried onto the [`Instant`] clock through the pair
/// (`wall_now`, `now`) that names the same moment on both clocks.
///
/// Only ever ADDS to `now`: a moment at or before `wall_now` maps to `now`
/// itself, which `<= now` already reads as due — so an arbitrarily old
/// deadline needs no subtraction below the `Instant` epoch. `None` for a
/// deadline too far ahead to represent, which bounds nothing.
fn margin_inside(
    deadline: chrono::DateTime<chrono::Utc>,
    wall_now: chrono::DateTime<chrono::Utc>,
    now: Instant,
) -> Option<Instant> {
    let margin = chrono::Duration::seconds(QUEUE_RELEASE_MARGIN.as_secs() as i64);
    let Some(target) = deadline.checked_sub_signed(margin) else {
        return Some(now);
    };
    match (target - wall_now).to_std() {
        Ok(ahead) => now.checked_add(ahead),
        Err(_) => Some(now),
    }
}

/// How long before arrival coord created or (re)leased `payload`, as its own
/// deadlines imply — from `lease_expires_at` (assuming coord's lease is
/// [`COORD_LEASE`]) or else from `queued_renewal_deadline` and the advertised
/// ceiling. Diagnostic only: it names a delivery lag or a runner clock ahead
/// of coord's, and never feeds a deadline.
fn coord_age_at_arrival(
    payload: &CiDispatchPayload,
    wall_now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::Duration> {
    let started = match (
        payload.lease_expires_at,
        payload.queued_renewal_deadline,
        payload.queued_renewal_max_age_secs,
    ) {
        (Some(lease_end), _, _) => {
            lease_end.checked_sub_signed(chrono::Duration::seconds(COORD_LEASE.as_secs() as i64))?
        }
        // `max_age` is payload-derived: an `as i64` cast would wrap a huge
        // value negative, and `Duration::seconds` panics past its range.
        (None, Some(ceiling), Some(max_age)) => ceiling
            .checked_sub_signed(chrono::Duration::try_seconds(i64::try_from(max_age).ok()?)?)?,
        _ => return None,
    };
    Some(wall_now - started)
}

/// A coord-implied age at arrival past which [`QueueStamp::on_arrival`] warns:
/// delivery normally takes seconds, so minutes mean a backlog or clock skew.
const COORD_AGE_WARN: chrono::Duration = chrono::Duration::minutes(3);

/// How far after arrival a coord-derived deadline may fall, at the earliest,
/// for a dispatch the keeper renews: one [`QUEUE_HEARTBEAT_INTERVAL`] (the
/// first keeper tick is at most that far away, and its release check runs
/// strictly before it) plus one [`QUEUE_HEARTBEAT_BATCH_TIMEOUT`] (the
/// renewal it sends has that long to be answered).
pub(crate) const FIRST_RENEWAL_GRACE: Duration =
    QUEUE_HEARTBEAT_INTERVAL.saturating_add(QUEUE_HEARTBEAT_BATCH_TIMEOUT);

/// A deferred dispatch and its [`QueueStamp`].
struct QueuedDispatch {
    payload: CiDispatchPayload,
    stamp: QueueStamp,
}

struct CiState {
    /// dispatch_id → cancel token for the running build.
    running: HashMap<String, CancellationToken>,
    /// Deferred (at-cap, or below headroom) dispatches, FIFO.
    queued: VecDeque<QueuedDispatch>,
    /// A [`spawn_headroom_waker`] task is in flight. At most one, ever —
    /// otherwise a box that stays under the headroom threshold would accrue one
    /// sleeping task per redelivered dispatch.
    waker_armed: bool,
    /// A [`spawn_queue_keeper`] task is in flight. At most one, and it disarms
    /// itself (under this lock) the tick it finds the queue empty.
    keeper_armed: bool,
}

impl CiState {
    fn new() -> Self {
        Self {
            running: HashMap::new(),
            queued: VecDeque::new(),
            waker_armed: false,
            keeper_armed: false,
        }
    }
}

fn ci_state() -> &'static Mutex<CiState> {
    static STATE: OnceLock<Mutex<CiState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(CiState::new()))
}

/// How often the queue-keeper renews coord's lease on every queued dispatch.
///
/// Coord's lease is 15 minutes and, before the keeper, a queued dispatch
/// renewed it NEVER: the runner contacted coord about a deferred dispatch only
/// once it was admitted, so any deferral longer than the lease was swept `lost`
/// with `started_at` NULL (plan
/// `2026-09-27-ci-node-shadow-dispatch-never-passes-checkout-race-lost-leases-unfiltered-selection`
/// Phase 3). One minute gives fifteen renewals per lease.
pub(crate) const QUEUE_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// Coord's dispatch lease (`ci_dispatch::LEASE_SECS`). Without queue
/// heartbeats a queued dispatch's lease runs out this long after coord created
/// it (or last renewed it), and coord's sweeper marks it `lost` within the
/// minute after; see [`UNRENEWED_HOLD`] for how far inside it the runner lets go.
///
/// MUST MATCH coord's `LEASE_SECS`. Coord's own values are authoritative
/// wherever coord sends them — `lease_expires_at` places the first renew-by
/// deadline and `queued_renewal_deadline` the queue ceiling with no runner
/// constant at all — but a successful renewal carries no new expiry, so the
/// renew-by deadline it moves to ([`UNRENEWED_HOLD`] after the renewing tick)
/// and the arrival-based cap on `lease_expires_at` both assume this length.
pub(crate) const COORD_LEASE: Duration = Duration::from_secs(15 * 60);

/// How far inside coord's queued-renewal ceiling the runner releases.
///
/// Applied to coord's own deadlines when the payload carries them
/// (`lease_expires_at`, `queued_renewal_deadline`), and otherwise to the
/// runner's arrival-based estimate of them. Coord measures its ceiling from
/// `created_at`; that estimate measures queue time from when the runner first
/// queued the dispatch, which is later by the delivery lag. The margin
/// absorbs that lag plus one [`QUEUE_HEARTBEAT_INTERVAL`] (the
/// release is only checked once per tick) plus one
/// [`QUEUE_HEARTBEAT_BATCH_TIMEOUT`] (the longest a tick's renewals run), with
/// room to spare: five minutes against a lag of seconds and 90 s of tick.
pub(crate) const QUEUE_RELEASE_MARGIN: Duration = Duration::from_secs(5 * 60);

/// The longest the runner holds a queued dispatch past coord's last renewal of
/// its lease (or past enqueue, before any): one [`COORD_LEASE`] less
/// [`QUEUE_RELEASE_MARGIN`] — 10 minutes.
///
/// Why inside the lease and not AT it: coord's sweeper (every 60 s) marks an
/// unrenewed row `lost` once its lease expires, and from then on a release is
/// answered `409 dispatch_terminal` and the reason is discarded. A release at
/// the one-lease mark therefore always arrives after the sweep — the runner's
/// clock starts later than coord's `created_at`, never earlier — so it could
/// not do the one thing it exists for, which is to put a reason in the ledger
/// and hand the work back while coord can still re-dispatch it. The margin
/// absorbs the delivery lag, one keeper tick, and the release POST itself.
pub(crate) const UNRENEWED_HOLD: Duration = Duration::from_secs(10 * 60);

/// How long after ARRIVAL this runner may hold `payload` in its admission
/// queue before RELEASING it back to coord as
/// `cancelled`/[`DEFERRED_PAST_LEASE_REASON`]. When coord sends
/// `queued_renewal_deadline` that, less [`QUEUE_RELEASE_MARGIN`], is the
/// authoritative bound and this only caps it ([`QueueStamp::on_arrival`]).
/// (Independently, [`UNRENEWED_HOLD`] bounds the hold from the last renewal.)
///
/// The goal is that queued work eventually BUILDS, not that its failure gets a
/// better name. So a dispatch whose coord renews queued leases is held for the
/// longest wait coord will honour: its advertised `queued_renewal_max_age_secs`
/// less [`QUEUE_RELEASE_MARGIN`] — 25 minutes against coord's 30. That outlasts
/// a build ahead of it running past one lease, which a one-lease release never
/// could: such a dispatch would only have ended `cancelled` instead of `lost`.
///
/// A dispatch from a coord that advertises no queued phase gets no heartbeat,
/// so its lease lapses one [`COORD_LEASE`] after creation whatever the runner
/// does; it is released at [`UNRENEWED_HOLD`], while the row is still live, so
/// the ledger carries a reason instead of a reason-free `lost`. The same
/// applies to a coord that advertises the phase but no ceiling, since the
/// runner then cannot know the limit it is inside.
///
/// Never below [`UNRENEWED_HOLD`]: a ceiling shorter than that would release
/// work the lease alone would have kept. Never above
/// [`MAX_ADVERTISED_QUEUE_CEILING`]: the ceiling is payload-derived, and an
/// absurd one (a corrupt payload, a units bug in coord) must neither overflow
/// `Instant` arithmetic nor pin a dispatch in the queue indefinitely.
pub(crate) fn queue_release_after(payload: &CiDispatchPayload) -> Duration {
    match (
        payload.coord_accepts_queue_heartbeat(),
        payload.queued_renewal_max_age_secs,
    ) {
        (true, Some(ceiling)) => Duration::from_secs(ceiling)
            .min(MAX_ADVERTISED_QUEUE_CEILING)
            .saturating_sub(QUEUE_RELEASE_MARGIN)
            .max(UNRENEWED_HOLD),
        _ => UNRENEWED_HOLD,
    }
}

/// The largest advertised `queued_renewal_max_age_secs` the runner believes:
/// 24 hours, against coord's 30 minutes. Anything above it is clamped to it
/// by [`queue_release_after`] — `renew_by` still releases a dispatch coord
/// stops renewing, so the clamp only bounds how long a renewed one may wait.
pub(crate) const MAX_ADVERTISED_QUEUE_CEILING: Duration = Duration::from_secs(24 * 60 * 60);

/// The `summary.reason` a released queued dispatch carries. A machine token,
/// not prose, so the ledger can be counted by it.
pub(crate) const DEFERRED_PAST_LEASE_REASON: &str = "deferred_past_lease";

/// The `summary.reason` for a queued dispatch released because coord refused
/// its lease renewal (past coord's queued-age ceiling).
pub(crate) const QUEUED_RENEWAL_REFUSED_REASON: &str = "queued_renewal_refused";

/// The `summary.reason` for a queued dispatch released because its lease went
/// [`UNRENEWED_HOLD`] without a successful renewal (coord unreachable, no
/// device credential …), so coord is about to sweep it.
pub(crate) const QUEUED_RENEWAL_LAPSED_REASON: &str = "queued_renewal_lapsed";

/// Why `q` must leave the queue as of `now`, if it must. Each deadline is due
/// AT its instant (`<= now`). The queue deadline is checked first, so a
/// dispatch that was never renewable (where both deadlines coincide) is named
/// [`DEFERRED_PAST_LEASE_REASON`].
fn expiry_reason(q: &QueuedDispatch, now: Instant) -> Option<&'static str> {
    if q.stamp.release_at <= now {
        Some(DEFERRED_PAST_LEASE_REASON)
    } else if q.stamp.renew_by <= now {
        Some(QUEUED_RENEWAL_LAPSED_REASON)
    } else {
        None
    }
}

/// Remove and return every queued dispatch whose [`expiry_reason`] fires as of
/// `now`, with that reason, preserving the FIFO order of the rest. Pure over
/// the injected clock, so the release rule is unit-tested without a timer.
fn take_expired(
    queue: &mut VecDeque<QueuedDispatch>,
    now: Instant,
) -> Vec<(QueuedDispatch, &'static str)> {
    let mut expired = Vec::new();
    let mut kept = VecDeque::with_capacity(queue.len());
    for q in queue.drain(..) {
        match expiry_reason(&q, now) {
            Some(reason) => expired.push((q, reason)),
            None => kept.push_back(q),
        }
    }
    *queue = kept;
    expired
}

/// Push a deferred dispatch and arm the queue-keeper if none is running.
/// `stamp` is `Some` for a RE-queue, which keeps both of its deadlines; `None`
/// stamps it on arrival. Returns `(queue depth, keeper newly armed)`; the
/// caller spawns the keeper OUTSIDE the lock.
fn enqueue_locked(
    state: &mut CiState,
    payload: CiDispatchPayload,
    stamp: Option<QueueStamp>,
) -> (usize, bool) {
    let stamp = stamp
        .unwrap_or_else(|| QueueStamp::on_arrival(&payload, Instant::now(), chrono::Utc::now()));
    state.queued.push_back(QueuedDispatch { payload, stamp });
    let arm = !state.keeper_armed;
    if arm {
        state.keeper_armed = true;
    }
    (state.queued.len(), arm)
}

/// What the queue-keeper does to coord. A trait so one keeper pass can be
/// driven in a test against a fake, with no coord and no device credential.
trait QueueTransport {
    /// Renew coord's lease on one queued dispatch.
    async fn heartbeat(&self, coord_base: &str, dispatch_id: &str) -> reporting::QueueHeartbeat;
    /// Hand one queued dispatch back to coord as `cancelled` with `reason`.
    fn release(&self, payload: &CiDispatchPayload, reason: &str);
}

/// The production transport: the device-JWT progress route and the
/// fire-and-forget cancelled result.
struct CoordQueueTransport;

impl QueueTransport for CoordQueueTransport {
    async fn heartbeat(&self, coord_base: &str, dispatch_id: &str) -> reporting::QueueHeartbeat {
        reporting::post_queued_heartbeat(coord_base, dispatch_id).await
    }

    fn release(&self, payload: &CiDispatchPayload, reason: &str) {
        if let Some(base) = report_base(payload) {
            reporting::post_cancelled_result_detached(
                base,
                payload.dispatch_id.clone(),
                reason.to_string(),
            );
        }
    }
}

/// One queue-keeper pass over `state`: release what has waited past its own
/// deadline, then renew coord's lease on each dispatch whose coord advertised
/// the queued phase. Returns `true` when the queue was found empty and the
/// keeper DISARMED — the caller's loop must then exit.
///
/// A renewal that reads back a TERMINAL state (coord swept or cancelled the
/// dispatch) drops it from the queue silently — the ledger is already settled,
/// and building it would be work nobody will read. A REFUSED renewal releases
/// the dispatch, but only if it is still queued: it may have been admitted
/// while the POST was in flight, and a running build is not the queue's to
/// release. A renewal that FAILS keeps the dispatch queued and does not move
/// its `renew_by`; the next tick retries, and `renew_by` releases it before
/// coord's sweeper would abandon it. A `Renewed` reply moves `renew_by` to
/// `now` + [`UNRENEWED_HOLD`] — `now` being the tick's START, which is no
/// later than coord's own renewal stamp, so the runner's view of the lease
/// errs short.
///
/// Renewals are sent concurrently, at most [`QUEUE_HEARTBEAT_CONCURRENCY`] in
/// flight, so a queue of N against an unreachable coord costs `ceil(N / 8)`
/// heartbeat timeouts (each a request timeout plus at most one credential
/// re-mint) rather than N. That is still unbounded in N, so the whole batch is
/// cut off at [`QUEUE_HEARTBEAT_BATCH_TIMEOUT`]: a renewal that has not answered
/// by then is dropped and counts as NOT renewed this tick (its `renew_by`
/// does not move), exactly like a `Failed` one. The tick therefore never
/// outlasts the batch bound, which [`UNRENEWED_HOLD`]'s margin is sized for.
///
/// The lock is held only to snapshot and to mutate; `report_base` (which may
/// read the profile) and every network call run with it released.
async fn keeper_tick<T: QueueTransport>(
    state: &Mutex<CiState>,
    transport: &T,
    now: Instant,
) -> bool {
    use futures::StreamExt as _;
    let (expired, renew, emptied) = {
        let mut guard = state.lock().unwrap();
        let expired = take_expired(&mut guard.queued, now);
        // Only dispatches whose coord advertised the queued phase are renewed;
        // see `CiDispatchPayload::coord_accepts_queue_heartbeat`.
        let renew: Vec<CiDispatchPayload> = guard
            .queued
            .iter()
            .filter(|q| q.payload.coord_accepts_queue_heartbeat())
            .map(|q| q.payload.clone())
            .collect();
        let emptied = guard.queued.is_empty();
        if emptied {
            // Disarmed under the same lock a push arms under, so a dispatch
            // queued after this point spawns a fresh keeper and this one exits
            // after its final releases.
            guard.keeper_armed = false;
        }
        (expired, renew, emptied)
    };
    for (q, reason) in expired {
        warn!(
            "ci_node: releasing dispatch {} for {} after {:?} queued on this device \
             (queue deadline {:?} after arrival, renew-by {:?} after arrival) — \
             reporting cancelled/{reason} so coord can re-dispatch it",
            q.payload.dispatch_id,
            q.payload.repo,
            now.saturating_duration_since(q.stamp.queued_since),
            q.stamp
                .release_at
                .saturating_duration_since(q.stamp.queued_since),
            q.stamp
                .renew_by
                .saturating_duration_since(q.stamp.queued_since)
        );
        transport.release(&q.payload, reason);
    }
    let targets: Vec<(String, String)> = renew
        .iter()
        .filter_map(|payload| match report_base(payload) {
            Some(base) => Some((payload.dispatch_id.clone(), base)),
            None => {
                warn!(
                    "ci_node: no coord base to renew queued dispatch {}",
                    payload.dispatch_id
                );
                None
            }
        })
        .collect();
    let sent = targets.len();
    let mut answered = 0usize;
    let mut replies = futures::stream::iter(targets)
        .map(|(dispatch_id, base)| async move {
            let reply = transport.heartbeat(&base, &dispatch_id).await;
            (dispatch_id, reply)
        })
        .buffer_unordered(QUEUE_HEARTBEAT_CONCURRENCY);
    let batch_deadline = tokio::time::Instant::now() + QUEUE_HEARTBEAT_BATCH_TIMEOUT;
    loop {
        let (dispatch_id, reply) =
            match tokio::time::timeout_at(batch_deadline, replies.next()).await {
                Ok(Some(next)) => next,
                Ok(None) => break,
                Err(_) => {
                    warn!(
                        "ci_node: {} of {sent} queued-lease renewals unanswered after \
                         {QUEUE_HEARTBEAT_BATCH_TIMEOUT:?} — dropping them; they count \
                         as not renewed this tick",
                        sent - answered
                    );
                    break;
                }
            };
        answered += 1;
        let dispatch_id = dispatch_id.as_str();
        match reply {
            reporting::QueueHeartbeat::Renewed => {
                mark_renewed(state, dispatch_id, now);
                debug!("ci_node: renewed coord lease on queued dispatch {dispatch_id}");
            }
            reporting::QueueHeartbeat::Terminal(ledger_state) => {
                info!(
                    "ci_node: coord reports queued dispatch {dispatch_id} already \
                     {ledger_state} — dropping it from the queue"
                );
                remove_queued(state, dispatch_id);
            }
            reporting::QueueHeartbeat::Refused => {
                if let Some(q) = remove_queued(state, dispatch_id) {
                    warn!(
                        "ci_node: coord refused the lease renewal for queued dispatch \
                         {dispatch_id} — releasing it as \
                         cancelled/{QUEUED_RENEWAL_REFUSED_REASON}"
                    );
                    transport.release(&q.payload, QUEUED_RENEWAL_REFUSED_REASON);
                }
            }
            reporting::QueueHeartbeat::Failed(why) => {
                warn!(
                    "ci_node: lease renewal for queued dispatch {dispatch_id} failed \
                     ({why}); retrying next tick"
                );
            }
        }
    }
    emptied
}

/// The queue-keeper task: [`run_queue_keeper`] over the process state and the
/// production transport.
fn spawn_queue_keeper() {
    tokio::spawn(run_queue_keeper(ci_state(), CoordQueueTransport));
}

/// One [`keeper_tick`] every [`QUEUE_HEARTBEAT_INTERVAL`] until a tick finds the
/// queue empty, the first one interval after arming.
///
/// FIXED-RATE, not sleep-after-tick: a tick against an unreachable coord takes
/// up to [`QUEUE_HEARTBEAT_BATCH_TIMEOUT`], and sleeping a full interval AFTER
/// it would stretch the period to interval + tick duration — the release check
/// then runs late by that much every minute, eating the margin
/// [`UNRENEWED_HOLD`] keeps inside coord's lease. `MissedTickBehavior::Delay`
/// means a tick that somehow overran the interval is followed by the next one
/// a full interval later rather than by a burst of catch-up ticks.
async fn run_queue_keeper<T: QueueTransport>(state: &Mutex<CiState>, transport: T) {
    let mut ticks = tokio::time::interval_at(
        tokio::time::Instant::now() + QUEUE_HEARTBEAT_INTERVAL,
        QUEUE_HEARTBEAT_INTERVAL,
    );
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        if keeper_tick(state, &transport, Instant::now()).await {
            return;
        }
    }
}

/// Most queue renewals in flight at once (see [`keeper_tick`]).
pub(crate) const QUEUE_HEARTBEAT_CONCURRENCY: usize = 8;

/// The longest one tick's renewal batch may take (see [`keeper_tick`]).
/// Half an interval: the tick always finishes before the next is due, and
/// `UNRENEWED_HOLD + QUEUE_HEARTBEAT_INTERVAL + QUEUE_HEARTBEAT_BATCH_TIMEOUT`
/// stays inside [`COORD_LEASE`].
pub(crate) const QUEUE_HEARTBEAT_BATCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Record a successful lease renewal at `at` on a dispatch that is still
/// queued: its renew-by deadline becomes `at` + [`UNRENEWED_HOLD`], never
/// earlier than it was. One admitted (or dropped) while the POST was in
/// flight is left alone — it is no longer the queue's.
fn mark_renewed(state: &Mutex<CiState>, dispatch_id: &str, at: Instant) {
    let mut guard = state.lock().unwrap();
    if let Some(q) = guard
        .queued
        .iter_mut()
        .find(|q| q.payload.dispatch_id == dispatch_id)
    {
        q.stamp.renew_by = q.stamp.renew_by.max(at + UNRENEWED_HOLD);
    }
}

/// Remove one dispatch from the queue, returning it if it was there.
fn remove_queued(state: &Mutex<CiState>, dispatch_id: &str) -> Option<QueuedDispatch> {
    let mut guard = state.lock().unwrap();
    let pos = guard
        .queued
        .iter()
        .position(|q| q.payload.dispatch_id == dispatch_id)?;
    guard.queued.remove(pos)
}

/// `(running, queued)` — this device's live CI occupancy, for the A1 resource
/// sample. The sample reports the same numbers admission decides on, so the
/// dashboard cannot show a device idle while it is deferring work.
pub(crate) fn occupancy() -> (usize, usize) {
    let state = ci_state().lock().unwrap();
    (state.running.len(), state.queued.len())
}

/// Coord base for reporting on a payload (payload-pinned URL first, profile
/// fallback).
fn report_base(payload: &CiDispatchPayload) -> Option<String> {
    let pinned = payload.coord_http_url.trim();
    if !pinned.is_empty() {
        return Some(pinned.trim_end_matches('/').to_string());
    }
    qontinui_runner_lib::profiles::connected_coord_base()
}

fn reject(payload: &CiDispatchPayload, reason: String) {
    warn!(
        "ci_node: rejecting dispatch {} for {}: {reason}",
        payload.dispatch_id, payload.repo
    );
    if let Some(base) = report_base(payload) {
        reporting::post_cancelled_result_detached(base, payload.dispatch_id.clone(), reason);
    }
}

/// Entry point from the WS subscription for `build_requested`.
pub(crate) fn submit(payload: CiDispatchPayload) {
    admit(payload, None);
}

/// Admission proper. `stamp` is `Some` when this is a RE-admission of a
/// dispatch popped off the queue, so a second deferral keeps its first stamp.
fn admit(payload: CiDispatchPayload, stamp: Option<QueueStamp>) {
    // Identifier safety FIRST: an unsafe dispatch_id can't even be reported
    // (it rides the result URL path), so it is dropped with a log only.
    if !super::dispatch_id_is_safe(&payload.dispatch_id) {
        warn!(
            "ci_node: dropping dispatch with unsafe dispatch_id (len={})",
            payload.dispatch_id.len()
        );
        return;
    }
    if !super::repo_slug_is_safe(&payload.repo) {
        reject(&payload, format!("unsafe repo slug {:?}", payload.repo));
        return;
    }

    let settings = crate::settings::get_ci_node_settings();
    // Probed OUTSIDE the state lock: `sysinfo` refreshes touch the OS, and
    // holding the admission mutex across that would serialise every dispatch
    // behind one machine probe.
    let headroom = probe_headroom();
    // The capacity is resolved here, once, for the same reason: an unset
    // `max_concurrent_builds` resolves to the host suggestion, which probes the
    // host. `admission_decision`, the under-lock re-check and the executor's
    // host share all use this one number.
    let host = super::host_sizing::probe();
    let max_concurrent = settings.effective_max_concurrent_builds_for(host);
    let decision = {
        let state = ci_state().lock().unwrap();
        // Dedup: a dispatch already running or queued here is a duplicate
        // delivery (WS replay) — ignore it rather than double-building.
        if state.running.contains_key(&payload.dispatch_id)
            || state
                .queued
                .iter()
                .any(|q| q.payload.dispatch_id == payload.dispatch_id)
        {
            info!(
                "ci_node: duplicate dispatch {} ignored (already running/queued)",
                payload.dispatch_id
            );
            return;
        }
        admission_decision(
            &settings,
            max_concurrent,
            &payload.repo,
            state.running.len(),
            headroom,
        )
    };

    match decision {
        Admission::Reject(reason) => reject(&payload, reason),
        Admission::Defer => {
            let (dispatch_id, repo) = (payload.dispatch_id.clone(), payload.repo.clone());
            let (depth, needs_waker, needs_keeper) = {
                let mut state = ci_state().lock().unwrap();
                let (_, needs_keeper) = enqueue_locked(&mut state, payload, stamp);
                // An at-cap defer is drained by `on_build_finished` — something
                // is running, so something will finish. A HEADROOM defer has no
                // such guarantee: with nothing running, nothing will ever
                // finish, and the dispatch would sit in the queue until its
                // lease expired with no local trace. Arm a waker for exactly
                // that case.
                let needs = state.running.is_empty() && !state.waker_armed;
                if needs {
                    state.waker_armed = true;
                }
                (state.queued.len(), needs, needs_keeper)
            };
            info!(
                "ci_node: deferring dispatch {dispatch_id} for {repo} (queue depth {depth}, \
                 waker_armed={needs_waker}) — at cap or below live headroom {headroom:?}"
            );
            if needs_waker {
                spawn_headroom_waker();
            }
            if needs_keeper {
                spawn_queue_keeper();
            }
        }
        Admission::Proceed => start_build(payload, settings, host, max_concurrent, stamp),
    }
}

/// How long a headroom-deferred dispatch waits before we re-test the box.
///
/// Long enough that a defer is not a busy-loop against `sysinfo`, short enough
/// that a spike which clears in a minute costs a minute. Memory pressure here
/// is typically transient — that is the whole reason this arm defers instead of
/// rejecting.
const HEADROOM_RETRY_SECS: u64 = 60;

/// Re-test admission for the head of the queue after [`HEADROOM_RETRY_SECS`].
///
/// Only ever one in flight (`waker_armed`). It disarms *before* re-submitting,
/// so a dispatch that defers again immediately re-arms rather than being
/// stranded by its own predecessor's flag.
fn spawn_headroom_waker() {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(HEADROOM_RETRY_SECS)).await;
        headroom_retry_in(ci_state(), &LiveReadmission);
    });
}

/// The headroom waker's body over injected state: disarm, then re-admit the
/// head of the queue WITH its stamp if nothing is running.
fn headroom_retry_in<R: Readmit>(state: &Mutex<CiState>, readmit: &R) {
    let next = {
        let mut state = state.lock().unwrap();
        state.waker_armed = false;
        // If a build started in the meantime, `on_build_finished` owns the
        // drain again and this waker has nothing to do.
        if state.running.is_empty() {
            state.queued.pop_front()
        } else {
            None
        }
    };
    if let Some(q) = next {
        info!(
            "ci_node: re-testing headroom for deferred dispatch {}",
            q.payload.dispatch_id
        );
        readmit.readmit(q.payload, q.stamp);
    }
}

/// Re-admission of a dispatch popped off the queue. It takes the stamp as a
/// required argument so that no drain path can re-admit a queued dispatch as
/// if it were fresh (which would reset both of its clocks); a trait so the
/// drain paths are tested against a recorder instead of the live admission.
trait Readmit {
    fn readmit(&self, payload: CiDispatchPayload, stamp: QueueStamp);
}

/// The production re-admission: [`admit`] with the carried stamp.
struct LiveReadmission;

impl Readmit for LiveReadmission {
    fn readmit(&self, payload: CiDispatchPayload, stamp: QueueStamp) {
        admit(payload, Some(stamp));
    }
}

/// Start one admitted build: disk floor gate, then spawn the executor task
/// with a fresh cancel token registered under the dispatch_id.
///
/// `host` and `max_concurrent` are the probe and resolved capacity `submit`
/// admitted against; they are threaded through so the re-check and the
/// executor's per-dispatch host share use the same N. `stamp` is carried for
/// the slot-race re-defer below, so it keeps the dispatch's original stamp.
fn start_build(
    payload: CiDispatchPayload,
    settings: CiNodeSettings,
    host: super::host_sizing::HostCapacity,
    max_concurrent: u32,
    stamp: Option<QueueStamp>,
) {
    let Some(root) = crate::agent_runtime::qontinui_root_dir() else {
        reject(
            &payload,
            "QONTINUI_ROOT not resolvable on this device".to_string(),
        );
        return;
    };

    // Is this build path on a declared external volume? `None` on a machine
    // with no declaration — which keeps every arm below byte-identical to the
    // pre-Phase-5 behaviour. Probed ONCE and threaded through, so the gate
    // decision and the log line below cannot disagree about the volume's state.
    let external = crate::external_volume::external_state_for(&root);
    let free_gb = free_disk_gb_for(&root);

    match disk_gate(free_gb, settings.min_free_disk_gb, external.as_ref(), &root) {
        DiskGate::Reject(reason) => {
            reject(&payload, reason);
            return;
        }
        DiskGate::Ok => match free_gb {
            Some(free) => info!(
                "ci_node: disk gate ok ({free} GiB free ≥ {} GiB floor{})",
                settings.min_free_disk_gb,
                if external.is_some() {
                    ", on the declared external volume"
                } else {
                    ""
                }
            ),
            // Only reachable for an INTERNAL path — `disk_gate` rejects an
            // unresolvable probe on an external one.
            None => warn!(
                "ci_node: could not resolve free disk for {} — proceeding (fail-open guard)",
                root.display()
            ),
        },
    }

    // Memory floor (plan §4.6): a build admitted onto a starved box would OOM
    // the developer's own work before it OOMs itself. Reads free COMMIT — the
    // same quantity the supervisor and `cargo-guard.sh` guard on and the same
    // one the A1 snapshot publishes; see `MIN_FREE_COMMIT_GB`.
    match crate::fleet::resource_sample::available_commit_bytes() {
        Some(avail) if commit_below_floor(avail, MIN_FREE_COMMIT_GB) => {
            reject(
                &payload,
                format!(
                    "free commit {} GiB is below the {MIN_FREE_COMMIT_GB} GiB floor",
                    avail / (1024 * 1024 * 1024)
                ),
            );
            return;
        }
        Some(_) => {}
        None => warn!("ci_node: could not resolve free commit — proceeding (fail-open guard)"),
    }

    let token = CancellationToken::new();
    let payload = match claim_slot_or_requeue(ci_state(), payload, stamp, max_concurrent, &token) {
        SlotClaim::Claimed(payload) => *payload,
        SlotClaim::Requeued {
            dispatch_id,
            needs_keeper,
        } => {
            info!("ci_node: slot taken while gating — deferring dispatch {dispatch_id}");
            if needs_keeper {
                spawn_queue_keeper();
            }
            return;
        }
    };

    let dispatch_id = payload.dispatch_id.clone();
    info!(
        "ci_node: starting dispatch {} repo={} sha={} check_name={:?}",
        dispatch_id, payload.repo, payload.head_sha, payload.check_name
    );
    tokio::spawn(async move {
        super::executor::run_dispatch(payload, root, token, host, max_concurrent).await;
        on_build_finished(&dispatch_id);
    });
}

/// What [`claim_slot_or_requeue`] did with a dispatch.
enum SlotClaim {
    /// A slot was free: the dispatch is now in `running` under the token.
    /// Boxed: the payload dwarfs the other variant.
    Claimed(Box<CiDispatchPayload>),
    /// The cap filled while it was being gated: it is back in the queue, with
    /// the stamp it came in with.
    Requeued {
        dispatch_id: String,
        needs_keeper: bool,
    },
}

/// Re-check the cap under the lock (admission's read was unlocked in-between
/// for the disk probe) and either register the build or re-queue it. A
/// re-queued dispatch keeps `stamp`, so the slot race cannot reset its clocks.
fn claim_slot_or_requeue(
    state: &Mutex<CiState>,
    payload: CiDispatchPayload,
    stamp: Option<QueueStamp>,
    max_concurrent: u32,
    token: &CancellationToken,
) -> SlotClaim {
    let mut state = state.lock().unwrap();
    if state.running.len() >= max_concurrent.max(1) as usize {
        let dispatch_id = payload.dispatch_id.clone();
        let (_, needs_keeper) = enqueue_locked(&mut state, payload, stamp);
        return SlotClaim::Requeued {
            dispatch_id,
            needs_keeper,
        };
    }
    state
        .running
        .insert(payload.dispatch_id.clone(), token.clone());
    SlotClaim::Claimed(Box::new(payload))
}

/// Build-finished hook: free the slot, then re-admit the oldest deferred
/// dispatch (fresh settings + fresh headroom + disk gate — conditions may have
/// changed while it waited).
fn on_build_finished(dispatch_id: &str) {
    on_build_finished_in(ci_state(), dispatch_id, &LiveReadmission);
}

/// [`on_build_finished`] over injected state: the popped dispatch is
/// re-admitted WITH its stamp.
fn on_build_finished_in<R: Readmit>(state: &Mutex<CiState>, dispatch_id: &str, readmit: &R) {
    let next = {
        let mut state = state.lock().unwrap();
        state.running.remove(dispatch_id);
        state.queued.pop_front()
    };
    if let Some(q) = next {
        info!(
            "ci_node: slot freed by {} — re-admitting deferred dispatch {}",
            dispatch_id, q.payload.dispatch_id
        );
        readmit.readmit(q.payload, q.stamp);
    }
}

/// Entry point from the WS subscription for `build_cancelled`.
pub(crate) fn cancel(dispatch_id: &str) {
    let (was_running, dequeued) = {
        let mut state = ci_state().lock().unwrap();
        if let Some(token) = state.running.get(dispatch_id) {
            token.cancel();
            (true, None)
        } else {
            let before = state.queued.len();
            let mut removed: Option<CiDispatchPayload> = None;
            state.queued.retain(|q| {
                if q.payload.dispatch_id == dispatch_id {
                    removed = Some(q.payload.clone());
                    false
                } else {
                    true
                }
            });
            (before != state.queued.len(), removed)
        }
    };
    if was_running && dequeued.is_none() {
        info!("ci_node: cancel requested for running dispatch {dispatch_id}");
    } else if let Some(payload) = dequeued {
        info!("ci_node: cancelled queued dispatch {dispatch_id} before start");
        if let Some(base) = report_base(&payload) {
            reporting::post_cancelled_result_detached(
                base,
                payload.dispatch_id,
                "cancelled by coord while queued on this device".to_string(),
            );
        }
    } else {
        info!("ci_node: cancel for unknown dispatch {dispatch_id} (not running/queued here)");
    }
}

/// Ceiling on the shutdown-path `ci_state` acquisition. See
/// [`cancel_all_for_shutdown`] for why giving up is safe.
const SHUTDOWN_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(250);

/// App-shutdown seam: cancel every running build's token and drop the
/// queue. Called from the `main.rs` window-close handler; the Windows Job
/// Object (kill-on-close) is the hard backstop for the process trees.
pub(crate) fn cancel_all_for_shutdown() {
    // BOUNDED + poison-recovering — Phase 2 step 5 asked for BOTH, and this
    // site only had the second.
    //
    // Poison-recovering: a `.unwrap()` here turned any earlier panic while
    // holding `ci_state` into a SECOND panic on the shutdown thread — which,
    // while this ran inline on the tao/UI thread, took the event loop down
    // with it. The state behind a poisoned lock is perfectly usable for what
    // this does.
    //
    // Bounded: `lock()` is unbounded, so a dispatch thread holding `ci_state`
    // while it does something slow parks the whole teardown here with no
    // ceiling. Cancellation is a courtesy — the Windows Job Object
    // (kill-on-close) is the hard backstop for the build process trees, and
    // coord's dispatch-lease sweeper covers a result that never makes it out —
    // so giving up is strictly better than overrunning the budget.
    let Some(mut state) =
        crate::safe_lock::lock_with_deadline(ci_state(), "ci_node ci_state", SHUTDOWN_LOCK_TIMEOUT)
    else {
        warn!(
            "ci_node: shutdown — could not acquire ci_state within {SHUTDOWN_LOCK_TIMEOUT:?}; \
             leaving cancellation to the Job Object and coord's lease sweeper"
        );
        return;
    };
    for (id, token) in state.running.iter() {
        info!("ci_node: shutdown — cancelling dispatch {id}");
        token.cancel();
    }
    state.queued.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued_payload(id: &str) -> CiDispatchPayload {
        serde_json::from_value(serde_json::json!({
            "dispatch_id": id,
            "repo": "qontinui/qontinui-runner",
            "head_sha": "0123456789abcdef0123456789abcdef01234567",
            "fetch_url": "https://coord.example/git/qontinui/qontinui-runner.git",
            "candidate_ref": "refs/heads/merge-candidate/x",
            "check_name": "qontinui-ci-node/shadow",
            "manifest_path": ".qontinui/ci.toml",
            "coord_http_url": "https://coord.example"
        }))
        .expect("fixture payload parses")
    }

    fn advertising(mut p: CiDispatchPayload, ceiling_secs: u64) -> CiDispatchPayload {
        p.progress_phases = vec!["running".into(), "queued".into()];
        p.queued_renewal_max_age_secs = Some(ceiling_secs);
        p
    }

    fn queued_at(p: CiDispatchPayload, queued_since: Instant) -> QueuedDispatch {
        QueuedDispatch {
            stamp: QueueStamp::fresh(&p, queued_since),
            payload: p,
        }
    }

    /// REGRESSION (plan 2026-09-27 Phase 3): a deferred dispatch used to sit in
    /// the queue with no coord contact until its lease lapsed. Each entry is
    /// released at ITS OWN deadline, the boundary inclusive, and the survivors
    /// keep FIFO order.
    #[test]
    fn a_dispatch_is_released_at_its_own_deadline_and_the_rest_keep_fifo_order() {
        // "now" sits in the future of every stamp, so no `Instant` subtraction
        // can underflow however recently the host booted.
        let now = Instant::now() + Duration::from_secs(60 * 60);
        let mut q: VecDeque<QueuedDispatch> = VecDeque::new();
        for (p, age_secs) in [
            // Legacy coord: released inside its one lease, at 10 min.
            (queued_payload("legacy-over"), 11 * 60),
            (queued_payload("legacy-edge"), 10 * 60),
            (queued_payload("legacy-kept"), 10 * 60 - 1),
            // Heartbeating coord, 30-min ceiling: 25-min deadline.
            (advertising(queued_payload("renewed-kept"), 1800), 20 * 60),
            (advertising(queued_payload("renewed-over"), 1800), 25 * 60),
            (queued_payload("young"), 0),
        ] {
            let mut entry = queued_at(p, now - Duration::from_secs(age_secs));
            if entry.payload.coord_accepts_queue_heartbeat() {
                // Renewed a minute ago, so only the queue deadline is in play.
                entry.stamp.renew_by = now - Duration::from_secs(60) + UNRENEWED_HOLD;
            }
            q.push_back(entry);
        }
        let released: Vec<(String, &str)> = take_expired(&mut q, now)
            .into_iter()
            .map(|(r, reason)| (r.payload.dispatch_id, reason))
            .collect();
        assert_eq!(
            released,
            [
                ("legacy-over".to_string(), DEFERRED_PAST_LEASE_REASON),
                ("legacy-edge".to_string(), DEFERRED_PAST_LEASE_REASON),
                ("renewed-over".to_string(), DEFERRED_PAST_LEASE_REASON),
            ]
        );
        let kept: Vec<&str> = q.iter().map(|r| r.payload.dispatch_id.as_str()).collect();
        assert_eq!(
            kept,
            ["legacy-kept", "renewed-kept", "young"],
            "FIFO order of the survivors is kept"
        );
    }

    /// The release deadline is derived from the ceiling the dispatching coord
    /// ADVERTISED, and always lands inside it by at least one heartbeat tick —
    /// so the runner releases before coord would start refusing renewals, and
    /// work queued behind a build longer than one lease still gets to build.
    #[test]
    fn the_queue_deadline_is_derived_from_the_advertised_ceiling_and_stays_inside_it() {
        for ceiling in [1800_u64, 3600, 2 * 15 * 60 + 1] {
            let release = queue_release_after(&advertising(queued_payload("x"), ceiling));
            assert!(
                release + QUEUE_HEARTBEAT_INTERVAL < Duration::from_secs(ceiling),
                "release {release:?} + one tick must stay inside the {ceiling}s ceiling"
            );
            assert!(
                release > COORD_LEASE,
                "a renewed dispatch must outwait one lease"
            );
        }
        assert_eq!(
            queue_release_after(&advertising(queued_payload("x"), 1800)),
            Duration::from_secs(25 * 60)
        );
        // No heartbeat → the lease lapses at one lease whatever we do, so the
        // release lands inside it, while the row is still live.
        assert_eq!(
            queue_release_after(&queued_payload("legacy")),
            UNRENEWED_HOLD
        );
        // Phase advertised but no ceiling: the limit is unknown, same hold.
        let mut phase_only = queued_payload("phase-only");
        phase_only.progress_phases = vec!["queued".into()];
        assert_eq!(queue_release_after(&phase_only), UNRENEWED_HOLD);
        // A ceiling under the hold never shortens the wait below it.
        assert_eq!(
            queue_release_after(&advertising(queued_payload("tiny"), 60)),
            UNRENEWED_HOLD
        );
    }

    /// REGRESSION (review LOW 1): a legacy-coord dispatch used to be released
    /// at the one-lease mark from the runner's own (later) clock, which is
    /// always after coord's sweeper has marked the row `lost` — so the release
    /// was answered `409 dispatch_terminal` and the reason never landed. The
    /// hold from the last renewal must end before coord's lease does, with a
    /// keeper tick and the delivery lag to spare.
    #[test]
    fn an_unrenewed_dispatch_is_released_while_coord_still_holds_its_lease() {
        assert_eq!(UNRENEWED_HOLD, COORD_LEASE - QUEUE_RELEASE_MARGIN);
        assert!(
            UNRENEWED_HOLD + QUEUE_HEARTBEAT_INTERVAL + QUEUE_HEARTBEAT_BATCH_TIMEOUT < COORD_LEASE,
            "the release must be sent before the lease expires, even a tick late \
             behind a renewal batch that ran to its bound"
        );
        assert!(
            QUEUE_HEARTBEAT_BATCH_TIMEOUT < QUEUE_HEARTBEAT_INTERVAL,
            "a tick finishes before the next one is due"
        );
    }

    /// A fake coord for [`keeper_tick`]: a scripted heartbeat answer, an
    /// optional "admit this dispatch while the POST is in flight" hook, and a
    /// record of every heartbeat and release.
    struct FakeTransport {
        reply: Mutex<reporting::QueueHeartbeat>,
        admit_during_heartbeat: Option<std::sync::Arc<Mutex<CiState>>>,
        heartbeats: Mutex<Vec<String>>,
        releases: Mutex<Vec<(String, String)>>,
    }

    impl FakeTransport {
        fn new(reply: reporting::QueueHeartbeat) -> Self {
            Self {
                reply: Mutex::new(reply),
                admit_during_heartbeat: None,
                heartbeats: Mutex::new(Vec::new()),
                releases: Mutex::new(Vec::new()),
            }
        }
    }

    impl QueueTransport for FakeTransport {
        async fn heartbeat(&self, _base: &str, dispatch_id: &str) -> reporting::QueueHeartbeat {
            self.heartbeats
                .lock()
                .unwrap()
                .push(dispatch_id.to_string());
            if let Some(state) = &self.admit_during_heartbeat {
                remove_queued(state, dispatch_id);
            }
            self.reply.lock().unwrap().clone()
        }

        fn release(&self, payload: &CiDispatchPayload, reason: &str) {
            self.releases
                .lock()
                .unwrap()
                .push((payload.dispatch_id.clone(), reason.to_string()));
        }
    }

    fn state_with(entries: Vec<QueuedDispatch>) -> std::sync::Arc<Mutex<CiState>> {
        let mut s = CiState::new();
        s.keeper_armed = true;
        s.queued.extend(entries);
        std::sync::Arc::new(Mutex::new(s))
    }

    #[tokio::test]
    async fn the_keeper_disarms_on_an_empty_queue_and_stays_armed_otherwise() {
        let now = Instant::now();
        let empty = state_with(Vec::new());
        let fake = FakeTransport::new(reporting::QueueHeartbeat::Renewed);
        assert!(
            keeper_tick(&empty, &fake, now).await,
            "an empty queue ends the keeper"
        );
        assert!(
            !empty.lock().unwrap().keeper_armed,
            "and disarms it under the lock"
        );

        let busy = state_with(vec![queued_at(advertising(queued_payload("a"), 1800), now)]);
        assert!(!keeper_tick(&busy, &fake, now).await);
        assert!(busy.lock().unwrap().keeper_armed);
        assert_eq!(*fake.heartbeats.lock().unwrap(), ["a"]);
        assert!(fake.releases.lock().unwrap().is_empty());
        assert_eq!(
            busy.lock().unwrap().queued.len(),
            1,
            "a renewal keeps it queued"
        );
    }

    #[tokio::test]
    async fn a_refused_renewal_releases_only_a_dispatch_that_is_still_queued() {
        let now = Instant::now();
        // Still queued when coord refuses: released with the refusal reason.
        let state = state_with(vec![queued_at(advertising(queued_payload("a"), 1800), now)]);
        let fake = FakeTransport::new(reporting::QueueHeartbeat::Refused);
        keeper_tick(&state, &fake, now).await;
        assert_eq!(
            *fake.releases.lock().unwrap(),
            [("a".to_string(), QUEUED_RENEWAL_REFUSED_REASON.to_string())]
        );
        assert!(state.lock().unwrap().queued.is_empty());

        // Admitted while the POST was in flight: the refusal is about a running
        // build now, and nothing is released.
        let state = state_with(vec![queued_at(advertising(queued_payload("b"), 1800), now)]);
        let mut fake = FakeTransport::new(reporting::QueueHeartbeat::Refused);
        fake.admit_during_heartbeat = Some(state.clone());
        keeper_tick(&state, &fake, now).await;
        assert!(fake.releases.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_terminal_read_back_drops_the_dispatch_without_releasing_it() {
        let now = Instant::now();
        let state = state_with(vec![
            queued_at(advertising(queued_payload("gone"), 1800), now),
            // A legacy dispatch is neither renewed nor touched.
            queued_at(queued_payload("legacy"), now),
        ]);
        let fake = FakeTransport::new(reporting::QueueHeartbeat::Terminal("lost".into()));
        keeper_tick(&state, &fake, now).await;
        assert!(
            fake.releases.lock().unwrap().is_empty(),
            "a settled row is not released"
        );
        assert_eq!(
            *fake.heartbeats.lock().unwrap(),
            ["gone"],
            "legacy is never heartbeated"
        );
        let left: Vec<String> = state
            .lock()
            .unwrap()
            .queued
            .iter()
            .map(|q| q.payload.dispatch_id.clone())
            .collect();
        assert_eq!(left, ["legacy"]);
    }

    #[tokio::test]
    async fn an_expired_dispatch_is_released_past_lease_by_the_tick() {
        let now = Instant::now() + Duration::from_secs(60 * 60);
        let state = state_with(vec![queued_at(
            queued_payload("stale"),
            now - Duration::from_secs(16 * 60),
        )]);
        let fake = FakeTransport::new(reporting::QueueHeartbeat::Renewed);
        assert!(keeper_tick(&state, &fake, now).await, "the queue emptied");
        assert_eq!(
            *fake.releases.lock().unwrap(),
            [("stale".to_string(), DEFERRED_PAST_LEASE_REASON.to_string())]
        );
    }

    /// Drive the keeper once a minute (a paused clock: `now` is injected) for
    /// `minutes` ticks after `start`, with coord answering `reply_at(minute)`.
    /// Returns the minute of the first release and its reason.
    async fn run_keeper_minutes(
        state: &Mutex<CiState>,
        fake: &FakeTransport,
        start: Instant,
        minutes: u64,
        reply_at: impl Fn(u64) -> reporting::QueueHeartbeat,
    ) -> Option<(u64, String)> {
        for minute in 1..=minutes {
            *fake.reply.lock().unwrap() = reply_at(minute);
            keeper_tick(state, fake, start + Duration::from_secs(minute * 60)).await;
            if let Some((_, reason)) = fake.releases.lock().unwrap().first() {
                return Some((minute, reason.clone()));
            }
        }
        None
    }

    /// REGRESSION (review LOW 2): the queue deadline ignored whether renewals
    /// were LANDING. With every heartbeat failing (coord unreachable, no device
    /// JWT), coord sweeps the row one lease after its last renewal, yet the
    /// runner held it to the 25-minute deadline and could admit and check out a
    /// build coord had already abandoned. It is now released UNRENEWED_HOLD
    /// after the last successful renewal.
    #[tokio::test]
    async fn a_dispatch_whose_renewals_keep_failing_is_released_before_coord_sweeps_it() {
        let start = Instant::now();
        let failed = || reporting::QueueHeartbeat::Failed("transport: unreachable".into());

        // Never renewed: released at the hold, not at the 25-minute deadline.
        let state = state_with(vec![queued_at(
            advertising(queued_payload("dark"), 1800),
            start,
        )]);
        let fake = FakeTransport::new(failed());
        assert_eq!(
            run_keeper_minutes(&state, &fake, start, 30, |_| failed()).await,
            Some((10, QUEUED_RENEWAL_LAPSED_REASON.to_string()))
        );

        // Renewed through minute 5, dark after: the hold runs from minute 5.
        let state = state_with(vec![queued_at(
            advertising(queued_payload("flaky"), 1800),
            start,
        )]);
        let fake = FakeTransport::new(failed());
        let renewed_until_5 = |m: u64| {
            if m <= 5 {
                reporting::QueueHeartbeat::Renewed
            } else {
                failed()
            }
        };
        assert_eq!(
            run_keeper_minutes(&state, &fake, start, 30, renewed_until_5).await,
            Some((15, QUEUED_RENEWAL_LAPSED_REASON.to_string()))
        );

        // Every renewal lands: held to the advertised deadline, past one lease.
        let state = state_with(vec![queued_at(
            advertising(queued_payload("live"), 1800),
            start,
        )]);
        let fake = FakeTransport::new(reporting::QueueHeartbeat::Renewed);
        assert_eq!(
            run_keeper_minutes(&state, &fake, start, 30, |_| {
                reporting::QueueHeartbeat::Renewed
            })
            .await,
            Some((25, DEFERRED_PAST_LEASE_REASON.to_string()))
        );
    }

    /// Renewals are sent concurrently: with three queued dispatches whose fake
    /// heartbeats each wait for the other two, a sequential keeper never
    /// passes the barrier — and, since the batch is now bounded, would END the
    /// tick with fewer than three heartbeats completed rather than hang, so the
    /// completions are counted.
    #[tokio::test]
    async fn queue_renewals_are_sent_concurrently() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Rendezvous(tokio::sync::Barrier, AtomicUsize);
        impl QueueTransport for Rendezvous {
            async fn heartbeat(&self, _base: &str, _id: &str) -> reporting::QueueHeartbeat {
                self.0.wait().await;
                self.1.fetch_add(1, Ordering::SeqCst);
                reporting::QueueHeartbeat::Renewed
            }
            fn release(&self, _payload: &CiDispatchPayload, _reason: &str) {}
        }
        let now = Instant::now();
        let state = state_with(
            ["a", "b", "c"]
                .into_iter()
                .map(|id| queued_at(advertising(queued_payload(id), 1800), now))
                .collect(),
        );
        let transport = Rendezvous(tokio::sync::Barrier::new(3), AtomicUsize::new(0));
        tokio::time::timeout(
            Duration::from_secs(10),
            keeper_tick(&state, &transport, now),
        )
        .await
        .expect("the tick finishes");
        assert_eq!(
            transport.1.load(Ordering::SeqCst),
            3,
            "all three heartbeats were in flight at once and completed"
        );
    }

    /// REGRESSION (review LOW: unbounded tick): the renewal batch is cut off at
    /// QUEUE_HEARTBEAT_BATCH_TIMEOUT. A renewal that never answers no longer
    /// holds the tick — and so the release check — hostage; it counts as not
    /// renewed, and the ones that did answer are recorded.
    #[tokio::test(start_paused = true)]
    async fn a_hung_renewal_batch_is_cut_off_and_counts_as_not_renewed() {
        struct HangsOn(&'static str);
        impl QueueTransport for HangsOn {
            async fn heartbeat(&self, _base: &str, id: &str) -> reporting::QueueHeartbeat {
                if id == self.0 {
                    std::future::pending::<()>().await;
                }
                reporting::QueueHeartbeat::Renewed
            }
            fn release(&self, _payload: &CiDispatchPayload, _reason: &str) {}
        }
        let queued = Instant::now();
        let now = queued + Duration::from_secs(60);
        let state = state_with(
            ["fast", "hung"]
                .into_iter()
                .map(|id| queued_at(advertising(queued_payload(id), 1800), queued))
                .collect(),
        );
        let started = tokio::time::Instant::now();
        tokio::time::timeout(
            QUEUE_HEARTBEAT_BATCH_TIMEOUT * 2,
            keeper_tick(&state, &HangsOn("hung"), now),
        )
        .await
        .expect("the tick ends at the batch bound, not never");
        assert_eq!(started.elapsed(), QUEUE_HEARTBEAT_BATCH_TIMEOUT);
        let renewed: Vec<(String, Instant)> = state
            .lock()
            .unwrap()
            .queued
            .iter()
            .map(|q| (q.payload.dispatch_id.clone(), q.stamp.renew_by))
            .collect();
        assert_eq!(
            renewed,
            [
                ("fast".to_string(), now + UNRENEWED_HOLD),
                ("hung".to_string(), queued + UNRENEWED_HOLD)
            ],
            "the answered renewal lands; the cut-off one stays queued, unrenewed"
        );
    }

    /// Wraps a shared transport so a test can read its record after handing
    /// the keeper ownership of the wrapper.
    struct Shared<T>(std::sync::Arc<T>);

    impl<T: QueueTransport> QueueTransport for Shared<T> {
        async fn heartbeat(&self, base: &str, id: &str) -> reporting::QueueHeartbeat {
            self.0.heartbeat(base, id).await
        }
        fn release(&self, payload: &CiDispatchPayload, reason: &str) {
            self.0.release(payload, reason)
        }
    }

    /// REGRESSION (review LOW: tick drift): the keeper slept a full interval
    /// AFTER each tick, so a slow tick stretched the period to interval + tick
    /// duration and the release check ran later every minute. Ticks are now
    /// fixed-rate: with every renewal taking 20 s they still start 60 s apart.
    #[tokio::test(start_paused = true)]
    async fn keeper_ticks_are_fixed_rate_however_long_a_tick_takes() {
        struct Slow(Mutex<Vec<tokio::time::Instant>>);
        impl QueueTransport for Slow {
            async fn heartbeat(&self, _base: &str, _id: &str) -> reporting::QueueHeartbeat {
                self.0.lock().unwrap().push(tokio::time::Instant::now());
                tokio::time::sleep(Duration::from_secs(20)).await;
                reporting::QueueHeartbeat::Renewed
            }
            fn release(&self, _payload: &CiDispatchPayload, _reason: &str) {}
        }
        let state = state_with(vec![queued_at(
            advertising(queued_payload("a"), 1800),
            Instant::now(),
        )]);
        let transport = std::sync::Arc::new(Slow(Mutex::new(Vec::new())));
        let start = tokio::time::Instant::now();
        let ran = tokio::time::timeout(
            QUEUE_HEARTBEAT_INTERVAL * 3 + Duration::from_secs(30),
            run_queue_keeper(&state, Shared(transport.clone())),
        )
        .await;
        assert!(ran.is_err(), "a non-empty queue keeps the keeper running");
        let offsets: Vec<Duration> = transport
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|t| t.duration_since(start))
            .collect();
        assert_eq!(
            offsets,
            [
                QUEUE_HEARTBEAT_INTERVAL,
                QUEUE_HEARTBEAT_INTERVAL * 2,
                QUEUE_HEARTBEAT_INTERVAL * 3
            ]
        );
    }

    /// A coord-supplied `lease_expires_at` anchors the hold to when coord
    /// started the lease, not to when the message arrived: a `build_requested`
    /// delivered 6 minutes late (9 minutes of lease left) is released 4 minutes
    /// after arrival, not 10 — while coord still holds it. Without the field
    /// the anchor is arrival, as before.
    #[tokio::test]
    async fn a_late_delivered_dispatch_is_released_by_its_lease_deadline_not_its_arrival() {
        let arrived = Instant::now();
        let wall_arrived = chrono::Utc::now();
        let lease = chrono::Duration::seconds(COORD_LEASE.as_secs() as i64);
        let failed = || reporting::QueueHeartbeat::Failed("transport: unreachable".into());
        let entry_for = |p: CiDispatchPayload| arrived_at(p, arrived, wall_arrived);

        let mut late = advertising(queued_payload("late"), 1800);
        late.lease_expires_at = Some(wall_arrived - chrono::Duration::minutes(6) + lease);
        let state = state_with(vec![entry_for(late)]);
        let fake = FakeTransport::new(failed());
        assert_eq!(
            run_keeper_minutes(&state, &fake, arrived, 30, |_| failed()).await,
            Some((4, QUEUED_RENEWAL_LAPSED_REASON.to_string()))
        );

        let state = state_with(vec![entry_for(advertising(
            queued_payload("no-deadline"),
            1800,
        ))]);
        let fake = FakeTransport::new(failed());
        assert_eq!(
            run_keeper_minutes(&state, &fake, arrived, 30, |_| failed()).await,
            Some((10, QUEUED_RENEWAL_LAPSED_REASON.to_string()))
        );
    }

    /// The entry a dispatch gets on its first enqueue at (`arrived`, `wall`).
    fn arrived_at(
        p: CiDispatchPayload,
        arrived: Instant,
        wall: chrono::DateTime<chrono::Utc>,
    ) -> QueuedDispatch {
        QueuedDispatch {
            stamp: QueueStamp::on_arrival(&p, arrived, wall),
            payload: p,
        }
    }

    /// A wall-clock deadline maps onto the `Instant` clock by ADDING to `now`
    /// only: the margin is taken off, a moment already past maps to `now`
    /// (due), and nothing is ever subtracted from `now`.
    #[test]
    fn a_coord_deadline_maps_to_an_instant_without_subtracting_from_now() {
        let now = Instant::now();
        let wall = chrono::Utc::now();
        let margin = chrono::Duration::seconds(QUEUE_RELEASE_MARGIN.as_secs() as i64);
        assert_eq!(
            margin_inside(wall + margin + chrono::Duration::minutes(7), wall, now),
            Some(now + Duration::from_secs(7 * 60))
        );
        assert_eq!(margin_inside(wall + margin, wall, now), Some(now));
        assert_eq!(
            margin_inside(wall + chrono::Duration::minutes(1), wall, now),
            Some(now),
            "inside the margin is already due"
        );
        assert_eq!(
            margin_inside(wall - chrono::Duration::days(365 * 100), wall, now),
            Some(now),
            "a deadline older than any Instant epoch is simply due"
        );
    }

    /// REGRESSION (review LOW 1): the hold was kept as a "last renewed"
    /// instant reconstructed by SUBTRACTING the lease's age from `now`. On
    /// Windows `Instant`'s epoch is boot, so within one hold of boot that
    /// subtraction failed and was clamped to arrival — an already-expired
    /// dispatch was then held a full UNRENEWED_HOLD more. Deadlines are now
    /// expiry instants built by addition, so `now` here is a bare
    /// `Instant::now()` with no headroom below it.
    ///
    /// What this test does and does not pin: the old bug fired only within
    /// one hold of boot, so on a host up longer than that the LEGACY half
    /// below passes against the old code too and catches nothing. The epoch
    /// case is pinned by the RENEWABLE half (the grace floor, which the old
    /// code had no notion of) and by
    /// `a_coord_deadline_maps_to_an_instant_without_subtracting_from_now`,
    /// which drives `margin_inside` directly with a deadline older than any
    /// `Instant` epoch.
    #[tokio::test]
    async fn an_already_expired_dispatch_is_not_held_a_full_hold_past_arrival() {
        let arrived = Instant::now();
        let wall = chrono::Utc::now();
        let ancient = wall - chrono::Duration::days(365 * 100);

        // Not renewable (legacy coord): due at arrival, released on the first
        // tick that sees it.
        let mut legacy = queued_payload("legacy-expired");
        legacy.lease_expires_at = Some(ancient);
        let entry = arrived_at(legacy, arrived, wall);
        assert_eq!(entry.stamp.renew_by, arrived);
        let mut q: VecDeque<QueuedDispatch> = VecDeque::from([entry]);
        let released: Vec<&str> = take_expired(&mut q, arrived)
            .into_iter()
            .map(|(_, reason)| reason)
            .collect();
        assert_eq!(released, [QUEUED_RENEWAL_LAPSED_REASON]);

        // Renewable: floored at one grace past arrival — coord is asked once,
        // not held for ten minutes on a guess.
        let mut current = advertising(queued_payload("expired"), 1800);
        current.lease_expires_at = Some(ancient);
        let entry = arrived_at(current.clone(), arrived, wall);
        assert_eq!(entry.stamp.renew_by, arrived + FIRST_RENEWAL_GRACE);

        // Coord already swept it: the first tick's renewal reads that back and
        // drops the row, releasing nothing.
        let state = state_with(vec![entry]);
        let fake = FakeTransport::new(reporting::QueueHeartbeat::Terminal("lost".into()));
        assert_eq!(
            run_keeper_minutes(&state, &fake, arrived, 2, |_| {
                reporting::QueueHeartbeat::Terminal("lost".into())
            })
            .await,
            None
        );
        assert_eq!(*fake.heartbeats.lock().unwrap(), ["expired"]);
        assert!(state.lock().unwrap().queued.is_empty());

        // Coord unreachable: released on the second tick, not at minute 10.
        let state = state_with(vec![arrived_at(current, arrived, wall)]);
        let failed = || reporting::QueueHeartbeat::Failed("transport: unreachable".into());
        let fake = FakeTransport::new(failed());
        assert_eq!(
            run_keeper_minutes(&state, &fake, arrived, 30, |_| failed()).await,
            Some((2, QUEUED_RENEWAL_LAPSED_REASON.to_string()))
        );
    }

    /// Coord's `queued_renewal_deadline` is authoritative for the queue
    /// ceiling. A dispatch delivered 20 minutes after coord created it (30-min
    /// ceiling, so coord refuses renewals from 10 minutes after arrival) used
    /// to be held to its arrival-based 25 minutes and end on coord's
    /// `queued_renewal_refused`. It is now released at the deadline less the
    /// margin — minute 5, while every renewal is still accepted — as
    /// `deferred_past_lease`.
    #[tokio::test]
    async fn a_late_delivery_is_released_inside_coords_queued_renewal_deadline() {
        let arrived = Instant::now();
        let wall = chrono::Utc::now();
        let mut late = advertising(queued_payload("late"), 1800);
        late.lease_expires_at = Some(wall - chrono::Duration::minutes(5));
        late.queued_renewal_deadline = Some(wall + chrono::Duration::minutes(10));
        let state = state_with(vec![arrived_at(late, arrived, wall)]);
        let fake = FakeTransport::new(reporting::QueueHeartbeat::Renewed);
        let released = run_keeper_minutes(&state, &fake, arrived, 30, |m| {
            if m < 10 {
                reporting::QueueHeartbeat::Renewed
            } else {
                reporting::QueueHeartbeat::Refused
            }
        })
        .await;
        assert_eq!(
            released,
            Some((5, DEFERRED_PAST_LEASE_REASON.to_string())),
            "released before coord's ceiling, with the queue-deadline reason"
        );
        assert_eq!(
            fake.heartbeats.lock().unwrap().len(),
            4,
            "renewed every tick until the release"
        );
    }

    /// REGRESSION (review LOW 2): a runner clock AHEAD of coord's inflates the
    /// age coord's wall-clock deadlines imply. Here they imply 12 minutes of
    /// age at arrival (lease ends in 3, ceiling in 18), which without a floor
    /// put renew-by in the past and released a healthy dispatch as
    /// `queued_renewal_lapsed` on the first tick, before the runner had asked
    /// coord anything. The first tick now always sends a renewal first.
    #[tokio::test]
    async fn a_skewed_clock_cannot_release_a_dispatch_before_one_renewal_attempt() {
        let arrived = Instant::now();
        let wall = chrono::Utc::now();
        let mut skewed = advertising(queued_payload("skewed"), 1800);
        skewed.lease_expires_at = Some(wall + chrono::Duration::minutes(3));
        skewed.queued_renewal_deadline = Some(wall + chrono::Duration::minutes(18));
        assert_eq!(
            coord_age_at_arrival(&skewed, wall),
            Some(chrono::Duration::minutes(12)),
            "the age the warning reports"
        );

        // Coord unreachable: one renewal goes out (minute 1) before the
        // release (minute 2).
        let failed = || reporting::QueueHeartbeat::Failed("transport: unreachable".into());
        let state = state_with(vec![arrived_at(skewed.clone(), arrived, wall)]);
        let fake = FakeTransport::new(failed());
        assert_eq!(
            run_keeper_minutes(&state, &fake, arrived, 30, |_| failed()).await,
            Some((2, QUEUED_RENEWAL_LAPSED_REASON.to_string()))
        );
        assert_eq!(*fake.heartbeats.lock().unwrap(), ["skewed"]);

        // Coord renews: held to coord's own ceiling less the margin.
        let state = state_with(vec![arrived_at(skewed, arrived, wall)]);
        let fake = FakeTransport::new(reporting::QueueHeartbeat::Renewed);
        assert_eq!(
            run_keeper_minutes(&state, &fake, arrived, 30, |_| {
                reporting::QueueHeartbeat::Renewed
            })
            .await,
            Some((13, DEFERRED_PAST_LEASE_REASON.to_string()))
        );
    }

    /// The grace a coord-derived deadline is floored at outlasts the first
    /// keeper tick plus its renewal batch, and stays well inside the hold.
    #[test]
    fn the_first_renewal_grace_covers_one_tick_and_its_batch() {
        assert_eq!(
            FIRST_RENEWAL_GRACE,
            QUEUE_HEARTBEAT_INTERVAL + QUEUE_HEARTBEAT_BATCH_TIMEOUT
        );
        assert!(FIRST_RENEWAL_GRACE > QUEUE_HEARTBEAT_INTERVAL);
        assert!(FIRST_RENEWAL_GRACE < UNRENEWED_HOLD);
    }

    /// REGRESSION (review M1): an absurd advertised ceiling used to reach
    /// `now + Duration::from_secs(u64::MAX)`, which panics on `Instant`
    /// overflow — inside `enqueue_locked`, under the `ci_state()` lock, so the
    /// panic poisoned the mutex for every later caller. It now enqueues and
    /// gets a bounded queue deadline.
    #[test]
    fn an_absurd_advertised_ceiling_enqueues_with_a_bounded_deadline() {
        let absurd = advertising(queued_payload("absurd"), u64::MAX);
        let bound = MAX_ADVERTISED_QUEUE_CEILING - QUEUE_RELEASE_MARGIN;
        assert_eq!(queue_release_after(&absurd), bound);

        let mut state = CiState::new();
        let before = Instant::now();
        let (depth, armed) = enqueue_locked(&mut state, absurd, None);
        assert_eq!((depth, armed), (1, true));
        let stamp = state.queued[0].stamp;
        assert!(stamp.release_at > stamp.queued_since);
        assert!(
            stamp.release_at <= Instant::now() + bound,
            "the queue deadline is bounded by the clamp"
        );
        assert!(stamp.queued_since >= before);
    }

    /// REGRESSION (review L3): `coord_age_at_arrival` cast the advertised
    /// ceiling `as i64` (u64::MAX wrapped to -1) and built a
    /// `chrono::Duration::seconds` from it, which panics past chrono's range.
    /// An unrepresentable ceiling now yields no age, and a representable one
    /// still does.
    #[test]
    fn an_unrepresentable_ceiling_yields_no_coord_age() {
        let wall = chrono::Utc::now();
        for max_age in [u64::MAX, i64::MAX as u64, 1 << 62] {
            let mut p = advertising(queued_payload("huge"), max_age);
            p.queued_renewal_deadline = Some(wall);
            assert_eq!(coord_age_at_arrival(&p, wall), None, "max_age {max_age}");
        }
        let mut p = advertising(queued_payload("sane"), 1800);
        p.queued_renewal_deadline = Some(wall + chrono::Duration::minutes(20));
        assert_eq!(
            coord_age_at_arrival(&p, wall),
            Some(chrono::Duration::minutes(10))
        );
    }

    /// `lease_expires_at` is read as RFC 3339; absent or null is `None`; and a
    /// payload carrying a field this runner does not know still parses.
    #[test]
    fn lease_expires_at_parses_as_rfc3339_and_is_optional() {
        let mut v = serde_json::json!({
            "dispatch_id": "d",
            "repo": "qontinui/qontinui-runner",
            "head_sha": "0123456789abcdef0123456789abcdef01234567",
            "fetch_url": "https://coord.example/git/x.git",
            "candidate_ref": "refs/heads/merge-candidate/x",
            "some_future_field": {"nested": true}
        });
        let p: CiDispatchPayload = serde_json::from_value(v.clone()).expect("parses");
        assert_eq!(p.lease_expires_at, None);
        v["lease_expires_at"] = "2026-10-08T12:15:00+02:00".into();
        let p: CiDispatchPayload = serde_json::from_value(v.clone()).expect("parses");
        assert_eq!(
            p.lease_expires_at,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-08T10:15:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
        v["lease_expires_at"] = serde_json::Value::Null;
        let p: CiDispatchPayload = serde_json::from_value(v.clone()).expect("null parses");
        assert_eq!(p.lease_expires_at, None);
        assert_eq!(p.queued_renewal_deadline, None);
        // Coord's own shape: UTC with microseconds.
        v["queued_renewal_deadline"] = "2026-10-08T10:30:00.123456Z".into();
        let p: CiDispatchPayload = serde_json::from_value(v).expect("parses");
        assert_eq!(
            p.queued_renewal_deadline,
            Some(
                chrono::DateTime::parse_from_rfc3339("2026-10-08T10:30:00.123456Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            )
        );
    }

    /// Records every re-admission instead of admitting.
    #[derive(Default)]
    struct RecordingReadmit(Mutex<Vec<(String, QueueStamp)>>);

    impl Readmit for RecordingReadmit {
        fn readmit(&self, payload: CiDispatchPayload, stamp: QueueStamp) {
            self.0.lock().unwrap().push((payload.dispatch_id, stamp));
        }
    }

    /// An old stamp with a distinct renewal time, so a reset of EITHER clock
    /// is visible.
    fn old_stamp() -> QueueStamp {
        let queued_since = Instant::now();
        QueueStamp {
            queued_since,
            release_at: queued_since + Duration::from_secs(1500),
            renew_by: queued_since + Duration::from_secs(720),
        }
    }

    fn queued_with(id: &str, stamp: QueueStamp) -> QueuedDispatch {
        let mut q = queued_at(advertising(queued_payload(id), 1800), stamp.queued_since);
        q.stamp = stamp;
        q
    }

    /// REGRESSION (review LOW 3): a finished build re-admits the queue head
    /// with the stamp it was queued under, not as a fresh submission.
    #[test]
    fn a_finished_build_readmits_the_queue_head_with_its_stamp() {
        let stamp = old_stamp();
        let state = state_with(vec![queued_with("next", stamp)]);
        state
            .lock()
            .unwrap()
            .running
            .insert("done".into(), CancellationToken::new());
        let rec = RecordingReadmit::default();
        on_build_finished_in(&state, "done", &rec);
        assert_eq!(*rec.0.lock().unwrap(), [("next".to_string(), stamp)]);
        let s = state.lock().unwrap();
        assert!(s.running.is_empty() && s.queued.is_empty());
    }

    /// REGRESSION (review LOW 3): the headroom waker re-admits with the stamp,
    /// and leaves the queue to `on_build_finished` while something runs.
    #[test]
    fn the_headroom_waker_readmits_with_the_stamp_only_when_idle() {
        let stamp = old_stamp();
        let state = state_with(vec![queued_with("waiting", stamp)]);
        state.lock().unwrap().waker_armed = true;
        let rec = RecordingReadmit::default();
        headroom_retry_in(&state, &rec);
        assert_eq!(*rec.0.lock().unwrap(), [("waiting".to_string(), stamp)]);
        assert!(!state.lock().unwrap().waker_armed, "the waker disarms");

        let busy = state_with(vec![queued_with("waiting", stamp)]);
        busy.lock()
            .unwrap()
            .running
            .insert("r".into(), CancellationToken::new());
        let rec = RecordingReadmit::default();
        headroom_retry_in(&busy, &rec);
        assert!(rec.0.lock().unwrap().is_empty());
        assert_eq!(busy.lock().unwrap().queued.len(), 1);
    }

    /// REGRESSION (review LOW 3): losing the slot race re-queues the dispatch
    /// with the stamp it was admitted under, and arms the keeper; winning it
    /// registers the build.
    #[test]
    fn the_slot_race_requeues_with_the_original_stamp_and_arms_the_keeper() {
        let stamp = old_stamp();
        let full = std::sync::Arc::new(Mutex::new(CiState::new()));
        full.lock()
            .unwrap()
            .running
            .insert("r".into(), CancellationToken::new());
        let claim = claim_slot_or_requeue(
            &full,
            advertising(queued_payload("late"), 1800),
            Some(stamp),
            1,
            &CancellationToken::new(),
        );
        assert!(matches!(
            claim,
            SlotClaim::Requeued {
                ref dispatch_id,
                needs_keeper: true
            } if dispatch_id == "late"
        ));
        let s = full.lock().unwrap();
        assert_eq!(s.queued[0].stamp, stamp);
        assert!(s.keeper_armed);
        drop(s);

        let free = std::sync::Arc::new(Mutex::new(CiState::new()));
        let claim = claim_slot_or_requeue(
            &free,
            queued_payload("go"),
            Some(stamp),
            1,
            &CancellationToken::new(),
        );
        assert!(matches!(claim, SlotClaim::Claimed(ref p) if p.dispatch_id == "go"));
        assert!(free.lock().unwrap().running.contains_key("go"));
    }

    /// Rollout safety: an older coord reads `phase: "queued"` as a build
    /// heartbeat and stamps `started_at`, so the runner may queue-heartbeat only
    /// a dispatch whose coord advertised the phase.
    #[test]
    fn queue_heartbeats_go_only_to_a_coord_that_advertised_them() {
        let legacy = queued_payload("legacy");
        assert!(
            !legacy.coord_accepts_queue_heartbeat(),
            "a payload with no progress_phases is from a coord that would promote the row"
        );
        let mut current = queued_payload("current");
        current.progress_phases = vec!["running".into(), "queued".into()];
        assert!(current.coord_accepts_queue_heartbeat());
        let mut running_only = queued_payload("running-only");
        running_only.progress_phases = vec!["running".into()];
        assert!(!running_only.coord_accepts_queue_heartbeat());
    }

    /// A re-admitted dispatch that defers again must keep its FIRST stamp, or
    /// a queue that keeps bouncing a dispatch would never release it.
    #[test]
    fn re_enqueue_keeps_the_original_stamp_and_arms_one_keeper() {
        let mut state = CiState::new();
        // A stamp from the "future" proves the carried value is used verbatim
        // rather than replaced by `now()`.
        let first = QueueStamp {
            queued_since: Instant::now() + Duration::from_secs(600),
            release_at: Instant::now() + Duration::from_secs(800),
            renew_by: Instant::now() + Duration::from_secs(700),
        };
        let (depth, armed) = enqueue_locked(&mut state, queued_payload("a"), Some(first));
        assert_eq!((depth, armed), (1, true));
        assert_eq!(state.queued[0].stamp, first);
        let (depth, armed) = enqueue_locked(&mut state, queued_payload("b"), None);
        assert_eq!((depth, armed), (2, false), "one keeper, ever");
        let fresh = state.queued[1].stamp;
        assert!(
            fresh.queued_since < first.queued_since
                && fresh.renew_by == fresh.queued_since + UNRENEWED_HOLD
                && fresh.release_at == fresh.queued_since + UNRENEWED_HOLD,
            "a fresh defer's deadlines run from now"
        );
    }

    fn settings(enabled: bool, allow: &[&str], cap: u32) -> CiNodeSettings {
        CiNodeSettings {
            enabled,
            max_concurrent_builds: Some(cap),
            repo_allowlist: allow.iter().map(|s| s.to_string()).collect(),
            min_free_disk_gb: 20,
            canonical_converge: false,
        }
    }

    const GIB: u64 = 1024 * 1024 * 1024;

    /// A fixed host (48 cores / 368 GiB, suggestion 12) for resolving an unset
    /// capacity without probing the machine the tests run on.
    const TEST_HOST: super::super::host_sizing::HostCapacity =
        super::super::host_sizing::HostCapacity {
            mem_bytes: Some(368 * GIB),
            cpus: 48,
        };

    /// The capacity `submit` would resolve for `s` — here against [`TEST_HOST`].
    fn cap_of(s: &CiNodeSettings) -> u32 {
        s.effective_max_concurrent_builds_for(TEST_HOST)
    }

    /// Plan 2026-09-22-ci-capacity-...: an UNSET capacity admits up to the host
    /// suggestion for the supplied capacity (12 here), and defers at it.
    #[test]
    fn unset_capacity_admits_up_to_the_host_suggestion() {
        let mut s = settings(true, &["qontinui-runner"], 1);
        s.max_concurrent_builds = None;
        let cap = cap_of(&s);
        assert_eq!(cap, 12);
        assert!(matches!(
            admission_decision(&s, cap, "qontinui/qontinui-runner", 11, blind()),
            Admission::Proceed
        ));
        assert!(matches!(
            admission_decision(&s, cap, "qontinui/qontinui-runner", 12, blind()),
            Admission::Defer
        ));
    }

    /// Every sensor unreadable. This is what an admission call looks like on a
    /// box whose telemetry has gone dark, and it must behave exactly as the
    /// pre-headroom code did — which is why every legacy test below passes it.
    fn blind() -> Headroom {
        Headroom::default()
    }

    #[test]
    fn disabled_is_a_hard_reject() {
        let s = settings(false, &["qontinui-runner"], 1);
        assert!(matches!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, blind()),
            Admission::Reject(r) if r.contains("disabled")
        ));
    }

    #[test]
    fn unlisted_repo_is_a_hard_reject_and_empty_allowlist_runs_nothing() {
        let s = settings(true, &["qontinui-runner"], 1);
        assert!(matches!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-coord", 0, blind()),
            Admission::Reject(r) if r.contains("repo_allowlist")
        ));
        // Empty allowlist = nothing runnable, even enabled.
        let s = settings(true, &[], 1);
        assert!(matches!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, blind()),
            Admission::Reject(_)
        ));
    }

    #[test]
    fn at_cap_defers_never_rejects() {
        let s = settings(true, &["qontinui-runner"], 1);
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 1, blind()),
            Admission::Defer
        );
        // Below cap proceeds.
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, blind()),
            Admission::Proceed
        );
        // cap=0 is treated as 1 (a nonsensical hand-edit must not brick
        // admission into permanent deferral at running=0).
        let s0 = settings(true, &["qontinui-runner"], 0);
        assert_eq!(
            admission_decision(&s0, cap_of(&s0), "qontinui/qontinui-runner", 0, blind()),
            Admission::Proceed
        );
    }

    // ---- §B2 lever #3: defer on live headroom ----

    #[test]
    fn swap_pressure_defers_even_with_a_free_slot() {
        let s = settings(true, &["qontinui-runner"], 4);
        // Below the ratio: proceed. `mem_avail`-equivalent is deliberately
        // healthy in BOTH cases — the point of leading on swap is that a
        // roomy-looking memory reading must not veto the swap signal.
        let calm = Headroom {
            swap_total_bytes: Some(8 * GIB),
            swap_used_bytes: Some(3 * GIB), // 37.5%
            commit_available_bytes: Some(24 * GIB),
            session_warn_floor_bytes: None,
            ..Headroom::default()
        };
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, calm),
            Admission::Proceed
        );

        let pressed = Headroom {
            swap_used_bytes: Some(4 * GIB), // exactly 50% — the threshold is inclusive
            ..calm
        };
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, pressed),
            Admission::Defer,
            "swap at the ceiling ratio must defer even though the box is far \
             below its concurrency cap and mem/commit look healthy"
        );
    }

    #[test]
    fn low_free_commit_defers_above_the_reject_floor() {
        let s = settings(true, &["qontinui-runner"], 4);
        // Between the reject floor (4 GiB) and the defer band (8 GiB): the
        // build waits, it is NOT turned away.
        let squeezed = Headroom {
            swap_total_bytes: Some(8 * GIB),
            swap_used_bytes: Some(0),
            commit_available_bytes: Some(6 * GIB),
            session_warn_floor_bytes: None,
            ..Headroom::default()
        };
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, squeezed),
            Admission::Defer
        );
        assert!(
            DEFER_FREE_COMMIT_GB > MIN_FREE_COMMIT_GB,
            "you defer before you reject — a defer band at or below the reject \
             floor would make the defer arm unreachable"
        );
        // Just inside the band boundary proceeds.
        assert_eq!(
            admission_decision(
                &s,
                cap_of(&s),
                "qontinui/qontinui-runner",
                0,
                Headroom {
                    commit_available_bytes: Some(DEFER_FREE_COMMIT_GB * GIB),
                    ..squeezed
                }
            ),
            Admission::Proceed
        );
    }

    #[test]
    fn headroom_never_rejects() {
        let s = settings(true, &["qontinui-runner"], 4);
        // The worst reading we can construct: swap full, commit gone.
        let starved = Headroom {
            swap_total_bytes: Some(8 * GIB),
            swap_used_bytes: Some(8 * GIB),
            commit_available_bytes: Some(0),
            session_warn_floor_bytes: None,
            ..Headroom::default()
        };
        for running in [0usize, 1, 9] {
            assert!(
                !matches!(
                    admission_decision(
                        &s,
                        cap_of(&s),
                        "qontinui/qontinui-runner",
                        running,
                        starved
                    ),
                    Admission::Reject(_)
                ),
                "headroom is a DEFER lever; coord prefers, the node decides, and \
                 a transient reading must never re-home work (running={running})"
            );
        }
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, starved),
            Admission::Defer
        );
    }

    #[test]
    fn an_unreadable_sensor_fails_open() {
        let s = settings(true, &["qontinui-runner"], 4);
        // Nothing readable at all — the pre-headroom behaviour, exactly.
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, blind()),
            Admission::Proceed,
            "a telemetry gap must never brick the lane"
        );
        // Half-blind boxes still use the half they can read, and a swap
        // ceiling of zero is 'no swap pressure to measure', not 'saturated'.
        let no_swap = Headroom {
            swap_total_bytes: Some(0),
            swap_used_bytes: Some(0),
            commit_available_bytes: Some(24 * GIB),
            session_warn_floor_bytes: None,
            ..Headroom::default()
        };
        assert_eq!(no_swap.swap_used_ratio(), None);
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, no_swap),
            Admission::Proceed
        );
        // A used-without-total reading is not a ratio.
        assert_eq!(
            Headroom {
                swap_total_bytes: None,
                swap_used_bytes: Some(9 * GIB),
                commit_available_bytes: None,
                session_warn_floor_bytes: None,
                ..Headroom::default()
            }
            .swap_used_ratio(),
            None
        );
        assert!(!headroom_defers(blind()));
    }

    #[test]
    fn swap_is_ranked_before_memory_not_after() {
        // The measured failure this ordering exists to prevent: on a saturated
        // box mem/commit read as an all-clear while swap is the metric that
        // actually moved (-13.5 +/- 11.2 M/day vs +138.6 +/- 41.7 M/day). If
        // commit were consulted first, or swap ignored, this would proceed.
        let saturating = Headroom {
            swap_total_bytes: Some(8 * GIB),
            swap_used_bytes: Some(7 * GIB),
            commit_available_bytes: Some(64 * GIB), // "plenty of memory"
            session_warn_floor_bytes: None,
            ..Headroom::default()
        };
        assert!(headroom_defers(saturating));
        assert_eq!(SWAP_DEFER_RATIO, 0.5);
    }

    // ---- The saturation axis (plan
    // `2026-08-27-fleet-telemetry-has-no-saturation-dimension-but-memory`,
    // Phase 3 — the runner-side arm beside `SWAP_DEFER_RATIO`) ----

    /// A thread-table reading, the shape the `wsl`/Linux lanes publish.
    fn threads(used: i64, max: i64) -> Option<crate::fleet::resource_sample::Saturation> {
        crate::fleet::resource_sample::Saturation::threads(
            Some(used),
            Some(max),
            crate::fleet::resource_sample::SaturationSource::Proc,
        )
    }

    /// **The incident, as an admission decision.** On 2026-08-27 this box could
    /// not `fork()` — 190,840 tasks against a `threads-max` of 192,146 — while
    /// reporting 73.3 GB free commit of 125.6 GB and no swap pressure at all,
    /// and coord kept dispatching CI to it for the entire event.
    ///
    /// Every memory term below is deliberately *healthy*: if this test passes
    /// only because commit is low, it is testing the wrong axis.
    #[test]
    fn saturation_defers_while_every_memory_gauge_reads_healthy() {
        let s = settings(true, &["qontinui-runner"], 4);
        let incident = Headroom {
            // 73.3 GB free commit — far above the 8 GiB defer band.
            commit_available_bytes: Some(73 * GIB),
            // No swap pressure: the Windows host lane publishes none, and the
            // VM's own swap was not the story either.
            swap_total_bytes: None,
            swap_used_bytes: None,
            session_warn_floor_bytes: None,
            saturation: threads(190_840, 192_146),
        };
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, incident),
            Admission::Defer,
            "a box at 99.3% of its task ceiling must stop taking CI work even \
             though every memory instrument on it reads healthy — that \
             independence is the entire justification for a third axis"
        );
        // And the memory terms alone would have proceeded, which is what makes
        // the assertion above about saturation and nothing else.
        assert_eq!(
            admission_decision(
                &s,
                cap_of(&s),
                "qontinui/qontinui-runner",
                0,
                Headroom {
                    saturation: None,
                    ..incident
                }
            ),
            Admission::Proceed
        );
    }

    /// DEFER, never REJECT — and therefore never a filter.
    ///
    /// `headroom_is_a_ranking_input_and_never_a_filter` is coord's half of this
    /// rule; this is the node's. Deferring is not filtering: a saturated box
    /// stays a candidate and is out-ranked, because with one sample-less
    /// machine and one busy one, excluding would elect nobody.
    #[test]
    fn the_saturation_arm_defers_and_never_rejects() {
        let s = settings(true, &["qontinui-runner"], 4);
        let pinned = Headroom {
            saturation: threads(192_146, 192_146), // 100%
            ..Headroom::default()
        };
        for running in [0usize, 1, 9] {
            assert!(
                !matches!(
                    admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", running, pinned),
                    Admission::Reject(_)
                ),
                "saturation is a DEFER lever; a rejecting node re-homes work \
                 that would have run fine once the leak was reaped \
                 (running={running})"
            );
        }
        assert!(headroom_defers(pinned));
    }

    /// The threshold, at its boundary and on both sides of it.
    #[test]
    fn the_saturation_threshold_is_inclusive_and_sits_where_the_plan_put_it() {
        assert_eq!(SATURATION_DEFER_RATIO, 0.80);
        let at = Headroom {
            saturation: threads(80, 100),
            ..Headroom::default()
        };
        assert_eq!(at.saturation_ratio(), Some(0.80));
        assert!(
            headroom_defers(at),
            "the boundary is inclusive, the same way SWAP_DEFER_RATIO's is"
        );
        assert!(!headroom_defers(Headroom {
            saturation: threads(79, 100),
            ..Headroom::default()
        }));
        // Steady state in the evidence: every healthy container sat at ≤ 68
        // PIDs against a 192,146 ceiling. Three orders of magnitude of margin
        // is why this threshold has no false-positive pressure on it.
        assert!(!headroom_defers(Headroom {
            saturation: threads(68, 192_146),
            ..Headroom::default()
        }));
    }

    /// FAIL OPEN: a platform with no readable ceiling contributes no term.
    ///
    /// This is the ordinary reading on every machine in the fleet until its
    /// runner is rebuilt, and on any Windows host whose job object sets no
    /// `ActiveProcessLimit`. It must behave exactly as the pre-saturation code
    /// did — unknown means no headroom opinion, never "saturated".
    #[test]
    fn an_unmeasured_saturation_axis_contributes_no_term() {
        let s = settings(true, &["qontinui-runner"], 4);
        assert_eq!(blind().saturation_ratio(), None);
        assert!(!headroom_defers(blind()));
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, blind()),
            Admission::Proceed
        );
        // A half pair cannot even be constructed, so it cannot reach here: the
        // publisher's type rejects it, which is why this arm needs no divisor
        // guard of its own.
        assert_eq!(
            crate::fleet::resource_sample::Saturation::threads(
                Some(190_840),
                None,
                crate::fleet::resource_sample::SaturationSource::Proc
            ),
            None
        );
        assert_eq!(
            crate::fleet::resource_sample::Saturation::threads(
                Some(1),
                Some(0),
                crate::fleet::resource_sample::SaturationSource::Proc
            ),
            None,
            "a zero ceiling would divide by zero, not read as saturated"
        );
    }

    // ---- §Part C item 1: the live-session floor widens the CI defer band ----

    /// A box below the floor its owner declared for interactive sessions stops
    /// accepting new CI work — and the third assertion is what makes this a
    /// test of the session term rather than of the shipped band: the SAME
    /// reading with no session floor proceeds.
    #[test]
    fn the_session_floor_widens_the_defer_band() {
        let s = settings(true, &["qontinui-runner"], 4);
        let guarded = Headroom {
            swap_total_bytes: Some(8 * GIB),
            swap_used_bytes: Some(0),
            // 9 GiB clears the shipped 8 GiB band on its own …
            commit_available_bytes: Some(9 * GIB),
            // … but the owner says a live session needs 10.
            session_warn_floor_bytes: Some(10 * GIB),
            ..Headroom::default()
        };
        assert_eq!(
            admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", 0, guarded),
            Admission::Defer,
            "below the session floor, the lane with somewhere else to go steps back"
        );
        assert_eq!(
            admission_decision(
                &s,
                cap_of(&s),
                "qontinui/qontinui-runner",
                0,
                Headroom {
                    commit_available_bytes: Some(10 * GIB),
                    ..guarded
                }
            ),
            Admission::Proceed,
            "at the raised threshold the box is admitting work again"
        );
        assert_eq!(
            admission_decision(
                &s,
                cap_of(&s),
                "qontinui/qontinui-runner",
                0,
                Headroom {
                    session_warn_floor_bytes: None,
                    ..guarded
                }
            ),
            Admission::Proceed,
            "identical reading, no session floor — so the defer above came from \
             the session term and nothing else"
        );
        // DEFER, never reject: a session floor is transient pressure, and the
        // module header's ordering ("a node that rejects on a transient reading
        // makes coord re-home work that would have run fine in a minute") holds
        // for this term exactly as for the others.
        for running in [0usize, 1, 9] {
            assert!(!matches!(
                admission_decision(&s, cap_of(&s), "qontinui/qontinui-runner", running, guarded),
                Admission::Reject(_)
            ));
        }
    }

    /// FAIL OPEN: `None` — a disabled session guard, or settings that could not
    /// be read — contributes no term at all.
    #[test]
    fn a_session_guard_with_no_opinion_leaves_the_band_alone() {
        assert_eq!(defer_commit_floor_gb(None), DEFER_FREE_COMMIT_GB);
        // `probe_headroom` maps `enabled == false` to `None`, not to the stored
        // floor and not to zero. Pin the zero case anyway: it must land on the
        // shipped band by INTENT (`max`), not by arithmetic luck.
        assert_eq!(defer_commit_floor_gb(Some(0)), DEFER_FREE_COMMIT_GB);

        let s = settings(true, &["qontinui-runner"], 4);
        assert_eq!(
            admission_decision(
                &s,
                cap_of(&s),
                "qontinui/qontinui-runner",
                0,
                Headroom {
                    commit_available_bytes: Some(9 * GIB),
                    session_warn_floor_bytes: None,
                    ..Headroom::default()
                }
            ),
            Admission::Proceed,
            "an owner who turned the guard off has not authorised a CI floor \
             inferred from the switch they turned off"
        );
        assert!(!headroom_defers(blind()));
    }

    /// The session term is RAISE-only. The shipped default (3 GiB) sits below
    /// this lane's band deliberately — `settings.rs` pins warn < `ci_node`'s
    /// 4 GiB reject floor — and must never drag the band down to meet it.
    #[test]
    fn the_session_term_can_only_raise_never_lower() {
        for floor_gb in 0..=DEFER_FREE_COMMIT_GB {
            assert_eq!(
                defer_commit_floor_gb(Some(floor_gb * GIB)),
                DEFER_FREE_COMMIT_GB,
                "a session floor at or under the defer band must leave it alone \
                 (asked for {floor_gb} GiB)"
            );
        }
        // Concretely: 5 GiB free still defers under the shipped 3 GiB session
        // floor, because `DEFER_FREE_COMMIT_GB` still applies underneath it.
        let s = settings(true, &["qontinui-runner"], 4);
        assert_eq!(
            admission_decision(
                &s,
                cap_of(&s),
                "qontinui/qontinui-runner",
                0,
                Headroom {
                    commit_available_bytes: Some(5 * GIB),
                    session_warn_floor_bytes: Some(3 * GIB),
                    ..Headroom::default()
                }
            ),
            Admission::Defer
        );
    }

    /// The sanity bound. Unclamped, an over-set floor would defer EVERY
    /// dispatch forever while the 60 s waker re-tested a healthy box — this
    /// lane has no `MEM_WAIT_MAX` to fail open through, unlike `cargo-guard.sh`.
    #[test]
    fn an_unreachable_session_floor_is_clamped_to_the_cap() {
        assert!(
            MAX_SESSION_DEFER_FLOOR_GB > DEFER_FREE_COMMIT_GB,
            "a cap at or below the defer band would make the session term a \
             no-op — which is why this lane cannot reuse the shell lane's 8"
        );
        // 16 GiB is the plausible over-set: the incident's top consumer was
        // ~17 GB, so it is the number an operator types afterwards.
        assert_eq!(
            defer_commit_floor_gb(Some(16 * GIB)),
            MAX_SESSION_DEFER_FLOOR_GB
        );
        assert_eq!(
            defer_commit_floor_gb(Some(1024 * GIB)),
            MAX_SESSION_DEFER_FLOOR_GB
        );
        // Anything under the cap is honoured verbatim.
        assert_eq!(
            defer_commit_floor_gb(Some((MAX_SESSION_DEFER_FLOOR_GB - 1) * GIB)),
            MAX_SESSION_DEFER_FLOOR_GB - 1
        );
        // Fractional floors round UP — 9.5 GiB is 10, not 9. Rounding down
        // would enforce something weaker than what was configured.
        assert_eq!(defer_commit_floor_gb(Some(19 * GIB / 2)), 10);

        // The point of the cap: a healthy box can still clear the raised bar.
        let s = settings(true, &["qontinui-runner"], 4);
        assert_eq!(
            admission_decision(
                &s,
                cap_of(&s),
                "qontinui/qontinui-runner",
                0,
                Headroom {
                    commit_available_bytes: Some(MAX_SESSION_DEFER_FLOOR_GB * GIB),
                    session_warn_floor_bytes: Some(64 * GIB),
                    ..Headroom::default()
                }
            ),
            Admission::Proceed,
            "a floor no box can reach must not brick this node's admission"
        );
    }

    #[test]
    fn allowlist_matches_slug_or_basename() {
        assert!(repo_allowed(
            &["qontinui-runner".to_string()],
            "qontinui/qontinui-runner"
        ));
        assert!(repo_allowed(
            &["qontinui/qontinui-runner".to_string()],
            "qontinui/qontinui-runner"
        ));
        assert!(repo_allowed(
            &["qontinui-runner".to_string()],
            "qontinui-runner"
        ));
        assert!(!repo_allowed(
            &["qontinui-runner".to_string()],
            "qontinui/qontinui-web"
        ));
    }

    #[test]
    fn commit_floor_threshold() {
        assert!(commit_below_floor(3 * GIB, MIN_FREE_COMMIT_GB));
        assert!(commit_below_floor(4 * GIB - 1, MIN_FREE_COMMIT_GB));
        assert!(!commit_below_floor(4 * GIB, MIN_FREE_COMMIT_GB));
        assert!(!commit_below_floor(64 * GIB, MIN_FREE_COMMIT_GB));
    }

    /// §A3: the floor and the published snapshot must resolve their memory
    /// quantity from ONE function.
    ///
    /// Pinned at the SOURCE level rather than by comparing two live readings —
    /// free commit changes between any two calls, so a value comparison here
    /// would be a flake generator. What is worth pinning is not the number, it
    /// is that there is only one place the number comes from: the old defect
    /// was invisible precisely because two lanes each had their own probe, and
    /// "4 GB free" here and "5 GB free" in the supervisor were not 1 GiB apart
    /// but two different quantities sharing a unit.
    #[test]
    fn the_memory_floor_reads_the_published_snapshot_field() {
        const SRC: &str = include_str!("admission.rs");
        let prod = SRC
            .split_once("\n#[cfg(test)]")
            .map(|(a, _)| a)
            .unwrap_or(SRC);
        assert!(
            !prod.contains("available_memory()"),
            "ci_node must resolve its memory floor from \
             `fleet::resource_sample::available_commit_bytes`, not a private \
             physical-available reading of its own (plan §A3)"
        );
        assert!(
            prod.contains("resource_sample::available_commit_bytes()"),
            "the floor's probe must be the shared one"
        );
    }

    #[test]
    fn volume_pick_prefers_longest_mount_prefix() {
        let mounts = vec![
            (PathBuf::from("C:\\"), 20 * GIB, 5 * GIB),
            (PathBuf::from("D:\\"), 200 * GIB, 100 * GIB),
        ];
        // Windows-shaped paths only resolve on Windows path semantics;
        // use starts_with-compatible shapes for a portable test instead.
        let mounts_portable = vec![
            (PathBuf::from("/"), 20 * GIB, 5 * GIB),
            (PathBuf::from("/data"), 200 * GIB, 100 * GIB),
        ];
        assert_eq!(
            pick_volume(&mounts_portable, Path::new("/data/qontinui-root")).map(|v| v.2),
            Some(100 * GIB)
        );
        // The sample reports total + mount alongside free, from this same pick.
        assert_eq!(
            pick_volume(&mounts_portable, Path::new("/data/qontinui-root")).map(|v| v.1),
            Some(200 * GIB)
        );
        assert_eq!(
            pick_volume(&mounts_portable, Path::new("/home/x")).map(|v| v.2),
            Some(5 * GIB)
        );
        assert_eq!(
            pick_volume(&mounts, Path::new("/nowhere")),
            None,
            "no matching mount → None (caller fails open)"
        );
    }

    // -----------------------------------------------------------------
    // External-volume tiering — plan
    // `2026-08-07-external-storage-tiering-for-fleet-disk-pressure`,
    // Phases 3 and 5. Every case below runs with NO dock attached and no
    // filesystem access, which is the point: the plan's HW-ABSENT branch
    // has to be fully verifiable on a machine that has no external drive.
    // -----------------------------------------------------------------

    use crate::external_volume::ExternalVolumeState;

    /// The binding rule from the plan's "The binding mechanism" §2: mount the
    /// external volume at a path INSIDE the workspace root, so the existing
    /// longest-mount-prefix match selects it for external paths and the
    /// internal volume for everything else — **with no change to
    /// `pick_volume`**. This test is what makes that claim checkable instead
    /// of asserted.
    #[test]
    fn external_mount_inside_the_root_wins_the_longest_prefix() {
        let mounts = vec![
            (PathBuf::from("/data"), 4000 * GIB, 100 * GIB), // internal, nearly full
            (PathBuf::from("/data/qontinui-ext"), 4000 * GIB, 3900 * GIB), // external
        ];
        // A path on the external mount resolves to the EXTERNAL volume's free
        // space, not the internal one it is nested inside.
        assert_eq!(
            pick_volume(&mounts, Path::new("/data/qontinui-ext/targets/coord")).map(|v| v.2),
            Some(3900 * GIB),
            "the longer mount prefix must win — otherwise every external path \
             would report the internal volume's free space"
        );
        // And everything else still resolves to the internal volume.
        assert_eq!(
            pick_volume(
                &mounts,
                Path::new("/data/qontinui-root/qontinui-coord/target")
            )
            .map(|v| v.2),
            Some(100 * GIB)
        );
    }

    const FLOOR: u64 = 20;
    const ROOT: &str = "/data/qontinui-ext/targets";

    #[test]
    fn internal_path_with_unresolvable_probe_still_proceeds() {
        // The no-regression half of the plan's admission row. `external:
        // None` is what every path looks like on a box with no declaration,
        // so this is also the "byte-identical to today" guarantee.
        assert_eq!(
            disk_gate(None, FLOOR, None, Path::new("/data/qontinui-root")),
            DiskGate::Ok,
            "an internal path with an unresolvable probe must keep failing OPEN"
        );
    }

    #[test]
    fn external_path_with_unresolvable_probe_is_rejected() {
        // The inversion this plan exists for.
        let got = disk_gate(
            None,
            FLOOR,
            Some(&ExternalVolumeState::Present),
            Path::new(ROOT),
        );
        match got {
            DiskGate::Reject(r) => {
                assert!(r.contains("EXTERNAL"), "reason should name why: {r}");
                assert!(r.contains("fail-closed"), "reason was: {r}");
            }
            DiskGate::Ok => panic!("an unresolvable probe on an external path must REJECT"),
        }
    }

    #[test]
    fn absent_external_volume_is_rejected_before_free_space_is_considered() {
        // Note the free-space argument says there is plenty of room. It is
        // irrelevant: room on WHAT? The volume is not mounted, so the stub we
        // would be measuring is on the internal disk.
        let got = disk_gate(
            Some(3900),
            FLOOR,
            Some(&ExternalVolumeState::Absent),
            Path::new(ROOT),
        );
        match got {
            DiskGate::Reject(r) => assert!(r.contains("NOT mounted"), "reason was: {r}"),
            DiskGate::Ok => panic!("an absent external volume must REJECT even with free space"),
        }
    }

    #[test]
    fn mismatched_external_volume_is_rejected_and_says_so_distinctly() {
        // The dangerous case: a volume IS mounted, it is just the wrong one.
        // It must not be reported as a disconnect — an operator who reads
        // "not mounted" will go and plug the drive in, which is not the fix.
        let got = disk_gate(
            Some(3900),
            FLOOR,
            Some(&ExternalVolumeState::Mismatched {
                expected: "{d913fcde}".into(),
                found: "{ffffffff}".into(),
            }),
            Path::new(ROOT),
        );
        match got {
            DiskGate::Reject(r) => {
                assert!(r.contains("WRONG volume"), "reason was: {r}");
                assert!(
                    !r.contains("NOT mounted"),
                    "must not read as a disconnect: {r}"
                );
            }
            DiskGate::Ok => panic!("a mismatched volume must REJECT"),
        }
    }

    #[test]
    fn present_external_volume_above_the_floor_proceeds() {
        assert_eq!(
            disk_gate(
                Some(3900),
                FLOOR,
                Some(&ExternalVolumeState::Present),
                Path::new(ROOT)
            ),
            DiskGate::Ok
        );
    }

    #[test]
    fn the_ordinary_floor_still_rejects_on_both_kinds_of_volume() {
        // Phase 5 must not accidentally become the ONLY reason a build is
        // refused: the pre-existing free-space floor keeps working, external
        // or not.
        for external in [None, Some(&ExternalVolumeState::Present)] {
            match disk_gate(Some(1), FLOOR, external, Path::new(ROOT)) {
                DiskGate::Reject(r) => {
                    assert!(r.contains("min_free_disk_gb"), "reason was: {r}")
                }
                DiskGate::Ok => panic!("1 GiB free is below the {FLOOR} GiB floor"),
            }
        }
    }
}
