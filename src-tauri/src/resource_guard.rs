//! Spawn-time resource gate — the last thing consulted before the runner
//! creates a **new** session (a PTY, or a secondary runner process).
//!
//! Plan `2026-08-07-runner-resource-guard-and-session-protection.md` §Part D.
//!
//! ## What this exists to prevent
//!
//! Overnight 2026-08-06→07 several Claude Code sessions running inside
//! runner-spawned terminals died while their terminal windows stayed open and
//! the runner process itself never restarted. Windows' Resource-Exhaustion-
//! Detector fired 12 times between 00:00 and 05:29, each firing naming
//! `vmmemWSL` (up to ~17 GB) plus concurrent `rustc.exe` / `clippy-driver.exe`.
//! That is commit-charge exhaustion: Windows kills whatever process is
//! mid-allocation when the ceiling is hit, while the parent shell — which is
//! not allocating — survives untouched. The runner's own `terminal::session`
//! lifecycle logged **zero** closes in that window, because nothing local was
//! watching.
//!
//! So this gate fires at exactly one moment: **the instant a new thing is about
//! to be spawned.** It never touches, throttles, pauses or closes anything that
//! is already alive — the fleet's standing `runner-lifecycle` doctrine (never
//! stop/kill a live runner or session) applied to the one place a *new* one gets
//! created. Adding a session is a choice; the sessions already running are not.
//!
//! ## Pure over injected inputs
//!
//! [`evaluate`] takes a reading and a settings struct and returns a verdict. It
//! probes nothing and reads no globals, which is what makes the thresholds
//! arguable in a test rather than in production. [`probe_for_spawn`] is the thin
//! live wrapper that supplies the two inputs. This is deliberately the shape
//! [`crate::ci_node::admission`] already uses — `headroom_defers` is split out
//! of `admission_decision` for exactly this reason (`ci_node/admission.rs`,
//! "Split out from `admission_decision` so the threshold policy can be exercised
//! on its own").
//!
//! ## Fail OPEN, always
//!
//! `commit_available_bytes()` returns `Option`. `None` means the sensor is
//! UNKNOWN, UNKNOWN means this gate has no opinion, and no opinion means
//! **proceed** — see [`SpawnGate::Proceed`]. Every other guard in this fleet's
//! ladder takes the same posture (`ci_node/admission.rs`'s `Headroom` doc:
//! "an unreadable sensor is UNKNOWN, and unknown means no headroom opinion at
//! all (fail open)"). The whole failure mode of a guard like this must be false
//! negatives — a missed warning — never a false positive that blocks the
//! operator's actual work on a telemetry gap.
//!
//! ## Host lane only
//!
//! The reading comes from [`crate::fleet::resource_sample::host_snapshot`],
//! which is host-lane-only by construction (§Part A step 3): the WSL probe forks
//! `wsl.exe` under a 5 s timeout, and a pre-PTY gate that can stall five seconds
//! on a cold-starting WSL VM is a worse user-facing failure than the one it
//! prevents. It is also the *correct* lane — with `pageReporting=true` the host
//! free-commit figure already nets out WSL's live usage, so it is not blind to
//! `vmmemWSL`; it is precisely the quantity that collapsed to 7.25 GB during the
//! incident.

use tauri::{AppHandle, Emitter};
use tracing::warn;

use crate::settings::SessionGuardSettings;

/// Tauri event carrying a resource-guard observation to the webview.
///
/// Consumed by `src/hooks/useResourceGuardNotifications.ts`, which turns it into
/// a toast on the runner's general toast system (`useToast` + `ToastContainer`).
/// Emitted for the WARN verdict and for a CRITICAL verdict that an override let
/// through — **not** for a CRITICAL refusal, which travels back to the caller as
/// the typed `Err` below and is surfaced by the blocking dialog (or, on an
/// unattended path, by that path's own error reporting). Emitting both would
/// stack a self-dismissing toast on top of the modal that is asking the operator
/// to decide.
pub(crate) const RESOURCE_GUARD_EVENT: &str = "resource-guard-notice";

/// Prefix on the `Err` string a CRITICAL refusal returns.
///
/// The spawn seams this gate lives on (`TerminalSession::spawn`,
/// `InstanceManager::launch_instance_with_app`) both signal failure as
/// `Result<_, String>`, and widening those to a typed error enum would ripple
/// through a dozen unrelated call sites for no gain. A stable prefix keeps the
/// refusal machine-recognisable end to end: `src/lib/resourceGuard.ts` matches
/// on it to decide "this is an overridable refusal, show the dialog" versus
/// "this is a real spawn failure, report it". Everything after the prefix is
/// human text and may be reworded freely; the prefix may not.
pub(crate) const CRITICAL_REFUSAL_PREFIX: &str = "resource_guard:critical:";

