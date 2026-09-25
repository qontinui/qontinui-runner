//! Renderer-memory self-watchdog (plan
//! `2026-06-09-runner-renderer-memory-watchdog-and-twin-slo`, Phase 1 — the
//! load-bearing half; Phase 3.1 is the heartbeat fields this module produces).
//!
//! # Why this exists
//!
//! The runner's WebView2 renderer can grow without bound (a re-enabled capture
//! module, a third-party webview leak, a regression) until Chromium replaces
//! the document with its "Error code: Out of Memory" page. The supervisor that
//! *could* watch and restart is a dev-only tool only the operator runs —
//! customer runners have no local watcher at all. So the runner watches and
//! heals **itself**.
//!
//! # What it measures (§1.1)
//!
//! The **total WebView2 working set**: the whole DESCENDANT SUBTREE of
//! `msedgewebview2.exe` processes, and NOTHING ELSE. Chromium nests renderer,
//! GPU, utility and crashpad processes under the *browser* process, which is
//! itself the runner's child — so a `parent == GetCurrentProcessId()` filter
//! sums ONE process, not the seven measured in the plan's §0b (788 MB total).
//! The snapshot is walked transitively: one pid→parent map per sample, then the
//! closure.
//!
//! **The runner's OWN working set is deliberately NOT part of that total**, and
//! that is a correction rather than a preference. It used to be added in, and
//! every threshold in §6 Q1 was sized against §0b's *"788 MB across 7
//! processes"* — 7 processes that are all `msedgewebview2.exe`, none of them the
//! runner. Measured on the operator box on 2026-09-25: own working set 523 MiB,
//! WebView2 subtree 1 488 MiB, so the old total read 2 012 MiB. Three things
//! followed, none of which depended on the renderer being unhealthy:
//!
//! * The ceiling budget was silently eaten and floated with load — a third of
//!   the 1.5 GB allowance went to a process a reload cannot touch.
//! * A heal could not reclaim it, so once own-WS pushed the total over the
//!   ceiling every heal under-reclaimed, [`escalate_storm`] latched and
//!   [`HealGovernor::clear_if_recovered`] could never see a non-breaching tick:
//!   `renderer_reload_storming` would have stuck `true` on a healthy device,
//!   permanently.
//! * Both slope arms regressed on a polluted series, attributing runner-side
//!   growth to the renderer. Own WS reached 508 MiB in 3 h 52 m on that box —
//!   ~1–2 MB/min, 20–40× the long arm's 0.05 MB/min threshold.
//!
//! Own WS is still SAMPLED and still REPORTED, as
//! [`Snapshot::own_process_bytes`] and the heartbeat's
//! `renderer_own_process_bytes` — it is a useful diagnostic. It just never
//! enters [`Snapshot::total_bytes`], either ceiling, or the slope ring.
//! [`Snapshot::total_bytes`] is exactly the sum of
//! [`Snapshot::processes`], so the total and the breakdown reconcile: they used
//! to differ by precisely own WS with nothing on the wire saying so, which
//! reads to an operator diffing them as a missing process.
//!
//! # Shipped DISABLED by default (§1.4)
//!
//! `enabled` defaults to **`false`**; `QONTINUI_RUNNER_MEM_WATCHDOG_ENABLED=1`
//! is the opt-in. Present but off, until a live calibration run sets defensible
//! thresholds — because the only live baseline anyone has taken **fails** §6
//! Q1's numbers even after the own-WS correction above: subtree 1.561e9 B
//! against the 1.5e9 total ceiling, and a worst single renderer of 1.060e9 B
//! against the 1.0e9 per-renderer ceiling (operator box, 2026-09-25). Ceilings
//! are checked before any slope and carry no warm-ring requirement, so on such
//! a box the FIRST tick breaches and the runner reloads itself. §1.4 asks for
//! *"Phase 1 acceptance must validate them against a live baseline before
//! customer rollout"*, and that run does not exist yet.
//!
//! Raising the ceilings to make today's box pass would be fitting a threshold
//! to one unvalidated sample — the opposite of what §1.4 asks — so the numbers
//! stay as resolved and the feature stays off until the calibration run
//! replaces them with measured ones.
//!
//! It also keeps a **per-process breakdown** — `{pid, kind, working_set_bytes,
//! first_seen}` — because that split is the cheapest diagnostic the plan can
//! carry and was the entire value of the §0b measurement: one renderer grew to
//! 382 MB while its sibling went *down*. `kind` comes from the `--type=`
//! switch on the process command line where that is readable, and is
//! [`WebViewProcessKind::Unknown`] where it is not; no other part of the
//! command line is retained, and none of it ever reaches the wire (§6 Q6:
//! aggregate byte counts and pids only, no content).
//!
//! # What it detects (§1.2), and what it does NOT
//!
//! **Two slope terms over one sample ring, plus two ceilings.** The plan
//! records two measured incidents with profiles ~700× apart:
//!
//! * §0  — ~84 MB/min while idle, OOM in under an hour.
//! * §0b — ~0.12 MB/min, OOM after ~2.4 days of uptime.
//!
//! A single window cannot see both: a 10-minute window sized for the first is
//! ~250× above the second and never fires on it. So a SHORT arm (minutes) and
//! a LONG arm (hours) are evaluated over the same ring and either one breaches.
//! The ceilings are a backstop: an absolute total, and — because §0b's renderer
//! hit its own per-process ceiling while the machine had 58.6 GB free and the
//! WebView2 total was under 1 GB — an absolute **per-renderer** ceiling that a
//! total-only check is blind to.
//!
//! ## The detection envelope, stated honestly
//!
//! **Neither slope arm sees a non-slope failure.** A single large allocation
//! failing, or fragmentation against the V8 cage, produces the same error page
//! with no slope at all — §0b says so explicitly. This module therefore does
//! **not** prevent renderer OOM and must not be described that way in a
//! release note, a dashboard or a comment: it reduces the OOM population it
//! can see coming. The uncovered arm is what [`crate::webview_recovery`]'s
//! crash-recovery ladder exists for, and the two are complementary — this one
//! avoids the crash it can predict, the ladder survives the one it cannot.
//!
//! # How it heals (§1.3)
//!
//! Through [`webview_recovery::trigger_ui_recovery`], never through
//! `ui_bridge_reload_webview` directly and never through
//! `ui_bridge_page_hard_refresh_handler` (coord finding `94bac529`: that one
//! still evals `location.replace()` and is inert on the error page). The bare
//! reload command returns on *dispatch* — its own doc says "accepted still
//! means only ACCEPTED" — so a watchdog that treated a dispatched reload as a
//! successful reclaim would mark the breach handled, reset its cooldown and go
//! quiet while memory was unchanged. That is the exact silent-failure shape
//! this plan exists to remove. `trigger_ui_recovery` instead verifies the
//! reload with a main-labeled pong from a NEW document and escalates to a
//! window recreate in the same call on dispatch failure, refusal or no pong.
//!
//! A *soft* SPA navigation is deliberately not used: it keeps the same JS
//! context and therefore the leaked heap. Only a hard reload tears down the
//! document. Terminal PTYs are Rust-side and survive it; the frontend
//! reconnects via `invoke("terminal_list")`.
//!
//! After a heal the watchdog takes a **post-heal sample** and records
//! `{pre_bytes, post_bytes, reclaimed_bytes}` plus the per-process split. A
//! reload that reclaims 300 MB and one that reclaims nothing are different
//! diagnoses (heap leak vs native/GPU/fragmentation), and "did not move the
//! number" is the reload-storm signal one cycle earlier and with evidence.
//!
//! # Guardrails (§6 Q2/Q3)
//!
//! * A visible countdown event before every auto-reload, which elapses
//!   **whether or not it is acknowledged** — the protection is load-bearing and
//!   must not be defeatable by inattention or by a backgrounded window.
//! * At most `MAX_RELOADS` heal attempts per `RELOAD_WINDOW_MIN`. Past that,
//!   with memory still climbing, it stops reloading and escalates all three
//!   ways: a loud `tracing::error!`, a persistent `renderer_reload_storming`
//!   flag for the device heartbeat, and a persistent in-app banner event.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use tracing::{debug, error, info, warn};

use crate::fleet::RendererMemoryHandles;
use crate::webview_recovery::{self, LatchReport, RecoveryOutcome, RecoveryReason};

/// Tauri event the frontend listens on for the countdown toast and for both
/// edges of the persistent storm banner. Payload is a [`WatchdogEvent`];
/// `src/hooks/useRendererMemoryWatchdog.ts` is the consumer.
pub const WATCHDOG_EVENT: &str = "renderer-memory-watchdog";

/// Fraction of a slope arm's window that must actually be spanned by samples
/// before that arm's slope is trusted. Without it, two early samples can
/// extrapolate an enormous slope out of ordinary allocator noise.
const READY_FRACTION: f64 = 0.5;

/// Minimum samples inside an arm's window before its slope is trusted.
const MIN_SAMPLES_FOR_SLOPE: usize = 3;

// ───────────────────────────── configuration ─────────────────────────────

/// Env-tunable watchdog configuration. Every knob is
/// `QONTINUI_RUNNER_MEM_WATCHDOG_*`; the defaults are the plan's §6 Q1
/// numbers as **re-resolved on 2026-09-21** against the §0b measurements (the
/// original 2026-07-18 set was falsified — neither of its terms would have
/// fired before the 2026-09-18 crash).
#[derive(Debug, Clone, PartialEq)]
pub struct WatchdogConfig {
    /// Master switch (`..._ENABLED`), mirroring the shape of
    /// `QONTINUI_COORD_INFRA_HEALTH_OBSERVER_ENABLED` — but defaulting to
    /// **`false`**, so this ships present-but-off and `=1` is the opt-in. See
    /// the module doc's "Shipped DISABLED by default": the one live baseline
    /// anyone has measured breaches both ceilings on its first tick, and §1.4
    /// requires a calibration run before customer rollout.
    pub enabled: bool,
    /// Sampling period (`..._TICK_SECS`, default 30 s).
    pub tick: Duration,
    /// SHORT arm: sustained MB/min over [`Self::window`] that breaches
    /// (`..._SLOPE_MB_PER_MIN`, default 30). §0's ~84 MB/min was ~3× this.
    pub slope_mb_per_min: f64,
    /// SHORT arm window (`..._WINDOW_MIN`, default 10 min).
    pub window: Duration,
    /// LONG arm: sustained MB/min over [`Self::slow_window`]
    /// (`..._SLOW_SLOPE_MB_PER_MIN`, default 0.05). Sized deliberately BELOW
    /// §0b's measured 0.12 MB/min: at 0.05 a renderer adds ~72 MB/day, growth
    /// no steady-state workload should show across six hours.
    pub slow_slope_mb_per_min: f64,
    /// LONG arm window (`..._SLOW_WINDOW_MIN`, default 360 min = 6 h).
    pub slow_window: Duration,
    /// Absolute TOTAL WebView2 working-set ceiling
    /// (`..._CEILING_BYTES`, default 1.5 GB — ~1.9× the 788 MB §0b measured on
    /// a 5-day-old runner under load).
    pub ceiling_bytes: u64,
    /// Absolute ceiling for the WORST SINGLE renderer
    /// (`..._RENDERER_CEILING_BYTES`, default 1 GB). §0b's renderer died at its
    /// own per-process ceiling with 58.6 GB of machine memory free and the
    /// WebView2 total under 1 GB, which a total-only ceiling cannot see.
    pub renderer_ceiling_bytes: u64,
    /// Visible countdown before an auto-reload proceeds (`..._WARN_SECS`,
    /// default 10).
    pub warn_secs: u64,
    /// Heal attempts permitted inside [`Self::reload_window`]
    /// (`..._MAX_RELOADS`, default 2).
    pub max_reloads: u64,
    /// Window the [`Self::max_reloads`] cap is measured over
    /// (`..._RELOAD_WINDOW_MIN`, default 15 min).
    pub reload_window: Duration,
    /// How long to let the rebuilt document settle before taking the post-heal
    /// sample (`..._SETTLE_SECS`, default 20). Bounded on purpose: the delta is
    /// diagnostic, and a long wait would delay re-arming the protection.
    pub settle_secs: u64,
    /// Below this reclaim, a completed reload counts as "did not move the
    /// number" — §1.3's evidence-backed storm signal, one cycle early
    /// (`..._MIN_RECLAIM_BYTES`, default 64 MiB).
    pub min_reclaim_bytes: u64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            // OFF until a live calibration run (§1.4). See `enabled`'s doc.
            enabled: false,
            tick: Duration::from_secs(30),
            slope_mb_per_min: 30.0,
            window: Duration::from_secs(10 * 60),
            slow_slope_mb_per_min: 0.05,
            slow_window: Duration::from_secs(360 * 60),
            ceiling_bytes: 1_500_000_000,
            renderer_ceiling_bytes: 1_000_000_000,
            warn_secs: 10,
            max_reloads: 2,
            reload_window: Duration::from_secs(15 * 60),
            settle_secs: 20,
            min_reclaim_bytes: 64 * 1024 * 1024,
        }
    }
}

impl WatchdogConfig {
    /// Read the configuration from the environment, falling back to
    /// [`Default`] for every key that is absent or unparseable.
    pub fn from_env() -> Self {
        let d = Self::default();
        let cfg = Self {
            // An OPT-IN gate, so an explicit truthy token or nothing — see
            // [`parse_bool_opt_in`]. The kill-switch parse this used to share
            // read an EMPTY value as ON.
            enabled: env_bool_opt_in("QONTINUI_RUNNER_MEM_WATCHDOG_ENABLED", d.enabled),
            tick: env_secs("QONTINUI_RUNNER_MEM_WATCHDOG_TICK_SECS", d.tick),
            slope_mb_per_min: env_f64(
                "QONTINUI_RUNNER_MEM_WATCHDOG_SLOPE_MB_PER_MIN",
                d.slope_mb_per_min,
            ),
            window: env_mins("QONTINUI_RUNNER_MEM_WATCHDOG_WINDOW_MIN", d.window),
            slow_slope_mb_per_min: env_f64(
                "QONTINUI_RUNNER_MEM_WATCHDOG_SLOW_SLOPE_MB_PER_MIN",
                d.slow_slope_mb_per_min,
            ),
            slow_window: env_mins(
                "QONTINUI_RUNNER_MEM_WATCHDOG_SLOW_WINDOW_MIN",
                d.slow_window,
            ),
            ceiling_bytes: env_u64_clamped(
                "QONTINUI_RUNNER_MEM_WATCHDOG_CEILING_BYTES",
                d.ceiling_bytes,
                MIN_CEILING_BYTES,
                u64::MAX,
            ),
            renderer_ceiling_bytes: env_u64_clamped(
                "QONTINUI_RUNNER_MEM_WATCHDOG_RENDERER_CEILING_BYTES",
                d.renderer_ceiling_bytes,
                MIN_CEILING_BYTES,
                u64::MAX,
            ),
            // §6 Q2 asks for a countdown that is actually visible, so 1 s is the
            // floor; the ceiling is a bound on a `tokio::sleep` inside a heal.
            warn_secs: env_u64_clamped(
                "QONTINUI_RUNNER_MEM_WATCHDOG_WARN_SECS",
                d.warn_secs,
                1,
                MAX_SLEEP_SECS,
            ),
            // 1, not 0: `0` would make every breach a storm, which is a silent
            // second kill switch rather than a reload budget.
            max_reloads: env_u64_clamped(
                "QONTINUI_RUNNER_MEM_WATCHDOG_MAX_RELOADS",
                d.max_reloads,
                1,
                1_000,
            ),
            reload_window: env_mins(
                "QONTINUI_RUNNER_MEM_WATCHDOG_RELOAD_WINDOW_MIN",
                d.reload_window,
            ),
            settle_secs: env_u64_clamped(
                "QONTINUI_RUNNER_MEM_WATCHDOG_SETTLE_SECS",
                d.settle_secs,
                0,
                MAX_SLEEP_SECS,
            ),
            // Capped at `i64::MAX` because [`heal`] compares it as `i64`: above
            // that the cast wraps negative and the no-reclaim check is off.
            min_reclaim_bytes: env_u64_clamped(
                "QONTINUI_RUNNER_MEM_WATCHDOG_MIN_RECLAIM_BYTES",
                d.min_reclaim_bytes,
                0,
                i64::MAX as u64,
            ),
        };
        cfg.warn_if_ring_is_bounded();
        cfg
    }

    /// Capacity for the sample ring, derived from the LONGER of the two slope
    /// windows so the long arm always has a full window to regress over — and
    /// BOUNDED by [`MAX_RING_SAMPLES`].
    ///
    /// Deliberately computed, never hardcoded: at a 30 s tick a 6 h window is
    /// 720 samples and a 24 h one 2 880 — tens of KB either way. The bound is
    /// what stops the two knobs' extremes multiplying: bounding each of them
    /// separately never bounded their product.
    fn ring_capacity(&self) -> usize {
        self.unbounded_ring_capacity().min(MAX_RING_SAMPLES)
    }

