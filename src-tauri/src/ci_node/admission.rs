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
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

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
/// Order is load-bearing: **rejects first, then defers.** Nothing below the
/// allowlist check can turn a `Defer` into a `Reject`, which is the property
/// `at_cap_defers_never_rejects` pins and which the headroom arm must not
/// weaken — coord *prefers*, the node *decides*, and a node that rejects on a
/// transient reading makes coord re-home work that would have run fine in a
/// minute.
pub(crate) fn admission_decision(
    settings: &CiNodeSettings,
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
    if running_count >= settings.max_concurrent_builds.max(1) as usize {
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

struct CiState {
    /// dispatch_id → cancel token for the running build.
    running: HashMap<String, CancellationToken>,
    /// Deferred (at-cap, or below headroom) dispatches, FIFO.
    queued: VecDeque<CiDispatchPayload>,
    /// A [`spawn_headroom_waker`] task is in flight. At most one, ever —
    /// otherwise a box that stays under the headroom threshold would accrue one
    /// sleeping task per redelivered dispatch.
    waker_armed: bool,
}

fn ci_state() -> &'static Mutex<CiState> {
    static STATE: OnceLock<Mutex<CiState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(CiState {
            running: HashMap::new(),
            queued: VecDeque::new(),
            waker_armed: false,
        })
    })
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
    let decision = {
        let state = ci_state().lock().unwrap();
        // Dedup: a dispatch already running or queued here is a duplicate
        // delivery (WS replay) — ignore it rather than double-building.
        if state.running.contains_key(&payload.dispatch_id)
            || state
                .queued
                .iter()
                .any(|p| p.dispatch_id == payload.dispatch_id)
        {
            info!(
                "ci_node: duplicate dispatch {} ignored (already running/queued)",
                payload.dispatch_id
            );
            return;
        }
        admission_decision(&settings, &payload.repo, state.running.len(), headroom)
    };

    match decision {
        Admission::Reject(reason) => reject(&payload, reason),
        Admission::Defer => {
            let (dispatch_id, repo) = (payload.dispatch_id.clone(), payload.repo.clone());
            let (depth, needs_waker) = {
                let mut state = ci_state().lock().unwrap();
                state.queued.push_back(payload);
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
                (state.queued.len(), needs)
            };
            info!(
                "ci_node: deferring dispatch {dispatch_id} for {repo} (queue depth {depth}, \
                 waker_armed={needs_waker}) — at cap or below live headroom {headroom:?}"
            );
            if needs_waker {
                spawn_headroom_waker();
            }
        }
        Admission::Proceed => start_build(payload, settings),
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
        let next = {
            let mut state = ci_state().lock().unwrap();
            state.waker_armed = false;
            // If a build started in the meantime, `on_build_finished` owns the
            // drain again and this waker has nothing to do.
            if state.running.is_empty() {
                state.queued.pop_front()
            } else {
                None
            }
        };
        if let Some(payload) = next {
            info!(
                "ci_node: re-testing headroom for deferred dispatch {}",
                payload.dispatch_id
            );
            submit(payload);
        }
    });
}

/// Start one admitted build: disk floor gate, then spawn the executor task
/// with a fresh cancel token registered under the dispatch_id.
fn start_build(payload: CiDispatchPayload, settings: CiNodeSettings) {
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
    {
        let mut state = ci_state().lock().unwrap();
        // Re-check the cap under the lock (submit's read was unlocked
        // in-between for the disk probe).
        if state.running.len() >= settings.max_concurrent_builds.max(1) as usize {
            drop(state);
            info!(
                "ci_node: slot taken while gating — deferring dispatch {}",
                payload.dispatch_id
            );
            ci_state().lock().unwrap().queued.push_back(payload);
            return;
        }
        state
            .running
            .insert(payload.dispatch_id.clone(), token.clone());
    }

    let dispatch_id = payload.dispatch_id.clone();
    info!(
        "ci_node: starting dispatch {} repo={} sha={} check_name={:?}",
        dispatch_id, payload.repo, payload.head_sha, payload.check_name
    );
    tokio::spawn(async move {
        super::executor::run_dispatch(payload, root, token).await;
        on_build_finished(&dispatch_id);
    });
}