/// One gibibyte, the unit the floors are quoted in.
const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Verdict of the spawn gate.
///
/// Deliberately three-valued rather than a bool. The three states carry
/// different *verdicts*, not different quantities — the same distinction the
/// rest of this fleet's ladder is built on (`cargo-guard.sh` defers at 5 GiB,
/// the supervisor's build pool defers at 5 GiB, `ci_node` hard-rejects at
/// 4 GiB, and all three read the same Windows free-commit number). A warn is
/// the lightest verdict in that ladder, which is why its floor sits lowest but
/// one; a block on a human's own spawn is the heaviest, which is why its floor
/// is lowest of all and why it is always overridable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpawnGate {
    /// Enough headroom — or no readable opinion at all. Spawn.
    Proceed,
    /// Below the warn floor. Spawn anyway, but tell the operator: the point of
    /// the warning is that they can free memory *before* the next spawn, and
    /// blocking here would be a heavier verdict than the evidence supports.
    Warn {
        lane: String,
        free_bytes: u64,
        floor_bytes: u64,
    },
    /// Below the critical floor. Refuse by default, and let an explicit
    /// override through — a false positive here blocks the operator's actual
    /// work, which is a worse failure than an occasional missed warning.
    Critical {
        lane: String,
        free_bytes: u64,
        floor_bytes: u64,
    },
}

/// Pure verdict over an injected reading and the machine owner's floors.
///
/// `free_commit_bytes` is `Option` because that is what
/// [`crate::fleet::resource_sample::available_commit_bytes`] returns, and the
/// `None` arm is load-bearing rather than incidental: off Windows the commit
/// concept does not exist, and on Windows `GlobalMemoryStatusEx` can fail. Both
/// are UNKNOWN, and UNKNOWN produces [`SpawnGate::Proceed`] — this gate never
/// converts "I could not measure" into "you may not start".
///
/// Boundaries are **strictly below**: a machine sitting exactly on its floor is
/// at the floor, not under it. Quoting a floor of 3 GiB and then warning at
/// exactly 3 GiB would make the number the panel displays a lie by one byte.
///
/// The critical arm is tested first so an inverted configuration
/// (`critical > warn`, which the settings command refuses to persist but a
/// hand-edited `settings.json` can still contain) resolves to the heavier
/// verdict rather than silently degrading to a warning.
pub(crate) fn evaluate(
    lane: &str,
    free_commit_bytes: Option<u64>,
    guard: &SessionGuardSettings,
) -> SpawnGate {
    if !guard.enabled {
        return SpawnGate::Proceed;
    }
    let Some(free) = free_commit_bytes else {
        // Unreadable sensor ⇒ no opinion ⇒ proceed. Fail open.
        return SpawnGate::Proceed;
    };
    if free < guard.critical_free_commit_bytes {
        return SpawnGate::Critical {
            lane: lane.to_string(),
            free_bytes: free,
            floor_bytes: guard.critical_free_commit_bytes,
        };
    }
    if free < guard.warn_free_commit_bytes {
        return SpawnGate::Warn {
            lane: lane.to_string(),
            free_bytes: free,
            floor_bytes: guard.warn_free_commit_bytes,
        };
    }
    SpawnGate::Proceed
}

/// Live verdict: read the floors, take one host-lane snapshot, [`evaluate`].
///
/// The settings read happens FIRST and short-circuits when the guard is
/// disabled, so a machine owner who turned the guard off pays nothing at all —
/// not even the snapshot's sysinfo refresh — on every spawn. [`evaluate`] also
/// honours `enabled` so the pure function is complete on its own; the check here
/// is about cost, not correctness.
///
/// The snapshot is [`crate::fleet::resource_sample::host_snapshot`] rather than
/// a bare [`crate::fleet::resource_sample::available_commit_bytes`] call because
/// that is the reading the A1 publisher sends to coord: the number this gate
/// trips on and the number the fleet dashboard renders are then literally one
/// reading, not two probes that agree today and drift tomorrow.
pub(crate) fn probe_for_spawn() -> SpawnGate {
    let guard = crate::settings::get_session_guard_settings();
    if !guard.enabled {
        return SpawnGate::Proceed;
    }
    let sample = crate::fleet::resource_sample::host_snapshot();
    evaluate(&sample.lane, sample.commit_available_bytes, &guard)
}

/// `bytes` as `"1.42 GiB"`. Two decimals because the shipped critical floor is
/// 1.5 GiB and rounding it to `2 GiB` in the very message that quotes it would
/// misreport the configured value.
fn format_gib(bytes: u64) -> String {
    format!("{:.2} GiB", bytes as f64 / GIB)
}