    /// What [`Self::ring_capacity`] would be with no bound — the quantity
    /// [`Self::warn_if_ring_is_bounded`] reports on.
    fn unbounded_ring_capacity(&self) -> usize {
        let longest = self.window.max(self.slow_window).as_secs_f64();
        let tick = self.tick.as_secs_f64().max(1.0);
        ((longest / tick).ceil() as usize).saturating_add(2)
    }

    /// Say so when the bound BITES, because the consequence is a silently
    /// shortened slope arm.
    ///
    /// [`slope_mb_per_min`] only becomes ready once the samples inside a window
    /// span [`READY_FRACTION`] of it, so a ring that cannot hold a full window's
    /// worth DISABLES that arm rather than degrading it. That is the
    /// silent-failure class this plan exists to remove, so a configuration which
    /// reaches the bound is named at startup instead of discovered later.
    fn warn_if_ring_is_bounded(&self) {
        let want = self.unbounded_ring_capacity();
        if want > MAX_RING_SAMPLES {
            warn!(
                requested_capacity = want,
                applied_capacity = MAX_RING_SAMPLES,
                window_min = self.window.as_secs_f64() / 60.0,
                slow_window_min = self.slow_window.as_secs_f64() / 60.0,
                tick_secs = self.tick.as_secs(),
                "renderer_watchdog: this window/tick combination needs more samples than the \
                 ring may hold — capped. The longer slope arm will regress over a SHORTENED \
                 window and may never become ready; raise ..._TICK_SECS or lower \
                 ..._SLOW_WINDOW_MIN."
            );
        }
    }

    /// The oldest sample worth keeping.
    fn retention(&self) -> Duration {
        self.window.max(self.slow_window)
    }
}

fn env_raw(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_string())
}

/// Pure half of [`env_bool_opt_in`]: the parse for a gate whose SAFE state is
/// OFF. It requires an explicit truthy token and reads everything else as off,
/// so it can only ever fail CLOSED.
///
/// Accepts `1`, `true`, `yes`, `on` — case-insensitive, surrounding whitespace
/// ignored. **Every other present value is OFF, the empty string included.**
///
/// # Why the inverse parse was wrong here
///
/// This replaced a default-ON kill switch's rule, "anything not spelled falsey
/// keeps it on". That bias is right for a feature that must stay up unless
/// somebody deliberately turns it off, and exactly wrong for an opt-in: making
/// [`WatchdogConfig::enabled`] default to `false` (§1.4 — ship present-but-off
/// until a calibration run) inverted its safety, turning the same rule into a
/// fail-OPEN switch. [`env_raw`] trims, so
/// `QONTINUI_RUNNER_MEM_WATCHDOG_ENABLED=` arrived as `Some("")` and ENABLED a
/// feature that reloads the user's window — and so did `=disabled`, `=none`,
/// `=n` and a typo like `=fasle`. An empty value is what a `.env` line with
/// nothing after the `=`, a `docker -e VAR` with no value, or a `$VAR` that
/// failed to expand all produce, and what a human writing any of those means is
/// "unset".
///
/// This module has no default-ON knob, so the fail-open parse is **gone** rather
/// than kept beside this one: leaving it would be dead code and a live footgun
/// for the next boolean added here. A knob that genuinely wants the default-ON
/// bias should carry its own helper, with its own argument for it, rather than
/// inherit one by reaching for the nearest boolean reader.
fn parse_bool_opt_in(value: Option<&str>, default: bool) -> bool {
    match value {
        Some(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        None => default,
    }
}

/// Whether a present value is a spelling of "off" this module RECOGNIZES, as
/// distinct from one [`parse_bool_opt_in`] merely declined to read as truthy —
/// an empty value, a typo, a word nobody implemented. Both are off; only the
/// second is worth a log line, because only the second may have been meant as
/// an opt-in.
fn is_recognized_off(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// Read an OPT-IN gate, announcing a present value that did not turn it on and
/// was not a recognized "off" — the shape an operator who believes they enabled
/// the watchdog would otherwise get in silence.
fn env_bool_opt_in(key: &str, default: bool) -> bool {
    let raw = env_raw(key);
    let on = parse_bool_opt_in(raw.as_deref(), default);
    if let Some(v) = raw.as_deref() {
        if !on && !is_recognized_off(v) {
            warn!(
                key,
                requested = v,
                "renderer_watchdog: {key} is set to a value this gate does not read as ON — \
                 staying OFF. An opt-in needs 1/true/yes/on; an EMPTY value counts as off, \
                 not as unset."
            );
        }
    }
    on
}

fn env_f64(key: &str, default: f64) -> f64 {
    env_raw(key)
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
}

/// Longest window any duration knob may name, in minutes (30 days).
///
/// A bound, not a preference. `Duration::from_secs_f64` **panics** on a value
/// that overflows a `Duration`, and [`WatchdogConfig::from_env`] runs on the
/// Tauri `setup` thread — so a finite-but-absurd `..._WINDOW_MIN=1e300` would
/// have killed startup rather than misconfiguring a watchdog.
const MAX_WINDOW_MINS: f64 = 30.0 * 24.0 * 60.0;

/// Longest a single sleep-bearing knob (`..._WARN_SECS`, `..._SETTLE_SECS`,
/// `..._TICK_SECS`) may name. Both of the first two are `tokio::sleep`s inside
/// a heal, so an hour is already far past useful.
const MAX_SLEEP_SECS: u64 = 3_600;

/// Hard ceiling on the sample ring, in samples.
///
/// A bound on the PRODUCT of two knobs that are each already bounded alone.
/// [`MAX_WINDOW_MINS`] caps a window at 30 days and `..._TICK_SECS` may be as
/// low as 1 s, so [`WatchdogConfig::ring_capacity`] could ask for 2 592 002
/// [`Sample`]s — roughly 62 MB pre-allocated by `VecDeque::with_capacity` in
/// [`run`], inside a watchdog whose whole job is to notice memory growth, plus
/// three `Vec`s of that length allocated on EVERY tick by [`slope_mb_per_min`].
///
/// 100 000 covers a 24 h window at a 1 s tick (86 402 samples) and is ~2.4 MB at
/// 24 bytes a sample; the shipped defaults use 722 of it. Above the bound the
/// ring is capped and [`WatchdogConfig::warn_if_ring_is_bounded`] says so,
/// because a ring too short to span its window silently disables that slope arm.
const MAX_RING_SAMPLES: usize = 100_000;

/// Smallest a byte ceiling may be. `0` is the value that mattered: it breaches
/// on **every** tick, which is an unattended reload loop rather than a
/// misconfiguration. 1 MiB is still low enough for a deliberate
/// force-the-breach acceptance run.
const MIN_CEILING_BYTES: u64 = 1024 * 1024;

/// Read a `u64` knob, CLAMPED into `min..=max`.
///
/// Its siblings all validate (`env_f64` rejects non-finite and non-positive,
/// `env_secs` rejects zero) and this one did not, which made three knobs into
/// silent footguns: `MAX_RELOADS=0` turned [`HealGovernor::decide`] into an
/// unconditional [`HealDecision::Storm`] — a second, undocumented kill switch;
/// `CEILING_BYTES=0` breached every tick; and a `MIN_RECLAIM_BYTES` above
/// `i64::MAX` wrapped NEGATIVE at the `as i64` in [`heal`], disabling the
/// no-reclaim check entirely. Clamping rather than falling back to the default
/// on purpose: the operator asked for an extreme, and the nearest legal extreme
/// is closer to that intent than the default is.
fn env_u64_clamped(key: &str, default: u64, min: u64, max: u64) -> u64 {
    let Some(v) = env_raw(key).and_then(|v| v.parse::<u64>().ok()) else {
        return default;
    };
    let clamped = clamp_u64(v, min, max);
    if clamped != v {
        warn!(
            key,
            requested = v,
            applied = clamped,
            "renderer_watchdog: {key} is outside {min}..={max} — clamped"
        );
    }
    clamped
}

/// Pure half of [`env_u64_clamped`], so the bounds are assertable without
/// mutating the process environment that every other test shares.
fn clamp_u64(value: u64, min: u64, max: u64) -> u64 {
    value.clamp(min, max)
}

/// Pure half of [`env_mins`]: minutes → a [`Duration`], BOUNDED.
///
/// The bound is the panic guard. `Duration::from_secs_f64` panics on a value
/// that overflows a `Duration`, and `..._WINDOW_MIN=1e300` parses as a perfectly
/// finite `f64` — so without the `min` this would have killed the Tauri `setup`
/// thread rather than misconfiguring a window.
// `min`/`max` rather than `clamp`, deliberately: `f64::clamp` PANICS on NaN
// bounds and RETURNS NaN for a NaN input, which `Duration::from_secs_f64` then
// panics on — the exact failure this function exists to prevent. `min` and `max`
// return the non-NaN operand, so a NaN here becomes a bounded number.
#[allow(clippy::manual_clamp)]
fn mins_to_duration(minutes: f64) -> Duration {
    Duration::from_secs_f64(minutes.min(MAX_WINDOW_MINS).max(0.0) * 60.0)
}

/// Read a seconds knob, ANNOUNCING both the clamp and the fallback.
///
/// It used to do both in silence while its sibling [`env_u64_clamped`] warned —
/// so `..._TICK_SECS=99999` became a 3 600 s tick and `..._TICK_SECS=0` became
/// the 30 s default, in each case leaving an operator holding a value the
/// watchdog was not using and no line anywhere saying so.
///
/// `0` and unparseable fall back to the default rather than clamping up to `1`:
/// a 1 s tick is the pathological end of [`MAX_RING_SAMPLES`], and it is not
/// what "0" asks for.
fn env_secs(key: &str, default: Duration) -> Duration {
    let Some(raw) = env_raw(key) else {
        return default;
    };
    let Some(secs) = raw.parse::<u64>().ok().filter(|s| *s > 0) else {
        warn!(
            key,
            requested = %raw,
            applied_secs = default.as_secs(),
            "renderer_watchdog: {key} is not a positive whole number of seconds — using the \
             default"
        );
        return default;
    };
    if secs > MAX_SLEEP_SECS {
        warn!(
            key,
            requested = secs,
            applied = MAX_SLEEP_SECS,
            "renderer_watchdog: {key} is above {MAX_SLEEP_SECS} s — clamped"
        );
    }
    Duration::from_secs(secs.min(MAX_SLEEP_SECS))
}

/// Read a minutes knob, ANNOUNCING both the clamp and the fallback — same
/// silence, same fix as [`env_secs`]. The clamp here is also the panic guard;
/// see [`MAX_WINDOW_MINS`].
fn env_mins(key: &str, default: Duration) -> Duration {
    let Some(raw) = env_raw(key) else {
        return default;
    };
    let Some(mins) = raw
        .parse::<f64>()
        .ok()
        .filter(|m| m.is_finite() && *m > 0.0)
    else {
        warn!(
            key,
            requested = %raw,
            applied_min = default.as_secs_f64() / 60.0,
            "renderer_watchdog: {key} is not a positive finite number of minutes — using the \
             default"
        );
        return default;
    };
    if mins > MAX_WINDOW_MINS {
        warn!(
            key,
            requested = mins,
            applied = MAX_WINDOW_MINS,
            "renderer_watchdog: {key} is above {MAX_WINDOW_MINS} min — clamped"
        );
    }
    mins_to_duration(mins)
}

// ──────────────────────────── the sample model ───────────────────────────

/// Which Chromium process a WebView2 pid is, derived from the `--type=` switch
/// on its command line.
///
/// [`WebViewProcessKind::Unknown`] is NOT a failure: reading another process's
/// command line can be refused, and the plan's §7 value (a per-window split)
/// survives an unlabeled pid. It is deliberately distinct from
/// [`WebViewProcessKind::Other`], which means the switch WAS read and named a
/// type this enum does not model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebViewProcessKind {
    /// The WebView2 host/browser process — no `--type=` switch at all.
    Browser,
    /// `--type=gpu-process`.
    Gpu,
    /// `--type=renderer` — where the leak in both recorded incidents lived.
    Renderer,
    /// `--type=utility` (network service, storage service, …).
    Utility,
    /// `--type=crashpad-handler`.
    Crashpad,
    /// A `--type=` this enum does not model.
    Other,
    /// The command line could not be read for this pid.
    Unknown,
}

impl WebViewProcessKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Gpu => "gpu",
            Self::Renderer => "renderer",
            Self::Utility => "utility",
            Self::Crashpad => "crashpad",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }

    /// Whether this kind may be evaluated against the per-renderer ceiling.
    ///
    /// `Unknown` counts, on purpose: an unreadable command line must not
    /// silently disable the one check that would have seen §0b's failure mode.
    /// Every labeled non-renderer kind is excluded so a large GPU or browser
    /// process cannot masquerade as a leaking renderer.
    fn is_renderer_candidate(self) -> bool {
        matches!(self, Self::Renderer | Self::Unknown)
    }
}

/// One WebView2 process in a sample. This is exactly what rides the heartbeat:
/// a pid, a coarse kind, a byte count and a first-seen timestamp. **No path, no
/// command line, no window title** — §6 Q6's "no content on the wire".
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSample {
    pub pid: u32,
    pub kind: WebViewProcessKind,
    pub working_set_bytes: u64,
    /// Unix milliseconds at which this watchdog first observed the pid. A
    /// renderer born at a reload is distinguishable from one that has been up
    /// since boot — which is precisely the distinction §0b turned on.
    pub first_seen_unix_ms: u64,
}

/// One complete sample of the WebView2 subtree.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// Every `msedgewebview2.exe` descendant, and **nothing else** — this is
    /// exactly `processes.iter().map(working_set_bytes).sum()`, which
    /// [`Snapshot::reconciles`] asserts. The runner's own working set is NOT in
    /// here; it is [`Self::own_process_bytes`]. See the module doc's "What it
    /// measures".
    pub total_bytes: u64,
    /// The per-process breakdown, largest first.
    pub processes: Vec<ProcessSample>,
    /// The runner's OWN working set. **Diagnostic only**: never in
    /// [`Self::total_bytes`], never checked against either ceiling, never
    /// pushed into the slope ring. A reload cannot reclaim it, so letting it
    /// drive a reload was a detector measuring the wrong quantity.
    pub own_process_bytes: u64,
    /// WebView2 descendants that were enumerated but whose working set could
    /// not be read (exited between the snapshot and the open, or `OpenProcess`
    /// refused). Counted rather than silently `continue`d: a partial sample
    /// UNDERSTATES memory, and understating it is a MISSED detection, so the
    /// number has to be visible next to the total it is missing from.
    pub unreadable_processes: u32,
}

impl Snapshot {
    /// Whether [`Self::total_bytes`] is the sum of [`Self::processes`].
    ///
    /// The invariant an operator relies on when diffing the total against the
    /// breakdown. It used to be false by exactly [`Self::own_process_bytes`],
    /// with nothing on the wire saying so — which reads as a process missing
    /// from the breakdown rather than as a different quantity in the total.
    pub fn reconciles(&self) -> bool {
        self.total_bytes
            == self
                .processes
                .iter()
                .map(|p| p.working_set_bytes)
                .fold(0u64, |a, b| a.saturating_add(b))
    }

    /// The worst single renderer-candidate process, as `(pid, bytes)`.
    pub fn worst_renderer(&self) -> Option<(u32, u64)> {
        self.processes
            .iter()
            .filter(|p| p.kind.is_renderer_candidate())
            .max_by_key(|p| p.working_set_bytes)
            .map(|p| (p.pid, p.working_set_bytes))
    }

    /// Compact one-line rendering for logs: `pid/kind=bytes`, largest first,
    /// then the two quantities that are NOT in the total — the runner's own
    /// working set and the count of descendants that could not be read. Both
    /// are named so the line reconciles by inspection: the `pid/kind=` terms
    /// sum to `total_bytes`, and `own=`/`unreadable=` say what is deliberately
    /// outside it and what is missing from it.
    fn summary(&self) -> String {
        let mut out = self
            .processes
            .iter()
            .map(|p| format!("{}/{}={}", p.pid, p.kind.as_str(), p.working_set_bytes))
            .collect::<Vec<_>>()
            .join(" ");
        out.push_str(&format!(
            " (own={} excluded, unreadable={})",
            self.own_process_bytes, self.unreadable_processes
        ));
        out
    }
}

/// One entry in the rolling ring the slope arms regress over.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    at: Instant,
    total_bytes: u64,
}

// ────────────────────────────── the detector ─────────────────────────────