/// Build-finished hook: free the slot, then re-admit the oldest deferred
/// dispatch (fresh settings + fresh headroom + disk gate — conditions may have
/// changed while it waited).
fn on_build_finished(dispatch_id: &str) {
    let next = {
        let mut state = ci_state().lock().unwrap();
        state.running.remove(dispatch_id);
        state.queued.pop_front()
    };
    if let Some(payload) = next {
        info!(
            "ci_node: slot freed by {} — re-admitting deferred dispatch {}",
            dispatch_id, payload.dispatch_id
        );
        submit(payload);
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
            state.queued.retain(|p| {
                if p.dispatch_id == dispatch_id {
                    removed = Some(p.clone());
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
    let Some(mut state) = crate::safe_lock::lock_with_deadline(
        ci_state(),
        "ci_node ci_state",
        SHUTDOWN_LOCK_TIMEOUT,
    ) else {
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

    fn settings(enabled: bool, allow: &[&str], cap: u32) -> CiNodeSettings {
        CiNodeSettings {
            enabled,
            max_concurrent_builds: cap,
            repo_allowlist: allow.iter().map(|s| s.to_string()).collect(),
            min_free_disk_gb: 20,
            canonical_converge: false,
        }
    }

    const GIB: u64 = 1024 * 1024 * 1024;

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
            admission_decision(&s, "qontinui/qontinui-runner", 0, blind()),
            Admission::Reject(r) if r.contains("disabled")
        ));
    }

    #[test]
    fn unlisted_repo_is_a_hard_reject_and_empty_allowlist_runs_nothing() {
        let s = settings(true, &["qontinui-runner"], 1);
        assert!(matches!(
            admission_decision(&s, "qontinui/qontinui-coord", 0, blind()),
            Admission::Reject(r) if r.contains("repo_allowlist")
        ));
        // Empty allowlist = nothing runnable, even enabled.
        let s = settings(true, &[], 1);
        assert!(matches!(
            admission_decision(&s, "qontinui/qontinui-runner", 0, blind()),
            Admission::Reject(_)
        ));
    }

    #[test]
    fn at_cap_defers_never_rejects() {
        let s = settings(true, &["qontinui-runner"], 1);
        assert_eq!(
            admission_decision(&s, "qontinui/qontinui-runner", 1, blind()),
            Admission::Defer
        );
        // Below cap proceeds.
        assert_eq!(
            admission_decision(&s, "qontinui/qontinui-runner", 0, blind()),
            Admission::Proceed
        );
        // cap=0 is treated as 1 (a nonsensical hand-edit must not brick
        // admission into permanent deferral at running=0).
        let s0 = settings(true, &["qontinui-runner"], 0);
        assert_eq!(
            admission_decision(&s0, "qontinui/qontinui-runner", 0, blind()),
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
            admission_decision(&s, "qontinui/qontinui-runner", 0, calm),
            Admission::Proceed
        );

        let pressed = Headroom {
            swap_used_bytes: Some(4 * GIB), // exactly 50% — the threshold is inclusive
            ..calm
        };
        assert_eq!(
            admission_decision(&s, "qontinui/qontinui-runner", 0, pressed),
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
            admission_decision(&s, "qontinui/qontinui-runner", 0, squeezed),
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
                    admission_decision(&s, "qontinui/qontinui-runner", running, starved),
                    Admission::Reject(_)
                ),
                "headroom is a DEFER lever; coord prefers, the node decides, and \
                 a transient reading must never re-home work (running={running})"
            );
        }
        assert_eq!(
            admission_decision(&s, "qontinui/qontinui-runner", 0, starved),
            Admission::Defer
        );
    }

    #[test]
    fn an_unreadable_sensor_fails_open() {
        let s = settings(true, &["qontinui-runner"], 4);
        // Nothing readable at all — the pre-headroom behaviour, exactly.
        assert_eq!(
            admission_decision(&s, "qontinui/qontinui-runner", 0, blind()),
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
            admission_decision(&s, "qontinui/qontinui-runner", 0, no_swap),
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
            admission_decision(&s, "qontinui/qontinui-runner", 0, incident),
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
                    admission_decision(&s, "qontinui/qontinui-runner", running, pinned),
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
            admission_decision(&s, "qontinui/qontinui-runner", 0, blind()),
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
            admission_decision(&s, "qontinui/qontinui-runner", 0, guarded),
            Admission::Defer,
            "below the session floor, the lane with somewhere else to go steps back"
        );
        assert_eq!(
            admission_decision(
                &s,
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
                admission_decision(&s, "qontinui/qontinui-runner", running, guarded),
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