/// The refusal text a CRITICAL verdict returns, prefixed for machine matching.
///
/// Names the lane, the current headroom and the configured floor, because a
/// refusal that says only "not enough memory" gives the operator nothing to act
/// on — they cannot tell whether to close a build, close a session, or raise a
/// floor that was set too high.
fn critical_refusal(what: &str, lane: &str, free_bytes: u64, floor_bytes: u64) -> String {
    format!(
        "{CRITICAL_REFUSAL_PREFIX} Not starting a new {what}: the {lane} lane has \
         {} of free commit, below the {} critical floor. Free memory (close a build \
         or a session) and try again, or start anyway to override. The floors live \
         in Settings > Resource Guard.",
        format_gib(free_bytes),
        format_gib(floor_bytes),
    )
}

/// Apply the gate to a spawn that is about to happen.
///
/// `what` is a short noun phrase for the thing being created ("terminal
/// session", "runner instance") — it lands verbatim in the operator-facing
/// message, so it must read as an object, not as a subsystem name.
///
/// `resource_override` is the caller's explicit "I know, start it anyway". It
/// only ever affects the CRITICAL arm; nothing suppresses the WARN notice,
/// because the notice is the entire product of that arm.
///
/// `app` is `Some` wherever a webview exists to receive the notice. It is
/// `Option` because [`crate::instance_manager::InstanceManager::launch_instance`]
/// can run before the AppHandle is shared (boot-time restore); a missing handle
/// downgrades to the log line and never changes the verdict.
///
/// Returns `Err(refusal)` **only** for an un-overridden CRITICAL verdict. Every
/// other path returns `Ok(())` — including every probe failure.
pub(crate) fn admit_spawn(
    what: &str,
    resource_override: bool,
    app: Option<&AppHandle>,
) -> Result<(), String> {
    match probe_for_spawn() {
        SpawnGate::Proceed => Ok(()),
        SpawnGate::Warn {
            lane,
            free_bytes,
            floor_bytes,
        } => {
            let message = format!(
                "Low memory: the {lane} lane has {} of free commit, below the {} warn \
                 floor. Starting this {what} anyway.",
                format_gib(free_bytes),
                format_gib(floor_bytes),
            );
            warn!(
                lane = %lane,
                free_bytes,
                floor_bytes,
                what = %what,
                "resource_guard: spawning below the session warn floor"
            );
            emit_notice(app, "warn", &lane, free_bytes, floor_bytes, &message);
            Ok(())
        }
        SpawnGate::Critical {
            lane,
            free_bytes,
            floor_bytes,
        } => {
            if resource_override {
                let message = format!(
                    "Started this {what} below the {} critical floor ({} free on the \
                     {lane} lane) — the resource guard was overridden.",
                    format_gib(floor_bytes),
                    format_gib(free_bytes),
                );
                warn!(
                    lane = %lane,
                    free_bytes,
                    floor_bytes,
                    what = %what,
                    "resource_guard: OVERRIDDEN — spawning below the session critical floor"
                );
                emit_notice(app, "override", &lane, free_bytes, floor_bytes, &message);
                return Ok(());
            }
            warn!(
                lane = %lane,
                free_bytes,
                floor_bytes,
                what = %what,
                "resource_guard: refusing to spawn below the session critical floor"
            );
            Err(critical_refusal(what, &lane, free_bytes, floor_bytes))
        }
    }
}