/// Why the watchdog considers the renderer to be in trouble.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Breach {
    /// SHORT arm — the §0 profile (fast leak, OOM within the hour).
    FastSlope { slope_mb_per_min: f64 },
    /// LONG arm — the §0b profile (slow leak, OOM after days).
    SlowSlope { slope_mb_per_min: f64 },
    /// Absolute total WebView2 working set.
    TotalCeiling { bytes: u64 },
    /// Absolute ceiling on one renderer process.
    RendererCeiling { pid: u32, bytes: u64 },
}

impl Breach {
    /// Stable token for logs and the event payload.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FastSlope { .. } => "fast_slope",
            Self::SlowSlope { .. } => "slow_slope",
            Self::TotalCeiling { .. } => "total_ceiling",
            Self::RendererCeiling { .. } => "renderer_ceiling",
        }
    }

    fn describe(self, cfg: &WatchdogConfig) -> String {
        match self {
            Self::FastSlope { slope_mb_per_min } => format!(
                "renderer memory climbing {slope_mb_per_min:.2} MB/min (> {:.2} over {:.0} min)",
                cfg.slope_mb_per_min,
                cfg.window.as_secs_f64() / 60.0
            ),
            Self::SlowSlope { slope_mb_per_min } => format!(
                "renderer memory creeping {slope_mb_per_min:.3} MB/min (> {:.3} over {:.0} min)",
                cfg.slow_slope_mb_per_min,
                cfg.slow_window.as_secs_f64() / 60.0
            ),
            Self::TotalCeiling { bytes } => format!(
                "total WebView2 working set {bytes} B >= ceiling {} B",
                cfg.ceiling_bytes
            ),
            Self::RendererCeiling { pid, bytes } => format!(
                "renderer pid {pid} working set {bytes} B >= per-renderer ceiling {} B",
                cfg.renderer_ceiling_bytes
            ),
        }
    }
}

/// Least-squares slope, in MB/min, of the samples inside `window` measured back
/// from the newest sample.
///
/// `None` when the arm is not ready: fewer than [`MIN_SAMPLES_FOR_SLOPE`]
/// samples in the window, or a spanned duration below [`READY_FRACTION`] of it.
/// A regression rather than an endpoint difference on purpose — at the long
/// arm's 0.05 MB/min threshold, six hours of growth is ~18 MB, which is inside
/// the ordinary fluctuation of a single working-set reading, so a
/// `(last - first)` estimate would be dominated by whichever two samples
/// happened to bound the window.
fn slope_mb_per_min(samples: &VecDeque<Sample>, window: Duration) -> Option<f64> {
    let newest = samples.back()?.at;
    let inside: Vec<&Sample> = samples
        .iter()
        .filter(|s| newest.duration_since(s.at) <= window)
        .collect();
    if inside.len() < MIN_SAMPLES_FOR_SLOPE {
        return None;
    }
    let first = inside.first()?.at;
    let span = newest.duration_since(first);
    if span.as_secs_f64() < window.as_secs_f64() * READY_FRACTION {
        return None;
    }

    let n = inside.len() as f64;
    let xs: Vec<f64> = inside
        .iter()
        .map(|s| s.at.duration_since(first).as_secs_f64() / 60.0)
        .collect();
    let ys: Vec<f64> = inside
        .iter()
        .map(|s| s.total_bytes as f64 / (1024.0 * 1024.0))
        .collect();
    let mean_x = xs.iter().sum::<f64>() / n;
    let mean_y = ys.iter().sum::<f64>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for (x, y) in xs.iter().zip(ys.iter()) {
        num += (x - mean_x) * (y - mean_y);
        den += (x - mean_x) * (x - mean_x);
    }
    if den <= f64::EPSILON {
        return None;
    }
    Some(num / den)
}

/// Decide whether `snapshot` (plus the ring it was just appended to) breaches.
///
/// Pure — no clock read, no I/O — so both slope arms, both ceilings and the
/// quiet case are unit-testable without a live webview. Ceilings are checked
/// before slopes because they are unambiguous; the fast arm before the slow arm
/// because it names the more urgent profile.
fn evaluate(
    samples: &VecDeque<Sample>,
    snapshot: &Snapshot,
    cfg: &WatchdogConfig,
) -> Option<Breach> {
    if snapshot.total_bytes >= cfg.ceiling_bytes {
        return Some(Breach::TotalCeiling {
            bytes: snapshot.total_bytes,
        });
    }
    if let Some((pid, bytes)) = snapshot.worst_renderer() {
        if bytes >= cfg.renderer_ceiling_bytes {
            return Some(Breach::RendererCeiling { pid, bytes });
        }
    }
    if let Some(slope) = slope_mb_per_min(samples, cfg.window) {
        if slope >= cfg.slope_mb_per_min {
            return Some(Breach::FastSlope {
                slope_mb_per_min: slope,
            });
        }
    }
    if let Some(slope) = slope_mb_per_min(samples, cfg.slow_window) {
        if slope >= cfg.slow_slope_mb_per_min {
            return Some(Breach::SlowSlope {
                slope_mb_per_min: slope,
            });
        }
    }
    None
}

// ───────────────────────────── heal governance ───────────────────────────

/// What the governor permits for the breach now in hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HealDecision {
    /// Within budget — warn, then run the recovery ladder.
    Proceed,
    /// Budget spent while memory keeps breaching: stop reloading and escalate.
    Storm,
}

/// Rate-limits heal attempts and owns the storm latch (§6 Q3).
///
/// Pure state machine, free of clock reads, so the cooldown and the escalation
/// are testable without sleeping through a 15-minute window.
#[derive(Debug, Default)]
struct HealGovernor {
    attempts: VecDeque<Instant>,
    storming: bool,
}

impl HealGovernor {
    fn prune(&mut self, now: Instant, window: Duration) {
        while let Some(front) = self.attempts.front() {
            if now.duration_since(*front) > window {
                self.attempts.pop_front();
            } else {
                break;
            }
        }
    }

    /// Whether a heal may run now.
    ///
    /// The `self.storming` term is load-bearing and was missing: the latch was
    /// WRITTEN by [`Self::latch_storm`] and read by no decision at all. With the
    /// default `max_reloads = 2`, a heal that reclaimed nothing logged *"escalating
    /// now instead of spending the remaining reload budget"*, latched the storm —
    /// and the very next breaching tick was told `Proceed`, because
    /// `attempts.len() == 1 < 2`. So the log line, the module doc and §1.3's
    /// "stop reloading and escalate" were all false as implemented, and the
    /// remaining budget was spent anyway.
    ///
    /// The latch still comes off: [`Self::clear_if_recovered`] clears it on a
    /// non-breaching tick once the attempt window has drained, which is the same
    /// edge that emits `storm_cleared` to the UI. So this is a stop, not a
    /// one-way door.
    fn decide(&mut self, now: Instant, cfg: &WatchdogConfig) -> HealDecision {
        self.prune(now, cfg.reload_window);
        if self.storming || self.attempts.len() as u64 >= cfg.max_reloads {
            HealDecision::Storm
        } else {
            HealDecision::Proceed
        }
    }

    /// Record that a heal was attempted (whatever its outcome) so a failing
    /// ladder cannot be hammered every tick.
    fn record_attempt(&mut self, now: Instant) {
        self.attempts.push_back(now);
    }

    /// Latch the storm state. Returns `true` the first time, so the loud log
    /// fires once per storm rather than every tick.
    fn latch_storm(&mut self) -> bool {
        let was = self.storming;
        self.storming = true;
        !was
    }

    /// A healthy tick with no attempts left in the window clears the latch, so
    /// the watchdog can protect again after a genuine recovery. Returns `true`
    /// when it actually cleared.
    fn clear_if_recovered(&mut self, now: Instant, cfg: &WatchdogConfig) -> bool {
        self.prune(now, cfg.reload_window);
        if self.storming && self.attempts.is_empty() {
            self.storming = false;
            true
        } else {
            false
        }
    }
}

// ──────────────────────────── shared telemetry ───────────────────────────

/// The live values the device heartbeat reports (Phase 3.1). Owned here
/// because the watchdog is the PRODUCER; `mcp_api` publishes clones of these
/// `Arc`s into `fleet`'s `OnceLock` when `ApiState` is constructed, exactly as
/// it does for the capture-backend telemetry.
///
/// A process-global `LazyLock` rather than a value threaded from `main` because
/// the two readers appear in either order: the watchdog task is spawned from
/// the Tauri `setup` hook, `ApiState` is built inside the MCP server task
/// spawned from the same hook, and neither may depend on winning that race.
struct TelemetryState {
    latest_ws_bytes: Arc<AtomicU64>,
    reload_total: Arc<AtomicU64>,
    storming: Arc<AtomicBool>,
    /// The most recent per-process breakdown. `Mutex` rather than atomics
    /// because it is a vector; read once per 30 s heartbeat and written once
    /// per 30 s sample, so contention is not a consideration.
    processes: Arc<Mutex<Vec<ProcessSample>>>,
    /// Unix ms at which the values above were last published from a READABLE
    /// sample; 0 before the first one.
    ///
    /// Without it the three scalars are bare numbers with no age, so a watchdog
    /// that died an hour ago is indistinguishable from a live one reading a
    /// healthy plateau — `capture_telemetry_snapshot` carries
    /// `last_capture_fallback_at` for exactly this reason.
    sampled_at_unix_ms: Arc<AtomicU64>,
    /// The runner's own working set at that sample — diagnostic, and
    /// deliberately NOT part of `latest_ws_bytes`. Reported so an operator can
    /// see the quantity the detector excludes (module doc, "What it measures").
    own_process_bytes: Arc<AtomicU64>,
    /// WebView2 descendants that sample could not read. Reported because the
    /// number it is missing from is `latest_ws_bytes`: a partial sample
    /// UNDERSTATES memory, and a twin alarming on an understated total with
    /// nothing beside it saying the sample was partial is the same
    /// silent-failure shape this plan exists to remove.
    unreadable_processes: Arc<AtomicU64>,
}

static TELEMETRY: LazyLock<TelemetryState> = LazyLock::new(|| TelemetryState {
    latest_ws_bytes: Arc::new(AtomicU64::new(0)),
    reload_total: Arc::new(AtomicU64::new(0)),
    storming: Arc::new(AtomicBool::new(false)),
    processes: Arc::new(Mutex::new(Vec::new())),
    sampled_at_unix_ms: Arc::new(AtomicU64::new(0)),
    own_process_bytes: Arc::new(AtomicU64::new(0)),
    unreadable_processes: Arc::new(AtomicU64::new(0)),
});

/// Clones of the watchdog's live telemetry handles, for
/// `fleet::publish_renderer_memory_handles`. Safe to call before the watchdog
/// starts — the handles exist from first touch and read as an honest zero
/// baseline until the first sample lands.
pub fn heartbeat_handles() -> RendererMemoryHandles {
    RendererMemoryHandles {
        latest_ws_bytes: TELEMETRY.latest_ws_bytes.clone(),
        reload_total: TELEMETRY.reload_total.clone(),
        storming: TELEMETRY.storming.clone(),
        processes: TELEMETRY.processes.clone(),
        sampled_at_unix_ms: TELEMETRY.sampled_at_unix_ms.clone(),
        own_process_bytes: TELEMETRY.own_process_bytes.clone(),
        unreadable_processes: TELEMETRY.unreadable_processes.clone(),
    }
}

/// Publish one sample for the heartbeat to read.
///
/// **An unreadable sample publishes NOTHING.** A snapshot with no processes in
/// it is UNKNOWN, not a renderer that shrank to zero, and storing its zero would
/// overwrite the last good reading with a manufactured measurement. Leaving the
/// previous values in place with an AGEING `sampled_at_unix_ms` is the honest
/// shape: a reader can see the number is stale. Before the first readable sample
/// the stamp is 0 and the bytes are 0 — an explicit baseline, which is what the
/// heartbeat field's doc already promises.
///
/// # Published as one GROUP
///
/// Every field below is written while the `processes` mutex is held, and
/// `fleet::renderer_memory_snapshot_from` reads all of them under that same
/// lock. Without it the four sample scalars were bare `Relaxed` stores beside a
/// separately-locked vector, so a heartbeat landing mid-publish could pair a new
/// `latest_ws_bytes` with the PREVIOUS `processes` — a total that does not
/// reconcile with its own breakdown. The window is microseconds against a 30 s
/// cadence, but [`Snapshot::reconciles`] is now an invariant the wire promises
/// and an operator diffs against, and "rare" is not the same as "true".
///
/// `reload_total` and `storming` are deliberately NOT in the group: they are a
/// counter and a latch owned by the heal path rather than properties of a
/// sample, so a heartbeat pairing a fresh reload count with the sample before it
/// is correct, not torn.
fn publish_sample(snapshot: &Snapshot) {
    if snapshot.processes.is_empty() {
        return;
    }
    // `poisoned.into_inner()`, like every READER of this mutex
    // (`fleet::renderer_memory_snapshot_from`, `process_facts`). The writer used
    // to give up on a poisoned lock (`if let Ok`), so one panic anywhere would
    // freeze the per-process breakdown on the heartbeat forever — silently,
    // while the readers went on serving the frozen vector as current.
    //
    // Taken FIRST and held across the scalar stores: this lock is what makes the
    // publish a group. See the doc above.
    let mut g = match TELEMETRY.processes.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    g.clone_from(&snapshot.processes);
    TELEMETRY
        .latest_ws_bytes
        .store(snapshot.total_bytes, Ordering::Relaxed);
    TELEMETRY
        .own_process_bytes
        .store(snapshot.own_process_bytes, Ordering::Relaxed);
    TELEMETRY
        .unreadable_processes
        .store(snapshot.unreadable_processes as u64, Ordering::Relaxed);
    // Stamped LAST, so the stamp never claims freshness for values that are not
    // yet stored — and inside the guard, so no reader can see a stamp that is
    // ahead of the vector it describes.
    TELEMETRY
        .sampled_at_unix_ms
        .store(unix_ms(), Ordering::Relaxed);
    drop(g);
}

// ─────────────────────────────── the UI event ────────────────────────────

/// Payload of [`WATCHDOG_EVENT`].
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchdogEvent {
    /// `"reload_warning"` — a countdown is running and the reload WILL proceed.
    /// `"storming"` — persistent banner; reloads have stopped.
    /// `"storm_cleared"` — the storm ended; retire that banner.
    /// `"reload_result"` — the pre/post reclaim report for a completed heal.
    pub kind: &'static str,
    /// Which detector fired, as [`Breach::as_str`].
    pub breach: &'static str,
    /// Total WebView2 working set when the event was raised.
    pub total_ws_bytes: u64,
    /// Seconds remaining before the reload proceeds; 0 for non-countdown kinds.
    pub countdown_secs: u64,
    /// Cumulative completed heals this session.
    pub reload_total: u64,
    /// Bytes reclaimed by the heal, for `"reload_result"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reclaimed_bytes: Option<i64>,
    /// Human-readable line for the toast or banner.
    pub message: String,
}

fn emit(app: &AppHandle, event: WatchdogEvent) {
    if let Err(e) = app.emit(WATCHDOG_EVENT, &event) {
        debug!("renderer_watchdog: emitting {WATCHDOG_EVENT} failed: {e}");
    }
}

// ──────────────────────────────── the loop ───────────────────────────────

/// Start the watchdog background task.
///
/// Always publishes the telemetry handles (so the heartbeat reports an honest
/// zero baseline even when the watchdog is off), then returns immediately. The
/// loop needs an [`AppHandle`] to run the recovery ladder and emit the UI
/// events, so it is spawned from the Tauri `setup` hook like the other
/// long-lived background tasks.
pub fn start(app: AppHandle) {
    let cfg = WatchdogConfig::from_env();
    // Touch the telemetry so the handles exist regardless of the kill-switch.
    LazyLock::force(&TELEMETRY);

    if !cfg.enabled {
        info!(
            "renderer_watchdog: OFF — this ships present-but-disabled until a live \
             calibration run sets defensible thresholds (plan §1.4). Set \
             QONTINUI_RUNNER_MEM_WATCHDOG_ENABLED=1 to opt in. The heartbeat will report a \
             zeroed renderer-memory baseline, which is a BASELINE and not a measurement."
        );
        return;
    }

    info!(
        tick_secs = cfg.tick.as_secs(),
        fast = format!(
            "{:.2} MB/min over {:.0} min",
            cfg.slope_mb_per_min,
            cfg.window.as_secs_f64() / 60.0
        ),
        slow = format!(
            "{:.3} MB/min over {:.0} min",
            cfg.slow_slope_mb_per_min,
            cfg.slow_window.as_secs_f64() / 60.0
        ),
        ceiling_bytes = cfg.ceiling_bytes,
        renderer_ceiling_bytes = cfg.renderer_ceiling_bytes,
        max_reloads = cfg.max_reloads,
        ring_capacity = cfg.ring_capacity(),
        "renderer_watchdog: starting (detects SLOPE and CEILING breaches only — \
         a non-slope OOM, e.g. one large allocation failing or V8-cage \
         fragmentation, is outside this envelope and is webview_recovery's ladder)"
    );

    tauri::async_runtime::spawn(async move {
        run(app, cfg).await;
    });
}

async fn run(app: AppHandle, cfg: WatchdogConfig) {
    let mut ring: VecDeque<Sample> = VecDeque::with_capacity(cfg.ring_capacity());
    let mut governor = HealGovernor::default();
    // Set once the ladder tells us this runner has no webview to heal
    // (`server_mode` / `no_main_window`). Sampling continues — the telemetry is
    // still worth reporting — but no heal is ever attempted again, and nothing
    // is counted as a reload.
    let mut self_heal_unavailable: Option<&'static str> = None;

    loop {
        tokio::time::sleep(cfg.tick).await;

        let snapshot = sample_webview2_subtree();
        publish_sample(&snapshot);

        // Nothing to reason about: non-Windows builds, or a sample that could
        // not read a single process. Keep the loop alive, skip breach logic —
        // an unreadable sample is UNKNOWN, never a healthy zero.
        //
        // The predicate is the BREAKDOWN, not the total. `total_bytes == 0` was
        // the only UNKNOWN guard, and it was unreachable on Windows while the
        // runner's own working set was part of the total — own WS is never zero
        // on a live process, so a sample that read NOTHING still looked like a
        // measurement of ~500 MB. Now that the total is the subtree alone, an
        // empty `processes` is the one honest statement of "read nothing", and
        // it is also exactly what `publish_sample` refuses to publish.
        if snapshot.processes.is_empty() {
            continue;
        }

        let now = Instant::now();
        push_sample(&mut ring, &cfg, now, snapshot.total_bytes);

        let breach = evaluate(&ring, &snapshot, &cfg);

        debug!(
            total_ws_bytes = snapshot.total_bytes,
            own_process_bytes = snapshot.own_process_bytes,
            unreadable_processes = snapshot.unreadable_processes,
            processes = snapshot.processes.len(),
            worst_renderer = ?snapshot.worst_renderer(),
            fast_slope = ?slope_mb_per_min(&ring, cfg.window),
            slow_slope = ?slope_mb_per_min(&ring, cfg.slow_window),
            samples = ring.len(),
            breach = breach.map(|b| b.as_str()).unwrap_or("none"),
            "renderer_watchdog: sample"
        );

        let Some(breach) = breach else {
            if governor.clear_if_recovered(now, &cfg) {
                TELEMETRY.storming.store(false, Ordering::Relaxed);
                info!(
                    total_ws_bytes = snapshot.total_bytes,
                    "renderer_watchdog: memory recovered and the reload window is clear — \
                     clearing the storm flag"
                );
                // The third of §6 Q3's surfaces does not clear itself: the
                // in-app banner is React state, so without this edge it could
                // only ever go up. See `emit_storm_cleared`.
                emit_storm_cleared(&app, &snapshot);
            }
            continue;
        };

        let reason = breach.describe(&cfg);

        if let Some(why) = self_heal_unavailable {
            debug!(
                why,
                breach = breach.as_str(),
                "renderer_watchdog: breach observed but this runner has no webview to heal"
            );
            continue;
        }

        match governor.decide(now, &cfg) {
            HealDecision::Storm => {
                escalate_storm(&app, &cfg, &mut governor, &snapshot, breach, &reason);
            }
            HealDecision::Proceed => {
                if let Some(why) = heal(&app, &cfg, &mut governor, &snapshot, breach, &reason).await
                {
                    self_heal_unavailable = Some(why);
                    // Wipe the ring: the samples that led here can no longer
                    // drive an action, and a stale slope would re-log forever.
                    ring.clear();
                }
            }
        }
    }
}

/// Append `total` to the ring, pruning by both age and capacity.
fn push_sample(ring: &mut VecDeque<Sample>, cfg: &WatchdogConfig, now: Instant, total: u64) {
    ring.push_back(Sample {
        at: now,
        total_bytes: total,
    });
    let retention = cfg.retention();
    while let Some(front) = ring.front() {
        if now.duration_since(front.at) > retention {
            ring.pop_front();
        } else {
            break;
        }
    }
    let cap = cfg.ring_capacity();
    while ring.len() > cap {
        ring.pop_front();
    }
}

/// §6 Q3's escalation: all three surfaces, not one.
fn escalate_storm(
    app: &AppHandle,
    cfg: &WatchdogConfig,
    governor: &mut HealGovernor,
    snapshot: &Snapshot,
    breach: Breach,
    reason: &str,
) {
    let first = governor.latch_storm();
    // (b) the persistent flag the device heartbeat reports.
    TELEMETRY.storming.store(true, Ordering::Relaxed);
    if first {
        // (a) the loud log.
        error!(
            breach = breach.as_str(),
            total_ws_bytes = snapshot.total_bytes,
            processes = %snapshot.summary(),
            "renderer_watchdog: STORM — {} heal attempts within {:.0} min did not outrun the \
             leak ({reason}); stopping reloads. A runner restart is recommended.",
            cfg.max_reloads,
            cfg.reload_window.as_secs_f64() / 60.0
        );
    } else {
        warn!(
            breach = breach.as_str(),
            total_ws_bytes = snapshot.total_bytes,
            "renderer_watchdog: still breaching while storming ({reason})"
        );
    }
    // (c) the persistent in-app banner.
    emit(
        app,
        WatchdogEvent {
            kind: "storming",
            breach: breach.as_str(),
            total_ws_bytes: snapshot.total_bytes,
            countdown_secs: 0,
            reload_total: TELEMETRY.reload_total.load(Ordering::Relaxed),
            reclaimed_bytes: None,
            message: format!(
                "Renderer memory leak the reload can't outrun — restart recommended. ({reason})"
            ),
        },
    );
}

/// §6 Q3's escalation, RETRACTED: the storm ended, so retire its banner.
///
/// The counterpart to [`escalate_storm`], and the reason it exists is a defect
/// this module had while all three of §6 Q3's surfaces were "shipped". Two of
/// them are level-triggered and clear themselves — the loud log is a log, and
/// `TELEMETRY.storming` goes back to `false` right here, so the heartbeat and
/// the twin gauge fall back to 0 on their own. The third does not: the in-app
/// banner is React state, and with no event on this edge it could only ever go
/// up. A banner that can never come down is not an alarm, it is a permanent red
/// light — the same class of defect an independent review found on this plan's
/// coord half, where a device roll-up gauge could never return to 0.
///
/// Emitted from the ONE edge `HealGovernor::clear_if_recovered` reports, which
/// is true only when the latch was actually set, so this is edge-triggered on
/// the Rust side too: one event per storm, not one per healthy tick.
fn emit_storm_cleared(app: &AppHandle, snapshot: &Snapshot) {
    emit(
        app,
        WatchdogEvent {
            kind: "storm_cleared",
            // No detector is firing — that is the whole point of this arm. The
            // field is `&'static str` and not optional, so it carries the
            // absence explicitly rather than a stale breach that has stopped
            // being true.
            breach: "none",
            total_ws_bytes: snapshot.total_bytes,
            countdown_secs: 0,
            reload_total: TELEMETRY.reload_total.load(Ordering::Relaxed),
            // Nothing was reclaimed BY this event; the reclaim reports belong to
            // `"reload_result"`.
            reclaimed_bytes: None,
            message: "Renderer memory recovered — the reload storm has cleared.".to_string(),
        },
    );
}

/// What a heal may do, given the state of the recovery ladder's own latch.
///
/// Read BEFORE the countdown, which is the whole point. `trigger_ui_recovery`
/// answers `Skipped { why: "already_in_progress" }` while a peer recovery run
/// holds the latch — but [`heal`] used to reach that answer only AFTER it had
/// already `warn!`ed, emitted the `reload_warning` countdown and slept
/// `warn_secs`. A peer run can hold the latch until
/// [`webview_recovery::RECOVERY_WEDGE_AFTER_MS`] degrades it to `Wedged`, so on
/// a 30 s tick the user got a run of "reloading in 10s" countdowns with no
/// reload behind any of them and no guardrail engaged. A countdown that precedes
/// a refusal is a lie to the user.
///
/// # How long that actually is — derived, not restated
///
/// `RECOVERY_WEDGE_AFTER_MS` is the SUM of the ladder's five bounded costs
/// (`RECOVERY_BACKOFF_MAX_MS` + `WINDOW_LABEL_RELEASE_TIMEOUT_MS` +
/// `RECREATE_PONG_DEADLINE_MS` + `RELOAD_PONG_DEADLINE_MS` +
/// `COLD_BUNDLE_BOOT_ALLOWANCE_MS`), so the only honest way to state it is to
/// compute it: `the_exposure_the_gate_removes_is_derived_from_the_ladder` pins
/// both the sum and the countdown count against those constants. At today's
/// values it is **215 000 ms — 3 min 35 s — i.e. ~7 countdowns at the shipped
/// 30 s tick**.
///
/// **An earlier version of this doc, the test comment below it, and
/// `c288b8c3c`'s commit message all said "~11 min" and "~20 toasts". Both
/// numbers were wrong by ~3×, and ~11 min was not merely wrong but EXCLUDED:**
/// `webview_recovery`'s own `recovery_wedge_threshold_cannot_drift_from_the_ladder`
/// asserts `RECOVERY_WEDGE_AFTER_MS < RECOVERY_ATTEMPT_RESET_MS` (10 min), so no
/// value of those five constants can put the latch hold there. The defect this
/// gate repairs was real; its stated size was not, and overstating the thing you
/// just fixed is its own kind of false report.
///
/// # What this gate does NOT remove
///
/// The latch is read once, before the sleep, and not re-read after it. So a peer
/// that takes the latch DURING a countdown this gate has already started still
/// produces a countdown followed by `Skipped { why: "already_in_progress" }`.
/// Two things bound that residual:
///
/// * the exposure falls from up to ~3 min 35 s of repeated countdowns to a
///   single `warn_secs` window (~10 s by default), and
/// * it is the benign end of the race — the peer's own run reloads the webview,
///   so the user was told a reload was coming and a reload comes.
///
/// Closing it completely would mean re-reading the latch after the sleep and
/// retracting an event already delivered to the frontend, which a fired toast
/// cannot do. Stated here rather than left as an unstated exception, because the
/// arms below read as absolute and are not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CountdownGate {
    /// The latch is free: warn, count down, then run the ladder.
    Announce,
    /// The latch is WEDGED. Skip the countdown — nothing is going to reload —
    /// but still call the ladder, so its own wedge reporting fires once per
    /// incident and the attempt is counted against the budget.
    StraightToLadder,
    /// A peer recovery run holds the latch and is inside its legitimate time.
    /// Say nothing, sleep nothing, count nothing: it is doing our work.
    Skip,
}

/// Pure half of the gate, so the three arms are assertable without a latch or a
/// webview.
fn countdown_gate(latch: &LatchReport) -> CountdownGate {
    match (latch.in_flight, latch.wedged) {
        (false, _) => CountdownGate::Announce,
        (true, true) => CountdownGate::StraightToLadder,
        (true, false) => CountdownGate::Skip,
    }
}

/// What the post-heal sample says about the reclaim.
///
/// # Why this is not just an `i64`
///
/// It used to be: `pre.total_bytes as i64 - post.total_bytes as i64`. An
/// unreadable post sample is [`Snapshot::default`] — a Toolhelp failure returns
/// one outright, and so does every `OpenProcess` being refused mid-recreate — so
/// `post.total_bytes` was `0` and that subtraction reported the whole pre-heal
/// total as reclaimed: a MAXIMAL apparent success manufactured out of a failed
/// measurement. The user was shown *"reclaimed 1560551424 bytes
/// (1560551424 B → 0 B)"*, and because `reclaimed < min_reclaim_bytes` was then
/// false, §1.3's "the heal did not move the number" escalation **could not fire
/// on a heal that could not be measured at all**. That is precisely the
/// silent-failure shape §1.3 exists to remove — and a forced-breach acceptance
/// run, with every process opening and closing under a recreate, is when a
/// partial sample is most likely.
///
/// The honest predicate (`processes.is_empty()`) was already applied in the
/// sample loop and in [`publish_sample`]; [`heal`] was the third [`Snapshot`]
/// consumer and the one that did not get it.
///
/// # UNKNOWN neither credits a reclaim nor escalates
///
/// A deliberate choice between two options that are wrong in different
/// directions, decided on blast radius:
///
/// * **Escalating on UNKNOWN** would latch a storm — persistent banner, twin
///   flag, and no further heals — on no evidence whatever. A transient Toolhelp
///   failure would then disable the protection until a quiet tick cleared it,
///   converting one unreadable sample into an outage of the watchdog.
/// * **Not crediting and not escalating** costs at most the REMAINDER of the
///   reload budget, and only while the sample stays unreadable. The attempt has
///   already been charged (`record_attempt` runs before the settle sleep), so a
///   heal whose reclaim is never measurable still reaches
///   [`HealDecision::Storm`] via `max_reloads` inside `reload_window`: the
///   escalation is delayed by at most `max_reloads - 1` attempts, not lost.
///   Meanwhile the next tick re-samples, and a breach that is still real earns a
///   [`Self::Measured`] verdict as soon as one sample reads.
///
/// So UNKNOWN is reported loudly (a `warn!`, and `reclaimed_bytes: None` on the
/// wire) and decides nothing. What it must never do is what it did: read as a
/// total reclaim, which credits a success AND suppresses the escalation in one
/// move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReclaimVerdict {
    /// The two samples are comparable, so `bytes` is the real delta — negative
    /// when the subtree GREW across the heal.
    Measured { bytes: i64 },
    /// The post-heal sample cannot be compared with the pre-heal one, so any
    /// delta between them is an artifact of the measurement rather than a
    /// property of the reload.
    Unknown { why: &'static str },
}

impl ReclaimVerdict {
    /// The delta, or `None` when it was not measured — what rides the UI event.
    /// `WatchdogEvent::reclaimed_bytes` is an `Option` precisely so an
    /// unmeasurable reclaim has a spelling that is not a number.
    fn measured_bytes(self) -> Option<i64> {
        match self {
            Self::Measured { bytes } => Some(bytes),
            Self::Unknown { .. } => None,
        }
    }
}

/// Compare a post-heal sample with the pre-heal one. Pure, so both UNKNOWN arms
/// are assertable without a webview.
///
/// The second arm is the subtler one: a post sample that read FEWER of the same
/// subtree than the pre sample did understates the post total by whatever those
/// processes hold, so its delta OVERSTATES the reclaim — the same direction of
/// error as the empty case, just smaller. [`Snapshot::unreadable_processes`] is
/// what makes it visible, and a partial sample is exactly what a recreate
/// produces as processes come and go under it.
fn classify_reclaim(pre: &Snapshot, post: &Snapshot) -> ReclaimVerdict {
    if post.processes.is_empty() {
        return ReclaimVerdict::Unknown {
            why: "post_sample_read_nothing",
        };
    }
    if post.unreadable_processes > pre.unreadable_processes {
        return ReclaimVerdict::Unknown {
            why: "post_sample_less_readable_than_pre",
        };
    }
    ReclaimVerdict::Measured {
        bytes: pre.total_bytes as i64 - post.total_bytes as i64,
    }
}