/// Best-effort webview notice. A failed emit is logged and swallowed: a toast
/// that could not be delivered must never turn into a spawn failure.
fn emit_notice(
    app: Option<&AppHandle>,
    severity: &str,
    lane: &str,
    free_bytes: u64,
    floor_bytes: u64,
    message: &str,
) {
    let Some(app) = app else {
        return;
    };
    if let Err(e) = app.emit(
        RESOURCE_GUARD_EVENT,
        serde_json::json!({
            "severity": severity,
            "lane": lane,
            "freeBytes": free_bytes,
            "floorBytes": floor_bytes,
            "message": message,
        }),
    ) {
        warn!("resource_guard: failed to emit {RESOURCE_GUARD_EVENT}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB_U64: u64 = 1024 * 1024 * 1024;

    /// The shipped defaults: 3 GiB warn, 1.5 GiB critical, enabled.
    fn defaults() -> SessionGuardSettings {
        SessionGuardSettings::default()
    }

    #[test]
    fn plenty_of_headroom_proceeds() {
        assert_eq!(
            evaluate("host", Some(32 * GIB_U64), &defaults()),
            SpawnGate::Proceed
        );
    }

    /// FAIL OPEN #1: an unreadable sensor is UNKNOWN, and UNKNOWN is not a
    /// reason to block. This is the arm that keeps the gate harmless off
    /// Windows (where free commit does not exist) and on a
    /// `GlobalMemoryStatusEx` failure.
    #[test]
    fn unreadable_sensor_proceeds() {
        assert_eq!(evaluate("host", None, &defaults()), SpawnGate::Proceed);
    }

    /// FAIL OPEN #2: a disabled guard has no opinion at ANY reading, including
    /// zero free commit. The machine owner's switch outranks the floors.
    #[test]
    fn disabled_guard_proceeds_at_every_reading() {
        let off = SessionGuardSettings {
            enabled: false,
            ..defaults()
        };
        for free in [None, Some(0), Some(GIB_U64), Some(64 * GIB_U64)] {
            assert_eq!(evaluate("host", free, &off), SpawnGate::Proceed);
        }
    }

    /// Between the two floors ⇒ warn, and the verdict carries the numbers the
    /// operator needs (which lane, how much is left, what it is being compared
    /// against). A verdict that carried only a boolean could not produce the
    /// message this gate's whole value is in.
    #[test]
    fn between_the_floors_warns_and_reports_both_numbers() {
        let g = defaults();
        assert_eq!(
            evaluate("host", Some(2 * GIB_U64), &g),
            SpawnGate::Warn {
                lane: "host".to_string(),
                free_bytes: 2 * GIB_U64,
                floor_bytes: g.warn_free_commit_bytes,
            }
        );
    }

    #[test]
    fn below_the_critical_floor_is_critical() {
        let g = defaults();
        assert_eq!(
            evaluate("host", Some(GIB_U64), &g),
            SpawnGate::Critical {
                lane: "host".to_string(),
                free_bytes: GIB_U64,
                floor_bytes: g.critical_free_commit_bytes,
            }
        );
    }

    /// Boundaries are STRICTLY below. Sitting exactly on a floor is at the
    /// floor, not under it — otherwise the number the Settings panel displays
    /// and the number the gate enforces differ by one byte.
    #[test]
    fn exactly_at_a_floor_does_not_trip_it() {
        let g = defaults();
        assert_eq!(
            evaluate("host", Some(g.warn_free_commit_bytes), &g),
            SpawnGate::Proceed
        );
        match evaluate("host", Some(g.critical_free_commit_bytes), &g) {
            SpawnGate::Warn { free_bytes, .. } => {
                assert_eq!(free_bytes, g.critical_free_commit_bytes)
            }
            other => panic!("expected Warn exactly at the critical floor, got {other:?}"),
        }
    }

    /// A hand-edited `settings.json` can transpose the floors (the
    /// `save_session_guard_settings` door refuses it, the file does not). The
    /// heavier verdict must win: degrading a transposed config to a warning
    /// would silently disable the block on the one machine whose config is
    /// already known to be wrong.
    #[test]
    fn inverted_floors_resolve_to_the_heavier_verdict() {
        let inverted = SessionGuardSettings {
            warn_free_commit_bytes: GIB_U64,
            critical_free_commit_bytes: 4 * GIB_U64,
            enabled: true,
        };
        match evaluate("host", Some(2 * GIB_U64), &inverted) {
            SpawnGate::Critical { floor_bytes, .. } => assert_eq!(floor_bytes, 4 * GIB_U64),
            other => panic!("expected Critical under transposed floors, got {other:?}"),
        }
    }

    /// The lane is carried through from the reading rather than hardcoded, so
    /// the message names the lane that was actually measured.
    #[test]
    fn lane_is_carried_from_the_reading() {
        match evaluate("wsl", Some(0), &defaults()) {
            SpawnGate::Critical { lane, .. } => assert_eq!(lane, "wsl"),
            other => panic!("expected Critical, got {other:?}"),
        }
    }

    /// The refusal string is what the operator reads and what
    /// `src/lib/resourceGuard.ts` matches on: prefix first, then the lane, the
    /// live headroom and the configured floor.
    #[test]
    fn refusal_names_the_prefix_the_lane_the_headroom_and_the_floor() {
        let msg = critical_refusal("terminal session", "host", 1_073_741_824, 1_610_612_736);
        assert!(msg.starts_with(CRITICAL_REFUSAL_PREFIX));
        assert!(msg.contains("terminal session"));
        assert!(msg.contains("host lane"));
        assert!(msg.contains("1.00 GiB"), "missing headroom: {msg}");
        assert!(msg.contains("1.50 GiB"), "missing floor: {msg}");
    }

    /// 1.5 GiB must render as `1.50 GiB`, not `2 GiB` — the default critical
    /// floor has no integer-GiB spelling and the message quotes it verbatim.
    #[test]
    fn format_gib_keeps_the_fractional_default_floor_honest() {
        assert_eq!(format_gib(3 * GIB_U64 / 2), "1.50 GiB");
        assert_eq!(format_gib(3 * GIB_U64), "3.00 GiB");
    }
}