/// Warn, wait out the countdown, run the VERIFIED recovery ladder, then report
/// the reclaim.
///
/// Returns `Some(why)` when the ladder says this runner has no webview at all,
/// which the caller latches so nothing is attempted again.
async fn heal(
    app: &AppHandle,
    cfg: &WatchdogConfig,
    governor: &mut HealGovernor,
    pre: &Snapshot,
    breach: Breach,
    reason: &str,
) -> Option<&'static str> {
    // Consult the ladder FIRST. Everything below — the loud line, the
    // countdown event, the `warn_secs` sleep — is a promise that a reload is
    // about to happen, and a peer recovery run holding the latch means it is
    // not. See [`CountdownGate`].
    let latch = webview_recovery::recovery_latch_report();
    let gate = countdown_gate(&latch);
    if gate == CountdownGate::Skip {
        debug!(
            in_flight_ms = ?latch.in_flight_ms,
            breach = breach.as_str(),
            total_ws_bytes = pre.total_bytes,
            "renderer_watchdog: a recovery run already holds the latch — no countdown, no \
             attempt; it is doing this work ({reason})"
        );
        return None;
    }

    if gate == CountdownGate::Announce {
        warn!(
            breach = breach.as_str(),
            total_ws_bytes = pre.total_bytes,
            processes = %pre.summary(),
            "renderer_watchdog: breach — {reason}. Recovering the webview in {} s.",
            cfg.warn_secs
        );

        // §6 Q2: always a visible countdown, and it elapses whether or not it
        // is acknowledged. A backgrounded or headless window must not be able
        // to defeat a load-bearing protection by inattention.
        emit(
            app,
            WatchdogEvent {
                kind: "reload_warning",
                breach: breach.as_str(),
                total_ws_bytes: pre.total_bytes,
                countdown_secs: cfg.warn_secs,
                reload_total: TELEMETRY.reload_total.load(Ordering::Relaxed),
                reclaimed_bytes: None,
                message: format!(
                    "Reclaiming renderer memory — reloading in {} s. Terminal sessions are \
                     preserved. ({reason})",
                    cfg.warn_secs
                ),
            },
        );
        tokio::time::sleep(Duration::from_secs(cfg.warn_secs)).await;
    } else {
        // `StraightToLadder`: the latch is wedged, so no reload is coming and a
        // countdown would be a lie. Fall through to the ladder anyway — it
        // reports the wedge once per incident and returns `Wedged`, which the
        // match below counts against the budget.
        warn!(
            in_flight_ms = ?latch.in_flight_ms,
            breach = breach.as_str(),
            "renderer_watchdog: recovery latch is WEDGED — skipping the countdown (nothing is \
             going to reload) and letting the ladder report it ({reason})"
        );
    }

    let outcome =
        webview_recovery::trigger_ui_recovery(app, RecoveryReason::RendererMemoryPressure).await;

    // Every attempt that actually reached the ladder counts against the
    // budget, including a failure — otherwise a ladder that fails every time
    // would be hammered once per tick.
    match outcome {
        RecoveryOutcome::Skipped {
            why: why @ ("server_mode" | "no_main_window"),
        } => {
            // Not a failure and not a reload: this runner has no webview by
            // design. Say it once, then stay quiet.
            info!(
                why,
                breach = breach.as_str(),
                "renderer_watchdog: this runner has no webview to heal — memory is still \
                 sampled and reported, but no recovery will be attempted"
            );
            return Some(why);
        }
        RecoveryOutcome::Skipped { why } => {
            // `already_in_progress` / `no_action_needed`: a recovery run is
            // already doing this work. Not ours to count, not a storm.
            debug!(
                why,
                breach = breach.as_str(),
                "renderer_watchdog: recovery ladder declined this call"
            );
            return None;
        }
        RecoveryOutcome::Wedged { in_flight_ms } => {
            error!(
                in_flight_ms,
                breach = breach.as_str(),
                "renderer_watchdog: recovery is WEDGED — the self-heal is unavailable while \
                 the in-flight run holds the latch ({reason})"
            );
            governor.record_attempt(Instant::now());
            return None;
        }
        RecoveryOutcome::Exhausted { attempts } => {
            error!(
                attempts,
                breach = breach.as_str(),
                "renderer_watchdog: recovery ladder budget is spent for this incident — \
                 escalating rather than retrying ({reason})"
            );
            governor.record_attempt(Instant::now());
            escalate_storm(app, cfg, governor, pre, breach, reason);
            return None;
        }
        RecoveryOutcome::Failed { ref detail, .. } => {
            error!(
                detail,
                breach = breach.as_str(),
                "renderer_watchdog: recovery rung FAILED — memory was not reclaimed ({reason})"
            );
            governor.record_attempt(Instant::now());
            return None;
        }
        RecoveryOutcome::Reloaded { .. } | RecoveryOutcome::Recreated { .. } => {}
    }

    // A reload or a recreate actually completed and was verified by the ladder.
    governor.record_attempt(Instant::now());
    let reload_total = TELEMETRY.reload_total.fetch_add(1, Ordering::Relaxed) + 1;

    // §1.3 / §7 — the pre/post delta. Bounded settle, then one more sample.
    tokio::time::sleep(Duration::from_secs(cfg.settle_secs)).await;
    let post = sample_webview2_subtree();
    publish_sample(&post);
    // NOT a bare subtraction: an unreadable post sample reads as a TOTAL reclaim
    // and would suppress the escalation below. See [`ReclaimVerdict`].
    let verdict = classify_reclaim(pre, &post);

    match verdict {
        ReclaimVerdict::Measured { bytes } => info!(
            outcome = outcome.as_str(),
            breach = breach.as_str(),
            pre_bytes = pre.total_bytes,
            post_bytes = post.total_bytes,
            reclaimed_bytes = bytes,
            reload_total,
            pre_processes = %pre.summary(),
            post_processes = %post.summary(),
            "renderer_watchdog: heal complete"
        ),
        // A `warn!`, not an `info!`: the reclaim figure an operator would
        // otherwise read here is the one this arm exists to refuse to print.
        ReclaimVerdict::Unknown { why } => warn!(
            why,
            outcome = outcome.as_str(),
            breach = breach.as_str(),
            pre_bytes = pre.total_bytes,
            post_bytes = post.total_bytes,
            reload_total,
            pre_processes = %pre.summary(),
            post_processes = %post.summary(),
            "renderer_watchdog: heal complete but the reclaim is UNKNOWN — the post-heal \
             sample is not comparable with the pre-heal one, so nothing is credited as \
             reclaimed and §1.3's no-reclaim escalation is not decided from it"
        ),
    }
    emit(
        app,
        WatchdogEvent {
            kind: "reload_result",
            breach: breach.as_str(),
            total_ws_bytes: post.total_bytes,
            countdown_secs: 0,
            reload_total,
            reclaimed_bytes: verdict.measured_bytes(),
            message: match verdict {
                ReclaimVerdict::Measured { bytes } => format!(
                    "Renderer reloaded — reclaimed {bytes} bytes ({} B → {} B).",
                    pre.total_bytes, post.total_bytes
                ),
                ReclaimVerdict::Unknown { .. } => {
                    "Renderer reloaded — how much memory that reclaimed could not be \
                     measured: the post-reload sample could not be read."
                        .to_string()
                }
            },
        },
    );

    // §1.3: "a reload that did not move the number is the reload-storm signal,
    // one cycle earlier and with evidence." Escalate NOW rather than spending
    // the rest of the budget discovering the same thing.
    //
    // Reachable on a MEASURED verdict ONLY. An UNKNOWN one used to answer this
    // predicate `false` with a fabricated maximal reclaim, which is the
    // suppression; it now cannot answer it at all, and the budget arm of
    // `HealGovernor::decide` remains the bound. [`ReclaimVerdict`] argues why
    // that is the right side to err on.
    if let ReclaimVerdict::Measured { bytes } = verdict {
        if bytes < cfg.min_reclaim_bytes as i64 {
            error!(
                pre_bytes = pre.total_bytes,
                post_bytes = post.total_bytes,
                reclaimed_bytes = bytes,
                min_reclaim_bytes = cfg.min_reclaim_bytes,
                "renderer_watchdog: the heal did not move the number — this is not a heap leak \
                 a reload can reclaim (native/GPU allocation or fragmentation). Escalating now \
                 instead of spending the remaining reload budget."
            );
            escalate_storm(app, cfg, governor, &post, breach, reason);
        }
    }

    None
}

// ─────────────────────── the Windows sampling plumbing ───────────────────
//
// Mirrors `health_monitor::get_memory_usage` (`GetProcessMemoryInfo` →
// `WorkingSetSize`) and reuses the `windows_sys` Toolhelp approach already in
// this crate rather than enabling `sysinfo`'s `processes` feature. The
// Toolhelp subtree walk and the per-pid working-set read are adapted from an
// abandoned July-2026 draft of this module; everything above them is not.

/// Remembers the first time each pid was observed, so `first_seen_unix_ms`
/// survives across samples, and caches the (immutable) command-line-derived
/// kind so the PEB read happens once per process, not once per tick.
#[cfg(target_os = "windows")]
static PROCESS_FACTS: LazyLock<Mutex<std::collections::HashMap<u32, (u64, WebViewProcessKind)>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Wall-clock milliseconds. Not `cfg`-gated: [`publish_sample`] stamps every
/// published sample on every platform.
fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Look up (or establish, and then remember) the first-seen stamp and kind for
/// `pid`. Entries for pids no longer in `live` are dropped so the map cannot
/// grow without bound across a long uptime.
#[cfg(target_os = "windows")]
fn process_facts(
    pid: u32,
    live: &[u32],
    classify: impl FnOnce() -> WebViewProcessKind,
) -> (u64, WebViewProcessKind) {
    let mut guard = match PROCESS_FACTS.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.retain(|k, _| live.contains(k));
    *guard.entry(pid).or_insert_with(|| (unix_ms(), classify()))
}

/// Sample every `msedgewebview2.exe` descendant of this process — and **only**
/// those — into [`Snapshot::total_bytes`].
///
/// The runner's own working set is read too, but carried BESIDE the total as
/// [`Snapshot::own_process_bytes`] and never summed into it: a reload cannot
/// reclaim the runner's own heap, so a total containing it ate the ceiling
/// budget, latched a storm that could never clear, and polluted both slope
/// series. This doc said "this process plus every descendant" — the sentence
/// retracted everywhere else in this module — while sitting on the function that
/// computes the quantity.
///
/// Returns an empty snapshot (total 0) when sampling is impossible — which the
/// caller treats as UNKNOWN and skips, never as a healthy zero.
#[cfg(target_os = "windows")]
fn sample_webview2_subtree() -> Snapshot {
    use std::collections::{HashMap, HashSet};
    use std::mem::MaybeUninit;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32First, Process32Next, PROCESSENTRY32, TH32CS_SNAPPROCESS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;

    let own_pid = unsafe { GetCurrentProcessId() };
    let own_ws = own_process_working_set();

    let mut parent_of: HashMap<u32, u32> = HashMap::new();
    let mut webview2_pids: HashSet<u32> = HashSet::new();

    // SAFETY: a Toolhelp snapshot is read through the documented
    // `Process32First`/`Process32Next` pair with a correctly sized
    // `PROCESSENTRY32`, and the handle is closed on every exit path.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Snapshot::default();
        }
        let mut entry = MaybeUninit::<PROCESSENTRY32>::uninit();
        (*entry.as_mut_ptr()).dwSize = std::mem::size_of::<PROCESSENTRY32>() as u32;
        if Process32First(snapshot, entry.as_mut_ptr()) != 0 {
            loop {
                let e = entry.assume_init_ref();
                parent_of.insert(e.th32ProcessID, e.th32ParentProcessID);
                if exe_name_is_webview2(&e.szExeFile) {
                    webview2_pids.insert(e.th32ProcessID);
                }
                if Process32Next(snapshot, entry.as_mut_ptr()) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
    }

    // The transitive closure, not the direct children: Chromium nests the
    // renderer/GPU/utility processes under the browser process, which is
    // itself our child, so a `parent == own_pid` filter would sum exactly one.
    let mut descendants: Vec<u32> = webview2_pids
        .iter()
        .copied()
        .filter(|pid| is_descendant_of(*pid, own_pid, &parent_of))
        .collect();
    descendants.sort_unstable();

    // The total is the SUBTREE ONLY. `own_ws` is carried beside it as a
    // diagnostic and never added in: a reload cannot reclaim the runner's own
    // heap, so a total containing it ate the ceiling budget, latched a storm
    // that could never clear, and polluted both slope series. Module doc,
    // "What it measures".
    let mut total = 0u64;
    let mut unreadable_processes = 0u32;
    let mut processes = Vec::with_capacity(descendants.len());
    for &pid in &descendants {
        let Some(ws) = process_working_set(pid) else {
            // COUNTED, not swallowed: an unread descendant makes the sample
            // understate memory, and understating it is a missed detection.
            unreadable_processes = unreadable_processes.saturating_add(1);
            continue;
        };
        total = total.saturating_add(ws);
        let (first_seen_unix_ms, kind) = process_facts(pid, &descendants, || {
            classify_command_line(process_command_line(pid).as_deref())
        });
        processes.push(ProcessSample {
            pid,
            kind,
            working_set_bytes: ws,
            first_seen_unix_ms,
        });
    }
    processes.sort_by_key(|p| std::cmp::Reverse(p.working_set_bytes));

    Snapshot {
        total_bytes: total,
        processes,
        own_process_bytes: own_ws,
        unreadable_processes,
    }
}

/// No WebView2 process model outside Windows. An empty snapshot reads as
/// UNKNOWN in the loop (the breach logic is skipped) and as an honest zero on
/// the heartbeat.
#[cfg(not(target_os = "windows"))]
fn sample_webview2_subtree() -> Snapshot {
    Snapshot::default()
}

/// Whether `pid` is a descendant of `ancestor`, following the parent chain.
/// Bounded, because pid reuse can produce a cycle.
#[cfg(target_os = "windows")]
fn is_descendant_of(
    pid: u32,
    ancestor: u32,
    parent_of: &std::collections::HashMap<u32, u32>,
) -> bool {
    let mut current = pid;
    for _ in 0..1024 {
        match parent_of.get(&current) {
            Some(&parent) if parent == ancestor => return true,
            Some(&parent) if parent == current || parent == 0 => return false,
            Some(&parent) => current = parent,
            None => return false,
        }
    }
    false
}

/// Case-insensitive match of a `PROCESSENTRY32.szExeFile` C-string against
/// `msedgewebview2.exe`. Reads the array as raw bytes so it is independent of
/// whether the element type is `u8` or `i8`.
#[cfg(target_os = "windows")]
fn exe_name_is_webview2<T>(sz_exe_file: &[T]) -> bool {
    let ptr = sz_exe_file.as_ptr() as *const u8;
    let cap = sz_exe_file.len();
    let mut len = 0usize;
    // SAFETY: `sz_exe_file` is a fixed-size C array; the read stops at its
    // length or the NUL terminator, whichever comes first.
    unsafe {
        while len < cap && *ptr.add(len) != 0 {
            len += 1;
        }
        let bytes = std::slice::from_raw_parts(ptr, len);
        std::str::from_utf8(bytes)
            .map(|s| s.eq_ignore_ascii_case("msedgewebview2.exe"))
            .unwrap_or(false)
    }
}

/// Working set of the current process. Mirrors
/// `health_monitor::get_memory_usage`.
#[cfg(target_os = "windows")]
fn own_process_working_set() -> u64 {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: a pseudo-handle to self plus a correctly sized out-parameter;
    // the struct is only read after a non-zero return.
    unsafe {
        let handle = GetCurrentProcess();
        let mut counters = MaybeUninit::<PROCESS_MEMORY_COUNTERS>::uninit();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        if GetProcessMemoryInfo(handle, counters.as_mut_ptr(), size) != 0 {
            counters.assume_init().WorkingSetSize as u64
        } else {
            0
        }
    }
}

/// Access rights [`process_working_set`] opens a WebView2 descendant with.
///
/// `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ` — what MSDN documents
/// `GetProcessMemoryInfo` as requiring: `PROCESS_QUERY_INFORMATION` **or**
/// `PROCESS_QUERY_LIMITED_INFORMATION`, **and** `PROCESS_VM_READ`.
///
/// # Why the `PROCESS_VM_READ` half is not an oversight
///
/// It was briefly dropped, on the theory that `GetProcessMemoryInfo` needs
/// nothing more than the query right and that the extra right was costing opens
/// — each refused open silently understating the total, i.e. costing a
/// detection. Three things contradict that theory and nothing measured supported
/// it:
///
/// * MSDN requires the right, as above.
/// * This repo's only other arbitrary-pid caller of the same API,
///   `doctor::strategies::get_memory_usage` — the shape §1.1 told this module to
///   mirror — has always asked for both.
/// * The claimed benefit cannot exist on the pids that matter. The subtree is
///   this process's own descendants, at the same user and integrity level, and
///   [`process_command_line`] opens those very pids WITH `PROCESS_VM_READ` and
///   must succeed, or classification would never return anything but
///   [`WebViewProcessKind::Unknown`]. The right being removed is one we
///   demonstrably hold wherever it is used.
///
/// The asymmetry of being wrong settles it. If the removal were wrong, every
/// descendant open fails, `processes` is empty on every tick, the loop
/// `continue`s forever and [`publish_sample`] stores nothing — a permanently
/// SILENT watchdog reporting a zero baseline, which is the failure this plan
/// exists to prevent. If keeping the right is wrong, some subset of opens fails
/// and [`Snapshot::unreadable_processes`] says so on the wire, beside the total
/// it is missing from. A blind detector is the worse failure, so this asks for
/// the documented rights and reports what it could not read.
///
/// Pinned by `the_working_set_open_keeps_the_vm_read_right_msdn_documents`.
#[cfg(target_os = "windows")]
const WORKING_SET_ACCESS: windows_sys::Win32::System::Threading::PROCESS_ACCESS_RIGHTS =
    windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION
        | windows_sys::Win32::System::Threading::PROCESS_VM_READ;

/// Working set of an arbitrary pid, or `None` when the process cannot be opened
/// (already exited, or access denied) — which the caller COUNTS as
/// [`Snapshot::unreadable_processes`].
///
/// Opens with [`WORKING_SET_ACCESS`]; that constant carries the argument for the
/// rights it names.
#[cfg(target_os = "windows")]
fn process_working_set(pid: u32) -> Option<u64> {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::OpenProcess;

    // SAFETY: the handle is checked for null, closed on both exit paths, and
    // the counters struct is read only after a non-zero return.
    unsafe {
        let handle = OpenProcess(WORKING_SET_ACCESS, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut counters = MaybeUninit::<PROCESS_MEMORY_COUNTERS>::uninit();
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = GetProcessMemoryInfo(handle, counters.as_mut_ptr(), size);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some(counters.assume_init().WorkingSetSize as u64)
    }
}

/// Read another process's command line, for the `--type=` classification only.
///
/// Windows exposes no Win32 call for this, so it goes through the documented
/// `NtQueryInformationProcess(ProcessBasicInformation)` → PEB →
/// `RTL_USER_PROCESS_PARAMETERS.CommandLine` chain with `ReadProcessMemory`.
/// **Every failure is `None`**, which classifies the process as
/// [`WebViewProcessKind::Unknown`] rather than failing the sample — the byte
/// counts, which are what the detector runs on, never depend on this.
///
/// Deliberately NOT a `Get-CimInstance Win32_Process` shell-out: that probe
/// class is what accumulated 512 stuck processes on this fleet, and this runs
/// on a 30 s timer. The result is cached per pid in [`PROCESS_FACTS`], so each
/// process is read exactly once.
#[cfg(target_os = "windows")]
fn process_command_line(pid: u32) -> Option<String> {
    use std::mem::MaybeUninit;
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Diagnostics::Debug::ReadProcessMemory;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PEB, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_VM_READ, RTL_USER_PROCESS_PARAMETERS,
    };

    // SAFETY: every pointer read below goes through `ReadProcessMemory`, which
    // validates the remote address and reports failure rather than faulting;
    // each call's return is checked before its out-parameter is assumed
    // initialised, and the process handle is closed on every path.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ, 0, pid);
        if handle.is_null() {
            return None;
        }
        let result = (|| {
            let mut pbi = MaybeUninit::<PROCESS_BASIC_INFORMATION>::uninit();
            let mut returned = 0u32;
            let status = NtQueryInformationProcess(
                handle,
                ProcessBasicInformation,
                pbi.as_mut_ptr().cast(),
                std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32,
                &mut returned,
            );
            if status != 0 {
                return None;
            }
            let peb_addr = pbi.assume_init().PebBaseAddress;
            if peb_addr.is_null() {
                return None;
            }

            let mut peb = MaybeUninit::<PEB>::uninit();
            if ReadProcessMemory(
                handle,
                peb_addr.cast(),
                peb.as_mut_ptr().cast(),
                std::mem::size_of::<PEB>(),
                std::ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            let params_addr = peb.assume_init().ProcessParameters;
            if params_addr.is_null() {
                return None;
            }

            let mut params = MaybeUninit::<RTL_USER_PROCESS_PARAMETERS>::uninit();
            if ReadProcessMemory(
                handle,
                params_addr.cast(),
                params.as_mut_ptr().cast(),
                std::mem::size_of::<RTL_USER_PROCESS_PARAMETERS>(),
                std::ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            let cmd = params.assume_init().CommandLine;
            // Bound the copy: `Length` is memory the runner did not write, and
            // the ONE thing that has to be enforced about it is PARITY. See
            // [`utf16_copy_len`] — the `.min(64 * 1024)` that used to stand here
            // was dead code (`Length` is a `u16`, so it cannot exceed 65535)
            // while the odd-length case it was meant to guard wrote one byte
            // past the buffer.
            let len_bytes = utf16_copy_len(cmd.Length);
            if len_bytes == 0 || cmd.Buffer.is_null() {
                return None;
            }
            let mut buf = vec![0u16; len_bytes / 2];
            if ReadProcessMemory(
                handle,
                cmd.Buffer.cast(),
                buf.as_mut_ptr().cast(),
                len_bytes,
                std::ptr::null_mut(),
            ) == 0
            {
                return None;
            }
            Some(String::from_utf16_lossy(&buf))
        })();
        CloseHandle(handle);
        result
    }
}

/// How many BYTES of a remote `UNICODE_STRING` may be copied into a
/// `vec![0u16; n / 2]`, given its `Length`.
///
/// Rounded DOWN to an even number, and that is the whole point. `Length` counts
/// bytes and is `u16`, so it is already bounded by 65535 — the `.min(64 * 1024)`
/// clamp that used to stand in for this could never fire. What it never
/// addressed is the case it was written for: for any ODD `Length`,
/// `vec![0u16; len / 2]` rounds down and allocates `len - 1` bytes while
/// `ReadProcessMemory` is asked to write `len`. RPM validates the REMOTE address
/// and then memcpys locally unchecked, so that is a one-byte heap overflow.
/// `Length == 1` is worse: `vec![0u16; 0]` allocates nothing and its
/// `as_mut_ptr()` is a dangling non-null pointer RPM would write a byte to.
///
/// A `Length` of 1 therefore copies 0 bytes, which the caller reads as
/// "unclassifiable" — [`WebViewProcessKind::Unknown`], the arm that already
/// exists for an unreadable command line.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn utf16_copy_len(length: u16) -> usize {
    (length as usize) & !1
}

/// The `--type=<token>` value from a Chromium command line, if present.
///
/// Reachable only from the Windows sampler; the tests exercise it on every
/// platform, which is why the classification itself is not `cfg`-gated.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn type_token(command_line: &str) -> Option<&str> {
    let rest = command_line.split("--type=").nth(1)?;
    // `split` on the delimiter set rather than `find` + `&rest[..end]`: a
    // command line read out of another process's PEB is
    // `String::from_utf16_lossy`'d, so a byte index derived from `find` is not
    // guaranteed to land on a char boundary and the slice would PANIC inside
    // the sampler. `clippy::string_slice` is denied in this crate for exactly
    // this class. `split` always yields at least one item.
    let token = rest
        .split(|c: char| c.is_whitespace() || c == '"' || c == '\'')
        .next()
        .unwrap_or_default();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Classify a WebView2 process from its command line.
///
/// `None` (unreadable) is [`WebViewProcessKind::Unknown`], NOT `Browser` —
/// the browser process is identified by the POSITIVE fact that a command line
/// was read and carried no `--type=`, never by the absence of evidence.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn classify_command_line(command_line: Option<&str>) -> WebViewProcessKind {
    let Some(cmd) = command_line else {
        return WebViewProcessKind::Unknown;
    };
    match type_token(cmd) {
        None => WebViewProcessKind::Browser,
        Some("renderer") => WebViewProcessKind::Renderer,
        Some("gpu-process") => WebViewProcessKind::Gpu,
        Some("utility") => WebViewProcessKind::Utility,
        Some("crashpad-handler") => WebViewProcessKind::Crashpad,
        Some(_) => WebViewProcessKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config whose windows are short enough to build test series against
    /// without changing the arithmetic the shipped defaults encode.
    fn cfg() -> WatchdogConfig {
        WatchdogConfig::default()
    }

    fn proc(pid: u32, kind: WebViewProcessKind, mb: u64) -> ProcessSample {
        proc_bytes(pid, kind, mb * 1024 * 1024)
    }

    /// `proc` in bytes, for the fixtures that need a figure either side of a
    /// ceiling rather than a round number of MiB.
    fn proc_bytes(pid: u32, kind: WebViewProcessKind, working_set_bytes: u64) -> ProcessSample {
        ProcessSample {
            pid,
            kind,
            working_set_bytes,
            first_seen_unix_ms: 0,
        }
    }

    /// The own-WS figure every fixture carries: a 500 MB runner beside the
    /// subtree. The number that used to be summed into `total_bytes`, and the
    /// reason a healthy box breached. Never read by the detector.
    const FIXTURE_OWN_BYTES: u64 = 523 * 1024 * 1024;

    /// Build a snapshot whose `total_bytes` is DERIVED from `processes`, and
    /// refuse to build one that does not reconcile.
    ///
    /// [`Snapshot::total_bytes`]'s doc now asserts `reconciles()`
    /// unconditionally, and the heartbeat field promises it on the wire, so a
    /// fixture naming a total independently of its own breakdown asserts against
    /// a `Snapshot` the sampler cannot produce. Every one of them did:
    /// `quiet_snapshot` took a `total_mb` that disagreed with its fixed 727 MiB
    /// breakdown at all three call values (700, 788, 800), and each hand-built
    /// ceiling fixture named a total no process in it accounted for.
    fn snapshot_of(processes: Vec<ProcessSample>) -> Snapshot {
        let total_bytes = processes
            .iter()
            .map(|p| p.working_set_bytes)
            .fold(0u64, |a, b| a.saturating_add(b));
        let snap = Snapshot {
            total_bytes,
            processes,
            own_process_bytes: FIXTURE_OWN_BYTES,
            unreadable_processes: 0,
        };
        assert!(
            snap.reconciles(),
            "a fixture that does not reconcile is not a sample the runner can produce"
        );
        snap
    }

    /// Build a ring of `count` samples spaced `tick` apart, starting at
    /// `start_mb` and growing `mb_per_min`, with `noise_mb` alternating on
    /// every other sample so a flat series is not artificially perfect.
    fn ramp(
        cfg: &WatchdogConfig,
        count: usize,
        start_mb: f64,
        mb_per_min: f64,
        noise_mb: f64,
    ) -> VecDeque<Sample> {
        let t0 = Instant::now();
        let mut ring = VecDeque::new();
        for i in 0..count {
            let dt = cfg.tick.mul_f64(i as f64);
            let minutes = dt.as_secs_f64() / 60.0;
            let noise = if i % 2 == 0 { noise_mb } else { -noise_mb };
            let mb = start_mb + mb_per_min * minutes + noise;
            ring.push_back(Sample {
                at: t0 + dt,
                total_bytes: (mb * 1024.0 * 1024.0) as u64,
            });
        }
        ring
    }

    /// A snapshot that breaches neither ceiling — so a test of the slope arms
    /// is testing the slope arms. 727 MiB across four processes, DERIVED: the
    /// total is no longer a parameter that could disagree with the breakdown.
    fn quiet_snapshot() -> Snapshot {
        snapshot_of(vec![
            proc(1, WebViewProcessKind::Browser, 91),
            proc(2, WebViewProcessKind::Gpu, 70),
            proc(3, WebViewProcessKind::Renderer, 184),
            proc(4, WebViewProcessKind::Renderer, 382),
        ])
    }

    // ── defaults ────────────────────────────────────────────────────────

    #[test]
    fn shipped_defaults_are_the_re_resolved_plan_numbers() {
        let c = WatchdogConfig::default();
        assert!(
            !c.enabled,
            "ships present-but-OFF until a live calibration run (§1.4): the only baseline \
             anyone has measured breaches BOTH ceilings on its first tick"
        );
        assert_eq!(c.tick, Duration::from_secs(30));
        assert_eq!(c.slope_mb_per_min, 30.0);
        assert_eq!(c.window, Duration::from_secs(600));
        assert_eq!(c.slow_slope_mb_per_min, 0.05);
        assert_eq!(c.slow_window, Duration::from_secs(21_600));
        assert_eq!(c.ceiling_bytes, 1_500_000_000);
        assert_eq!(c.renderer_ceiling_bytes, 1_000_000_000);
        assert_eq!(c.warn_secs, 10);
        assert_eq!(c.max_reloads, 2);
        assert_eq!(c.reload_window, Duration::from_secs(900));
    }

    #[test]
    fn ring_capacity_is_derived_from_the_longer_window() {
        // 6 h at a 30 s tick is 720 samples, not a hardcoded constant.
        let c = WatchdogConfig::default();
        assert_eq!(c.ring_capacity(), 722);
        let slower = WatchdogConfig {
            slow_window: Duration::from_secs(24 * 3600),
            ..WatchdogConfig::default()
        };
        assert_eq!(slower.ring_capacity(), 2882);
    }

    #[test]
    fn the_ring_is_bounded_even_at_the_window_and_tick_extremes() {
        // Both knobs are bounded individually and their PRODUCT was not: 30 days
        // (MAX_WINDOW_MINS) at a 1 s tick asks for 2 592 002 samples, which
        // `run`'s `VecDeque::with_capacity` would pre-allocate at ~62 MB — inside
        // a MEMORY watchdog — and `slope_mb_per_min` would then build three
        // `Vec`s of that length on every tick.
        let extreme = WatchdogConfig {
            slow_window: mins_to_duration(MAX_WINDOW_MINS),
            tick: Duration::from_secs(1),
            ..WatchdogConfig::default()
        };
        assert_eq!(extreme.unbounded_ring_capacity(), 2_592_002);
        assert_eq!(
            extreme.ring_capacity(),
            MAX_RING_SAMPLES,
            "the CAPACITY is bounded, not merely the window"
        );

        // And the bound does not bite on anything realistic — the shipped
        // defaults, or a full day at a one-second tick.
        let d = WatchdogConfig::default();
        assert_eq!(d.ring_capacity(), 722);
        assert_eq!(d.unbounded_ring_capacity(), 722);
        let day_at_one_second = WatchdogConfig {
            slow_window: Duration::from_secs(24 * 3600),
            tick: Duration::from_secs(1),
            ..WatchdogConfig::default()
        };
        assert_eq!(day_at_one_second.ring_capacity(), 86_402);
        assert!(day_at_one_second.ring_capacity() < MAX_RING_SAMPLES);
    }

    // ── slope arm: the §0 (fast) profile ────────────────────────────────

    #[test]
    fn fast_ramp_trips_the_short_arm() {
        let c = cfg();
        // §0's measured ~84 MB/min over a full 10-minute window.
        let ring = ramp(&c, 21, 300.0, 84.0, 0.0);
        let slope = slope_mb_per_min(&ring, c.window).expect("short arm ready");
        assert!((slope - 84.0).abs() < 0.5, "slope was {slope}");
        assert_eq!(
            evaluate(&ring, &quiet_snapshot(), &c),
            Some(Breach::FastSlope {
                slope_mb_per_min: slope
            })
        );
    }

    #[test]
    fn a_fast_ramp_does_not_need_the_long_arm_to_be_ready() {
        // 10 minutes of samples cannot span 6 hours; the long arm must abstain
        // rather than extrapolate, and the short arm must still fire.
        let c = cfg();
        let ring = ramp(&c, 21, 300.0, 84.0, 0.0);
        assert!(slope_mb_per_min(&ring, c.slow_window).is_none());
        assert!(matches!(
            evaluate(&ring, &quiet_snapshot(), &c),
            Some(Breach::FastSlope { .. })
        ));
    }

    // ── slope arm: the §0b (slow) profile — the 2026-09-18 incident ─────

    #[test]
    fn replayed_slow_ramp_trips_the_long_arm() {
        // §0b measured ~0.12 MB/min. 6 h of 30 s samples = 720 points, ~43 MB
        // of total growth — far below anything the short arm can see.
        let c = cfg();
        let ring = ramp(&c, 720, 700.0, 0.12, 0.0);
        assert!(
            slope_mb_per_min(&ring, c.window).unwrap() < c.slope_mb_per_min,
            "the short arm must NOT fire on the slow profile"
        );
        let slow = slope_mb_per_min(&ring, c.slow_window).expect("long arm ready");
        assert!((slow - 0.12).abs() < 0.01, "slow slope was {slow}");
        assert!(matches!(
            evaluate(&ring, &quiet_snapshot(), &c),
            Some(Breach::SlowSlope { .. })
        ));
    }

    #[test]
    fn the_slow_ramp_survives_working_set_noise() {
        // The regression, not an endpoint difference, is what makes this hold:
        // ±25 MB of sample-to-sample noise is ~35× the 0.7 MB the signal moves
        // between two adjacent samples.
        let c = cfg();
        let ring = ramp(&c, 720, 700.0, 0.12, 25.0);
        let slow = slope_mb_per_min(&ring, c.slow_window).expect("long arm ready");
        assert!((slow - 0.12).abs() < 0.02, "slow slope was {slow}");
        assert!(matches!(
            evaluate(&ring, &quiet_snapshot(), &c),
            Some(Breach::SlowSlope { .. })
        ));
    }

    // ── the quiet case: a false positive here is worse than no watchdog ──

    #[test]
    fn a_big_but_stable_baseline_trips_nothing_over_a_full_long_window() {
        // The operator's ~9-session primary: 788 MB total (§0b's measurement),
        // flat with ±25 MB of noise, for a full 6 h.
        let c = cfg();
        let ring = ramp(&c, 720, 788.0, 0.0, 25.0);
        assert!(slope_mb_per_min(&ring, c.window).unwrap().abs() < c.slope_mb_per_min);
        assert!(
            slope_mb_per_min(&ring, c.slow_window).unwrap().abs() < c.slow_slope_mb_per_min,
            "a flat noisy baseline must not read as a slow leak"
        );
        assert_eq!(evaluate(&ring, &quiet_snapshot(), &c), None);
    }

    #[test]
    fn a_short_ring_abstains_rather_than_extrapolating() {
        // Two samples 30 s apart across a 500 MB jump is 1000 MB/min — and must
        // trip nothing, because neither arm's window is spanned.
        let c = cfg();
        let t0 = Instant::now();
        let mut ring = VecDeque::new();
        ring.push_back(Sample {
            at: t0,
            total_bytes: 300 * 1024 * 1024,
        });
        ring.push_back(Sample {
            at: t0 + c.tick,
            total_bytes: 800 * 1024 * 1024,
        });
        assert!(slope_mb_per_min(&ring, c.window).is_none());
        assert_eq!(evaluate(&ring, &quiet_snapshot(), &c), None);
    }

    // ── ceilings ────────────────────────────────────────────────────────

    #[test]
    fn the_total_ceiling_fires_without_any_slope() {
        let c = cfg();
        let ring = ramp(&c, 21, 1500.0, 0.0, 0.0);
        // Two renderers summing PAST the total ceiling with neither at the
        // per-renderer one, so this stays a total-ceiling test and the fixture
        // reconciles. It used to name a 1.5 GB total behind a single 200 MiB
        // process.
        let snap = snapshot_of(vec![
            proc_bytes(3, WebViewProcessKind::Renderer, 750_000_000),
            proc_bytes(4, WebViewProcessKind::Renderer, 750_000_001),
        ]);
        assert_eq!(snap.total_bytes, 1_500_000_001);
        assert!(
            snap.worst_renderer().map(|(_, b)| b).unwrap_or(0) < c.renderer_ceiling_bytes,
            "no single process may breach, or this would be testing the other arm"
        );
        assert_eq!(
            evaluate(&ring, &snap, &c),
            Some(Breach::TotalCeiling {
                bytes: 1_500_000_001
            })
        );
    }

    #[test]
    fn the_per_renderer_ceiling_fires_while_the_total_is_healthy() {
        // §0b's actual failure mode: one renderer at its own ceiling with the
        // WebView2 total well under the total ceiling. A total-only check is
        // blind to this.
        let c = cfg();
        let ring = ramp(&c, 21, 1200.0, 0.0, 0.0);
        let snap = snapshot_of(vec![
            proc(1, WebViewProcessKind::Browser, 91),
            proc_bytes(19492, WebViewProcessKind::Renderer, 1_000_000_001),
        ]);
        assert!(snap.total_bytes < c.ceiling_bytes);
        assert_eq!(
            evaluate(&ring, &snap, &c),
            Some(Breach::RendererCeiling {
                pid: 19492,
                bytes: 1_000_000_001
            })
        );
    }

    #[test]
    fn a_huge_browser_process_is_not_mistaken_for_a_renderer() {
        let c = cfg();
        let ring = ramp(&c, 21, 1200.0, 0.0, 0.0);
        let snap = snapshot_of(vec![proc_bytes(
            1,
            WebViewProcessKind::Browser,
            1_200_000_000,
        )]);
        assert_eq!(evaluate(&ring, &snap, &c), None);
    }

    #[test]
    fn an_unclassifiable_process_still_counts_against_the_renderer_ceiling() {
        // An unreadable command line must not silently disable the check that
        // would have seen the failure mode both incidents had.
        let c = cfg();
        let ring = ramp(&c, 21, 1200.0, 0.0, 0.0);
        let snap = snapshot_of(vec![proc_bytes(
            777,
            WebViewProcessKind::Unknown,
            1_000_000_001,
        )]);
        assert!(matches!(
            evaluate(&ring, &snap, &c),
            Some(Breach::RendererCeiling { pid: 777, .. })
        ));
    }

    // ── the ring itself ─────────────────────────────────────────────────

    #[test]
    fn push_sample_prunes_by_age_and_by_capacity() {
        let c = WatchdogConfig {
            window: Duration::from_secs(60),
            slow_window: Duration::from_secs(120),
            ..WatchdogConfig::default()
        };
        let mut ring = VecDeque::new();
        let t0 = Instant::now();
        for i in 0..20 {
            push_sample(&mut ring, &c, t0 + c.tick.mul_f64(i as f64), 100);
        }
        // Retention is 120 s at a 30 s tick → at most 5 samples survive.
        assert!(ring.len() <= 5, "ring held {} samples", ring.len());
        assert!(ring.len() <= c.ring_capacity());
    }

    // ── heal governance: cooldown + storm escalation ────────────────────

    #[test]
    fn the_reload_cooldown_permits_exactly_max_reloads_per_window() {
        let c = cfg();
        let mut g = HealGovernor::default();
        let t0 = Instant::now();
        assert_eq!(g.decide(t0, &c), HealDecision::Proceed);
        g.record_attempt(t0);
        assert_eq!(g.decide(t0, &c), HealDecision::Proceed);
        g.record_attempt(t0);
        // Budget spent (max_reloads = 2).
        assert_eq!(g.decide(t0, &c), HealDecision::Storm);
    }

    #[test]
    fn the_cooldown_reopens_once_the_window_has_passed() {
        let c = cfg();
        let mut g = HealGovernor::default();
        let t0 = Instant::now();
        g.record_attempt(t0);
        g.record_attempt(t0);
        assert_eq!(g.decide(t0, &c), HealDecision::Storm);
        let later = t0 + c.reload_window + Duration::from_secs(1);
        assert_eq!(g.decide(later, &c), HealDecision::Proceed);
    }

    #[test]
    fn the_storm_latch_fires_its_loud_log_once_and_then_stays_set() {
        let mut g = HealGovernor::default();
        assert!(g.latch_storm(), "first latch reports it is new");
        assert!(!g.latch_storm(), "a second latch must not re-log");
        assert!(g.storming);
    }

    #[test]
    fn a_quiet_window_clears_the_storm_latch() {
        let c = cfg();
        let mut g = HealGovernor::default();
        let t0 = Instant::now();
        g.record_attempt(t0);
        g.latch_storm();
        // Still inside the reload window: the latch holds.
        assert!(!g.clear_if_recovered(t0 + Duration::from_secs(60), &c));
        assert!(g.storming);
        // Past it, with no attempts left: the watchdog may protect again.
        assert!(g.clear_if_recovered(t0 + c.reload_window + Duration::from_secs(1), &c));
        assert!(!g.storming);
    }

    // ── per-process classification (§1.1 / §7) ──────────────────────────

    #[test]
    fn type_token_is_extracted_from_a_chromium_command_line() {
        assert_eq!(
            type_token(r#""C:\x\msedgewebview2.exe" --type=renderer --lang=en-US"#),
            Some("renderer")
        );
        assert_eq!(
            type_token("app.exe --type=gpu-process"),
            Some("gpu-process")
        );
        assert_eq!(type_token(r#"app.exe --type=utility""#), Some("utility"));
        assert_eq!(type_token("app.exe --no-type-switch"), None);
    }

    #[test]
    fn command_lines_classify_into_the_kinds_the_breakdown_reports() {
        use WebViewProcessKind::*;
        assert_eq!(
            classify_command_line(Some(r#""x.exe" --embedded"#)),
            Browser
        );
        assert_eq!(
            classify_command_line(Some("x.exe --type=renderer")),
            Renderer
        );
        assert_eq!(classify_command_line(Some("x.exe --type=gpu-process")), Gpu);
        assert_eq!(classify_command_line(Some("x.exe --type=utility")), Utility);
        assert_eq!(
            classify_command_line(Some("x.exe --type=crashpad-handler")),
            Crashpad
        );
        assert_eq!(classify_command_line(Some("x.exe --type=ppapi")), Other);
    }

    #[test]
    fn an_unreadable_command_line_is_unknown_and_never_browser() {
        // Absence of evidence is not evidence of the browser process: calling
        // it `Browser` would exempt a leaking renderer from the per-renderer
        // ceiling on every box where the PEB read is refused.
        assert_eq!(classify_command_line(None), WebViewProcessKind::Unknown);
    }

    #[test]
    fn the_worst_renderer_ignores_labelled_non_renderers() {
        let snap = quiet_snapshot();
        assert_eq!(snap.worst_renderer(), Some((4, 382 * 1024 * 1024)));
    }

    #[test]
    fn the_process_breakdown_carries_no_content() {
        // §6 Q6: pids, a coarse kind, byte counts and a first-seen stamp — and
        // nothing else. Pinned by serializing one and reading its keys back.
        let json = serde_json::to_value(proc(19492, WebViewProcessKind::Renderer, 382)).unwrap();
        let obj = json.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["firstSeenUnixMs", "kind", "pid", "workingSetBytes"]);
    }

    // ── the UI event contract ───────────────────────────────────────────

    #[test]
    fn the_ui_event_serializes_the_camel_case_names_the_listener_reads() {
        // `src/hooks/useRendererMemoryWatchdog.ts` mirrors these names by hand,
        // so a rename here that is not made there silently drops a field on the
        // floor — which is how the whole frontend half came to be missing in the
        // first place. Pinned on the countdown arm, which carries every field.
        let json = serde_json::to_value(WatchdogEvent {
            kind: "reload_warning",
            breach: Breach::FastSlope {
                slope_mb_per_min: 84.0,
            }
            .as_str(),
            total_ws_bytes: 1_600_000_000,
            countdown_secs: 10,
            reload_total: 0,
            reclaimed_bytes: None,
            message: "Reclaiming renderer memory".to_string(),
        })
        .unwrap();
        let obj = json.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "breach",
                "countdownSecs",
                "kind",
                "message",
                "reloadTotal",
                "totalWsBytes"
            ],
            "reclaimedBytes must be ABSENT (not null) when it is None — the \
             listener types it optional on that basis"
        );
        assert_eq!(obj["kind"], "reload_warning");
        assert_eq!(obj["breach"], "fast_slope");
    }

    #[test]
    fn the_reclaim_report_adds_the_signed_delta() {
        let json = serde_json::to_value(WatchdogEvent {
            kind: "reload_result",
            breach: Breach::TotalCeiling { bytes: 1 }.as_str(),
            total_ws_bytes: 1_700_000_000,
            countdown_secs: 0,
            reload_total: 1,
            // A reload can leave the renderer BIGGER; the field is `i64` for
            // exactly that, and the listener must not clamp it at zero.
            reclaimed_bytes: Some(-4_000_000),
            message: "Renderer reloaded".to_string(),
        })
        .unwrap();
        assert_eq!(json["reclaimedBytes"], -4_000_000);
    }

    #[test]
    fn the_storm_has_a_clearing_edge_and_it_names_no_detector() {
        // §6 Q3's banner is the one surface of the three that cannot clear
        // itself: the log is a log and `TELEMETRY.storming` is level-triggered,
        // but React state only moves when an event moves it. So the clear arm
        // exists, and it must not carry a breach that has stopped being true.
        let json = serde_json::to_value(WatchdogEvent {
            kind: "storm_cleared",
            breach: "none",
            total_ws_bytes: 400_000_000,
            countdown_secs: 0,
            reload_total: 2,
            reclaimed_bytes: None,
            message: "Renderer memory recovered — the reload storm has cleared.".to_string(),
        })
        .unwrap();
        assert_eq!(json["kind"], "storm_cleared");
        assert_eq!(json["breach"], "none");
        assert!(json.get("reclaimedBytes").is_none());
    }

    #[test]
    fn the_clear_edge_fires_once_per_storm_not_once_per_healthy_tick() {
        // `emit_storm_cleared` is called from the ONE site
        // `clear_if_recovered` reports true, so the governor's edge behaviour IS
        // the event's idempotence. A healthy tick with no storm latched must
        // report nothing at all, or the listener would see a clear per tick.
        let c = cfg();
        let mut g = HealGovernor::default();
        let now = Instant::now();
        assert!(
            !g.clear_if_recovered(now, &c),
            "no storm was latched, so there is no edge to report"
        );
        g.latch_storm();
        assert!(g.clear_if_recovered(now, &c), "the storm ended: one edge");
        assert!(
            !g.clear_if_recovered(now, &c),
            "already clear — a second clear is not an edge"
        );
    }

    // ── what the detector measures (own WS is NOT in it) ────────────────

    #[test]
    fn the_total_is_the_subtree_alone_and_reconciles_with_the_breakdown() {
        // The invariant an operator relies on when diffing the two. It used to
        // be false by exactly the runner's own working set, with nothing on the
        // wire saying so — which reads as a MISSING PROCESS rather than as a
        // different quantity.
        let snap = quiet_snapshot();
        assert!(
            snap.reconciles(),
            "total {} != sum of the breakdown",
            snap.total_bytes
        );
        assert!(
            snap.own_process_bytes > 0,
            "the fixture carries an own-WS figure, so this is a real exclusion"
        );
        assert_eq!(
            snap.total_bytes,
            727 * 1024 * 1024,
            "the own-WS term must not have crept back into the total, and the fixture's total \
             is DERIVED from its breakdown rather than named beside it"
        );
    }

    #[test]
    fn own_working_set_can_never_breach_either_ceiling() {
        // The measured shape on the operator box, 2026-09-25: own 523 MiB,
        // subtree 1 488 MiB. Summed, the old total was 2 012 MiB against a
        // 1.5 GB ceiling — so a HEALTHY renderer breached, every heal
        // under-reclaimed (a reload cannot touch the runner's own heap),
        // `escalate_storm` latched, and `clear_if_recovered` could never see a
        // non-breaching tick: `renderer_reload_storming` stuck true forever.
        let c = cfg();
        let ring = ramp(&c, 21, 1_400.0, 0.0, 0.0);
        let snap = Snapshot {
            own_process_bytes: 10_000_000_000,
            ..snapshot_of(vec![
                proc(1, WebViewProcessKind::Browser, 140),
                proc(2, WebViewProcessKind::Renderer, 900),
                proc(3, WebViewProcessKind::Gpu, 107),
                proc(4, WebViewProcessKind::Renderer, 253),
            ])
        };
        assert!(snap.total_bytes < c.ceiling_bytes);
        assert_eq!(
            evaluate(&ring, &snap, &c),
            None,
            "a 10 GB own working set must not produce a breach of any kind"
        );
    }

    #[test]
    fn an_unreadable_descendant_is_counted_rather_than_swallowed() {
        // A partial sample UNDERSTATES memory, and understating it is a MISSED
        // detection — the opposite of a false positive, and the one that costs
        // an OOM. The count has to be visible beside the total it is missing
        // from, which is what `summary()` prints.
        let snap = Snapshot {
            unreadable_processes: 2,
            ..snapshot_of(vec![proc(3, WebViewProcessKind::Renderer, 200)])
        };
        assert!(snap.reconciles(), "the total still matches what WAS read");
        let line = snap.summary();
        assert!(line.contains("unreadable=2"), "summary was: {line}");
        assert!(
            line.contains(&format!("own={FIXTURE_OWN_BYTES} excluded")),
            "summary was: {line}"
        );
    }

    #[test]
    fn an_unreadable_post_heal_sample_is_unknown_not_a_total_reclaim() {
        // `Snapshot::default()` is what a Toolhelp failure returns, so
        // `post.total_bytes == 0` and the old bare subtraction reported the ENTIRE
        // pre-heal total as reclaimed — a maximal apparent success built out of a
        // failed measurement, shown to the user as
        // "reclaimed 1560551424 bytes (1560551424 B → 0 B)".
        let pre = quiet_snapshot();
        let post = Snapshot::default();
        assert_eq!(
            classify_reclaim(&pre, &post),
            ReclaimVerdict::Unknown {
                why: "post_sample_read_nothing"
            }
        );
        assert_eq!(
            classify_reclaim(&pre, &post).measured_bytes(),
            None,
            "nothing was measured, so nothing rides the wire as a reclaim"
        );
        // The figure it would otherwise have credited is the whole pre-heal total.
        assert_eq!(
            pre.total_bytes as i64 - post.total_bytes as i64,
            727 * 1024 * 1024,
            "this is the number the old arithmetic printed as reclaimed"
        );
    }

    #[test]
    fn a_less_readable_post_sample_is_unknown_because_it_understates_the_total() {
        // The subtler arm: a post sample that read FEWER of the same subtree
        // understates the post total, so its delta OVERSTATES the reclaim — same
        // direction of error as the empty case, just smaller. A recreate, with
        // processes coming and going under it, is exactly where this happens.
        let pre = quiet_snapshot();
        let post = Snapshot {
            unreadable_processes: pre.unreadable_processes + 1,
            ..quiet_snapshot()
        };
        assert_eq!(
            classify_reclaim(&pre, &post),
            ReclaimVerdict::Unknown {
                why: "post_sample_less_readable_than_pre"
            }
        );
        // Equally readable is comparable, even when neither sample is complete.
        let both_partial_pre = Snapshot {
            unreadable_processes: 3,
            ..quiet_snapshot()
        };
        let both_partial_post = Snapshot {
            unreadable_processes: 3,
            ..snapshot_of(vec![proc(1, WebViewProcessKind::Browser, 91)])
        };
        assert!(matches!(
            classify_reclaim(&both_partial_pre, &both_partial_post),
            ReclaimVerdict::Measured { .. }
        ));
    }

    #[test]
    fn unknown_neither_credits_a_reclaim_nor_decides_the_escalation() {
        // The choice recorded on `ReclaimVerdict`: UNKNOWN decides nothing. What
        // matters is that §1.3's escalation predicate is UNREACHABLE on it — the
        // `if let Measured` in `heal` — rather than being answered `false` by a
        // fabricated maximal reclaim, which is how it was suppressed.
        let c = cfg();
        let pre = quiet_snapshot();

        // A MEASURED no-reclaim still escalates: the §1.3 signal is intact.
        let ReclaimVerdict::Measured { bytes } = classify_reclaim(&pre, &quiet_snapshot()) else {
            panic!("two comparable samples must measure");
        };
        assert_eq!(bytes, 0);
        assert!(
            bytes < c.min_reclaim_bytes as i64,
            "a heal that moved nothing is still the storm signal"
        );

        // A MEASURED real reclaim does not.
        let smaller = snapshot_of(vec![proc(1, WebViewProcessKind::Browser, 91)]);
        let ReclaimVerdict::Measured { bytes } = classify_reclaim(&pre, &smaller) else {
            panic!("two comparable samples must measure");
        };
        assert!(
            bytes >= c.min_reclaim_bytes as i64,
            "636 MiB is a real reclaim"
        );

        // Neither arm is reachable from UNKNOWN, in either flavour.
        for unknown in [
            classify_reclaim(&pre, &Snapshot::default()),
            classify_reclaim(
                &pre,
                &Snapshot {
                    unreadable_processes: 1,
                    ..quiet_snapshot()
                },
            ),
        ] {
            assert!(matches!(unknown, ReclaimVerdict::Unknown { .. }));
            assert_eq!(
                unknown.measured_bytes(),
                None,
                "UNKNOWN credits nothing, and carries no number for the escalation to test"
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_working_set_open_keeps_the_vm_read_right_msdn_documents() {
        use windows_sys::Win32::System::Threading::{
            PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
        };
        // MSDN requires PROCESS_QUERY_[LIMITED_]INFORMATION *and* PROCESS_VM_READ
        // for `GetProcessMemoryInfo`, and `doctor::strategies::get_memory_usage` —
        // this repo's only other arbitrary-pid caller of it, and the shape §1.1
        // told this module to mirror — has always asked for both. The right was
        // briefly dropped on the theory that it cost opens; if that theory is
        // wrong EVERY descendant open fails, `processes` is empty on every tick
        // and the watchdog is permanently silent behind a zero baseline. Pinned so
        // the removal cannot come back unevidenced.
        assert_ne!(
            WORKING_SET_ACCESS & PROCESS_VM_READ,
            0,
            "GetProcessMemoryInfo is documented as needing PROCESS_VM_READ"
        );
        assert_ne!(WORKING_SET_ACCESS & PROCESS_QUERY_LIMITED_INFORMATION, 0);
        assert_eq!(
            WORKING_SET_ACCESS,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ,
            "exactly the two documented rights, and nothing broader"
        );
    }

    #[test]
    fn an_empty_breakdown_is_the_unknown_predicate_not_a_zero_total() {
        // `total_bytes == 0` was the only UNKNOWN guard and was UNREACHABLE on
        // Windows while own WS was in the total — a live process never reads 0,
        // so a sample that read NOTHING still looked like ~500 MB of
        // measurement. With the subtree-only total the two agree again.
        let unknown = Snapshot::default();
        assert!(unknown.processes.is_empty());
        assert_eq!(unknown.total_bytes, 0);
        assert!(unknown.reconciles());
    }

    #[test]
    fn a_latched_storm_refuses_a_further_heal_with_budget_remaining() {
        // The latch was WRITTEN and read by no decision. With max_reloads = 2, a
        // heal that reclaimed nothing escalated, latched — and the next
        // breaching tick was told `Proceed` because `attempts.len() == 1 < 2`,
        // spending the budget the log had just said it would not spend.
        let c = cfg();
        assert_eq!(c.max_reloads, 2, "the defect needs budget left over");
        let mut g = HealGovernor::default();
        let t0 = Instant::now();

        assert_eq!(g.decide(t0, &c), HealDecision::Proceed);
        g.record_attempt(t0);
        // One attempt spent of two: the budget is NOT the reason to stop.
        assert_eq!(
            g.decide(t0 + Duration::from_secs(30), &c),
            HealDecision::Proceed,
            "without a storm, one attempt of two leaves the budget open"
        );

        g.latch_storm();
        assert_eq!(
            g.decide(t0 + Duration::from_secs(60), &c),
            HealDecision::Storm,
            "a latched storm stops further heals even with budget remaining"
        );

        // And it is a stop, not a one-way door: the quiet edge still clears it.
        let after = t0 + c.reload_window + Duration::from_secs(1);
        assert!(g.clear_if_recovered(after, &c));
        assert_eq!(g.decide(after, &c), HealDecision::Proceed);
    }

    #[test]
    fn the_countdown_is_gated_on_the_ladder_before_anything_is_promised() {
        // A countdown that precedes a refusal is a lie to the user: a peer
        // recovery run can hold the latch until RECOVERY_WEDGE_AFTER_MS degrades
        // it to `Wedged`, which is a run of "reloading in 10s" toasts with no
        // reload behind any of them. How LONG that is comes from the constant and
        // not from a number typed here — this comment used to say "~11 min" and
        // "~20 toasts", both wrong by ~3×. See
        // `the_exposure_the_gate_removes_is_derived_from_the_ladder`.
        assert_eq!(
            countdown_gate(&webview_recovery::classify_latch(None)),
            CountdownGate::Announce,
            "a free latch is the only state that may promise a reload"
        );
        assert_eq!(
            countdown_gate(&webview_recovery::classify_latch(Some(1_000))),
            CountdownGate::Skip,
            "a peer run inside its time: say nothing, sleep nothing, count nothing"
        );
        assert_eq!(
            countdown_gate(&webview_recovery::classify_latch(Some(
                webview_recovery::RECOVERY_WEDGE_AFTER_MS
            ))),
            CountdownGate::StraightToLadder,
            "wedged: no countdown, but the ladder still gets called so it reports"
        );
    }

    #[test]
    fn the_exposure_the_gate_removes_is_derived_from_the_ladder() {
        // The size of the defect this gate repaired was restated as a number in
        // three places and was wrong by ~3× in all of them ("~11 min",
        // "~20 toasts"). It is computed from `webview_recovery`'s own constants
        // here so it cannot drift again, and so a future retune of any of the
        // five terms updates the claim instead of falsifying it.
        let wedge_ms = webview_recovery::RECOVERY_WEDGE_AFTER_MS;
        assert_eq!(
            wedge_ms,
            webview_recovery::RECOVERY_BACKOFF_MAX_MS
                + webview_recovery::WINDOW_LABEL_RELEASE_TIMEOUT_MS
                + webview_recovery::RECREATE_PONG_DEADLINE_MS
                + webview_recovery::RELOAD_PONG_DEADLINE_MS
                + webview_recovery::COLD_BUNDLE_BOOT_ALLOWANCE_MS,
            "the longest a peer may hold the latch is the sum of the ladder's five bounded costs"
        );
        assert_eq!(
            wedge_ms, 215_000,
            "3 min 35 s at today's values — not the ~11 min three comments claimed"
        );

        // ~11 min was not merely wrong, it was EXCLUDED by a test in this same
        // tree: `webview_recovery` asserts the wedge threshold stays strictly
        // under the incident reset, which is 10 minutes.
        assert!(
            wedge_ms < webview_recovery::RECOVERY_ATTEMPT_RESET_MS,
            "pinned by recovery_wedge_threshold_cannot_drift_from_the_ladder"
        );
        assert!(webview_recovery::RECOVERY_ATTEMPT_RESET_MS <= 10 * 60 * 1_000);

        // And the dependent arithmetic, which was wrong in the same direction.
        let tick_ms = WatchdogConfig::default().tick.as_millis() as u64;
        assert_eq!(tick_ms, 30_000);
        assert_eq!(
            wedge_ms / tick_ms,
            7,
            "~7 countdowns at the shipped tick before the latch degrades, not ~20"
        );

        // The residual the gate does NOT remove: a peer taking the latch inside
        // the countdown still yields one countdown. That is `warn_secs`, not the
        // full hold — the property the doc's "What this gate does NOT remove"
        // section states.
        let residual_ms = WatchdogConfig::default().warn_secs * 1_000;
        assert!(
            residual_ms < wedge_ms / 10,
            "the residual exposure is one warn window ({residual_ms} ms), not the hold"
        );
    }

    #[test]
    fn a_remote_command_line_length_is_masked_to_an_even_byte_count() {
        // `UNICODE_STRING.Length` is a `u16` count of BYTES from memory the
        // runner did not write. The `.min(64 * 1024)` that used to stand in for
        // this could never fire (65536 > u16::MAX), while `vec![0u16; len / 2]`
        // rounded DOWN — so an ODD length allocated `len - 1` bytes and asked
        // `ReadProcessMemory` to write `len`: a one-byte heap overflow.
        for even in [0u16, 2, 64, 4_096, u16::MAX - 1] {
            assert_eq!(utf16_copy_len(even), even as usize);
        }
        // `Length == 1` is the worst case: `vec![0u16; 0]` allocates nothing and
        // its `as_mut_ptr()` is a dangling non-null pointer. 0 bytes copied is
        // read as "unclassifiable", an arm that already exists.
        assert_eq!(utf16_copy_len(1), 0);
        assert_eq!(utf16_copy_len(3), 2);
        assert_eq!(utf16_copy_len(65_535), 65_534);
        for len in 0u16..=4_096 {
            let n = utf16_copy_len(len);
            assert!(n.is_multiple_of(2), "{len} → {n} is odd");
            assert!(n <= len as usize, "{len} → {n} grew");
            assert_eq!(
                vec![0u16; n / 2].len() * 2,
                n,
                "the buffer must hold exactly the bytes RPM is asked to write"
            );
        }
    }

    // ── env plumbing ────────────────────────────────────────────────────

    #[test]
    fn the_opt_in_gate_requires_an_explicit_truthy_token() {
        // The gate FAILED OPEN. `env_raw` trims, so `..._ENABLED=` arrived as
        // `Some("")`, which the inherited "anything not spelled falsey is on"
        // parse read as ENABLED — turning a feature that reloads the user's
        // window ON for a `.env` line with nothing after the `=`, a
        // `docker -e VAR` with no value, or a `$VAR` that failed to expand. This
        // branch's whole safety argument is that it ships present-but-off until
        // §1.4's calibration exists, so a switch that opens on an empty value
        // undoes it.
        let off = WatchdogConfig::default().enabled;
        assert!(!off, "the default this gate guards is OFF");

        // Every truthy spelling, in the casings and padding an operator writes.
        for on in ["1", "true", "TRUE", " on ", "yes", "On", "  YES  "] {
            assert!(
                parse_bool_opt_in(Some(on), off),
                "{on:?} is an explicit opt-in and must enable"
            );
        }

        // The defect itself.
        assert!(
            !parse_bool_opt_in(Some(""), off),
            "an EMPTY value is what `VAR=` produces, and what a human writing it means is unset"
        );
        assert!(
            !parse_bool_opt_in(Some("   "), off),
            "whitespace-only is the same thing once `env_raw` has trimmed it"
        );

        // Unrecognized tokens and typos: off, never on.
        for other in [
            "disabled",
            "none",
            "n",
            "fasle",
            "tru",
            "enabled",
            "2",
            "yes please",
            "-1",
        ] {
            assert!(
                !parse_bool_opt_in(Some(other), off),
                "{other:?} is not an explicit opt-in and must NOT enable"
            );
        }

        // The recognized off-spellings are off AND deliberate, so they are the
        // ones that earn no log line.
        for falsey in ["0", "false", " OFF ", "no", "No"] {
            assert!(!parse_bool_opt_in(Some(falsey), off));
            assert!(is_recognized_off(falsey), "{falsey:?} is a deliberate off");
        }
        for declined in ["", "   ", "disabled", "fasle", "tru"] {
            assert!(
                !is_recognized_off(declined),
                "{declined:?} is off by DECLINE, which is what the warn line exists to name"
            );
        }

        // Absent still means "keep the default", in both directions.
        assert!(!parse_bool_opt_in(None, false));
        assert!(parse_bool_opt_in(None, true));
        assert!(
            !parse_bool_opt_in(None, WatchdogConfig::default().enabled),
            "unset resolves to the shipped OFF default"
        );
    }

    #[test]
    fn the_numeric_knobs_are_clamped_rather_than_taken_at_face_value() {
        let d = WatchdogConfig::default();
        // MAX_RELOADS=0 made `decide` return Storm unconditionally — a silent
        // SECOND kill switch, undocumented and indistinguishable from a storm.
        assert_eq!(clamp_u64(0, 1, 1_000), 1);
        let mut g = HealGovernor::default();
        assert_eq!(
            g.decide(
                Instant::now(),
                &WatchdogConfig {
                    max_reloads: clamp_u64(0, 1, 1_000),
                    ..d.clone()
                }
            ),
            HealDecision::Proceed,
            "the clamped floor leaves one real reload rather than an instant storm"
        );
        // CEILING_BYTES=0 breached on every single tick — an unattended reload
        // loop, not a misconfiguration.
        assert_eq!(clamp_u64(0, MIN_CEILING_BYTES, u64::MAX), MIN_CEILING_BYTES);
        // MIN_RECLAIM_BYTES above i64::MAX wrapped NEGATIVE at the `as i64` in
        // `heal`, which disabled the no-reclaim check silently.
        let huge = clamp_u64(u64::MAX, 0, i64::MAX as u64);
        assert_eq!(huge, i64::MAX as u64);
        assert!(huge as i64 > 0, "the cast must not wrap negative");
        // A legal value passes through untouched.
        assert_eq!(clamp_u64(64 * 1024 * 1024, 0, i64::MAX as u64), 67_108_864);
    }

    #[test]
    fn an_absurd_but_finite_window_is_bounded_instead_of_panicking() {
        // `from_env` runs on the Tauri SETUP thread, so a panic here does not
        // misconfigure a watchdog — it kills startup. `1e300` is finite and
        // positive, so every other filter passes it straight to
        // `Duration::from_secs_f64`, which panics on overflow.
        let capped = mins_to_duration(1e300);
        assert_eq!(capped, Duration::from_secs_f64(MAX_WINDOW_MINS * 60.0));
        assert_eq!(mins_to_duration(f64::MAX), capped);
        // Ordinary values are untouched, including the shipped defaults.
        assert_eq!(mins_to_duration(10.0), Duration::from_secs(600));
        assert_eq!(mins_to_duration(360.0), Duration::from_secs(21_600));
        assert_eq!(mins_to_duration(0.5), Duration::from_secs(30));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn exe_name_match_is_case_insensitive() {
        let mut buf = [0u8; 260];
        let name = b"MsEdgeWebView2.exe";
        buf[..name.len()].copy_from_slice(name);
        assert!(exe_name_is_webview2(&buf));

        let mut other = [0u8; 260];
        let n2 = b"chrome.exe";
        other[..n2.len()].copy_from_slice(n2);
        assert!(!exe_name_is_webview2(&other));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_subtree_walk_follows_grandchildren() {
        // The whole point of §1.1: a `parent == own_pid` filter sums ONE
        // process. runner → browser → renderer must resolve as a descendant.
        let parent_of: std::collections::HashMap<u32, u32> =
            [(100, 1), (200, 100), (300, 200), (400, 1)]
                .into_iter()
                .collect();
        assert!(is_descendant_of(300, 100, &parent_of));
        assert!(is_descendant_of(200, 100, &parent_of));
        assert!(!is_descendant_of(400, 100, &parent_of));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_subtree_walk_terminates_on_a_pid_reuse_cycle() {
        let parent_of: std::collections::HashMap<u32, u32> =
            [(10, 20), (20, 10)].into_iter().collect();
        assert!(!is_descendant_of(10, 999, &parent_of));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_own_process_working_set_is_readable() {
        assert!(own_process_working_set() > 0);
    }
}
